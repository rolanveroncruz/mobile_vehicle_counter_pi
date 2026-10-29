//! Shared database connection setup for both servers.
//!
//! `run_data_collector` and `run_frontend_api` are separate processes that share one
//! SQLite file. They must open it with identical settings, so the setup lives
//! here instead of being duplicated in each `run_server()`.

use std::path::PathBuf;
use std::time::Duration;

use migration::{Migrator, MigratorTrait};
use sea_orm::sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous};
use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};

/// How long a statement waits for a competing writer before failing with
/// `SQLITE_BUSY` ("database is locked"). With WAL only writers contend, so
/// this only matters when the API writes (users/locations/sublocations) at the
/// same moment the collector is inserting an event.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// What `connect` should do when the SQLite database file does not exist.
///
/// This is decided in code, not in `DATABASE_URL`: the URL in `.env` keeps
/// `mode=rw`, and only a caller that explicitly asks for
/// [`MissingDatabase::CreateAndMigrate`] gets the file created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingDatabase {
    /// Fail at startup with `unable to open database file`.
    Fail,
    /// Create the parent directory and the database file, then run all pending
    /// migrations. Used by the data collector.
    CreateAndMigrate,
}

/// Connects to the SQLite database at `db_url` and configures every
/// connection for two processes sharing one file:
///
/// * `journal_mode = WAL`: readers (the API) and the writer (the collector) no
///   longer block each other. WAL is stored in the database file, but we set
///   it on every connection anyway so it holds even for a freshly created or
///   restored file.
/// * `synchronous = NORMAL`: the recommended companion to WAL. Safe against an
///   application crash; only a power loss / OS crash can lose the most recent
///   commits (the database itself stays consistent).
/// * `busy_timeout`: wait briefly for the write lock instead of failing at once.
/// * `foreign_keys = ON`: the schema relies on the RESTRICT/CASCADE rules.
///
/// These are applied as PRAGMAs, **not** as URL query parameters: sqlx rejects
/// unknown parameters such as `journal_mode` and refuses to start.
///
/// `on_missing` decides what happens when the database file does not exist
/// (with `CreateAndMigrate`, leftover `-wal`/`-shm` files next to a missing
/// database are an error, and creating a new database is logged loudly):
///
/// * [`MissingDatabase::Fail`]: a missing file is always an error, even if the
///   URL says `mode=rwc` (`create_if_missing(false)` overrides the URL).
/// * [`MissingDatabase::CreateAndMigrate`]: the parent directory is created,
///   the file is created (`create_if_missing`, overriding `mode=rw` from the
///   URL), and `Migrator::up` is run. Migrations only apply when something is
///   pending, so this is also safe for an existing, up-to-date database, and it
///   brings an empty/unmigrated file up to date.
pub async fn connect(
    db_url: &str,
    on_missing: MissingDatabase,
) -> Result<DatabaseConnection, DbErr> {
    let create = on_missing == MissingDatabase::CreateAndMigrate;
    let mut created_path: Option<PathBuf> = None;

    if create {
        if let Some(path) = sqlite_file_path(db_url) {
            // Refuse to build a new database on top of leftover WAL files.
            check_no_leftover_files(&path)?;
            if !path.exists() {
                created_path = Some(path);
            }
        }
        create_parent_dir(db_url)?;
    }

    let mut options = ConnectOptions::new(db_url.to_owned());
    options.map_sqlx_sqlite_opts(move |sqlite_options| {
        sqlite_options
            .create_if_missing(create)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT)
            .foreign_keys(true)
    });

    let db_conn = Database::connect(options).await?;

    if create {
        Migrator::up(&db_conn, None).await?;
    }

    if let Some(path) = created_path {
        let shown = std::path::absolute(&path).unwrap_or(path);
        eprintln!("============================================================");
        eprintln!("WARNING: database file not found. A NEW, EMPTY database was");
        eprintln!("created and migrated at: {}", shown.display());
        eprintln!("It contains the seeded admin user and no locations/cameras.");
        eprintln!("If this was not intended, check DATABASE_URL.");
        eprintln!("============================================================");
    }

    Ok(db_conn)
}

/// The SQLite side files that sit next to a WAL-mode database.
const SIDE_FILE_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

