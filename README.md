# Mobile Vehicle Counter Server


This is the Autonomous Systems Inc.'s project to count vehicles, and analyze traffic through a subdivision gate.

This specific project is the Server. It accepts POSTED data from deployed raspberry pi 5 units, and serves data to an angular frontend to report data to analysts.

We separate this into two servers so that we can keep revising the server for the frontend (which we expect to need changes), while keeping the data receiving server constantly running:

1. one listens to events from the deployed raspberry pi 5 units, and writes these to the sqlite database.
2. the other reads from the database and publishes the data to the angular frontend.


## Two Servers
This project creates two binaries (in `src/bin/`), both thin wrappers around a `run_server()` in the library crate:

| Binary           | Calls                          | Default address                  | Purpose                                                         |
|------------------|--------------------------------|----------------------------------|-----------------------------------------------------------------|
| `data_collector` | `collector_server::run_server()` | `127.0.0.1:3001` (`COLLECTOR_ADDR`) | Receives POSTED base64 events, saves images to disk, inserts rows. |
| `frontend_api`   | `api_server::run_server()`       | `127.0.0.1:3000` (hard-coded)       | Backend for the angular frontend (login, locations, event stats). |

`default-run` in `Cargo.toml` is `frontend_api`, so a bare `cargo run` starts the **API**, not the collector. Use `cargo run --bin data_collector` for the collector.

There is also `migrate_data` (`cargo run --bin migrate_data`), a **one-off** tool that copies legacy data from `out/alpr_events.db` into `server_db/server.db`. It hard-codes both paths relative to the current directory and seeds a legacy test location (`TCEV`), sublocation (`Balimbing`) and two cameras. It is not part of normal operation.


## Design Decisions (read before changing things)

### Two processes, one shared SQLite file
- The collector and the API are **separate processes that share one SQLite file**. They share no code paths at runtime and have **separate state structs** (`CollectorState` vs `handlers::app_state::AppState`) on purpose: only the collector writes image files.
- The reason for the split is operational: the API will be redeployed/restarted often; the collector should stay up so no Pi events are lost. **Restarting the API must never require restarting the collector.**
- The collector is the main writer; the API mostly reads (but it also writes: `users`, `location`, `sublocation` CRUD). Concurrent access relies on SQLite's locking. Keep write transactions short.
- Both servers bind to `127.0.0.1` only. They are meant to sit behind nginx.

### Collector: reverse proxy and route
- The collector listens on `127.0.0.1:3001` (not 3000; 3000 is the API). nginx needs a `proxy_pass` to it.
- The route is **`POST /api/alpr/event`** (singular `event`).
- Payload (JSON): `camid`, `date`, `plate`, `plate_image`, `image`. `image` and `plate_image` are base64 (standard alphabet) JPEGs. `date` must be **RFC 3339 with offset**, e.g. `2026-10-09T16:44:25.443+08:00`.
- The collector **generates its own event id** (UUID v4); the Pi does not supply one. Retrying a POST therefore creates a **duplicate event** (there is no idempotency key).
- Images are saved as `event_<uuid>.jpg` and `event_<uuid>_plate.jpg`. Only the **filenames** are stored in the DB, not paths or bytes. The directory is not configured per-row, so the API/frontend must know where the images directory is.

### Collector: validation order and responses
The handler deliberately validates cheaply first and does expensive work last, and cleans up after itself:

| Step | Failure                          | Response |
|------|----------------------------------|----------|
| 1    | Body is not valid JSON / fields missing | `400` |
| 2    | `date` not RFC 3339              | `400` |
| 3    | `camid` not in the `camera` table | `422` (**unknown cameras are rejected**; cameras must be registered first) |
| 3    | DB error looking up camera       | `500` |
| 5    | Base64 decode / disk write fails | `500` (a half-written scene image is deleted) |
| 6    | DB insert fails                  | `500` (both image files are deleted) |

- `camid` is matched against `camera.id` in exactly one place, `find_camera()`. If the device-reported id ever stops being the camera primary key, change only that function.
- Files are written **before** the DB insert, and removed if the insert fails, so a failure should not leave orphaned images. A crash between the two steps still could.

