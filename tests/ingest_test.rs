//! The camera-facing endpoint: the Pi never rejects a camera post because of its
//! content, and answers `200` only once the event is committed.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use common::{MockCollector, Reply};
use mobile_vehicle_counter_pi::event::Event;
use mobile_vehicle_counter_pi::ingest::{self, IngestState, UNPARSEABLE};
use mobile_vehicle_counter_pi::sender::{SenderSettings, Step, build_client, process_next};
use mobile_vehicle_counter_pi::store::{STATUS_DELIVERED, STATUS_FAILED, STATUS_PENDING, Store};
use tokio::sync::Notify;
use tower::ServiceExt;

const CAP: usize = 64 * 1024 * 1024;

struct Harness {
    store: Store,
    wake: Arc<Notify>,
    app: Router,
}

fn harness_with(store: Store, max_body: usize) -> Harness {
    let wake = Arc::new(Notify::new());
    let app = ingest::router(
        IngestState {
            store: store.clone(),
            wake: wake.clone(),
        },
        max_body,
    );
    Harness { store, wake, app }
}

fn harness() -> Harness {
    harness_with(Store::open_in_memory().unwrap(), CAP)
}

async fn post(app: &Router, path: &str, body: Vec<u8>) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn camera_json(plate: &str) -> Vec<u8> {
    serde_json::json!({
        "camid": "CAM1",
        "date": "2026-10-30T10:00:00+08:00",
        "plate": plate,
        "image": "/9j/4AAQ",
        "plate_image": "/9j/4AAQ",
    })
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn a_valid_post_is_stored_pending_with_a_new_event_id_and_acknowledged() {
    let h = harness();

    let status = post(&h.app, "/api/alpr/events", camera_json("ABC1234")).await;

    assert_eq!(status, StatusCode::OK);
    let rows = h.store.all().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the row exists by the time the camera hears 200"
    );
    let row = &rows[0];
    assert_eq!(row.status, STATUS_PENDING);
    let event: Event = serde_json::from_slice(&row.body).unwrap();
    assert_eq!(event.event_id, row.event_id);
    assert_eq!(
        uuid::Uuid::parse_str(&event.event_id)
            .unwrap()
            .get_version_num(),
        4
    );
    assert_eq!(event.camid, "CAM1");
    assert_eq!(event.plate, "ABC1234");
    assert_eq!(event.date, "2026-10-30T10:00:00+08:00");
    assert_eq!(event.image, "/9j/4AAQ");
    assert_eq!(event.plate_image, "/9j/4AAQ");
}

#[tokio::test]
async fn identical_posts_are_two_events() {
    // Two genuine events can look alike. The Pi does not de-duplicate camera posts.
    let h = harness();
    post(&h.app, "/api/alpr/events", camera_json("ABC1234")).await;
    post(&h.app, "/api/alpr/events", camera_json("ABC1234")).await;
    let rows = h.store.all().unwrap();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0].event_id, rows[1].event_id);
}

#[tokio::test]
async fn a_valid_post_wakes_the_sender() {
    let h = harness();
    post(&h.app, "/api/alpr/events", camera_json("ABC1234")).await;
    tokio::time::timeout(Duration::from_secs(1), h.wake.notified())
        .await
        .expect("the sender should be woken");
}

#[tokio::test]
async fn unparseable_posts_are_acknowledged_and_kept_as_failed_rows() {
    let missing_field =
        serde_json::json!({"camid": "CAM1", "date": "d", "plate": "p", "image": "i"})
            .to_string()
            .into_bytes();
    let not_a_string = serde_json::json!(
        {"camid": "CAM1", "date": "d", "plate": 5, "image": "i", "plate_image": "x"}
    )
    .to_string()
    .into_bytes();
    let bad_inputs: Vec<Vec<u8>> = vec![
        b"{not json".to_vec(),
        b"".to_vec(),
        b"[]".to_vec(),
        vec![0xff, 0xfe, 0x00],
        missing_field,
        not_a_string,
    ];

    for raw in bad_inputs {
        let h = harness();
        let status = post(&h.app, "/api/alpr/events", raw.clone()).await;

        assert_eq!(
            status,
            StatusCode::OK,
            "the camera is never refused for content"
        );
        let rows = h.store.all().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, STATUS_FAILED);
        assert_eq!(rows[0].body, raw, "the raw bytes are kept for inspection");
        assert!(
            rows[0]
                .last_error
                .as_deref()
                .unwrap()
                .starts_with(UNPARSEABLE)
        );
        assert_eq!(h.store.next_due(i64::MAX).unwrap(), None, "never sent");
    }
}

#[tokio::test]
async fn the_date_is_not_checked_by_the_pi() {
    let h = harness();
    let body = serde_json::json!({
        "camid": "CAM1", "date": "yesterday", "plate": "A",
        "image": "i", "plate_image": "p",
    })
    .to_string()
    .into_bytes();

    assert_eq!(post(&h.app, "/api/alpr/events", body).await, StatusCode::OK);
    assert_eq!(h.store.all().unwrap()[0].status, STATUS_PENDING);
}

