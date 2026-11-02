//! Pure rules: what to do with a stored event after one delivery attempt.
//!
//! This is the README's "Errors and what the Pi should do" table, in code.
//! Nothing here does I/O, so every row of the table is unit-tested below.

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::event::event_id_from_body;

/// What to do with an event after one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Stored and verified. Set the status to `delivered`.
    Delivered,
    /// Not confirmed. Leave the status `pending` and try again later.
    /// `slow` means the problem needs outside action (`401` token, `422` camera
    /// not registered), so the wait starts at the backoff cap.
    Retry { slow: bool, reason: String },
    /// Retrying the same bytes can never work. Set the status to `failed` and alert.
    Reject { reason: String },
}

/// Lowercase hex SHA-256, the same as `sha256sum`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The collector's `200` reply.
#[derive(Debug, Deserialize)]
struct Ack {
    event_id: String,
    content_hash: String,
}

/// Checks the collector's receipt against what was sent.
///
/// The event is delivered only if the reply carries the `event_id` that was sent
/// and a `content_hash` equal to the SHA-256 of the exact bytes that were sent.
pub fn verify_receipt(
    response_body: &[u8],
    sent_event_id: &str,
    sent_body: &[u8],
) -> Result<(), String> {
    let ack: Ack = serde_json::from_slice(response_body)
        .map_err(|error| format!("200 reply is not a valid receipt: {error}"))?;
    if ack.event_id != sent_event_id {
        return Err(format!(
            "receipt is for event_id {} but {} was sent",
            ack.event_id, sent_event_id
        ));
    }
    let expected = sha256_hex(sent_body);
    if ack.content_hash != expected {
        return Err(format!(
            "receipt content_hash {} does not match the sent bytes ({})",
            ack.content_hash, expected
        ));
    }
    Ok(())
}

/// Classifies the collector's reply to one POST.
///
/// `sent_body` are the stored bytes that were sent, unchanged.
pub fn classify(status: u16, response_body: &[u8], sent_body: &[u8]) -> Outcome {
    match status {
        200 => {
            let Some(sent_event_id) = event_id_from_body(sent_body) else {
                return Outcome::Reject {
                    reason: "stored body has no event_id".to_string(),
                };
            };
            match verify_receipt(response_body, &sent_event_id, sent_body) {
                Ok(()) => Outcome::Delivered,
                // A bad receipt is never "delivered", but retrying is the safe
                // response: the same bytes return the same receipt.
                Err(reason) => Outcome::Retry {
                    slow: false,
                    reason,
                },
            }
        }
        // Needs outside action, but the same bytes succeed once it is done.
        401 => Outcome::Retry {
            slow: true,
            reason: "HTTP 401: collector token rejected".to_string(),
        },
        422 => Outcome::Retry {
            slow: true,
            reason: "HTTP 422: camid is not registered on the server".to_string(),
        },
        429 => Outcome::Retry {
            slow: false,
            reason: "HTTP 429: rate limited".to_string(),
        },
        500..=599 => Outcome::Retry {
            slow: false,
            reason: format!("HTTP {status}: server or proxy failure"),
        },
        400 | 403 | 404 | 409 | 413 => Outcome::Reject {
            reason: format!("HTTP {status}: {}", reject_meaning(status)),
        },
        // Not in the README's table (other 2xx, 3xx, 4xx). Retrying the same
        // bytes is not expected to help, so keep the event for a human.
        other => Outcome::Reject {
            reason: format!("HTTP {other}: unexpected status"),
        },
    }
}

fn reject_meaning(status: u16) -> &'static str {
    match status {
        400 => "collector says the request is malformed",
        403 => "wrong HTTP method",
        404 => "wrong URL",
        409 => "event_id exists with different content",
        413 => "body too large for the collector",
        _ => "rejected",
    }
}