### Dates and time zones
- `alpr_events.date` is `timestamp_with_time_zone`. The offset sent by the Pi is preserved (`DateTime<FixedOffset>`), not normalised to UTC.
- The API accepts/returns RFC 3339 with offsets and compares via `chrono`, so mixed offsets compare correctly. Be aware SQLite has no native tz type, so ordering/range filters in SQL operate on the stored text; keep all writers using the same RFC 3339 format.
- Time ranges are **half-open**: `start <= date < end`.

### Binned events (`/api/binned_events`)
- Binning is done **in Rust, not SQL**: all events in the range are loaded into memory, empty bins are generated, then each event is assigned to a bin (a linear scan per event). Simple and fine at current volumes; revisit (SQL `GROUP BY` or a sorted/index-based assignment) if ranges get large (e.g. months of minute-bins).
- The last bin is truncated at `end_datetime`.
- Month bins advance by calendar month and clamp the day (Jan 31 -> Feb 28/29). Note that clamping is applied to the *running* start, so after clamping, later bins keep the clamped day (Jan 31 -> Feb 28 -> Mar 28).
- `advance_time` has a `panic!` that should be unreachable; an enormous bin count (e.g. `minutes` over years) is the more realistic risk.
- Events are matched to sublocations through `camera.sublocation_id` (inner join), so an event only appears if its camera belongs to one of the requested sublocations.

### Data model
`location` -> `sublocation` -> `camera` -> `alpr_events`. All primary keys are **strings** (UUIDs for API-created rows; the legacy migration uses readable ids like `TCEV`). `users.id` is an integer.

| Relationship                | On delete | On update | Consequence |
|-----------------------------|-----------|-----------|-------------|
| `sublocation` -> `location` | CASCADE   | CASCADE   | Deleting a location deletes its sublocations. |
| `camera` -> `sublocation`   | RESTRICT  | CASCADE   | A sublocation with cameras **cannot be deleted** (API returns 500, not 409). |
| `alpr_events` -> `camera`   | RESTRICT  | RESTRICT  | A camera with events cannot be deleted or re-id'd. |

- Because of the above, deleting a **location** that contains a sublocation with cameras will fail (the cascade hits the `camera` RESTRICT). Events are effectively permanent.
- `camera.orientation` has a CHECK constraint: only `Inbound` or `Outbound`.
- There is **no API to create cameras**. Cameras are inserted by hand / by the legacy migration, and the collector rejects events from unregistered cameras.
- `location.description` is required (`NOT NULL`); `sublocation.description` is optional.
- SQLite foreign keys are enforced by sqlx by default; do not disable them.
- The entities in `src/entities/` are `sea-orm-codegen` output ("@generated"). Prefer regenerating from the DB over hand-editing.

### Authentication
- `POST /api/login` is public; **every other API route is behind `auth_middleware`** (JWT Bearer, HS256, secret from `JWT_SECRET`). `route_layer` is used so unmatched paths still 404 rather than 401.
- Tokens expire in **24 hours** (`expires_in: 86400`). There is no refresh token and no revocation; `is_active = false` only blocks *new* logins, not already-issued tokens.
- Passwords are hashed with **Argon2** (PHC string in `users.encrypted_password`). Login returns `401` for unknown user, inactive user, bad hash and wrong password alike (no user enumeration).
- `JWT_SECRET` is read from the environment **on every request**, not at startup. If it is missing, the server starts fine and then returns `500` on every login/authenticated request.
- **The collector endpoint has no authentication.** It relies on being reachable only through nginx / the network. Consider a shared secret or IP allow-list before exposing it publicly.
- The first migration seeds a user **`admin@admin` / `asdf`**. Change or remove it in any real deployment. (The integration tests log in with it.)
- CORS is `allow_origin(Any)` on both servers and only allows `GET`/`POST` (note: the API also has `DELETE` routes, which a browser preflight would block cross-origin; this is fine only while the frontend is served same-origin via nginx).

