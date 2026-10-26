//! The retry schedule for a failed model request.
//!
//! A provider that answers `429` says how long to wait. The harness has to honour it: retrying
//! earlier is the same request that just failed, and hammering a rate limit is how one 429
//! becomes a ban.

use harness_core::agent::retry_delay_secs;
use harness_core::HarnessError;

/// The provider's own `Retry-After` is the delay, not the exponential floor.
#[test]
fn a_rate_limit_waits_as_long_as_the_provider_said() {
    let error = HarnessError::RateLimited {
        provider: "nvidia".to_string(),
        retry_after_secs: Some(30),
    };

    assert_eq!(
        retry_delay_secs(&error, 0),
        30,
        "the first retry waits the full window the provider asked for"
    );
}

/// The backoff still grows across attempts, and never below the provider's floor.
#[test]
fn the_backoff_grows_but_never_undercuts_the_provider() {
    let error = HarnessError::RateLimited {
        provider: "nvidia".to_string(),
        retry_after_secs: Some(5),
    };

    assert_eq!(retry_delay_secs(&error, 0), 5);
    assert_eq!(
        retry_delay_secs(&error, 3),
        8,
        "exponential backoff takes over once it exceeds the provider's window"
    );
}

/// An error with no timing of its own gets the plain exponential backoff.
#[test]
fn an_untimed_error_backs_off_exponentially() {
    let error = HarnessError::ProviderError {
        provider: "nvidia".to_string(),
        message: "connection reset".to_string(),
    };

    assert_eq!(retry_delay_secs(&error, 0), 1);
    assert_eq!(retry_delay_secs(&error, 1), 2);
    assert_eq!(retry_delay_secs(&error, 2), 4);
    assert_eq!(retry_delay_secs(&error, 3), 8);
}

/// A rate limit with no stated window still backs off rather than retrying instantly.
#[test]
fn a_windowless_rate_limit_still_backs_off() {
    let error = HarnessError::RateLimited {
        provider: "nvidia".to_string(),
        retry_after_secs: None,
    };

    assert_eq!(retry_delay_secs(&error, 0), 1);
    assert_eq!(retry_delay_secs(&error, 2), 4);
}
