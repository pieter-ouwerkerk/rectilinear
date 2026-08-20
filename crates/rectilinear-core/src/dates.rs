//! Date input normalization for filter parameters.
//!
//! Accepts absolute dates (`2026-08-01`, RFC 3339 timestamps) and relative
//! durations (`7d`, `24h`, `90m`), normalizing everything to an RFC 3339 UTC
//! timestamp string suitable for lexicographic comparison against the
//! `created_at`/`updated_at` columns.

use anyhow::Result;
use chrono::{DateTime, Utc};

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
                    'd' => chrono::Duration::days(n),
                    'h' => chrono::Duration::hours(n),
                    _ => chrono::Duration::minutes(n),
                };
                return Ok((now - delta).format("%Y-%m-%dT%H:%M:%SZ").to_string());
            }
        }
    }

    // RFC 3339 timestamp
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.with_timezone(&Utc).format("%Y-%m-%dT%H:%M:%SZ").to_string());
    }

    // Plain date: start of day UTC
    if let Ok(date) = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return Ok(format!("{}T00:00:00Z", date.format("%Y-%m-%d")));
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
            "2026-08-01T00:00:00Z"
        );
    }

    #[test]
    fn accepts_rfc3339_timestamp_unchanged() {
        assert_eq!(
            resolve_date_input("2026-08-01T12:30:00Z", now()).unwrap(),
            "2026-08-01T12:30:00Z"
        );
    }

    #[test]
    fn normalizes_rfc3339_with_offset_to_utc() {
        assert_eq!(
            resolve_date_input("2026-08-01T12:30:00+02:00", now()).unwrap(),
            "2026-08-01T10:30:00Z"
        );
    }

    #[test]
    fn resolves_relative_days_before_now() {
        assert_eq!(
            resolve_date_input("7d", now()).unwrap(),
            "2026-08-13T18:00:00Z"
        );
    }

    #[test]
    fn resolves_relative_hours_before_now() {
        assert_eq!(
            resolve_date_input("24h", now()).unwrap(),
            "2026-08-19T18:00:00Z"
        );
    }

    #[test]
    fn resolves_relative_minutes_before_now() {
        assert_eq!(
            resolve_date_input("90m", now()).unwrap(),
            "2026-08-20T16:30:00Z"
        );
    }

    #[test]
    fn rejects_garbage_with_error_naming_accepted_forms() {
        let err = resolve_date_input("next tuesday", now()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("next tuesday"), "error should echo input: {msg}");
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
}