### API conventions
- Most "query" endpoints are `POST` with a JSON body (`/api/time_limits`, `/api/binned_events`, `/api/bin_events_details`) because they take lists of sublocation ids.
- Handlers return bare `StatusCode`s on error (no JSON error body) and log details with `eprintln!`. Success codes: `201` create, `204` delete, `404` when deleting a missing id.


## Configuration

Loaded with `dotenvy` from `.env` in the **current working directory**.

| Variable        | Used by          | Notes |
|-----------------|------------------|-------|
| `DATABASE_URL`  | both servers, tests | **Required** (the process panics at startup if absent). Use `sqlite://server_db/server.db?mode=rw`. Relative paths resolve against the cwd. |
| `JWT_SECRET`    | `frontend_api`, tests | Required at request time (see Authentication). |
| `COLLECTOR_ADDR`| `data_collector` | Default `127.0.0.1:3001`. |
| `IMAGE_DIR`     | `data_collector` | Default `images`. Created automatically if missing. |

Gotchas:
- The images directory variable is **`IMAGE_DIR`** (`.env` used to say `IMAGES`, which the code never read).
- **`dotenvy` does not override variables already set in the shell.** A stale `DATABASE_URL` exported in your shell (e.g. from another project) silently wins over `.env`. If the server complains about an unrelated database driver, run `unset DATABASE_URL`.
- **Do not put `journal_mode=WAL` (or any other pragma) in `DATABASE_URL`.** sqlx 0.9's SQLite URL parser only accepts `mode`, `cache`, `immutable` and `vfs`; any other parameter is a hard error (`unknown query parameter 'journal_mode'`) and the server refuses to start. SQLite pragmas are set in code instead, see "SQLite connection settings" below.
- `DATABASE_URL` should use `mode=rw`, so a missing database file fails fast at startup. Use `mode=rwc` only when you deliberately want a file created.
- Everything is relative to the working directory (`.env`, `server_db/`, `images/`). Run both servers from the project root (or use absolute paths / systemd `WorkingDirectory`).


## SQLite connection settings (WAL)

Both servers open the database through **`db::connect()`** (`src/db.rs`), never `Database::connect` directly. This keeps the two processes configured identically. Every connection is opened with:

| Setting | Value | Why |
|---------|-------|-----|
| `journal_mode` | `WAL` | The collector writes while the API reads. In SQLite's default rollback-journal mode a writer blocks readers (`database is locked`); in WAL readers and the single writer do not block each other. |
| `synchronous` | `NORMAL` | The usual WAL companion. Safe against an application crash; a power loss / OS crash can lose the most recent commits, but cannot corrupt the database. |
| `busy_timeout` | 5 s | Writers wait for the write lock instead of failing immediately. Only matters when the API writes (users/locations/sublocations) at the same moment as the collector. |
| `foreign_keys` | `ON` | The schema relies on its RESTRICT/CASCADE rules. |

Decisions and reminders:
- WAL is set **on every connection** rather than once with `PRAGMA journal_mode=WAL` on the file. It is persistent in the file, but doing it in code means a freshly migrated or restored database cannot silently be left in rollback-journal mode.
- Verified: with a write transaction held open, the API still answers (login `200` in ~0.5 s); the same scenario in rollback-journal mode gives `database is locked`.
- WAL needs both processes on **the same machine** (it uses shared memory, `server.db-shm`). Do **not** put the database on NFS/SMB.
- Expect `server.db-wal` and `server.db-shm` next to `server.db`. Do not delete them while a server is running.
- **Backups:** copying only `server.db` can miss committed data still in the `-wal` file. Use `sqlite3 server_db/server.db ".backup backup.db"` (or copy all three files).
- There is still only **one writer at a time**. If collector write volume ever outgrows that, the next step is a client/server database (e.g. Postgres), not more SQLite tuning.
- SeaORM defaults a SQLite pool to **one connection** when `max_connections` is not set, and `db::connect` does not change that. Concurrency between the two *processes* comes from WAL; within one process, queries are serialised through that single connection. Raise it in `db::connect` if the API becomes a bottleneck.
- The integration tests in `tests/` still call `Database::connect` directly (plain rollback-journal behaviour unless the file is already in WAL). That is fine for them, but they do not exercise `db::connect`.


