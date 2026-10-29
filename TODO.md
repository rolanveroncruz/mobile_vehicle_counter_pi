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
- [ ] **The collector endpoint (`POST /api/alpr/event`) has no authentication in the Rust
      code.** The shared secret is enforced by nginx (`deploy/nginx-collector.conf`), so this is
      only safe while the collector listens on `127.0.0.1` behind nginx. If it is ever exposed
      directly, add a token check in the handler. See "Authentication" in the README.

## Correctness

- [ ] **Single time zone assumption (`+08:00`).** Event dates are stored as text and
      compared as text by SQLite, so they are all normalised to `+08:00` (`src/time.rs`),
      by the collector when saving and by the API for query bounds. This is correct only
      while the whole system is in one time zone (Philippines). If cameras or users ever span
      several zones, review this properly: either store UTC plus a separate zone, or filter
      by instant instead of by text. Existing rows would then need a one-off conversion.
- [ ] **Dates written outside the collector are not normalised.** `migrate_data` and
      hand-inserted rows go straight to SQLite. They must already be `+08:00`, or they will be
      mis-ordered and mis-filtered. Consider a CHECK constraint or a startup sanity check that
      reports any `alpr_events.date` not ending in `+08:00`.
- [ ] **Integration test for the time zone behaviour.** The unit tests in `src/time.rs` cover
      the conversion, and it was verified by hand against the running servers, but there is no
      automated end-to-end test (POST events with `+08:00` / `Z` / `-05:00`, then query in
      several offsets). It needs a collector test harness, which does not exist yet.

## Users and permissions

- [ ] **The user model is TBD.** The `users` table is a placeholder for the frontend login.
      Who the users are, roles and permissions are undecided. Today any valid login can
      call every route. When this is designed: add a UNIQUE index on `users.email`
      (duplicates are currently accepted), and decide how users are created and deactivated.
      Also note that tokens already issued keep working after `is_active` is set to false,
      for up to 24 hours.

## Testing

- [ ] **Integration tests bypass `db::connect`.** `tests/location_test.rs` and
      `tests/sublocations_test.rs` call `Database::connect` directly, so they run without
      WAL and without migrations. They also depend on a pre-existing database that already
      contains the seeded `admin@admin` user, so they fail against a freshly wiped database
      until the collector has been started once. Switch them to a temporary database created
      through `db::connect(.., MissingDatabase::CreateAndMigrate)`.

## Retry protocol

- [ ] **No test over real HTTP.** `tests/collector_test.rs` calls `process_event()` directly, so
      routing, the 20 MB body limit and the `Content-Type` of the reply are only checked by
      hand. A small test that builds the router (`build_collector_server`) and sends requests
      with `tower::ServiceExt::oneshot` would close that gap.
- [ ] **Duplicate vs. conflict by raw bytes.** Because the hash is over raw bytes, the same
      event re-serialised differently (key order, spacing, an uppercase UUID) is a `409`, not a
      duplicate. That is deliberate (see README_PI.md: resend the saved bytes) but strict; if
      the Pi cannot guarantee byte-identical retries, consider hashing a canonical form instead.
- [ ] **No clean-up of orphaned images.** A crash between writing the image files and the
      database insert leaves two files that no row refers to. A retry reuses (overwrites) the
      same filenames, so these only remain if the Pi never retries that event.

## Operations

- [ ] **No graceful shutdown.** Both servers call `axum::serve(listener, app)` without
      `with_graceful_shutdown`, so a `SIGTERM` (e.g. `systemctl restart`) ends the process
      immediately and can cut an in-flight request, such as a collector event between writing
      the images and inserting the row. Add a shutdown signal handler that lets in-flight
      requests finish, especially for the collector.

- [ ] **Concurrent collectors are unsupported.** Only one collector may run; two started at
      once against a missing database can race in `Migrator::up`
      (`UNIQUE constraint failed: seaql_migrations.version`). Fine for the planned
      single-service deployment; revisit if that ever changes.
- [ ] **`migrate_data` is a one-off** that hard-codes `server_db/server.db` and `out/alpr_events.db`
      and needs the tables to exist. After a database reset, start the collector once
      (so it migrates) before running it.
