//! The sender against a mock collector: one test per delivery rule in the README.

mod common;

use std::time::Duration;

use common::{MockCollector, Reply};
use mobile_vehicle_counter_pi::event::{self, CameraEvent};
use mobile_vehicle_counter_pi::outcome::Outcome;
use mobile_vehicle_counter_pi::sender::{SenderSettings, Step, build_client, process_next};
use mobile_vehicle_counter_pi::store::{
    STATUS_DELIVERED, STATUS_FAILED, STATUS_PENDING, Store, now_ms,
};

const CAP: Duration = Duration::from_secs(300);

fn settings(url: &str) -> SenderSettings {
    SenderSettings {
        url: url.to_string(),
        token: "test-token".to_string(),
        backoff_cap: CAP,
    }
}

fn camera(plate: &str) -> CameraEvent {
    CameraEvent {
        camid: "CAM1".into(),
        date: "2026-10-30T10:00:00+08:00".into(),
        plate: plate.into(),
        image: "/9j/4AAQ".into(),
        plate_image: "/9j/4AAQ".into(),
    }
}

/// Stores one new event and returns its id and stored body.
fn add_event(store: &Store, plate: &str) -> (String, Vec<u8>) {
    let (id, body) = event::build(camera(plate));
    store.insert_pending(&id, &body).unwrap();
    (id, body)
}

/// Runs one delivery step as if it were far in the future, so that events waiting
/// for a retry are due.
async fn step(store: &Store, url: &str) -> Step {
    process_next(
        store,
        &build_client(Duration::from_secs(5)),
        &settings(url),
        i64::MAX,
    )
    .await
    .unwrap()
}

fn outcome(step: Step) -> Outcome {
    match step {
        Step::Handled { outcome, .. } => outcome,
        Step::Idle => panic!("expected an event to be attempted"),
    }
}

#[tokio::test]
async fn a_first_delivery_is_marked_delivered() {
    let store = Store::open_in_memory().unwrap();
    let (id, body) = add_event(&store, "ABC1234");
    let collector = MockCollector::always(Reply::Ack { duplicate: false }).await;

    assert_eq!(
        outcome(step(&store, &collector.url).await),
        Outcome::Delivered
    );

    let row = store.get(&id).unwrap().unwrap();
    assert_eq!(row.status, STATUS_DELIVERED);
    assert_eq!(row.attempts, 1);
    assert!(row.delivered_at.is_some());
    assert_eq!(
        collector.bodies(),
        vec![body],
        "the stored bytes are sent unchanged"
    );
    assert_eq!(collector.tokens(), vec![Some("test-token".to_string())]);
    assert_eq!(
        collector.content_types(),
        vec![Some("application/json".to_string())]
    );
}

#[tokio::test]
async fn a_duplicate_200_also_counts_as_delivered() {
    let store = Store::open_in_memory().unwrap();
    let (id, _) = add_event(&store, "ABC1234");
    let collector = MockCollector::always(Reply::Ack { duplicate: true }).await;

    assert_eq!(
        outcome(step(&store, &collector.url).await),
        Outcome::Delivered
    );
    assert_eq!(store.get(&id).unwrap().unwrap().status, STATUS_DELIVERED);
}

#[tokio::test]
async fn nothing_due_means_idle_and_no_request() {
    let store = Store::open_in_memory().unwrap();
    let collector = MockCollector::always(Reply::Ack { duplicate: false }).await;
    assert_eq!(step(&store, &collector.url).await, Step::Idle);
    assert_eq!(collector.request_count(), 0);
}