#[tokio::test]
async fn extra_camera_fields_are_accepted_and_not_forwarded() {
    let h = harness();
    let body = serde_json::json!({
        "camid": "CAM1", "date": "2026-10-30T10:00:00Z", "plate": "A",
        "image": "i", "plate_image": "p", "confidence": 0.93,
    })
    .to_string()
    .into_bytes();
    post(&h.app, "/api/alpr/events", body).await;
    let row = &h.store.all().unwrap()[0];
    assert_eq!(row.status, STATUS_PENDING);
    let sent: serde_json::Value = serde_json::from_slice(&row.body).unwrap();
    assert!(sent.get("confidence").is_none());
}

#[tokio::test]
async fn a_post_larger_than_axums_default_limit_is_accepted() {
    // axum's own default is 2 MB. Events are routinely bigger than that.
    let h = harness();
    let big_image = "A".repeat(5 * 1024 * 1024);
    let body = serde_json::json!({
        "camid": "CAM1", "date": "2026-10-30T10:00:00Z", "plate": "A",
        "image": big_image, "plate_image": "p",
    })
    .to_string()
    .into_bytes();
    assert!(body.len() > 2 * 1024 * 1024);

    assert_eq!(post(&h.app, "/api/alpr/events", body).await, StatusCode::OK);
    assert_eq!(h.store.counts().unwrap().pending, 1);
}

#[tokio::test]
async fn an_event_too_big_for_the_collector_is_still_stored_and_fails_later() {
    // 21 MB is over the collector's 20 MB limit but under the Pi's cap. It must be
    // kept, sent, refused with 413, and end up 'failed' (never lost).
    let h = harness();
    let image = "A".repeat(21 * 1024 * 1024);
    let body = serde_json::json!({
        "camid": "CAM1", "date": "2026-10-30T10:00:00Z", "plate": "A",
        "image": image, "plate_image": "p",
    })
    .to_string()
    .into_bytes();
    assert_eq!(post(&h.app, "/api/alpr/events", body).await, StatusCode::OK);
    let id = h.store.all().unwrap()[0].event_id.clone();

    let collector = MockCollector::always(Reply::Status(413)).await;
    let settings = SenderSettings {
        url: collector.url.clone(),
        token: "t".into(),
        backoff_cap: Duration::from_secs(300),
    };
    let step = process_next(
        &h.store,
        &build_client(Duration::from_secs(30)),
        &settings,
        i64::MAX / 2,
    )
    .await
    .unwrap();

    assert!(matches!(step, Step::Handled { .. }));
    assert_eq!(h.store.get(&id).unwrap().unwrap().status, STATUS_FAILED);
}

#[tokio::test]
async fn a_post_above_the_size_cap_is_refused_and_stores_nothing() {
    let h = harness_with(Store::open_in_memory().unwrap(), 1_000);
    let status = post(&h.app, "/api/alpr/events", vec![b'x'; 5_000]).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(h.store.all().unwrap().len(), 0);

    // A post under the cap still works on the same router.
    let small = serde_json::json!({
        "camid": "C", "date": "d", "plate": "p", "image": "i", "plate_image": "x",
    })
    .to_string()
    .into_bytes();
    assert!(small.len() < 1_000);
    assert_eq!(
        post(&h.app, "/api/alpr/events", small).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn only_post_on_the_one_route_is_served() {
    let h = harness();
    assert_eq!(
        post(&h.app, "/other", camera_json("A")).await,
        StatusCode::NOT_FOUND
    );
    let get = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/alpr/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(h.store.all().unwrap().len(), 0);
}

#[tokio::test]
async fn if_the_database_cannot_be_written_the_camera_gets_a_500_not_a_200() {
    // The Pi must not claim an event is stored when it is not.
    let dir = std::env::temp_dir().join(format!("pi_ingest_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pi.db");
    let store = Store::open(&path).unwrap();
    // Break the database from outside, as a corrupted or damaged file would be.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("DROP TABLE events;")
        .unwrap();
    let h = harness_with(store, CAP);

    let status = post(&h.app, "/api/alpr/events", camera_json("ABC1234")).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_post_to_delivery_round_trip() {
    // Camera -> Pi -> collector -> back to 'delivered', end to end in one process.
    let h = harness();
    post(&h.app, "/api/alpr/events", camera_json("ABC1234")).await;
    let collector = MockCollector::always(Reply::Ack { duplicate: false }).await;
    let settings = SenderSettings {
        url: collector.url.clone(),
        token: "t".into(),
        backoff_cap: Duration::from_secs(300),
    };

    let step = process_next(
        &h.store,
        &build_client(Duration::from_secs(5)),
        &settings,
        i64::MAX / 2,
    )
    .await
    .unwrap();

    assert!(matches!(step, Step::Handled { .. }));
    let row = &h.store.all().unwrap()[0];
    assert_eq!(row.status, STATUS_DELIVERED);
    assert_eq!(collector.bodies(), vec![row.body.clone()]);
}
