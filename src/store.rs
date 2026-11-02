//! The Pi's local SQLite database: the only place events live.
//!
//! Each event is one row. The `body` column holds the **exact request-body bytes**
//! that are sent to the collector, serialised once when the event was accepted.
//!
//! Rows are only ever inserted and updated. Nothing in this crate deletes them
//! (the operator clears old image data by hand).
//!
//! The database is opened with `journal_mode = WAL` and `synchronous = FULL`, so a
//! committed event survives a power cut. The schema is versioned with
//! `PRAGMA user_version` and the ordered list in [`MIGRATIONS`].

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_DELIVERED: &str = "delivered";
pub const STATUS_FAILED: &str = "failed";

/// Ordered schema steps. Step `n` takes the database from `user_version = n-1` to
/// `n`. Append new steps; never edit one that has shipped.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE events (
        event_id        TEXT PRIMARY KEY,
        status          TEXT NOT NULL DEFAULT 'pending'
                        CHECK (status IN ('pending', 'delivered', 'failed')),
        body            BLOB NOT NULL,
        attempts        INTEGER NOT NULL DEFAULT 0,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        delivered_at    TEXT,
        next_attempt_at INTEGER NOT NULL DEFAULT 0,
        last_error      TEXT
    );
    CREATE INDEX idx_events_due ON events (status, next_attempt_at, created_at);
"];

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Milliseconds since the Unix epoch. Used for `next_attempt_at`.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// An event that is ready to be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueEvent {
    pub event_id: String,
    /// The exact bytes to POST.
    pub body: Vec<u8>,
    /// Attempts made so far.
    pub attempts: i64,
}

/// A whole row, for tests and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub event_id: String,
    pub status: String,
    pub body: Vec<u8>,
    pub attempts: i64,
    pub created_at: String,
    pub delivered_at: Option<String>,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub pending: i64,
    pub delivered: i64,
    pub failed: i64,
}

