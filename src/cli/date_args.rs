//! Shared conversion of CLI date-filter flags into normalized `DateFilters`.

use anyhow::Result;

use crate::db::DateFilters;

/// Normalize the four optional CLI date flags. Any unparseable value is a hard
/// error — a silently dropped date predicate would return plausible wrong results.
pub fn parse_date_filters(
    updated_after: Option<&str>,
    updated_before: Option<&str>,
    created_after: Option<&str>,
    created_before: Option<&str>,
) -> Result<DateFilters> {
    let now = chrono::Utc::now();
    let resolve = |input: Option<&str>| -> Result<Option<String>> {
        input
            .map(|v| crate::dates::resolve_date_input(v, now))
            .transpose()
    };
    Ok(DateFilters {
        updated_after: resolve(updated_after)?,
        updated_before: resolve(updated_before)?,
        created_after: resolve(created_after)?,
        created_before: resolve(created_before)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_normalized_absolute_dates() {
        let filters = parse_date_filters(Some("2026-08-01"), None, Some("2026-07-01T06:00:00Z"), None).unwrap();
        assert_eq!(filters.updated_after.as_deref(), Some("2026-08-01T00:00:00Z"));
        assert_eq!(filters.created_after.as_deref(), Some("2026-07-01T06:00:00Z"));
        assert!(filters.updated_before.is_none());
        assert!(filters.created_before.is_none());
    }

    #[test]
    fn rejects_invalid_input_instead_of_dropping_the_filter() {
        let err = parse_date_filters(None, Some("whenever"), None, None).unwrap_err();
        assert!(err.to_string().contains("whenever"));
    }

    #[test]
    fn all_none_yields_empty_filters() {
        assert!(parse_date_filters(None, None, None, None).unwrap().is_empty());
    }
}
