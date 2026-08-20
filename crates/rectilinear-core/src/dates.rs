//! Date input normalization for filter parameters.
//!
//! Accepts absolute dates (`2026-08-01`, RFC 3339 timestamps) and relative
//! durations (`7d`, `24h`, `90m`), normalizing everything to an RFC 3339 UTC
//! timestamp string suitable for lexicographic comparison against the
//! `created_at`/`updated_at` columns.

use anyhow::Result;
use chrono::{DateTime, Utc};

/// Stored Linear timestamps carry millisecond precision ("…:01.261Z"), and
/// filters are compared lexicographically in SQL. Emitting the same precision
/// keeps '.' vs 'Z' ordering from misplacing records inside the boundary second.
fn format_utc(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Normalize a user-supplied date filter input to an RFC 3339 UTC timestamp.
///
/// Relative durations are interpreted as "this long before `now`".
pub fn resolve_date_input(input: &str, now: DateTime<Utc>) -> Result<String> {
    let input = input.trim();

    // Relative duration: <positive integer><d|h|m>
    if let Some(unit) = input.chars().last() {
        if matches!(unit, 'd' | 'h' | 'm') {
            let amount = &input[..input.len() - 1];
            if let Ok(n) = amount.parse::<i64>() {
                if n <= 0 {
                    anyhow::bail!(
                        "Invalid date filter '{input}': relative durations must be positive (e.g. 7d, 24h, 90m)"
                    );
                }
                let delta = match unit {
                    'd' => chrono::Duration::try_days(n),
                    'h' => chrono::Duration::try_hours(n),
                    _ => chrono::Duration::try_minutes(n),
                };
                let ts = delta
                    .and_then(|d| now.checked_sub_signed(d))
                    .ok_or_else(|| {
                        anyhow::anyhow!("Invalid date filter '{input}': duration is too large")
                    })?;
                return Ok(format_utc(ts));
            }
        }
    }

    // RFC 3339 timestamp
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(format_utc(dt.with_timezone(&Utc)));
    }

    // Plain date: start of day UTC
    if let Ok(date) = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return Ok(format!("{}T00:00:00.000Z", date.format("%Y-%m-%d")));
    }

    anyhow::bail!(
        "Invalid date filter '{input}'. Accepted forms: YYYY-MM-DD (e.g. 2026-08-01), \
         an RFC 3339 timestamp (e.g. 2026-08-01T00:00:00Z), or a relative duration \
         (e.g. 7d, 24h, 90m)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 20, 18, 0, 0).unwrap()
    }

    #[test]
    fn accepts_plain_date_as_start_of_day_utc() {
        assert_eq!(
            resolve_date_input("2026-08-01", now()).unwrap(),
            "2026-08-01T00:00:00.000Z"
        );
    }

    #[test]
    fn accepts_rfc3339_timestamp_unchanged() {
        assert_eq!(
            resolve_date_input("2026-08-01T12:30:00Z", now()).unwrap(),
            "2026-08-01T12:30:00.000Z"
        );
    }

    #[test]
    fn normalizes_rfc3339_with_offset_to_utc() {
        assert_eq!(
            resolve_date_input("2026-08-01T12:30:00+02:00", now()).unwrap(),
            "2026-08-01T10:30:00.000Z"
        );
    }

    #[test]
    fn resolves_relative_days_before_now() {
        assert_eq!(
            resolve_date_input("7d", now()).unwrap(),
            "2026-08-13T18:00:00.000Z"
        );
    }

    #[test]
    fn resolves_relative_hours_before_now() {
        assert_eq!(
            resolve_date_input("24h", now()).unwrap(),
            "2026-08-19T18:00:00.000Z"
        );
    }

    #[test]
    fn resolves_relative_minutes_before_now() {
        assert_eq!(
            resolve_date_input("90m", now()).unwrap(),
            "2026-08-20T16:30:00.000Z"
        );
    }

    #[test]
    fn rejects_garbage_with_error_naming_accepted_forms() {
        let err = resolve_date_input("next tuesday", now()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("next tuesday"),
            "error should echo input: {msg}"
        );
        assert!(
            msg.contains("2026-08-01") || msg.contains("YYYY-MM-DD"),
            "error should name accepted forms: {msg}"
        );
    }

    #[test]
    fn rejects_empty_input() {
        assert!(resolve_date_input("", now()).is_err());
    }

    #[test]
    fn rejects_zero_and_negative_durations() {
        assert!(resolve_date_input("0d", now()).is_err());
        assert!(resolve_date_input("-3d", now()).is_err());
    }

    #[test]
    fn huge_relative_durations_error_instead_of_panicking() {
        for input in [
            "999999999999999d",
            "9223372036854775807h",
            "999999999999999999m",
        ] {
            let result = std::panic::catch_unwind(|| resolve_date_input(input, now()));
            let value = result.expect("must not panic on user input");
            assert!(value.is_err(), "{input} should be a validation error");
        }
    }

    #[test]
    fn normalized_timestamps_carry_millisecond_precision() {
        // Linear stores timestamps with milliseconds ("...:01.261Z"). A filter
        // normalized without them compares wrong lexicographically: '.' < 'Z'
        // makes "...:01.261Z" sort BELOW "...:01Z", excluding records inside
        // the boundary second. Matching precision fixes the comparison.
        let cutoff = resolve_date_input("2026-08-20T18:04:01Z", now()).unwrap();
        assert_eq!(cutoff, "2026-08-20T18:04:01.000Z");
        let stored = "2026-08-20T18:04:01.261Z";
        assert!(
            stored > cutoff.as_str(),
            "boundary-second record must sort at/after the cutoff"
        );
        let plain = resolve_date_input("2026-08-01", now()).unwrap();
        assert_eq!(plain, "2026-08-01T00:00:00.000Z");
    }
}
