//! Settings, read from the environment (a `.env` file is loaded by the binary).
//!
//! | Variable                | Meaning                                          | Default                  |
//! |-------------------------|--------------------------------------------------|--------------------------|
//! | `COLLECTOR_URL`         | Collector endpoint (`http://` or `https://`)     | required                 |
//! | `COLLECTOR_TOKEN`       | Value of the `X-Collector-Token` header          | required                 |
//! | `PI_DB_PATH`            | Local SQLite file                                | `pi_data/pi_events.db`   |
//! | `PI_LISTEN_ADDR`        | Where nginx forwards the cameras' POSTs to       | `127.0.0.1:3000`         |
//! | `REQUEST_TIMEOUT_SECS`  | Per-request timeout to the collector             | `60`                     |
//! | `MAX_CAMERA_BODY_BYTES` | Cap on one camera POST                           | `67108864` (64 MB)       |
//! | `BACKOFF_CAP_SECS`      | Longest wait between attempts for one event      | `300`                    |

use std::env;
use std::str::FromStr;
use std::time::Duration;

pub const DEFAULT_DB_PATH: &str = "pi_data/pi_events.db";
/// Loopback only: the cameras reach the Pi through nginx (see `deploy/nginx-pi.conf`),
/// which forwards to this address. The camera endpoint has no authentication, so
/// it must not be exposed on the LAN directly.
pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:3000";
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 60;
/// Well above the collector's own 20 MB limit, so that an event the collector
/// would refuse (`413`) is still stored here and fails through the normal path,
/// instead of being lost because the camera cannot retry.
pub const DEFAULT_MAX_CAMERA_BODY_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_BACKOFF_CAP_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub struct Config {
    pub collector_url: String,
    pub collector_token: String,
    pub db_path: String,
    pub listen_addr: String,
    pub request_timeout: Duration,
    pub max_camera_body_bytes: usize,
    pub backoff_cap: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|key| env::var(key).ok())
    }

    /// Builds the configuration from any key lookup, so it can be tested without
    /// touching the process environment.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let collector_url = required(&get, "COLLECTOR_URL")?;
        if !(collector_url.starts_with("http://") || collector_url.starts_with("https://")) {
            return Err(format!(
                "COLLECTOR_URL must start with http:// or https://, got {collector_url:?}"
            ));
        }
        let collector_token = required(&get, "COLLECTOR_TOKEN")?;

        let request_timeout_secs: u64 =
            parse_or(&get, "REQUEST_TIMEOUT_SECS", DEFAULT_REQUEST_TIMEOUT_SECS)?;
        let backoff_cap_secs: u64 = parse_or(&get, "BACKOFF_CAP_SECS", DEFAULT_BACKOFF_CAP_SECS)?;
        let max_camera_body_bytes: usize =
            parse_or(&get, "MAX_CAMERA_BODY_BYTES", DEFAULT_MAX_CAMERA_BODY_BYTES)?;
        if request_timeout_secs == 0 || backoff_cap_secs == 0 || max_camera_body_bytes == 0 {
            return Err(
                "REQUEST_TIMEOUT_SECS, BACKOFF_CAP_SECS and MAX_CAMERA_BODY_BYTES must be above 0"
                    .to_string(),
            );
        }

        Ok(Self {
            collector_url,
            collector_token,
            db_path: get("PI_DB_PATH").unwrap_or_else(|| DEFAULT_DB_PATH.to_string()),
            listen_addr: get("PI_LISTEN_ADDR").unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_string()),
            request_timeout: Duration::from_secs(request_timeout_secs),
            max_camera_body_bytes,
            backoff_cap: Duration::from_secs(backoff_cap_secs),
        })
    }
}

fn required(get: &impl Fn(&str) -> Option<String>, key: &str) -> Result<String, String> {
    match get(key) {
        Some(value) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        _ => Err(format!("{key} must be set (in the environment or .env)")),
    }
}

fn parse_or<T: FromStr>(
    get: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: T,
) -> Result<T, String> {
    match get(key) {
        None => Ok(default),
        Some(value) => value
            .trim()
            .parse()
            .map_err(|_| format!("{key} has an invalid value: {value:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn defaults_apply_when_only_required_settings_are_given() {
        let config = Config::from_lookup(lookup(&[
            ("COLLECTOR_URL", "http://collector.test/api/alpr/events"),
            ("COLLECTOR_TOKEN", "secret"),
        ]))
        .unwrap();
        assert_eq!(config.db_path, DEFAULT_DB_PATH);
        assert_eq!(config.listen_addr, DEFAULT_LISTEN_ADDR);
        assert_eq!(config.request_timeout, Duration::from_secs(60));
        assert_eq!(config.max_camera_body_bytes, 64 * 1024 * 1024);
        assert_eq!(config.backoff_cap, Duration::from_secs(300));
    }

    #[test]
    fn the_camera_endpoint_listens_on_loopback_by_default() {
        assert_eq!(DEFAULT_LISTEN_ADDR, "127.0.0.1:3000");
    }

    #[test]
    fn missing_url_or_token_is_an_error() {
        assert!(Config::from_lookup(lookup(&[("COLLECTOR_TOKEN", "x")])).is_err());
        assert!(Config::from_lookup(lookup(&[("COLLECTOR_URL", "http://a/b")])).is_err());
        assert!(
            Config::from_lookup(lookup(&[
                ("COLLECTOR_URL", "http://a/b"),
                ("COLLECTOR_TOKEN", "   ")
            ]))
            .is_err()
        );
    }

    #[test]
    fn url_must_be_http_or_https() {
        let err = Config::from_lookup(lookup(&[
            ("COLLECTOR_URL", "ftp://a/b"),
            ("COLLECTOR_TOKEN", "x"),
        ]))
        .unwrap_err();
        assert!(err.contains("COLLECTOR_URL"), "{err}");
    }

    #[test]
    fn overrides_and_bad_numbers() {
        let config = Config::from_lookup(lookup(&[
            ("COLLECTOR_URL", "https://a/b"),
            ("COLLECTOR_TOKEN", "x"),
            ("REQUEST_TIMEOUT_SECS", "30"),
            ("PI_LISTEN_ADDR", "127.0.0.1:4000"),
        ]))
        .unwrap();
        assert_eq!(config.request_timeout, Duration::from_secs(30));
        assert_eq!(config.listen_addr, "127.0.0.1:4000");

        assert!(
            Config::from_lookup(lookup(&[
                ("COLLECTOR_URL", "https://a/b"),
                ("COLLECTOR_TOKEN", "x"),
                ("REQUEST_TIMEOUT_SECS", "abc"),
            ]))
            .is_err()
        );
        assert!(
            Config::from_lookup(lookup(&[
                ("COLLECTOR_URL", "https://a/b"),
                ("COLLECTOR_TOKEN", "x"),
                ("BACKOFF_CAP_SECS", "0"),
            ]))
            .is_err()
        );
    }
}
