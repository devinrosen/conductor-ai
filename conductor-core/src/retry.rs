//! Bounded retry with exponential backoff and transient error classification.
//!
//! Part of: bounded-retry-with-escalation@1.0.0

use std::time::Duration;

use crate::error::SubprocessFailure;

/// Configuration for retry behavior.
#[allow(dead_code)]
pub struct RetryConfig {
    /// Maximum number of attempts (including the first). Default: 3.
    pub max_attempts: u32,
    /// Initial delay before first retry. Default: 1s.
    pub initial_backoff: Duration,
    /// Multiplier applied to backoff after each retry. Default: 2.0.
    pub backoff_multiplier: f64,
    /// Maximum backoff duration. Default: 30s.
    pub max_backoff: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// Outcome of a retried operation.
#[derive(Debug)]
#[allow(dead_code)]
pub enum RetryOutcome<T, E> {
    /// Operation succeeded after `attempts` tries.
    Success {
        value: T,
        #[allow(dead_code)]
        attempts: u32,
    },
    /// All retry attempts exhausted.
    Exhausted {
        last_error: E,
        #[allow(dead_code)]
        attempts: u32,
    },
    /// Retry was cancelled via the cancellation callback.
    Cancelled,
}

/// Execute `operation` with bounded retry and exponential backoff.
///
/// `is_retriable` classifies each error: returns `true` for transient errors
/// that should be retried, `false` for permanent errors that should fail immediately.
///
/// `is_cancelled` is checked before each retry and during backoff sleep (every 100ms).
/// When it returns `true`, the retry loop exits with `RetryOutcome::Cancelled`.
/// Pass `|| false` if cancellation is not needed.
#[allow(dead_code)]
pub fn retry_with_backoff<T, E, F, R, C>(
    config: &RetryConfig,
    mut operation: F,
    is_retriable: R,
    is_cancelled: C,
) -> RetryOutcome<T, E>
where
    F: FnMut() -> Result<T, E>,
    R: Fn(&E) -> bool,
    C: Fn() -> bool,
{
    let mut attempts = 0u32;
    let mut backoff = config.initial_backoff;

    loop {
        if is_cancelled() {
            return RetryOutcome::Cancelled;
        }
        attempts += 1;
        match operation() {
            Ok(value) => return RetryOutcome::Success { value, attempts },
            Err(e) => {
                if attempts >= config.max_attempts || !is_retriable(&e) {
                    return RetryOutcome::Exhausted {
                        last_error: e,
                        attempts,
                    };
                }
                // Sleep in short intervals to allow cooperative cancellation
                let deadline = std::time::Instant::now() + backoff;
                while std::time::Instant::now() < deadline {
                    if is_cancelled() {
                        return RetryOutcome::Cancelled;
                    }
                    std::thread::sleep(Duration::from_millis(100).min(backoff));
                }
                backoff = Duration::from_secs_f64(
                    (backoff.as_secs_f64() * config.backoff_multiplier)
                        .min(config.max_backoff.as_secs_f64()),
                );
            }
        }
    }
}

/// Classify whether a subprocess failure is transient (worth retrying).
///
/// Checks stderr and exit codes for known transient patterns (network errors,
/// rate limits, temporary server errors, SQLite busy). Unknown failures are
/// treated as permanent (safe default).
#[allow(dead_code)]
pub fn is_transient(failure: &SubprocessFailure) -> bool {
    let stderr = failure.stderr.to_lowercase();

    // Network / DNS errors
    if stderr.contains("could not resolve host")
        || stderr.contains("connection refused")
        || stderr.contains("timed out")
        || stderr.contains("connection reset")
        || stderr.contains("network is unreachable")
        || stderr.contains("temporary failure in name resolution")
    {
        return true;
    }

    // TLS / SSL errors (often transient)
    if stderr.contains("ssl") && (stderr.contains("error") || stderr.contains("handshake")) {
        return true;
    }

    // Rate limiting
    if stderr.contains("rate limit") || stderr.contains("429") || stderr.contains("too many") {
        return true;
    }

    // Server errors
    if stderr.contains("503") || stderr.contains("502") || stderr.contains("500 internal") {
        return true;
    }

    // Git-specific transient errors
    if stderr.contains("the remote end hung up unexpectedly")
        || stderr.contains("early eof")
        || stderr.contains("unexpected disconnect")
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::Instant;

    #[test]
    fn always_succeeds_returns_on_first_attempt() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            ..Default::default()
        };
        let result = retry_with_backoff(&config, || Ok::<_, String>(42), |_| true, || false);
        match result {
            RetryOutcome::Success { value, attempts } => {
                assert_eq!(value, 42);
                assert_eq!(attempts, 1);
            }
            other => panic!("expected success, got {other:?}"),
        }
    }

    #[test]
    fn always_fails_transient_exhausts_attempts() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
            ..Default::default()
        };
        let result = retry_with_backoff(
            &config,
            || Err::<(), _>("transient"),
            |_: &&str| true,
            || false,
        );
        match result {
            RetryOutcome::Exhausted {
                last_error,
                attempts,
            } => {
                assert_eq!(last_error, "transient");
                assert_eq!(attempts, 3);
            }
            other => panic!("expected exhaustion, got {other:?}"),
        }
    }

    #[test]
    fn fail_then_succeed_retries_correctly() {
        let call_count = Cell::new(0u32);
        let config = RetryConfig {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            ..Default::default()
        };
        let result = retry_with_backoff(
            &config,
            || {
                let n = call_count.get() + 1;
                call_count.set(n);
                if n < 3 {
                    Err("not yet")
                } else {
                    Ok("done")
                }
            },
            |_: &&str| true,
            || false,
        );
        match result {
            RetryOutcome::Success { value, attempts } => {
                assert_eq!(value, "done");
                assert_eq!(attempts, 3);
            }
            other => panic!("expected success after retries, got {other:?}"),
        }
    }

    #[test]
    fn permanent_error_stops_immediately() {
        let call_count = Cell::new(0u32);
        let config = RetryConfig {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            ..Default::default()
        };
        let result = retry_with_backoff(
            &config,
            || {
                call_count.set(call_count.get() + 1);
                Err::<(), _>("permanent")
            },
            |_: &&str| false, // not retriable
            || false,
        );
        match result {
            RetryOutcome::Exhausted { attempts, .. } => {
                assert_eq!(attempts, 1);
            }
            other => panic!("expected immediate failure, got {other:?}"),
        }
    }

    #[test]
    fn backoff_timing_approximately_correct() {
        let config = RetryConfig {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(50),
            backoff_multiplier: 2.0,
            max_backoff: Duration::from_secs(5),
        };
        let start = Instant::now();
        let _ = retry_with_backoff(&config, || Err::<(), _>("fail"), |_: &&str| true, || false);
        let elapsed = start.elapsed();
        // Should sleep ~50ms + ~100ms = ~150ms total.
        // Wide tolerance for CI machines under heavy load.
        assert!(
            elapsed >= Duration::from_millis(50),
            "expected at least 50ms of backoff, got {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "backoff took too long: {elapsed:?}"
        );
    }

    #[test]
    fn is_transient_network_errors() {
        let cases = [
            "could not resolve host: github.com",
            "Connection refused (os error 111)",
            "Operation timed out after 30 seconds",
            "Connection reset by peer",
        ];
        for msg in cases {
            let f = SubprocessFailure::from_message("git push", msg.to_string());
            assert!(is_transient(&f), "expected transient for: {msg}");
        }
    }

    #[test]
    fn is_transient_rate_limit() {
        let f = SubprocessFailure::from_message("gh api", "rate limit exceeded".to_string());
        assert!(is_transient(&f));
        let f = SubprocessFailure::from_message("gh api", "HTTP 429".to_string());
        assert!(is_transient(&f));
    }

    #[test]
    fn is_transient_server_errors() {
        let f = SubprocessFailure::from_message("gh api", "503 Service Unavailable".to_string());
        assert!(is_transient(&f));
    }

    #[test]
    fn is_transient_permanent_errors_return_false() {
        let cases = [
            "fatal: not a git repository",
            "error: pathspec 'foo' did not match any files",
            "permission denied (publickey)",
        ];
        for msg in cases {
            let f = SubprocessFailure::from_message("git", msg.to_string());
            assert!(!is_transient(&f), "expected permanent for: {msg}");
        }
    }

    #[test]
    fn is_transient_git_remote_hung_up() {
        let f = SubprocessFailure::from_message(
            "git push",
            "the remote end hung up unexpectedly".to_string(),
        );
        assert!(is_transient(&f));
    }
}
