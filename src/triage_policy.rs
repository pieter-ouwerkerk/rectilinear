/// Returns true when an issue became prioritized after the caller's last sync.
///
/// An issue that was already prioritized locally may be intentionally
/// re-triaged. This guard exists only to prevent overwriting a concurrent
/// triage decision made after an unprioritized issue entered the queue.
pub(crate) fn was_prioritized_since_sync(synced_priority: i32, latest_priority: i32) -> bool {
    synced_priority == 0 && latest_priority != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_prioritized_issue_can_be_retriaged() {
        assert!(!was_prioritized_since_sync(2, 2));
    }

    #[test]
    fn newly_prioritized_issue_preserves_concurrent_triage_guard() {
        assert!(was_prioritized_since_sync(0, 2));
    }

    #[test]
    fn still_unprioritized_issue_can_be_triaged() {
        assert!(!was_prioritized_since_sync(0, 0));
    }
}
