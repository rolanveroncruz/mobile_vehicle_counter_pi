# TODO

Known items that were deliberately deferred. Nothing here is blocking.

## Security

- [ ] **Default admin credentials.** The first migration seeds `admin@admin` / `asdf`
      (`migration/src/m20261017_064037_create_users.rs`). Because the collector
      re-creates and re-migrates a deleted database, this user comes back after every
      "reset after testing". Change or remove it before go-live. Ideas: read the initial
      admin password from an env var, or force a password change on first login.
- [ ] **`DATABASE_URL` is printed at collector startup** (`println!("DATABASE_URL is:...")`
      in `src/collector_server.rs`). Harmless for a SQLite path, but it would leak
      credentials if the database ever changes to Postgres/MySQL. Remove it or redact it.
- [ ] **`.env` is committed to git** and contains `JWT_SECRET`. Replace the committed file
      with a `.env.example`, add `.env` to `.gitignore`, and rotate the secret.
- [ ] **The collector endpoint (`POST /api/alpr/event`) has no authentication.**
      See "Authentication" in the README.

## Testing

- [ ] **Integration tests bypass `db::connect`.** `tests/location_test.rs` and
      `tests/sublocations_test.rs` call `Database::connect` directly, so they run without
      WAL and without migrations. They also depend on a pre-existing database that already
      contains the seeded `admin@admin` user, so they fail against a freshly wiped database
      until the collector has been started once. Switch them to a temporary database created
      through `db::connect(.., MissingDatabase::CreateAndMigrate)`.

## Operations

- [ ] **Concurrent collectors are unsupported.** Only one collector may run; two started at
      once against a missing database can race in `Migrator::up`
      (`UNIQUE constraint failed: seaql_migrations.version`). Fine for the planned
      single-service deployment; revisit if that ever changes.
- [ ] **`migrate_data` is a one-off** that hard-codes `server_db/server.db` and `out/alpr_events.db`
      and needs the tables to exist. After a database reset, start the collector once
      (so it migrates) before running it.
