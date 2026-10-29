//! Shared database connection setup for both servers.
//!
//! `data_collector` and `frontend_api` are separate processes that share one
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
/// `on_missing` decides what happens when the database file does not exist:
///
/// * [`MissingDatabase::Fail`]: the connection is opened exactly as the URL
///   says (`mode=rw` fails fast on a missing file).
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

    if create {
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

    Ok(db_conn)
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
}