## The database file and what happens when it is missing

The schema is owned by the `migration/` crate (SeaORM migrations), **not** by either server. **Neither server creates tables or runs migrations.**

What actually happens (verified by running the binaries). `.env` now uses `mode=rw`, so the `mode=rw` rows are the ones that apply by default:

| Situation | Result |
|-----------|--------|
| `mode=rwc`, directory exists, **file missing** | Servers **start normally** and SQLite silently creates an **empty 0-byte** file. Every DB-backed request then fails with `500` (`no such table: ...`). This is why `.env` uses `mode=rw`. |
| `mode=rwc`, **directory (`server_db/`) missing** | Both servers **exit immediately** with `unable to open database file` (code 14). |
| `mode=rw` (no create), file missing | Both servers **exit immediately** with `unable to open database file`. |
| Variable missing | Both servers **panic** at startup (`DATABASE_URL must be set in .env file`). |
| File deleted **while running** | Processes **stay alive**; requests return `500`. (On Linux the open handle keeps the old inode, so behavior can vary; restart after restoring.) |
| Tables missing (empty/unmigrated DB) | Processes **stay alive**; API returns `500` (`no such table: users`), collector returns `500` (`no such table: camera`). **Events from the Pis are rejected** until the DB is migrated. |

Design implications / reminders:
- The failure is **loud at startup only when the file cannot be opened**. The dangerous case is `mode=rwc` with a missing file: the collector looks healthy but is silently dropping every event with `500`. Monitor for non-2xx from the collector (the Pis should retry/queue on failure).
- Per-request DB errors never crash the processes. Handlers map DB errors to `500` and log them.
- Neither server retries the connection: the connection is opened **once at startup**. If the DB is unavailable at boot, the process exits and must be restarted by a supervisor (systemd `Restart=on-failure`).
- `.env` uses `mode=rw` so a missing/misplaced database fails fast instead of producing an empty one. The migration CLI is what creates the file (see below), so servers never need `rwc`.
- Because the two servers are independent, one being down does not affect the other, but they fail **together** if they share the same missing/broken database file.

### Creating / migrating the database
```sh
mkdir -p server_db
cd migration
DATABASE_URL="sqlite://../server_db/server.db?mode=rwc" cargo run -- up
```
The migration command is the one place `mode=rwc` is appropriate, because it must be able to create the file. (See `migration/README.md` for the other CLI commands. Note the path is relative to `migration/`, and the shell `DATABASE_URL` caveat above applies. The migration CLI uses its own plain connection, so it does not go through `db::connect`.) The migrations create the tables and seed the `admin@admin` user. After migrating you must still insert at least one `location` -> `sublocation` -> `camera` chain before the collector will accept any event.


## Testing
- `tests/location_test.rs` and `tests/sublocations_test.rs` are **integration tests that run against the real database in `DATABASE_URL`** (not a temp/in-memory DB), log in as `admin@admin`/`asdf`, create rows, and delete them. They therefore need a migrated database and `JWT_SECRET`, and will leave rows behind if a test fails midway. Don't point them at production data.
- There are **no tests for the collector** (`collector_server.rs`) or for the events endpoints.
- `http_tests/login.http` is a manual request file for the IDE HTTP client (API on port 3000).


## Repository notes
- `.gitignore` excludes `target/`, `.idea/`, `images/`, `out/` and `migration/target`. **`.env` is not ignored and is currently tracked in git**, so `JWT_SECRET` is committed. `server_db/server.db` is not tracked today, but it is not ignored either, so it could be added by accident (it contains the seeded admin user's hash). Consider ignoring both and committing a `.env.example` instead; rotate `JWT_SECRET` if this repo is ever shared.
- `out/` holds the legacy database (`alpr_events.db`) and legacy images, used only by `migrate_data`.
