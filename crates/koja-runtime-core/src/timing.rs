//! Shared conversion policy for user-provided process durations.

use std::time::Duration;

/// Converts a signed millisecond value to a duration. Negative values
/// behave as zero across every runtime adapter.
pub fn duration_from_user_millis(milliseconds: i64) -> Duration {
    Duration::from_millis(milliseconds.max(0) as u64)
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
    fn nonnegative_milliseconds_preserve_their_value() {
        assert_eq!(duration_from_user_millis(0), Duration::ZERO);
        assert_eq!(duration_from_user_millis(250), Duration::from_millis(250),);
    }
}
