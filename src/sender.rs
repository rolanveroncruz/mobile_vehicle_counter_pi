//! The delivery loop: send stored events to the collector, verify the receipt,
//! and record the result in the database.
//!
//! One event at a time. The oldest `pending` event whose `next_attempt_at` has
//! passed goes first. An event that has to wait (backoff) does not hold up newer
//! events.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::outcome::{Outcome, classify, transport_error};
use crate::store::{DueEvent, Store, now_ms};

/// How often the loop looks for work when nothing wakes it.
const IDLE_POLL: Duration = Duration::from_secs(1);
/// First wait after a failed attempt; doubles each time up to the cap.
const BACKOFF_START: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct SenderSettings {
    pub url: String,
    pub token: String,
    pub backoff_cap: Duration,
}

/// What one call to [`process_next`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing is due.
    Idle,
    /// One event was attempted.
    Handled { event_id: String, outcome: Outcome },
}

/// An HTTP client for the collector. It never follows redirects: a redirected POST
/// can turn into a GET, which would silently lose the event body.
pub fn build_client(request_timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(request_timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("the HTTP client configuration is valid")
}

/// How long to wait before the next attempt.
///
/// `attempts` is the number of attempts made so far, including the one that just
/// failed. The wait is 2 s, 4 s, 8 s ... up to `cap`. With `slow` (a problem that
/// needs outside action, such as a rejected token) it starts at `cap`. `jitter`
/// is in `[0, 1]`; the result is between half and all of the nominal wait, so a
/// recovering server is not hit by every event at once.
pub fn backoff_delay(attempts: i64, slow: bool, cap: Duration, jitter: f64) -> Duration {
    let nominal = if slow {
        cap
    } else {
        let doublings = (attempts.clamp(1, 32) - 1) as u32;
        BACKOFF_START
            .saturating_mul(1u32.checked_shl(doublings).unwrap_or(u32::MAX))
            .min(cap)
    };
    nominal.mul_f64(0.5 + 0.5 * jitter.clamp(0.0, 1.0))
}

/// POSTs the stored bytes, unchanged, and says what to do with the event.
async fn attempt(client: &reqwest::Client, settings: &SenderSettings, body: &[u8]) -> Outcome {
    let response = match client
        .post(&settings.url)
        .header("Content-Type", "application/json")
        .header("X-Collector-Token", &settings.token)
        .body(body.to_vec())
        .send()
        .await
    {
        Ok(response) => response,
        // Timeout or no connection: the event may or may not have been stored.
        // Resending the same bytes is exactly what the protocol is for.
        Err(error) => return transport_error(format!("request failed: {error}")),
    };
    let status = response.status().as_u16();
    match response.bytes().await {
        Ok(reply) => classify(status, &reply, body),
        Err(error) => transport_error(format!("could not read the reply (HTTP {status}): {error}")),
    }
}

/// Records an outcome in the database. Rows are updated, never deleted.
fn record(
    store: &Store,
    event: &DueEvent,
    outcome: &Outcome,
    backoff_cap: Duration,
    now_ms: i64,
) -> rusqlite::Result<()> {
    match outcome {
        Outcome::Delivered => {
            store.mark_delivered(&event.event_id)?;
            println!("delivered event {}", event.event_id);
        }
        Outcome::Retry { slow, reason } => {
            let delay = backoff_delay(event.attempts + 1, *slow, backoff_cap, rand::random());
            let next = now_ms.saturating_add(delay.as_millis() as i64);
            store.schedule_retry(&event.event_id, next, reason)?;
            eprintln!(
                "WARN event {} not delivered ({reason}); retry in {:.0}s",
                event.event_id,
                delay.as_secs_f64()
            );
        }
        Outcome::Reject { reason } => {
            store.mark_failed(&event.event_id, reason)?;
            eprintln!(
                "ERROR ALERT: event {} is now 'failed' and will not be retried: {reason}",
                event.event_id
            );
        }
    }
    Ok(())
}

/// Attempts the next due event, if any, and records the result.
///
/// `now_ms` is passed in so that tests can control which events are due.
pub async fn process_next(
    store: &Store,
    client: &reqwest::Client,
    settings: &SenderSettings,
    now_ms: i64,
) -> rusqlite::Result<Step> {
    let due = {
        let store = store.clone();
        tokio::task::spawn_blocking(move || store.next_due(now_ms))
            .await
            .expect("database task panicked")?
    };
    let Some(event) = due else {
        return Ok(Step::Idle);
    };

    let outcome = attempt(client, settings, &event.body).await;

    {
        let store = store.clone();
        let event = event.clone();
        let outcome = outcome.clone();
        let cap = settings.backoff_cap;
        tokio::task::spawn_blocking(move || record(&store, &event, &outcome, cap, now_ms))
            .await
            .expect("database task panicked")?;
    }
    Ok(Step::Handled {
        event_id: event.event_id,
        outcome,
    })
}

/// Runs forever: send due events, then wait for new ones (or a short poll).
pub async fn run(
    store: Store,
    settings: SenderSettings,
    request_timeout: Duration,
    wake: Arc<Notify>,
) {
    let client = build_client(request_timeout);
    loop {
        match process_next(&store, &client, &settings, now_ms()).await {
            Ok(Step::Handled { .. }) => continue,
            Ok(Step::Idle) => {}
            Err(error) => {
                eprintln!("ERROR database error in the sender: {error}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }
        tokio::select! {
            _ = wake.notified() => {}
            _ = tokio::time::sleep(IDLE_POLL) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Duration = Duration::from_secs(300);

    fn secs(attempts: i64, slow: bool, jitter: f64) -> f64 {
        backoff_delay(attempts, slow, CAP, jitter).as_secs_f64()
    }

    #[test]
    fn the_wait_doubles_from_two_seconds() {
        assert_eq!(secs(1, false, 1.0), 2.0);
        assert_eq!(secs(2, false, 1.0), 4.0);
        assert_eq!(secs(3, false, 1.0), 8.0);
        assert_eq!(secs(4, false, 1.0), 16.0);
    }

    #[test]
    fn the_wait_is_capped() {
        assert_eq!(secs(9, false, 1.0), 300.0);
        assert_eq!(secs(10, false, 1.0), 300.0);
        assert_eq!(secs(1_000_000, false, 1.0), 300.0);
        assert_eq!(secs(i64::MAX, false, 1.0), 300.0);
    }

    #[test]
    fn slow_problems_start_at_the_cap() {
        assert_eq!(secs(1, true, 1.0), 300.0);
        assert_eq!(secs(5, true, 1.0), 300.0);
    }

    #[test]
    fn jitter_gives_between_half_and_all_of_the_wait() {
        assert_eq!(secs(3, false, 0.0), 4.0);
        assert_eq!(secs(3, false, 0.5), 6.0);
        assert_eq!(secs(3, false, 1.0), 8.0);
        assert_eq!(secs(3, false, -5.0), 4.0, "out-of-range jitter is clamped");
        assert_eq!(secs(3, false, 5.0), 8.0);
    }

    #[test]
    fn a_zero_or_negative_attempt_count_is_treated_as_the_first() {
        assert_eq!(secs(0, false, 1.0), 2.0);
        assert_eq!(secs(-3, false, 1.0), 2.0);
    }

    #[test]
    fn a_cap_below_the_first_wait_wins() {
        let delay = backoff_delay(1, false, Duration::from_secs(1), 1.0);
        assert_eq!(delay, Duration::from_secs(1));
    }
}
