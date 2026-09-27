//! Shared conversion policy for user-provided process durations.

use std::time::{Duration, Instant};

/// Converts a signed millisecond value to a duration. Negative values
/// behave as zero across every runtime adapter.
pub fn duration_from_user_millis(milliseconds: i64) -> Duration {
    Duration::from_millis(milliseconds.max(0) as u64)
}

/// The extern `timeout_ms` convention for bounded I/O waits: a negative
/// value means no deadline, anything else is a bound from now.
pub fn deadline_from_user_millis(timeout_ms: i64) -> Option<Instant> {
    if timeout_ms < 0 {
        return None;
    }
    Some(Instant::now() + duration_from_user_millis(timeout_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_milliseconds_clamp_to_zero() {
        assert_eq!(duration_from_user_millis(-1), Duration::ZERO);
        assert_eq!(duration_from_user_millis(i64::MIN), Duration::ZERO);
    }

    #[test]
    fn negative_timeout_means_no_deadline() {
        assert_eq!(deadline_from_user_millis(-1), None);
        assert_eq!(deadline_from_user_millis(i64::MIN), None);
    }

    #[test]
    fn nonnegative_timeout_is_a_bound_from_now() {
        let before = Instant::now();
        let deadline = deadline_from_user_millis(250).expect("bounded");
        assert!(deadline >= before + Duration::from_millis(250));
        assert!(deadline <= Instant::now() + Duration::from_millis(250));
    }

    #[test]
    fn nonnegative_milliseconds_preserve_their_value() {
        assert_eq!(duration_from_user_millis(0), Duration::ZERO);
        assert_eq!(duration_from_user_millis(250), Duration::from_millis(250),);
    }
}
