//! The endpoint the ALPR cameras POST to: `POST /api/alpr/events`.
//!
//! Cameras cannot retry, so the Pi **never rejects a camera post because of what
//! it contains**:
//!
//! * Valid camera JSON becomes a `pending` event with a new `event_id`.
//! * Anything else (invalid JSON, a missing or non-string field) is stored as a
//!   `failed` row holding the raw bytes, and kept for inspection. It is never sent.
//! * The `date` is not checked here. A bad one is sent, the collector answers
//!   `400`, and the event becomes `failed` through the normal path.
//!
//! `200` is returned only after the row is committed. If the database cannot be
//! written the answer is `500`, because the Pi cannot honestly say the event is
//! stored. The only content-independent refusal is a body above the size cap
//! (`413`), which protects the SD card from a faulty camera.

use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::post,
};
use tokio::sync::Notify;

use crate::event::{self, CameraEvent, new_event_id};
use crate::store::Store;

/// Reason stored with a camera post that could not become an event.
pub const UNPARSEABLE: &str = "unparseable camera payload";

#[derive(Clone)]
pub struct IngestState {
    pub store: Store,
    /// Woken after each new `pending` event so the sender looks at once.
    pub wake: Arc<Notify>,
}

/// What became of one camera post.
#[derive(Debug, PartialEq, Eq)]
pub enum Accepted {
    /// Stored as a `pending` event with this id.
    Pending(String),
    /// Stored as a `failed` row with this id; it will not be sent.
    Failed(String),
}

/// The camera-facing router. `max_body_bytes` caps one post.
pub fn router(state: IngestState, max_body_bytes: usize) -> Router {
    Router::new()
        .route("/api/alpr/events", post(handle_camera_post))
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .with_state(state)
}

/// Stores one camera post. Synchronous: the row is committed when this returns.
pub fn accept(store: &Store, body: &[u8]) -> rusqlite::Result<Accepted> {
    match serde_json::from_slice::<CameraEvent>(body) {
        Ok(camera) => {
            let (event_id, request_body) = event::build(camera);
            store.insert_pending(&event_id, &request_body)?;
            Ok(Accepted::Pending(event_id))
        }
        Err(error) => {
            let event_id = new_event_id();
            store.insert_failed(&event_id, body, &format!("{UNPARSEABLE}: {error}"))?;
            Ok(Accepted::Failed(event_id))
        }
    }
}

async fn handle_camera_post(
    State(state): State<IngestState>,
    body: Bytes,
) -> (StatusCode, &'static str) {
    let store = state.store.clone();
    let result = tokio::task::spawn_blocking(move || accept(&store, &body)).await;
    match result {
        Ok(Ok(Accepted::Pending(event_id))) => {
            println!("stored event {event_id}");
            state.wake.notify_one();
            (StatusCode::OK, "OK")
        }
        Ok(Ok(Accepted::Failed(event_id))) => {
            eprintln!("ERROR stored unparseable camera post as failed row {event_id}");
            (StatusCode::OK, "OK")
        }
        Ok(Err(error)) => {
            eprintln!("ERROR EVENT LOST: could not write to the database: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, "could not store the event")
        }
        Err(error) => {
            eprintln!("ERROR EVENT LOST: database task failed: {error}");
            (StatusCode::INTERNAL_SERVER_ERROR, "could not store the event")
        }
    }
}