/// A cheap-to-clone handle to the database. Calls are synchronous; from async
/// code run them with `tokio::task::spawn_blocking`.
#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    /// Opens (creating if needed) the database at `path`, creating its parent
    /// directory, applying the pragmas and any pending schema steps.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "could not create database directory {}: {error}",
                        parent.display()
                    )
                })?;
            }
        }
        Self::init(Connection::open(path)?)
    }

    /// An in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self, Box<dyn Error + Send + Sync>> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, Box<dyn Error + Send + Sync>> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")?;
        migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic in another thread cannot leave the database half-written (every
        // change is a single statement), so a poisoned lock is safe to reuse.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Stores a new event, ready to send. The insert is committed before this
    /// returns.
    pub fn insert_pending(&self, event_id: &str, body: &[u8]) -> rusqlite::Result<()> {
        self.lock().execute(
            "INSERT INTO events (event_id, body) VALUES (?1, ?2)",
            params![event_id, body],
        )?;
        Ok(())
    }

    /// Stores something that could not become an event (for example camera JSON
    /// that did not parse). It is never sent; it is kept for inspection.
    pub fn insert_failed(&self, event_id: &str, raw: &[u8], reason: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "INSERT INTO events (event_id, status, body, last_error) VALUES (?1, 'failed', ?2, ?3)",
            params![event_id, raw, reason],
        )?;
        Ok(())
    }

    /// The oldest `pending` event whose `next_attempt_at` has passed.
    pub fn next_due(&self, now_ms: i64) -> rusqlite::Result<Option<DueEvent>> {
        self.lock()
            .query_row(
                "SELECT event_id, body, attempts FROM events
                 WHERE status = 'pending' AND next_attempt_at <= ?1
                 ORDER BY created_at, rowid
                 LIMIT 1",
                params![now_ms],
                |row| {
                    Ok(DueEvent {
                        event_id: row.get(0)?,
                        body: row.get(1)?,
                        attempts: row.get(2)?,
                    })
                },
            )
            .optional()
    }

    /// Verified delivery. Only a `pending` row can become `delivered`.
    pub fn mark_delivered(&self, event_id: &str) -> rusqlite::Result<bool> {
        let changed = self.lock().execute(
            "UPDATE events
             SET status = 'delivered', attempts = attempts + 1, last_error = NULL,
                 delivered_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE event_id = ?1 AND status = 'pending'",
            params![event_id],
        )?;
        Ok(changed == 1)
    }

    /// Not confirmed: stays `pending`, tried again at `next_attempt_at_ms`.
    pub fn schedule_retry(
        &self,
        event_id: &str,
        next_attempt_at_ms: i64,
        reason: &str,
    ) -> rusqlite::Result<bool> {
        let changed = self.lock().execute(
            "UPDATE events
             SET attempts = attempts + 1, next_attempt_at = ?2, last_error = ?3
             WHERE event_id = ?1 AND status = 'pending'",
            params![event_id, next_attempt_at_ms, reason],
        )?;
        Ok(changed == 1)
    }

    /// Retrying the same bytes can never work. The row is kept, marked `failed`.
    pub fn mark_failed(&self, event_id: &str, reason: &str) -> rusqlite::Result<bool> {
        let changed = self.lock().execute(
            "UPDATE events
             SET status = 'failed', attempts = attempts + 1, last_error = ?2
             WHERE event_id = ?1 AND status = 'pending'",
            params![event_id, reason],
        )?;
        Ok(changed == 1)
    }

    pub fn counts(&self) -> rusqlite::Result<Counts> {
        let conn = self.lock();
        let count = |status: &str| -> rusqlite::Result<i64> {
            conn.query_row(
                "SELECT COUNT(*) FROM events WHERE status = ?1",
                params![status],
                |row| row.get(0),
            )
        };
        Ok(Counts {
            pending: count(STATUS_PENDING)?,
            delivered: count(STATUS_DELIVERED)?,
            failed: count(STATUS_FAILED)?,
        })
    }

    pub fn get(&self, event_id: &str) -> rusqlite::Result<Option<StoredEvent>> {
        self.lock()
            .query_row(
                "SELECT event_id, status, body, attempts, created_at, delivered_at,
                        next_attempt_at, last_error
                 FROM events WHERE event_id = ?1",
                params![event_id],
                row_to_stored,
            )
            .optional()
    }

    /// Every row, oldest first.
    pub fn all(&self) -> rusqlite::Result<Vec<StoredEvent>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT event_id, status, body, attempts, created_at, delivered_at,
                    next_attempt_at, last_error
             FROM events ORDER BY created_at, rowid",
        )?;
        let rows = statement.query_map([], row_to_stored)?;
        rows.collect()
    }
}

fn row_to_stored(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    Ok(StoredEvent {
        event_id: row.get(0)?,
        status: row.get(1)?,
        body: row.get(2)?,
        attempts: row.get(3)?,
        created_at: row.get(4)?,
        delivered_at: row.get(5)?,
        next_attempt_at: row.get(6)?,
        last_error: row.get(7)?,
    })
}

