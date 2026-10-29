//! Integration tests for the data collector's retry / duplicate protocol.
//!
//! Each test builds its own temporary database through `db::connect` (so it runs
//! with the same WAL settings and migrations as production, and needs neither
//! `.env` nor a pre-existing database) and its own images directory, then calls
//! `process_event` directly with raw request bytes, exactly as the HTTP handler does.

use std::path::PathBuf;

use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use mobile_vehicle_counter_server::collector_server::{
    CollectorState, EventAck, content_hash, process_event,
};
use mobile_vehicle_counter_server::db::{self, MissingDatabase};
use mobile_vehicle_counter_server::entities::alpr_events;
use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait};
use uuid::Uuid;

/// A throwaway database + images directory, deleted on drop.
struct TestEnv {
    state: CollectorState,
    dir: PathBuf,
}

impl TestEnv {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("collector_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}/test.db?mode=rw", dir.display());
        let db_conn = db::connect(&url, MissingDatabase::CreateAndMigrate)
            .await
            .unwrap();
        let images_dir = dir.join("images");
        std::fs::create_dir_all(&images_dir).unwrap();

        // The collector rejects events from unregistered cameras.
        for sql in [
            "INSERT INTO location(id, name, description) VALUES('L1','Loc','d')",
            "INSERT INTO sublocation(id, name, description, location_id) VALUES('S1','Sub',NULL,'L1')",
            "INSERT INTO camera(id, name, description, sublocation_id, orientation) VALUES('CAM1','Cam','d','S1','Inbound')",
        ] {
            db_conn.execute_unprepared(sql).await.unwrap();
        }

        Self {
            state: CollectorState {
                db_conn,
                images_dir,
            },
            dir,
        }
    }

    async fn event_count(&self) -> u64 {
        alpr_events::Entity::find()
            .count(&self.state.db_conn)
            .await
            .unwrap()
    }

    fn image_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.state.images_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Builds a request body. Returned as bytes because the hash is over raw bytes.
fn body(event_id: &str, camid: &str, plate: &str, image: &[u8]) -> Vec<u8> {
    serde_json::json!({
        "event_id": event_id,
        "camid": camid,
        "date": "2026-10-30T10:00:00.123Z",
        "plate": plate,
        "image": b64(image),
        "plate_image": b64(b"plate-bytes"),
    })
    .to_string()
    .into_bytes()
}

fn ack(text: &str) -> EventAck {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not an ack ({e}): {text}"))
}

#[tokio::test]
async fn first_post_is_stored_and_acked_with_the_hash_of_the_body() {
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();
    let req = body(&id, "CAM1", "ABC1234", b"scene");

    let (status, text) = process_event(&env.state, &req).await;

    assert_eq!(status, StatusCode::OK);
    let a = ack(&text);
    assert_eq!(a.event_id, id);
    assert!(!a.duplicate);
    // What the Pi would compute independently over the bytes it sent.
    assert_eq!(a.content_hash, content_hash(&req));
    assert_eq!(a.content_hash.len(), 64);

    assert_eq!(env.event_count().await, 1);
    let row = alpr_events::Entity::find_by_id(id.clone())
        .one(&env.state.db_conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.content_hash.as_deref(), Some(a.content_hash.as_str()));
    assert_eq!(row.plate, "ABC1234");
    // Stored in the +08:00 storage offset.
    assert_eq!(row.date.to_rfc3339(), "2026-10-30T18:00:00.123+08:00");
    assert_eq!(
        env.image_files(),
        vec![format!("event_{id}.jpg"), format!("event_{id}_plate.jpg")]
    );
}

#[tokio::test]
async fn retrying_the_same_bytes_is_a_200_duplicate_and_stores_nothing_new() {
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();
    let req = body(&id, "CAM1", "ABC1234", b"scene");

    let (_, first) = process_event(&env.state, &req).await;
    let (status, second) = process_event(&env.state, &req).await;
    let (status3, third) = process_event(&env.state, &req).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(status3, StatusCode::OK);
    let (a1, a2, a3) = (ack(&first), ack(&second), ack(&third));
    assert!(!a1.duplicate);
    assert!(a2.duplicate && a3.duplicate);
    // The ack is identical apart from the flag, so the Pi's check still passes.
    assert_eq!(a1.content_hash, a2.content_hash);
    assert_eq!(a1.event_id, a2.event_id);

    assert_eq!(env.event_count().await, 1);
    assert_eq!(env.image_files().len(), 2, "a retry must not add files");
}

