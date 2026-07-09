//! ErrorWithholder — retryable-error gate with exponential backoff.
//!
//! Wraps each provider call in the ReAct loop.  When the provider returns a
//! transient error (HTTP 429/5xx, connection timeout) or an empty model
//! response, the withholder absorbs it and allows the caller to retry after a
//! delay.  Non-retryable errors and excessive consecutive failures escalate.

use std::time::Duration;
use tokio::time::sleep;

use crate::providers::trait_def::ProviderError;

// ── ErrorWithholder ────────────────────────────────────────────────────────

/// Tracks consecutive retryable errors and enforces a backoff + max-attempt
/// policy so the agent loop doesn't hammer a degraded provider.
pub struct ErrorWithholder {
    /// How many retryable errors have occurred in a row.
    pub consecutive_errors: u32,
    /// Maximum number of consecutive retries before escalation.
    pub max_attempts: u32,
    /// Base backoff in milliseconds (1 s = 1000 ms).
    pub base_delay_ms: u64,
}

impl ErrorWithholder {
    /// Create a new withholder with the default policy:
    /// base delay 1 s, exponential steps (1 s / 2 s / 4 s), max 3 attempts.
    pub fn new() -> Self {
        Self {
            consecutive_errors: 0,
            max_attempts: 3,
            base_delay_ms: 1000,
        }
    }

    /// Create a withholder with custom parameters.
    pub fn with_policy(max_attempts: u32, base_delay_ms: u64) -> Self {
        Self {
            consecutive_errors: 0,
            max_attempts,
            base_delay_ms,
        }
    }

    /// Reset the consecutive-error counter (call after a successful provider
    /// call that produced usable output so later failures don't escalate early).
    pub fn reset(&mut self) {
        self.consecutive_errors = 0;
    }

    /// How many retries have been consumed so far (0 = none yet).
    pub fn attempts_used(&self) -> u32 {
        self.consecutive_errors
    }

    /// Check whether a [`ProviderError`] is transient and therefore retryable.
    pub fn is_retryable(error: &ProviderError) -> bool {
        match error {
            ProviderError::Api { status, message } => {
                matches!(*status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 529)
                    || message_looks_transient(message)
            }
            ProviderError::Http(e) => {
                let lower = e.to_lowercase();
                lower.contains("timeout")
                    || lower.contains("timed out")
                    || lower.contains("connection")
                    || lower.contains("reset")
                    || lower.contains("broken pipe")
                    || lower.contains("temporarily")
                    || lower.contains("429")
                    || lower.contains("502")
                    || lower.contains("503")
                    || lower.contains("504")
                    || lower.contains("529")
            }
            // Parse errors and missing keys are permanent — don't retry.
            ProviderError::Parse(_) | ProviderError::NoApiKey => false,
        }
    }

    /// Evaluate a provider error.
    ///
    /// * Returns `Ok(())` if the error is retryable and we haven't exceeded
    ///   `max_attempts`.  The caller should sleep `current_backoff()` and retry.
    /// * Returns `Err(error)` if the error is non-retryable or we have hit the
    ///   attempt ceiling — the caller must propagate the error.
    pub fn tolerate(&mut self, error: ProviderError) -> Result<(), ProviderError> {
        if !Self::is_retryable(&error) {
            self.consecutive_errors = 0; // reset on permanent errors
            return Err(error);
        }

        self.consecutive_errors += 1;

        if self.consecutive_errors > self.max_attempts {
            self.consecutive_errors = 0;
            return Err(error);
        }

        Ok(())
    }

    /// Record a transient empty model response (no content, no tool calls).
    ///
    /// * `Ok(attempt)` — still under budget; caller should backoff and re-call.
    /// * `Err(())` — exhausted retries; caller should fail the turn.
    pub fn tolerate_empty(&mut self) -> Result<u32, ()> {
        self.consecutive_errors += 1;
        if self.consecutive_errors > self.max_attempts {
            self.consecutive_errors = 0;
            return Err(());
        }
        Ok(self.consecutive_errors)
    }

    /// How long the caller should sleep before retrying, based on the current
    /// `consecutive_errors` count: 1 s → 2 s → 4 s.
    pub fn current_backoff(&self) -> Duration {
        let factor = 1u64 << (self.consecutive_errors.saturating_sub(1));
        // Clamp at 30 seconds as a safety ceiling.
        let ms = (self.base_delay_ms * factor).min(30_000);
        Duration::from_millis(ms)
    }

    /// Convenience: sleep for the current backoff duration.
    pub async fn sleep_backoff(&self) {
        sleep(self.current_backoff()).await;
    }
}