fn migrate(conn: &Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > MIGRATIONS.len() as i64 {
        return Err(format!(
            "the database has schema version {version}, but this program only knows up to {}. \
             Use a newer program.",
            MIGRATIONS.len()
        )
        .into());
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        // One transaction per step. If it fails, the connection is dropped by the
        // caller and SQLite rolls the step back.
        conn.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {}; COMMIT;",
            index + 1
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("pi_store_{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn db(&self) -> PathBuf {
            self.0.join("nested").join("pi.db")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    /// Bytes that would not survive a round trip through text or JSON handling.
    fn awkward_bytes() -> Vec<u8> {
        let mut bytes = br#"{ "a" :  1,"b":"x"}"#.to_vec();
        bytes.extend_from_slice(b"\n\r\n\t  \xff\xfe\x00 trailing ");
        bytes
    }

    #[test]
    fn new_events_are_pending_and_due_immediately() {
        let store = store();
        store.insert_pending("e1", b"body").unwrap();
        let row = store.get("e1").unwrap().unwrap();
        assert_eq!(row.status, STATUS_PENDING);
        assert_eq!(row.attempts, 0);
        assert_eq!(row.next_attempt_at, 0);
        assert_eq!(row.delivered_at, None);
        assert_eq!(row.last_error, None);
        assert!(row.created_at.ends_with('Z'), "{}", row.created_at);
        assert_eq!(store.next_due(now_ms()).unwrap().unwrap().event_id, "e1");
    }

    #[test]
    fn the_stored_body_is_returned_byte_for_byte() {
        let store = store();
        let bytes = awkward_bytes();
        store.insert_pending("e1", &bytes).unwrap();
        assert_eq!(store.get("e1").unwrap().unwrap().body, bytes);
        let first = store.next_due(now_ms()).unwrap().unwrap().body;
        let second = store.next_due(now_ms()).unwrap().unwrap().body;
        assert_eq!(first, bytes);
        assert_eq!(first, second, "a retry must send identical bytes");
    }

    #[test]
    fn the_same_event_id_cannot_be_stored_twice() {
        let store = store();
        store.insert_pending("e1", b"one").unwrap();
        assert!(store.insert_pending("e1", b"two").is_err());
        assert!(store.insert_failed("e1", b"three", "x").is_err());
        assert_eq!(store.get("e1").unwrap().unwrap().body, b"one");
    }

    #[test]
    fn a_committed_event_survives_reopening_the_database() {
        let temp = TempDir::new();
        {
            let store = Store::open(temp.db()).unwrap();
            store.insert_pending("e1", &awkward_bytes()).unwrap();
            store.schedule_retry("e1", 12345, "boom").unwrap();
        }
        let store = Store::open(temp.db()).unwrap();
        let row = store.get("e1").unwrap().unwrap();
        assert_eq!(row.body, awkward_bytes());
        assert_eq!(row.attempts, 1);
        assert_eq!(row.next_attempt_at, 12345);
        assert_eq!(row.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn open_creates_the_parent_directory_and_sets_the_schema_version() {
        let temp = TempDir::new();
        let store = Store::open(temp.db()).unwrap();
        assert!(temp.db().exists());
        let version: i64 = store
            .lock()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let mode: String = store
            .lock()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let sync: i64 = store
            .lock()
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sync, 2, "synchronous = FULL");
    }

    #[test]
    fn reopening_an_up_to_date_database_changes_nothing() {
        let temp = TempDir::new();
        Store::open(temp.db())
            .unwrap()
            .insert_pending("e1", b"x")
            .unwrap();
        let again = Store::open(temp.db()).unwrap();
        assert_eq!(again.counts().unwrap().pending, 1);
    }

    #[test]
    fn a_database_from_a_newer_program_is_refused() {
        let temp = TempDir::new();
        let path = temp.db();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 999;")
            .unwrap();
        let error = Store::open(&path).err().expect("must refuse").to_string();
        assert!(error.contains("999"), "{error}");
    }

    #[test]
    fn the_status_column_only_accepts_the_three_statuses() {
        let store = store();
        let result = store.lock().execute(
            "INSERT INTO events (event_id, status, body) VALUES ('e', 'weird', x'00')",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn next_due_returns_the_oldest_pending_event_first() {
        let store = store();
        for id in ["a", "b", "c"] {
            store.insert_pending(id, id.as_bytes()).unwrap();
        }
        let mut order = Vec::new();
        while let Some(due) = store.next_due(now_ms()).unwrap() {
            order.push(due.event_id.clone());
            assert!(store.mark_delivered(&due.event_id).unwrap());
        }
        assert_eq!(order, ["a", "b", "c"]);
    }

    #[test]
    fn events_waiting_for_a_retry_are_skipped_without_blocking_newer_ones() {
        let store = store();
        store.insert_pending("old", b"1").unwrap();
        store.insert_pending("new", b"2").unwrap();
        let now = now_ms();
        store.schedule_retry("old", now + 60_000, "later").unwrap();

        assert_eq!(store.next_due(now).unwrap().unwrap().event_id, "new");
        store.mark_delivered("new").unwrap();
        assert_eq!(store.next_due(now).unwrap(), None);
        assert_eq!(
            store.next_due(now + 60_000).unwrap().unwrap().event_id,
            "old"
        );
    }

    #[test]
    fn delivered_and_failed_events_are_never_due() {
        let store = store();
        store.insert_pending("ok", b"1").unwrap();
        store.insert_pending("bad", b"2").unwrap();
        store.insert_failed("raw", b"3", "unparseable").unwrap();
        store.mark_delivered("ok").unwrap();
        store.mark_failed("bad", "HTTP 409").unwrap();
        assert_eq!(store.next_due(i64::MAX).unwrap(), None);
    }

    #[test]
    fn outcomes_update_the_row_and_never_delete_it() {
        let store = store();
        for id in ["delivered", "failed", "retried"] {
            store.insert_pending(id, id.as_bytes()).unwrap();
        }
        assert!(store.mark_delivered("delivered").unwrap());
        assert!(store.mark_failed("failed", "HTTP 409: conflict").unwrap());
        assert!(store.schedule_retry("retried", 99, "HTTP 503").unwrap());

        assert_eq!(store.all().unwrap().len(), 3, "no row may disappear");
        let delivered = store.get("delivered").unwrap().unwrap();
        assert_eq!(delivered.status, STATUS_DELIVERED);
        assert_eq!(delivered.attempts, 1);
        assert!(delivered.delivered_at.is_some());
        let failed = store.get("failed").unwrap().unwrap();
        assert_eq!(failed.status, STATUS_FAILED);
        assert_eq!(failed.last_error.as_deref(), Some("HTTP 409: conflict"));
        assert_eq!(failed.delivered_at, None);
        let retried = store.get("retried").unwrap().unwrap();
        assert_eq!(retried.status, STATUS_PENDING);
        assert_eq!(retried.attempts, 1);
        assert_eq!(retried.next_attempt_at, 99);
        assert_eq!(
            store.counts().unwrap(),
            Counts {
                pending: 1,
                delivered: 1,
                failed: 1
            }
        );
    }

    #[test]
    fn only_pending_rows_can_be_changed_by_an_outcome() {
        let store = store();
        store.insert_pending("e1", b"x").unwrap();
        store.mark_delivered("e1").unwrap();
        assert!(!store.mark_failed("e1", "late").unwrap());
        assert!(!store.schedule_retry("e1", 5, "late").unwrap());
        assert!(!store.mark_delivered("e1").unwrap());
        let row = store.get("e1").unwrap().unwrap();
        assert_eq!(row.status, STATUS_DELIVERED);
        assert_eq!(row.attempts, 1);
        assert!(!store.mark_delivered("missing").unwrap());
    }

    #[test]
    fn rows_stored_as_failed_keep_the_raw_bytes_and_the_reason() {
        let store = store();
        store
            .insert_failed("raw", b"{not json", "unparseable camera payload")
            .unwrap();
        let row = store.get("raw").unwrap().unwrap();
        assert_eq!(row.status, STATUS_FAILED);
        assert_eq!(row.body, b"{not json");
        assert_eq!(row.attempts, 0);
        assert_eq!(
            row.last_error.as_deref(),
            Some("unparseable camera payload")
        );
    }

    #[test]
    fn no_source_file_contains_a_delete_statement() {
        // The needle is assembled at run time so that this test does not match itself.
        let needle = ["delete", " from"].concat();
        for (name, source) in [
            ("store.rs", include_str!("store.rs")),
            ("sender.rs", include_str!("sender.rs")),
            ("ingest.rs", include_str!("ingest.rs")),
        ] {
            assert!(
                !source.to_lowercase().contains(&needle),
                "{name} must never delete rows"
            );
        }
    }
}