#[tokio::test]
async fn a_retry_does_not_rewrite_or_delete_the_original_images() {
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();
    let req = body(&id, "CAM1", "ABC1234", b"original-scene");
    process_event(&env.state, &req).await;

    let scene = env.state.images_dir.join(format!("event_{id}.jpg"));
    assert_eq!(std::fs::read(&scene).unwrap(), b"original-scene");

    process_event(&env.state, &req).await;

    assert_eq!(std::fs::read(&scene).unwrap(), b"original-scene");
}

#[tokio::test]
async fn same_event_id_with_different_content_is_a_409_and_changes_nothing() {
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();
    process_event(&env.state, &body(&id, "CAM1", "ABC1234", b"scene")).await;

    let (status, text) =
        process_event(&env.state, &body(&id, "CAM1", "ZZZ9999", b"other-scene")).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(text.contains("different content"), "{text}");
    assert_eq!(env.event_count().await, 1);
    let row = alpr_events::Entity::find_by_id(id.clone())
        .one(&env.state.db_conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.plate, "ABC1234", "the original must be untouched");
    let scene = env.state.images_dir.join(format!("event_{id}.jpg"));
    assert_eq!(std::fs::read(scene).unwrap(), b"scene");
}

#[tokio::test]
async fn different_event_ids_with_otherwise_identical_data_are_two_events() {
    // The reason for a Pi-supplied id: two genuine events that look alike are kept.
    let env = TestEnv::new().await;
    let (s1, _) = process_event(
        &env.state,
        &body(&Uuid::new_v4().to_string(), "CAM1", "ABC1234", b"scene"),
    )
    .await;
    let (s2, t2) = process_event(
        &env.state,
        &body(&Uuid::new_v4().to_string(), "CAM1", "ABC1234", b"scene"),
    )
    .await;

    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
    assert!(!ack(&t2).duplicate);
    assert_eq!(env.event_count().await, 2);
}

