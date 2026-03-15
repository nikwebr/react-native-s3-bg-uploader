use std::future::Future;

/// Gemeinsame Retry-Policy
pub struct RetryPolicy {
    pub max_retries: usize,
}

impl RetryPolicy {
    pub fn new(max_retries: usize) -> Self {
        Self { max_retries }
    }

    pub fn delay_ms(&self, attempt: usize) -> Option<u32> {
        if attempt < self.max_retries {
            Some(calculate_retry_delay_ms(attempt))
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
fn calculate_retry_delay_ms(attempt: usize) -> u32 {
    (1 << (attempt - 1)) * 1000
}

fn retry_exhausted_error_with(max_retries: usize, last_error: Option<String>) -> String {
    format!("Failed after {} retries: {:?}", max_retries, last_error)
}