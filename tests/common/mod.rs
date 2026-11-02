//! A scriptable stand-in for the collector, for the sender tests.
//!
//! It runs a real HTTP server on a loopback port and records every request it
//! receives, so tests can check that retries send identical bytes.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use mobile_vehicle_counter_pi::outcome::sha256_hex;

/// What the mock collector does with one request.
#[derive(Clone)]
pub enum Reply {
    /// A correct `200` receipt for the bytes received.
    Ack { duplicate: bool },
    /// `200`, but the `content_hash` is wrong.
    AckWrongHash,
    /// `200`, but for a different event.
    AckWrongId,
    /// `200` with a body that is not a receipt.
    AckGarbage,
    /// Any status with a plain-text body.
    Status(u16),
    /// Waits, then sends a correct receipt (used to trigger client timeouts).
    SlowAck(Duration),
}

pub struct Received {
    pub body: Vec<u8>,
    pub token: Option<String>,
    pub content_type: Option<String>,
}

type Script = Arc<dyn Fn(usize, &[u8]) -> Reply + Send + Sync>;

#[derive(Clone)]
struct MockState {
    received: Arc<Mutex<Vec<Received>>>,
    script: Script,
}

pub struct MockCollector {
    pub url: String,
    received: Arc<Mutex<Vec<Received>>>,
}

impl MockCollector {
    /// Starts a collector on a free loopback port. `script(n, body)` decides the
    /// reply to request number `n` (0-based) that carried `body`.
    pub async fn start(script: impl Fn(usize, &[u8]) -> Reply + Send + Sync + 'static) -> Self {
        let received = Arc::new(Mutex::new(Vec::new()));
        let state = MockState {
            received: received.clone(),
            script: Arc::new(script),
        };
        let app = Router::new()
            .route("/api/alpr/events", post(handle))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            url: format!("http://{address}/api/alpr/events"),
            received,
        }
    }

    /// Always replies the same way.
    pub async fn always(reply: Reply) -> Self {
        Self::start(move |_, _| reply.clone()).await
    }

    pub fn request_count(&self) -> usize {
        self.received.lock().unwrap().len()
    }

    pub fn bodies(&self) -> Vec<Vec<u8>> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.body.clone())
            .collect()
    }

    pub fn tokens(&self) -> Vec<Option<String>> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.token.clone())
            .collect()
    }

    pub fn content_types(&self) -> Vec<Option<String>> {
        self.received
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.content_type.clone())
            .collect()
    }
}

fn event_id_of(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["event_id"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn receipt(event_id: &str, hash: &str, duplicate: bool) -> String {
    serde_json::json!({
        "event_id": event_id, "content_hash": hash, "duplicate": duplicate
    })
    .to_string()
}

async fn handle(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let number = {
        let mut received = state.received.lock().unwrap();
        received.push(Received {
            body: body.to_vec(),
            token: headers
                .get("x-collector-token")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            content_type: headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        });
        received.len() - 1
    };
    let id = event_id_of(&body);
    let hash = sha256_hex(&body);
    match (state.script)(number, &body) {
        Reply::Ack { duplicate } => (StatusCode::OK, receipt(&id, &hash, duplicate)),
        Reply::AckWrongHash => (StatusCode::OK, receipt(&id, &sha256_hex(b"other"), false)),
        Reply::AckWrongId => (
            StatusCode::OK,
            receipt("00000000-0000-4000-8000-000000000000", &hash, false),
        ),
        Reply::AckGarbage => (StatusCode::OK, "OK".to_string()),
        Reply::Status(code) => (
            StatusCode::from_u16(code).unwrap(),
            "error text".to_string(),
        ),
        Reply::SlowAck(delay) => {
            tokio::time::sleep(delay).await;
            (StatusCode::OK, receipt(&id, &hash, false))
        }
    }
}