#[tokio::test]
async fn uuid_spelling_variants_are_the_same_event() {
    // Normalised to lowercase-hyphenated, so an uppercase respelling cannot become
    // a second event. The bytes differ, so it is reported as a conflict.
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();
    process_event(&env.state, &body(&id, "CAM1", "ABC1234", b"scene")).await;

    let (status, _) = process_event(
        &env.state,
        &body(&id.to_uppercase(), "CAM1", "ABC1234", b"scene"),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(env.event_count().await, 1);
}

#[tokio::test]
async fn invalid_event_ids_are_rejected_with_400_and_write_nothing() {
    let env = TestEnv::new().await;
    for bad in ["", "not-a-uuid", "../../etc/passwd", "123"] {
        let (status, _) = process_event(&env.state, &body(bad, "CAM1", "ABC1234", b"s")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "event_id {bad:?}");
    }
    assert_eq!(env.event_count().await, 0);
    assert!(env.image_files().is_empty());
}

#[tokio::test]
async fn a_missing_event_id_is_a_400() {
    let env = TestEnv::new().await;
    let req = serde_json::json!({
        "camid": "CAM1", "date": "2026-10-30T10:00:00Z", "plate": "A",
        "image": b64(b"s"), "plate_image": b64(b"p"),
    })
    .to_string();

    let (status, _) = process_event(&env.state, req.as_bytes()).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn existing_rejections_are_unchanged() {
    let env = TestEnv::new().await;
    let id = || Uuid::new_v4().to_string();

    let (s, _) = process_event(&env.state, b"garbage").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, _) = process_event(&env.state, &body(&id(), "NOPE", "A", b"s")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    let bad_date = serde_json::json!({
        "event_id": id(), "camid": "CAM1", "date": "yesterday", "plate": "A",
        "image": b64(b"s"), "plate_image": b64(b"p"),
    })
    .to_string();
    let (s, _) = process_event(&env.state, bad_date.as_bytes()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let bad_b64 = serde_json::json!({
        "event_id": id(), "camid": "CAM1", "date": "2026-10-30T10:00:00Z", "plate": "A",
        "image": "!!!", "plate_image": b64(b"p"),
    })
    .to_string();
    let (s, _) = process_event(&env.state, bad_b64.as_bytes()).await;
    assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR);

    assert_eq!(env.event_count().await, 0);
    assert!(
        env.image_files().is_empty(),
        "failures must not leave files"
    );
}

#[tokio::test]
async fn a_failed_attempt_can_be_retried_successfully() {
    // The Pi retries after a failure. A rejected attempt must not "reserve" the id.
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();

    let (s, _) = process_event(&env.state, &body(&id, "UNREGISTERED", "A", b"s")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Camera gets registered, then the Pi retries the same event.
    env.state
        .db_conn
        .execute_unprepared(
            "INSERT INTO camera(id, name, description, sublocation_id, orientation) \
             VALUES('UNREGISTERED','c','d','S1','Outbound')",
        )
        .await
        .unwrap();
    let (s, text) = process_event(&env.state, &body(&id, "UNREGISTERED", "A", b"s")).await;

    assert_eq!(s, StatusCode::OK);
    assert!(!ack(&text).duplicate);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_retries_of_one_event_store_it_once() {
    // The retry may arrive while the first attempt is still in flight. Whatever
    // the interleaving, exactly one row and one pair of files must result, and
    // every request must get a 200 (never a 500 from the unique constraint).
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();
    let req = body(&id, "CAM1", "ABC1234", &vec![7u8; 200_000]);

    let mut tasks = Vec::new();
    for _ in 0..12 {
        let state = env.state.clone();
        let req = req.clone();
        tasks.push(tokio::spawn(
            async move { process_event(&state, &req).await },
        ));
    }
    let mut originals = 0;
    for task in tasks {
        let (status, text) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{text}");
        if !ack(&text).duplicate {
            originals += 1;
        }
    }

    assert_eq!(originals, 1, "exactly one request is the original");
    assert_eq!(env.event_count().await, 1);
    assert_eq!(env.image_files().len(), 2);
    let scene = env.state.images_dir.join(format!("event_{id}.jpg"));
    assert_eq!(std::fs::read(scene).unwrap(), vec![7u8; 200_000]);
}

#[tokio::test]
async fn rows_without_a_hash_do_not_conflict_with_each_other() {
    // Rows that predate the protocol (e.g. copied by migrate_data) have a NULL
    // hash. The UNIQUE index must allow any number of them.
    let env = TestEnv::new().await;
    for n in 0..3 {
        env.state
            .db_conn
            .execute_unprepared(&format!(
                "INSERT INTO alpr_events(id, camera_id, date, plate, image, plate_image) \
                 VALUES('legacy-{n}','CAM1','2026-01-01T00:00:00+08:00','P','i','p')"
            ))
            .await
            .unwrap();
    }
    assert_eq!(env.event_count().await, 3);

    // And a new event can still be added next to them.
    let (s, _) = process_event(
        &env.state,
        &body(&Uuid::new_v4().to_string(), "CAM1", "A", b"s"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(env.event_count().await, 4);
}

#[tokio::test]
async fn reusing_the_id_of_a_legacy_row_is_a_conflict_not_a_duplicate() {
    // The stored hash is NULL, so the content cannot be compared. Claiming
    // "duplicate" would make the Pi discard data we never actually stored.
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();
    env.state
        .db_conn
        .execute_unprepared(&format!(
            "INSERT INTO alpr_events(id, camera_id, date, plate, image, plate_image) \
             VALUES('{id}','CAM1','2026-01-01T00:00:00+08:00','P','i','p')"
        ))
        .await
        .unwrap();

    let (status, _) = process_event(&env.state, &body(&id, "CAM1", "A", b"s")).await;

    assert_eq!(status, StatusCode::CONFLICT);
}

#[test]
fn content_hash_is_lowercase_hex_sha256() {
    // Known SHA-256 test vector, so the Pi can verify its implementation too.
    assert_eq!(
        content_hash(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        content_hash(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[tokio::test]
async fn a_retry_after_a_crash_between_files_and_insert_recovers() {
    // Simulates a crash after the images were written but before the row was
    // inserted: files exist, no row. The Pi's retry must succeed and leave the
    // correct (new) bytes in place, rather than being blocked by the leftovers.
    let env = TestEnv::new().await;
    let id = Uuid::new_v4().to_string();
    std::fs::write(
        env.state.images_dir.join(format!("event_{id}.jpg")),
        b"stale-from-crash",
    )
    .unwrap();
    std::fs::write(
        env.state.images_dir.join(format!("event_{id}_plate.jpg")),
        b"stale-plate",
    )
    .unwrap();
    assert_eq!(env.event_count().await, 0);

    let (status, text) =
        process_event(&env.state, &body(&id, "CAM1", "ABC1234", b"good-scene")).await;

    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!ack(&text).duplicate);
    assert_eq!(env.event_count().await, 1);
    assert_eq!(env.image_files().len(), 2, "no extra files");
    let scene = env.state.images_dir.join(format!("event_{id}.jpg"));
    assert_eq!(std::fs::read(scene).unwrap(), b"good-scene");
}
