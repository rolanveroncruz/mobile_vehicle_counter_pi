//! Shared database connection setup for both servers.
//!
//! `data_collector` and `frontend_api` are separate processes that share one
//! SQLite file. They must open it with identical settings, so the setup lives
//! here instead of being duplicated in each `run_server()`.

use std::time::Duration;

use sea_orm::sqlx::sqlite::{SqliteJournalMode, SqliteSynchronous};
use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbErr};

/// How long a statement waits for a competing writer before failing with
/// `SQLITE_BUSY` ("database is locked"). With WAL only writers contend, so
/// this only matters when the API writes (users/locations/sublocations) at the
/// same moment the collector is inserting an event.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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
/// Whether a missing file is created is still controlled by the `mode=` part of
/// the URL (`rwc` creates it, `rw` fails fast).
pub async fn connect(db_url: &str) -> Result<DatabaseConnection, DbErr> {
    let mut options = ConnectOptions::new(db_url.to_owned());
    options.map_sqlx_sqlite_opts(|sqlite_options| {
        sqlite_options
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT)
            .foreign_keys(true)
    });

    Database::connect(options).await
}
