//! The single time zone used for every stored event date.
//!
//! `alpr_events.date` is stored as RFC 3339 **text**, and SQLite compares and
//! sorts that text character by character. That matches time order only when
//! every stored value (and every query bound) uses the **same offset**.
//!
//! The whole system runs in the Philippines, so all dates are normalised to
//! `+08:00` before they are written (the collector) or used to filter rows
//! (the API). If the deployment ever spans more than one time zone, this
//! assumption has to be revisited properly. See `TODO.md`.

use chrono::{DateTime, FixedOffset};

/// UTC+08:00, Philippine Standard Time (no daylight saving).
pub const STORAGE_OFFSET_SECS: i32 = 8 * 3600;

/// The offset every stored event date uses.
pub fn storage_offset() -> FixedOffset {
    FixedOffset::east_opt(STORAGE_OFFSET_SECS).expect("+08:00 is a valid offset")
}

/// Converts `dt` to the storage offset. The instant in time is unchanged; only
/// the offset used to write it down changes.
pub fn to_storage_offset(dt: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    dt.with_timezone(&storage_offset())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    #[test]
    fn utc_is_converted_to_plus_eight() {
        let converted = to_storage_offset(parse("2026-10-09T09:00:00Z"));
        assert_eq!(converted.to_rfc3339(), "2026-10-09T17:00:00+08:00");
    }

    #[test]
    fn plus_eight_is_unchanged() {
        let converted = to_storage_offset(parse("2026-10-09T16:44:25.443+08:00"));
        assert_eq!(converted.to_rfc3339(), "2026-10-09T16:44:25.443+08:00");
    }

    #[test]
    fn other_offsets_are_converted() {
        let converted = to_storage_offset(parse("2026-10-09T00:30:00-05:00"));
        assert_eq!(converted.to_rfc3339(), "2026-10-09T13:30:00+08:00");
    }

    #[test]
    fn the_instant_never_changes() {
        for s in [
            "2026-10-09T09:00:00Z",
            "2026-10-09T16:44:25.443+08:00",
            "2026-12-31T23:59:59.999-12:00",
        ] {
            let original = parse(s);
            assert_eq!(to_storage_offset(original), original);
        }
    }

    /// The whole point of the module: after conversion, comparing the text
    /// gives the same answer as comparing the moments in time.
    #[test]
    fn text_order_matches_time_order_after_conversion() {
        let samples = [
            "2026-10-09T08:44:25.443Z",
            "2026-10-09T16:44:25.443+08:00",
            "2026-10-09T09:00:00+00:00",
            "2026-10-09T17:00:00+08:00",
            "2026-10-09T16:44:25+08:00",
            "2026-10-09T03:00:00-05:00",
            "2026-10-10T00:00:00+08:00",
        ];
        for a in samples {
            for b in samples {
                let (da, db) = (parse(a), parse(b));
                let (ta, tb) = (
                    to_storage_offset(da).to_rfc3339(),
                    to_storage_offset(db).to_rfc3339(),
                );
                assert_eq!(da < db, ta < tb, "{a} vs {b}");
            }
        }
    }
}