#[tokio::test]
async fn a_timeout_then_a_retry_sends_the_same_bytes_and_is_delivered() {
    let store = Store::open_in_memory().unwrap();
    let (id, body) = add_event(&store, "ABC1234");
    // The first request is stored by the collector but its reply is too slow.
    let collector = MockCollector::start(|n, _| {
        if n == 0 {
            Reply::SlowAck(Duration::from_secs(2))
        } else {
            Reply::Ack { duplicate: true }
        }
    })
    .await;
    let impatient = build_client(Duration::from_millis(300));

    let first = process_next(&store, &impatient, &settings(&collector.url), i64::MAX)
        .await
        .unwrap();
    assert!(matches!(outcome(first), Outcome::Retry { .. }));
    let row = store.get(&id).unwrap().unwrap();
    assert_eq!(
        row.status, STATUS_PENDING,
        "a timeout never loses or fails the event"
    );
    assert_eq!(row.attempts, 1);
    assert!(row.last_error.is_some());

    assert_eq!(
        outcome(step(&store, &collector.url).await),
        Outcome::Delivered
    );
    assert_eq!(store.get(&id).unwrap().unwrap().status, STATUS_DELIVERED);

    let bodies = collector.bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0], body);
    assert_eq!(bodies[0], bodies[1], "a retry sends byte-identical bodies");
}

#[tokio::test]
async fn no_connection_keeps_the_event_pending() {
    let store = Store::open_in_memory().unwrap();
    let (id, _) = add_event(&store, "ABC1234");
    // Reserve a port, then close it so nothing is listening.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/api/alpr/events", listener.local_addr().unwrap())
    };

    let result = outcome(step(&store, &closed).await);

    assert!(
        matches!(result, Outcome::Retry { slow: false, .. }),
        "{result:?}"
    );
    let row = store.get(&id).unwrap().unwrap();
    assert_eq!(row.status, STATUS_PENDING);
    assert_eq!(row.attempts, 1);
}

#[tokio::test]
async fn statuses_that_can_never_work_mark_the_event_failed_and_keep_it() {
    for status in [400u16, 403, 404, 409, 413] {
        let store = Store::open_in_memory().unwrap();
        let (id, body) = add_event(&store, "ABC1234");
        let collector = MockCollector::always(Reply::Status(status)).await;

        let result = outcome(step(&store, &collector.url).await);

        assert!(
            matches!(result, Outcome::Reject { .. }),
            "{status}: {result:?}"
        );
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(row.status, STATUS_FAILED, "HTTP {status}");
        assert_eq!(row.body, body, "a failed event keeps its body");
        assert!(row.last_error.unwrap().contains(&status.to_string()));
        // Not tried again.
        assert_eq!(step(&store, &collector.url).await, Step::Idle);
        assert_eq!(collector.request_count(), 1);
    }
}

#[tokio::test]
async fn statuses_to_retry_leave_the_event_pending_with_a_later_attempt_time() {
    for status in [401u16, 422, 429, 500, 502, 503, 504] {
        let store = Store::open_in_memory().unwrap();
        let (id, _) = add_event(&store, "ABC1234");
        let collector = MockCollector::always(Reply::Status(status)).await;
        let before = now_ms();

        let result = outcome(
            process_next(
                &store,
                &build_client(Duration::from_secs(5)),
                &settings(&collector.url),
                before,
            )
            .await
            .unwrap(),
        );

        assert!(
            matches!(result, Outcome::Retry { .. }),
            "{status}: {result:?}"
        );
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(row.status, STATUS_PENDING, "HTTP {status}");
        assert_eq!(row.attempts, 1);
        assert!(
            row.next_attempt_at > before,
            "HTTP {status}: retry must be scheduled later"
        );
        // Not due again straight away.
        assert_eq!(
            process_next(
                &store,
                &build_client(Duration::from_secs(5)),
                &settings(&collector.url),
                before
            )
            .await
            .unwrap(),
            Step::Idle
        );
    }
}

#[tokio::test]
async fn token_and_unregistered_camera_wait_longer_than_a_server_error() {
    async fn wait_after(status: u16) -> i64 {
        let store = Store::open_in_memory().unwrap();
        let (id, _) = add_event(&store, "ABC1234");
        let collector = MockCollector::always(Reply::Status(status)).await;
        let now = now_ms();
        process_next(
            &store,
            &build_client(Duration::from_secs(5)),
            &settings(&collector.url),
            now,
        )
        .await
        .unwrap();
        store.get(&id).unwrap().unwrap().next_attempt_at - now
    }
    let server_error = wait_after(503).await;
    let unauthorised = wait_after(401).await;
    let unregistered = wait_after(422).await;

    assert!(
        server_error <= 2_000,
        "first server-error wait is at most 2 s: {server_error}"
    );
    assert!(
        unauthorised >= 150_000,
        "401 waits at least half the cap: {unauthorised}"
    );
    assert!(
        unregistered >= 150_000,
        "422 waits at least half the cap: {unregistered}"
    );
}

