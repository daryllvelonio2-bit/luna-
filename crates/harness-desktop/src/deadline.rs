use std::time::{Duration, Instant};

/// The budget a single desktop command runs under.
///
/// Every platform call takes a deadline rather than sleeping for a fixed
/// interval: a bus that answers in a millisecond must not cost a second, and a
/// bus that never answers must not hang the turn.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    expires_at: Instant,
}

impl Deadline {
    pub fn new(timeout: Duration) -> Self {
        Self {
            expires_at: Instant::now() + timeout,
        }
    }

    /// A deadline that never expires. For callers that manage their own budget.
    pub fn none() -> Self {
        Self {
            expires_at: Instant::now() + Duration::from_secs(60 * 60),
        }
    }

    /// The time left, or `None` when it has run out.
    pub fn remaining(&self) -> Option<Duration> {
        self.expires_at.checked_duration_since(Instant::now())
    }

    pub fn expired(&self) -> bool {
        self.remaining().is_none()
    }

    /// The error a caller reports when the budget ran out.
    pub fn timeout_error(&self) -> crate::error::DesktopError {
        crate::error::DesktopError::new(crate::error::ErrorCode::Timeout, "The desktop operation ran out of time")
        .with_suggestion("Retry with a larger timeout, or take a fresh snapshot")
        .not_delivered()
    }
}

/// The default budget for one desktop command.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_deadline_has_time_left() {
        let deadline = Deadline::new(Duration::from_secs(5));
        assert!(!deadline.expired());
        assert!(deadline.remaining().is_some());
    }

    #[test]
    fn a_spent_deadline_expires() {
        let deadline = Deadline::new(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(deadline.expired());
        assert!(deadline.remaining().is_none());
    }

    #[test]
    fn a_spent_deadline_reports_a_timeout() {
        let deadline = Deadline::new(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        let error = deadline.timeout_error();
        assert_eq!(error.code, crate::error::ErrorCode::Timeout);
    }
}
