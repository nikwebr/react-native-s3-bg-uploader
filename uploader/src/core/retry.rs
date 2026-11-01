use std::future::Future;

/// Gemeinsame Retry-Policy
pub struct RetryPolicy {
    pub max_retries: usize,
    /// Delay before the second attempt, in milliseconds. Doubles per attempt.
    pub base_delay_ms: u32,
    /// Upper bound for a single backoff delay, in milliseconds.
    pub max_delay_ms: u32,
}

impl RetryPolicy {
    /// Backoff for API calls whose failure can be a real server-side problem
    /// (`startUpload`, `completeUpload`): 1s, 2s, 4s, 8s.
    pub fn new(max_retries: usize) -> Self {
        Self {
            max_retries,
            base_delay_ms: 1000,
            max_delay_ms: 8000,
        }
    }

    /// Backoff for a single presigned part `PUT`: 250ms, 500ms, 1s, 2s.
    ///
    /// A part `PUT` is idempotent and its body is already in memory, so waiting out
    /// the 1s/2s/4s/8s ladder only delays the upload of a part whose response was
    /// lost. That ladder was a large part of the "still takes ~30 s after 100%"
    /// report: with the old delays a single dropped part cost up to 15 s of pure
    /// sleeping before it was even re-sent, on top of the time the transport needs to
    /// notice the dead connection. A short, still-increasing delay is enough to ride
    /// out a flapping link without stalling the file.
    pub fn part_upload() -> Self {
        Self {
            max_retries: crate::core::MAX_RETRIES,
            base_delay_ms: 250,
            max_delay_ms: 2000,
        }
    }

    pub fn delay_ms(&self, attempt: usize) -> Option<u32> {
        if attempt < self.max_retries {
            Some(calculate_retry_delay_ms(
                attempt,
                self.base_delay_ms,
                self.max_delay_ms,
            ))
        } else {
            None
        }
    }
}

pub fn run_with_retry_string<T, F, R>(
    policy: &RetryPolicy,
    mut op: F,
    mut on_retry: R,
) -> Result<T, String>
where
    F: FnMut(usize) -> Result<T, String>,
    R: FnMut(usize, &str, u32),
{
    let mut last_error: Option<String> = None;

    for attempt in 1..=policy.max_retries {
        match op(attempt) {
            Ok(value) => return Ok(value),
            Err(err) => {
                // Don't retry pause/stale interrupts — propagate immediately.
                if err.contains("paused") || err.contains("Interrupted") || err.contains("stale run") {
                    return Err(err);
                }
                last_error = Some(err);
                if let Some(delay_ms) = policy.delay_ms(attempt) {
                    if let Some(ref err_str) = last_error {
                        on_retry(attempt, err_str, delay_ms);
                    }
                } else {
                    break;
                }
            }
        }
    }

    Err(retry_exhausted_error_with(policy.max_retries, last_error))
}

pub async fn run_with_retry_string_async<T, F, Fut, R, RFut>(
    policy: &RetryPolicy,
    mut op: F,
    mut on_retry: R,
) -> Result<T, String>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = Result<T, String>>,
    R: FnMut(usize, &str, u32) -> RFut,
    RFut: Future<Output = ()>,
{
    let mut last_error: Option<String> = None;

    for attempt in 1..=policy.max_retries {
        match op(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                last_error = Some(err);
                if let Some(delay_ms) = policy.delay_ms(attempt) {
                    if let Some(ref err_str) = last_error {
                        on_retry(attempt, err_str, delay_ms).await;
                    }
                } else {
                    break;
                }
            }
        }
    }

    Err(retry_exhausted_error_with(policy.max_retries, last_error))
}

/// Berechnet das Backoff-Delay für Retries (exponentielles Backoff)
fn calculate_retry_delay_ms(attempt: usize, base_delay_ms: u32, max_delay_ms: u32) -> u32 {
    base_delay_ms
        .saturating_mul(1u32 << (attempt - 1).min(15))
        .min(max_delay_ms)
}

fn retry_exhausted_error_with(max_retries: usize, last_error: Option<String>) -> String {
    format!("Failed after {} retries: {:?}", max_retries, last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_policy_keeps_the_documented_ladder() {
        let policy = RetryPolicy::new(5);
        let delays: Vec<u32> = (1..=5).filter_map(|a| policy.delay_ms(a)).collect();
        assert_eq!(delays, vec![1000, 2000, 4000, 8000]);
    }

    #[test]
    fn part_upload_retries_promptly_and_within_the_completion_budget() {
        let policy = RetryPolicy::part_upload();
        let delays: Vec<u32> = (1..=5).filter_map(|a| policy.delay_ms(a)).collect();
        assert_eq!(delays, vec![250, 500, 1000, 2000]);

        // A part whose response was lost must be re-sent well inside the 20 s that the
        // e2e suite allows between the first 100% callback and COMPLETED. Sleeping is
        // only part of that budget, so keep the whole ladder small.
        let total: u32 = delays.iter().sum();
        assert!(total <= 5000, "part retry delays add up to {total}ms");
    }

    #[test]
    fn exhausted_retry_reports_the_last_error() {
        let policy = RetryPolicy::part_upload();
        let result = run_with_retry_string(
            &policy,
            |_| Err::<(), _>("network error".to_string()),
            |_, _, _| {},
        );
        assert!(result.unwrap_err().contains("network error"));
    }

    #[test]
    fn pause_and_stale_run_interrupts_are_never_retried() {
        let policy = RetryPolicy::part_upload();
        for interrupt in ["paused", "Interrupted", "stale run"] {
            let mut attempts = 0;
            let result = run_with_retry_string(
                &policy,
                |_| {
                    attempts += 1;
                    Err::<(), _>(interrupt.to_string())
                },
                |_, _, _| {},
            );
            assert_eq!(result.unwrap_err(), interrupt);
            assert_eq!(attempts, 1);
        }
    }
}