#[tokio::test]
async fn a_bad_receipt_never_marks_the_event_delivered() {
    for reply in [Reply::AckWrongHash, Reply::AckWrongId, Reply::AckGarbage] {
        let store = Store::open_in_memory().unwrap();
        let (id, _) = add_event(&store, "ABC1234");
        let collector = MockCollector::always(reply).await;

        let result = outcome(step(&store, &collector.url).await);

        assert!(matches!(result, Outcome::Retry { .. }), "{result:?}");
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(row.status, STATUS_PENDING);
        assert!(row.last_error.is_some());
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    // A redirected POST could be re-sent as a GET and lose the body.
    let store = Store::open_in_memory().unwrap();
    let (id, _) = add_event(&store, "ABC1234");
    let collector = MockCollector::always(Reply::Status(301)).await;

    let result = outcome(step(&store, &collector.url).await);

    assert!(matches!(result, Outcome::Reject { .. }), "{result:?}");
    assert_eq!(store.get(&id).unwrap().unwrap().status, STATUS_FAILED);
    assert_eq!(collector.request_count(), 1);
}

#[tokio::test]
async fn events_are_sent_oldest_first_one_at_a_time() {
    let store = Store::open_in_memory().unwrap();
    let ids: Vec<String> = ["A", "B", "C"]
        .iter()
        .map(|plate| add_event(&store, plate).0)
        .collect();
    let collector = MockCollector::always(Reply::Ack { duplicate: false }).await;

    let mut sent = Vec::new();
    while let Step::Handled { event_id, .. } = step(&store, &collector.url).await {
        sent.push(event_id);
    }

    assert_eq!(sent, ids);
    assert_eq!(collector.request_count(), 3);
}

#[tokio::test]
async fn a_stuck_event_does_not_block_newer_events() {
    let store = Store::open_in_memory().unwrap();
    let (stuck, _) = add_event(&store, "STUCK");
    let (fine, fine_body) = add_event(&store, "FINE");
    // The collector refuses the first event for now (422) and accepts the rest.
    let stuck_body = store.get(&stuck).unwrap().unwrap().body;
    let collector = MockCollector::start(move |_, body| {
        if body == stuck_body.as_slice() {
            Reply::Status(422)
        } else {
            Reply::Ack { duplicate: false }
        }
    })
    .await;
    let client = build_client(Duration::from_secs(5));
    let now = now_ms();

    process_next(&store, &client, &settings(&collector.url), now)
        .await
        .unwrap();
    let second = process_next(&store, &client, &settings(&collector.url), now)
        .await
        .unwrap();

    assert!(matches!(second, Step::Handled { ref event_id, .. } if *event_id == fine));
    assert_eq!(store.get(&fine).unwrap().unwrap().status, STATUS_DELIVERED);
    assert_eq!(store.get(&stuck).unwrap().unwrap().status, STATUS_PENDING);
    assert_eq!(collector.bodies()[1], fine_body);
}

#[tokio::test]
async fn the_delivery_path_never_removes_rows() {
    let store = Store::open_in_memory().unwrap();
    for plate in ["A", "B", "C", "D"] {
        add_event(&store, plate);
    }
    // Deliver one, reject one, retry one, leave one untried.
    let collector = MockCollector::start(|n, _| match n {
        0 => Reply::Ack { duplicate: false },
        1 => Reply::Status(409),
        _ => Reply::Status(503),
    })
    .await;
    let client = build_client(Duration::from_secs(5));
    let now = now_ms();
    for _ in 0..3 {
        process_next(&store, &client, &settings(&collector.url), now)
            .await
            .unwrap();
    }

    assert_eq!(store.all().unwrap().len(), 4);
    let counts = store.counts().unwrap();
    assert_eq!((counts.delivered, counts.failed, counts.pending), (1, 1, 2));
}
