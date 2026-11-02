# TODO

Known items that were deliberately deferred. Nothing here is blocking.

## Alerting and operations

- [ ] **"Alert" is only a log line.** When an event becomes `failed`, the Pi logs a line
      containing `ERROR ALERT`. Nobody is notified. Decide on a channel (email, a
      push message, a status endpoint polled by monitoring).
- [ ] **No way to retry a `failed` event.** Once the cause is fixed, the operator has to
      set the row back by hand:
      `UPDATE events SET status='pending', next_attempt_at=0 WHERE event_id='...';`
      (the same bytes and `event_id` are sent). A small command-line tool or a
      documented procedure would be safer than raw SQL.
- [ ] **Database retention is manual.** Nothing deletes rows or clears image data, so
      `pi_events.db` grows without limit. The operator clears old image data by hand.
      Until then, watch the free space on the SD card: if the database cannot be
      written, the Pi answers the camera with `500` and the event is lost (the cameras
      cannot retry).
- [ ] **No disk-space check.** The Pi could log a warning (or stop accepting) when free
      space is low, before writes start failing.
- [ ] **Concurrent instances are unsupported.** Two clients on one database file would
      both send the same pending events. This is harmless to the collector (the second is
      a duplicate) but wasteful. Use a single service.

## Security

- [ ] **`.env` is still tracked by git.** It is now in `.gitignore`, but a file that is
      already tracked stays tracked. Run `git rm --cached .env` and commit, and rotate the
      collector token if a real one was ever committed.
- [ ] **HTTP while testing.** Until the server has a certificate the token travels in clear
      text. Switch `COLLECTOR_URL` to `https://` (the `reqwest` client is built with
      `rustls`, so no extra setup is needed on the Pi).
- [ ] **The camera endpoint has no authentication.** Intended: the cameras and the Pi are
      alone on a LAN. If that ever changes, bind `PI_LISTEN_ADDR` to the camera-facing
      interface only, or add a token.

## Correctness

- [ ] **Camera timestamp format is assumed.** The Pi forwards `date` unchanged. If a
      camera sends a local time without an offset, the collector answers `400` and every
      event becomes `failed`. Confirm with a real camera payload. If needed, add a
      conversion step with a configured time zone.
- [ ] **`camid` must match `camera.id` on the server.** The Pi forwards it unchanged. An
      unregistered camera makes events wait as `pending` (collector `422`) until the camera
      is registered on the server.
- [ ] **Unknown HTTP statuses are treated as `failed`.** Anything not in the README's table
      (other 2xx, 3xx, other 4xx) marks the event `failed` rather than retrying forever.
      Reconsider if the collector or nginx starts returning others (for example `408`).
- [ ] **A very large camera post is held in memory.** Posts are read into memory before they
      are stored (up to `MAX_CAMERA_BODY_BYTES`, default 64 MB), and the sender holds one
      event body in memory per attempt. Fine on a Pi with a few hundred MB free, but not
      streamed.
- [ ] **Failed rows from unparseable posts hold the camera's raw bytes**, not a collector
      request body. They are never sent.

## Testing

- [ ] **`tests/contract.sh` is manual.** It needs the sibling server project and `sqlite3`
      and `curl`. Run it after changing the protocol on either side. It is not part of
      `cargo test`.
- [ ] **No test for graceful shutdown or the process wiring** in `src/bin/run_pi_client.rs`.

## Documentation

- [ ] **Two copies of the protocol.** `README.md` here and `README_PI.md` in the server project
      describe the same contract. The server's `TODO.md` and `deploy/nginx-collector.conf`
      still point at `README_PI.md`. Decide which one is the source of truth.
- [ ] **Stale generated files.** `README.pdf`, `PLAN.pdf` and `mermaid-filter.err` come from
      the PDF tooling and `README.pdf` is out of date. Regenerate or remove them.