/// Returns the `-wal` / `-shm` files that exist next to `db_path`, but only when
/// the database file itself is missing. Side files next to an existing database
/// are normal (WAL mode) and are not reported.
fn leftover_side_files(db_path: &std::path::Path) -> Vec<PathBuf> {
    if db_path.exists() {
        return Vec::new();
    }
    SIDE_FILE_SUFFIXES
        .iter()
        .map(|suffix| {
            let mut name = db_path.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name)
        })
        .filter(|p| p.exists())
        .collect()
}

/// Fails if the database file is missing but its WAL side files are not.
///
/// A stale `-wal` file next to a freshly created database can be replayed into
/// it or confuse SQLite, so the operator must delete them and restart.
fn check_no_leftover_files(db_path: &std::path::Path) -> Result<(), DbErr> {
    let leftovers = leftover_side_files(db_path);
    if leftovers.is_empty() {
        return Ok(());
    }
    let list = leftovers
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(DbErr::Custom(format!(
        "database file {} is missing, but leftover SQLite files exist: {list}. \
         Delete the leftover file(s) and restart the collector.",
        db_path.display()
    )))
}

/// Creates the directory that will contain the database file, since SQLite
/// creates the file but never its directory.
///
/// Only plain `sqlite:` file URLs are handled. In-memory URLs are ignored.
fn create_parent_dir(db_url: &str) -> Result<(), DbErr> {
    let Some(path) = sqlite_file_path(db_url) else {
        return Ok(());
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DbErr::Custom(format!(
                    "could not create database directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
    }
    Ok(())
}

/// Extracts the file path from a `sqlite://path?query` or `sqlite:path?query`
/// URL. Returns `None` for in-memory databases.
fn sqlite_file_path(db_url: &str) -> Option<PathBuf> {
    let rest = db_url.strip_prefix("sqlite:")?;
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let path = rest.split('?').next().unwrap_or("");

    if path.is_empty() || path == ":memory:" || rest.contains("mode=memory") {
        return None;
    }
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_relative_path_with_query() {
        assert_eq!(
            sqlite_file_path("sqlite://server_db/server.db?mode=rw"),
            Some(PathBuf::from("server_db/server.db"))
        );
    }

    #[test]
    fn parses_without_slashes() {
        assert_eq!(
            sqlite_file_path("sqlite:server_db/server.db"),
            Some(PathBuf::from("server_db/server.db"))
        );
    }

    #[test]
    fn parses_absolute_path() {
        assert_eq!(
            sqlite_file_path("sqlite:///var/lib/app/server.db?mode=rw"),
            Some(PathBuf::from("/var/lib/app/server.db"))
        );
    }

    #[test]
    fn ignores_in_memory() {
        assert_eq!(sqlite_file_path("sqlite::memory:"), None);
        assert_eq!(sqlite_file_path("sqlite://:memory:"), None);
    }

    #[test]
    fn ignores_non_sqlite() {
        assert_eq!(sqlite_file_path("postgres://localhost/db"), None);
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("db_unit_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_leftovers_when_nothing_exists() {
        let dir = scratch_dir("none");
        assert!(check_no_leftover_files(&dir.join("s.db")).is_ok());
    }

    #[test]
    fn leftover_wal_without_db_is_an_error() {
        let dir = scratch_dir("wal");
        std::fs::write(dir.join("s.db-wal"), b"x").unwrap();
        let err = check_no_leftover_files(&dir.join("s.db"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("s.db-wal"), "{err}");
        assert!(err.contains("Delete"), "{err}");
    }

    #[test]
    fn leftover_shm_without_db_is_an_error() {
        let dir = scratch_dir("shm");
        std::fs::write(dir.join("s.db-shm"), b"x").unwrap();
        assert!(check_no_leftover_files(&dir.join("s.db")).is_err());
    }

    #[test]
    fn side_files_next_to_existing_db_are_fine() {
        let dir = scratch_dir("ok");
        std::fs::write(dir.join("s.db"), b"x").unwrap();
        std::fs::write(dir.join("s.db-wal"), b"x").unwrap();
        std::fs::write(dir.join("s.db-shm"), b"x").unwrap();
        assert!(check_no_leftover_files(&dir.join("s.db")).is_ok());
    }
}