/// A transport failure (timeout, no connection, unreadable reply). The request
/// may or may not have been stored; resending the same bytes is what the
/// protocol is for.
pub fn transport_error(reason: impl Into<String>) -> Outcome {
    Outcome::Retry {
        slow: false,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &[u8] = br#"{"event_id":"49ab6e94-d7ae-4952-a0d2-d0fe89d25771","camid":"CAM1"}"#;
    const ID: &str = "49ab6e94-d7ae-4952-a0d2-d0fe89d25771";

    fn receipt(event_id: &str, hash: &str, duplicate: bool) -> Vec<u8> {
        serde_json::json!({
            "event_id": event_id, "content_hash": hash, "duplicate": duplicate
        })
        .to_string()
        .into_bytes()
    }

    fn good_receipt(duplicate: bool) -> Vec<u8> {
        receipt(ID, &sha256_hex(BODY), duplicate)
    }

    fn is_retry(outcome: &Outcome, want_slow: bool) -> bool {
        matches!(outcome, Outcome::Retry { slow, .. } if *slow == want_slow)
    }

    #[test]
    fn hash_test_vectors_from_the_readme() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verified_200_is_delivered_for_new_and_duplicate_events_alike() {
        assert_eq!(
            classify(200, &good_receipt(false), BODY),
            Outcome::Delivered
        );
        assert_eq!(classify(200, &good_receipt(true), BODY), Outcome::Delivered);
    }

    #[test]
    fn a_200_with_a_wrong_hash_is_never_delivered() {
        let reply = receipt(ID, &sha256_hex(b"something else"), false);
        assert!(is_retry(&classify(200, &reply, BODY), false));
    }

    #[test]
    fn a_200_with_a_wrong_event_id_is_never_delivered() {
        let reply = receipt(
            "00000000-0000-4000-8000-000000000000",
            &sha256_hex(BODY),
            false,
        );
        assert!(is_retry(&classify(200, &reply, BODY), false));
    }

    #[test]
    fn a_200_with_an_unreadable_receipt_is_never_delivered() {
        assert!(is_retry(&classify(200, b"OK", BODY), false));
        assert!(is_retry(&classify(200, b"", BODY), false));
        assert!(is_retry(
            &classify(200, br#"{"event_id":"x"}"#, BODY),
            false
        ));
    }

    #[test]
    fn an_uppercase_hash_does_not_match() {
        let reply = receipt(ID, &sha256_hex(BODY).to_uppercase(), false);
        assert!(is_retry(&classify(200, &reply, BODY), false));
    }

    #[test]
    fn a_200_for_a_body_without_an_event_id_is_rejected() {
        let outcome = classify(200, &good_receipt(false), b"garbage");
        assert!(matches!(outcome, Outcome::Reject { .. }));
    }

    #[test]
    fn statuses_that_can_never_succeed_are_rejected() {
        for status in [400, 403, 404, 409, 413] {
            let outcome = classify(status, b"text", BODY);
            assert!(
                matches!(outcome, Outcome::Reject { .. }),
                "{status}: {outcome:?}"
            );
        }
    }

    #[test]
    fn token_and_unregistered_camera_retry_slowly() {
        assert!(is_retry(&classify(401, b"Unauthorized", BODY), true));
        assert!(is_retry(&classify(422, b"unknown camera", BODY), true));
    }

    #[test]
    fn rate_limit_and_server_failures_retry_normally() {
        for status in [429, 500, 502, 503, 504, 599] {
            assert!(
                is_retry(&classify(status, b"", BODY), false),
                "status {status}"
            );
        }
    }

    #[test]
    fn statuses_outside_the_table_are_rejected_not_silently_retried() {
        for status in [201, 204, 301, 302, 405, 418] {
            let outcome = classify(status, b"", BODY);
            assert!(
                matches!(outcome, Outcome::Reject { .. }),
                "{status}: {outcome:?}"
            );
        }
    }

    #[test]
    fn transport_errors_retry_normally() {
        assert!(is_retry(&transport_error("timed out"), false));
    }

    #[test]
    fn reasons_say_what_happened() {
        let Outcome::Reject { reason } = classify(409, b"", BODY) else {
            panic!("expected reject")
        };
        assert!(reason.contains("409"), "{reason}");
    }
}
