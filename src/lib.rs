//! Pi client: accepts ALPR events from cameras on the LAN, stores them in a local
//! SQLite database, and delivers them to the collector with retries.
//!
//! See `README.md` for the protocol. Module map:
//!
//! * [`config`]: settings from the environment / `.env`.
//! * [`event`]: the collector's request body, built once per event.
//! * [`store`]: the local SQLite database (the only place events live).
//! * [`ingest`]: the HTTP endpoint the cameras POST to.
//! * [`outcome`]: pure rules that turn a collector reply into a status.
//! * [`sender`]: the delivery loop.

pub mod config;
pub mod event;
pub mod ingest;
pub mod outcome;
pub mod sender;
pub mod store;
