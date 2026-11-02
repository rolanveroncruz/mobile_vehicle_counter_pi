//! The event as the collector expects it, and the one place it is built.
//!
//! The request body is serialised **once**, when the event is accepted from the
//! camera, and those exact bytes are stored and resent on every attempt. Nothing
//! in this crate rebuilds a body from parsed fields.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a camera POSTs to the Pi: the collector's payload without `event_id`.
/// Unknown extra fields are ignored. Every field must be a string.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CameraEvent {
    pub camid: String,
    /// Expected to be RFC 3339 with an offset. The Pi does not check it: if it is
    /// wrong, the collector answers `400` and the event becomes `failed`.
    pub date: String,
    pub plate: String,
    /// Scene image, base64 JPEG.
    pub image: String,
    /// Cropped plate image, base64 JPEG.
    pub plate_image: String,
}

/// The collector's request body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub event_id: String,
    pub camid: String,
    pub date: String,
    pub plate: String,
    pub image: String,
    pub plate_image: String,
}

/// A new random (version 4) event id, lowercase and hyphenated.
///
/// `Uuid::new_v4()` draws from the operating system's random source. This is the
/// only place in the crate where an id is created. Never derive an id from the
/// plate, the time or a counter.
pub fn new_event_id() -> String {
    Uuid::new_v4().to_string()
}

/// Assigns a new `event_id` to a camera event and serialises the collector's
/// request body once. Returns the id and the exact bytes to store and send.
pub fn build(camera: CameraEvent) -> (String, Vec<u8>) {
    let event = Event {
        event_id: new_event_id(),
        camid: camera.camid,
        date: camera.date,
        plate: camera.plate,
        image: camera.image,
        plate_image: camera.plate_image,
    };
    let body = serde_json::to_vec(&event).expect("an Event always serialises");
    (event.event_id, body)
}

/// Reads the `event_id` back out of stored request-body bytes. `None` means the
/// bytes are not a JSON object with a string `event_id`.
pub fn event_id_from_body(body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct IdOnly {
        event_id: String,
    }
    serde_json::from_slice::<IdOnly>(body)
        .ok()
        .map(|parsed| parsed.event_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> CameraEvent {
        CameraEvent {
            camid: "CAM1".into(),
            date: "2026-10-30T10:00:00+08:00".into(),
            plate: "ABC1234".into(),
            image: "/9j/4AAQ".into(),
            plate_image: "/9j/4AAQ".into(),
        }
    }

    #[test]
    fn event_id_is_a_lowercase_hyphenated_v4_uuid() {
        for _ in 0..50 {
            let id = new_event_id();
            let parsed = Uuid::parse_str(&id).expect("a valid UUID");
            assert_eq!(parsed.get_version_num(), 4);
            assert_eq!(id, parsed.hyphenated().to_string());
            assert_eq!(id, id.to_lowercase());
        }
    }

    #[test]
    fn ids_are_not_repeated() {
        let ids: std::collections::HashSet<String> = (0..1000).map(|_| new_event_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn built_body_holds_the_six_fields_and_the_same_id_is_read_back() {
        let (id, body) = build(camera());
        let event: Event = serde_json::from_slice(&body).unwrap();
        assert_eq!(event.event_id, id);
        assert_eq!(event.camid, "CAM1");
        assert_eq!(event.date, "2026-10-30T10:00:00+08:00");
        assert_eq!(event.plate, "ABC1234");
        assert_eq!(event.image, "/9j/4AAQ");
        assert_eq!(event.plate_image, "/9j/4AAQ");
        assert_eq!(event_id_from_body(&body), Some(id));
    }

    #[test]
    fn every_event_gets_its_own_id_even_with_identical_content() {
        let (id1, body1) = build(camera());
        let (id2, body2) = build(camera());
        assert_ne!(id1, id2);
        assert_ne!(body1, body2);
    }

    #[test]
    fn event_id_from_body_rejects_garbage() {
        assert_eq!(event_id_from_body(b"not json"), None);
        assert_eq!(event_id_from_body(b"{}"), None);
        assert_eq!(event_id_from_body(br#"{"event_id": 5}"#), None);
        assert_eq!(event_id_from_body(b""), None);
    }

    #[test]
    fn camera_events_ignore_unknown_fields_but_need_every_field_as_a_string() {
        let ok = r#"{"camid":"C","date":"d","plate":"p","image":"i","plate_image":"x","extra":1}"#;
        assert!(serde_json::from_str::<CameraEvent>(ok).is_ok());
        let missing = r#"{"camid":"C","date":"d","plate":"p","image":"i"}"#;
        assert!(serde_json::from_str::<CameraEvent>(missing).is_err());
        let not_string = r#"{"camid":"C","date":"d","plate":5,"image":"i","plate_image":"x"}"#;
        assert!(serde_json::from_str::<CameraEvent>(not_string).is_err());
    }
}