fn message_looks_transient(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("timeout")
        || lower.contains("temporarily")
        || lower.contains("try again")
        || lower.contains("capacity")
}

impl Default for ErrorWithholder {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retryable_429() {
        let err = ProviderError::Api {
            status: 429,
            message: "rate limited".into(),
        };
        assert!(ErrorWithholder::is_retryable(&err));
    }

    #[test]
    fn test_retryable_529() {
        let err = ProviderError::Api {
            status: 529,
            message: "overloaded".into(),
        };
        assert!(ErrorWithholder::is_retryable(&err));
    }

    #[test]
    fn test_retryable_5xx() {
        for status in [500u16, 502, 503, 504] {
            let err = ProviderError::Api {
                status,
                message: "temporary".into(),
            };
            assert!(
                ErrorWithholder::is_retryable(&err),
                "status {status} should be retryable"
            );
        }
    }

    #[test]
    fn test_non_retryable_4xx() {
        let err = ProviderError::Api {
            status: 400,
            message: "bad request".into(),
        };
        assert!(!ErrorWithholder::is_retryable(&err));
    }

    #[test]
    fn test_tolerate_empty() {
        let mut wh = ErrorWithholder::with_policy(3, 1);
        assert!(wh.tolerate_empty().is_ok());
        assert!(wh.tolerate_empty().is_ok());
        assert!(wh.tolerate_empty().is_ok());
        assert!(wh.tolerate_empty().is_err());
    }

    #[test]
    fn test_non_retryable_parse() {
        let err = ProviderError::Parse("bad json".into());
        assert!(!ErrorWithholder::is_retryable(&err));
    }

    #[test]
    fn test_non_retryable_no_api_key() {
        let err = ProviderError::NoApiKey;
        assert!(!ErrorWithholder::is_retryable(&err));
    }

    #[test]
    fn test_tolerate_allows_up_to_max() {
        let mut wh = ErrorWithholder::with_policy(3, 1); // tiny delay for tests
        let err = ProviderError::Api {
            status: 429,
            message: "".into(),
        };
        assert!(wh.tolerate(err.clone()).is_ok()); // 1
        assert!(wh.tolerate(err.clone()).is_ok()); // 2
        assert!(wh.tolerate(err.clone()).is_ok()); // 3
        // 4th should escalate
        assert!(wh.tolerate(err).is_err());
    }

    #[test]
    fn test_tolerate_escalates_non_retryable() {
        let mut wh = ErrorWithholder::new();
        let err = ProviderError::Parse("bad".into());
        assert!(wh.tolerate(err).is_err());
        assert_eq!(wh.consecutive_errors, 0); // reset on permanent
    }

    #[test]
    fn test_backoff_exponential() {
        let mut wh = ErrorWithholder::with_policy(3, 1000);
        // No errors yet -> backoff is 1 s (base * 2^0)
        assert_eq!(wh.current_backoff().as_millis(), 1000);

        // Tolerate one
        wh.tolerate(ProviderError::Api {
            status: 429,
            message: "".into(),
        })
        .ok();
        assert_eq!(wh.consecutive_errors, 1);
        assert_eq!(wh.current_backoff().as_millis(), 1000); // 2^(1-1) = 1

        // Tolerate second
        wh.tolerate(ProviderError::Api {
            status: 429,
            message: "".into(),
        })
        .ok();
        assert_eq!(wh.consecutive_errors, 2);
        assert_eq!(wh.current_backoff().as_millis(), 2000); // 2^(2-1) = 2

        // Tolerate third
        wh.tolerate(ProviderError::Api {
            status: 429,
            message: "".into(),
        })
        .ok();
        assert_eq!(wh.consecutive_errors, 3);
        assert_eq!(wh.current_backoff().as_millis(), 4000); // 2^(3-1) = 4
    }

    #[test]
    fn test_backoff_clamped() {
        let mut wh = ErrorWithholder::with_policy(10, 1000);
        // Artificially set high count
        wh.consecutive_errors = 10;
        let ms = wh.current_backoff().as_millis();
        assert!(ms <= 30000, "backoff should be clamped at 30 s, got {ms} ms");
    }

    #[test]
    fn test_reset_clears_count() {
        let mut wh = ErrorWithholder::new();
        wh.tolerate(ProviderError::Api {
            status: 429,
            message: "".into(),
        })
        .ok();
        assert_eq!(wh.consecutive_errors, 1);
        wh.reset();
        assert_eq!(wh.consecutive_errors, 0);
    }
}
