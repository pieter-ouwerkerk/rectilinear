//! Proactive rate-limit pacing from Linear's `x-ratelimit-*` response
//! headers: once the remaining request budget runs low, requests are
//! spread across the time left until the budget resets instead of
//! bursting into a 429 and a long flat sleep.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Start pacing once fewer than this many requests remain in the window.
pub const PACE_THRESHOLD: u64 = 200;
/// Never proactively sleep longer than this before a single request; if the
/// budget is truly exhausted the reactive 429 handling takes over with the
/// server-provided reset time.
pub const MAX_PACE_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
struct RateBudget {
    remaining: u64,
    reset_at: DateTime<Utc>,
}

/// Tracks the most recently observed rate-limit budget and computes how long
/// to wait before the next request so the remaining budget lasts until reset.
#[derive(Debug, Default)]
pub struct RatePacer {
    budget: Mutex<Option<RateBudget>>,
}

impl RatePacer {
    /// Record the budget reported by a response's rate-limit headers.
    /// Observations without a reset time are ignored: with no window end
    /// there is nothing to spread requests across.
    pub fn observe(&self, remaining: Option<u64>, reset_at: Option<DateTime<Utc>>) {
        if let (Some(remaining), Some(reset_at)) = (remaining, reset_at) {
            *self.budget.lock().unwrap() = Some(RateBudget {
                remaining,
                reset_at,
            });
        }
    }

    /// How long to wait before sending the next request. Zero while the
    /// budget is healthy, unknown, or already reset.
    pub fn delay(&self, now: DateTime<Utc>) -> Duration {
        let Some(budget) = *self.budget.lock().unwrap() else {
            return Duration::ZERO;
        };
        if budget.remaining >= PACE_THRESHOLD || budget.reset_at <= now {
            return Duration::ZERO;
        }
        let until_reset = (budget.reset_at - now).to_std().unwrap_or(Duration::ZERO);
        if budget.remaining == 0 {
            return MAX_PACE_DELAY;
        }
        let spread = until_reset / u32::try_from(budget.remaining).unwrap_or(u32::MAX);
        spread.min(MAX_PACE_DELAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap()
    }

    #[test]
    fn no_delay_before_any_observation() {
        let pacer = RatePacer::default();
        assert_eq!(pacer.delay(now()), std::time::Duration::ZERO);
    }

    #[test]
    fn no_delay_while_budget_is_healthy() {
        let pacer = RatePacer::default();
        pacer.observe(Some(1200), Some(now() + Duration::minutes(30)));
        assert_eq!(pacer.delay(now()), std::time::Duration::ZERO);
    }

    #[test]
    fn low_budget_spreads_requests_until_reset() {
        let pacer = RatePacer::default();
        // 100 requests left, 500 seconds until reset -> 5s between requests.
        pacer.observe(Some(100), Some(now() + Duration::seconds(500)));
        assert_eq!(pacer.delay(now()), std::time::Duration::from_secs(5));
    }

    #[test]
    fn delay_is_capped() {
        let pacer = RatePacer::default();
        pacer.observe(Some(1), Some(now() + Duration::hours(1)));
        assert_eq!(pacer.delay(now()), MAX_PACE_DELAY);
    }

    #[test]
    fn exhausted_budget_waits_at_the_cap() {
        let pacer = RatePacer::default();
        pacer.observe(Some(0), Some(now() + Duration::seconds(90)));
        assert_eq!(pacer.delay(now()), MAX_PACE_DELAY);
    }

    #[test]
    fn no_delay_after_reset_time_passes() {
        let pacer = RatePacer::default();
        pacer.observe(Some(3), Some(now() - Duration::seconds(1)));
        assert_eq!(pacer.delay(now()), std::time::Duration::ZERO);
    }

    #[test]
    fn observation_without_reset_time_never_delays() {
        let pacer = RatePacer::default();
        pacer.observe(Some(3), None);
        assert_eq!(pacer.delay(now()), std::time::Duration::ZERO);
    }
}
