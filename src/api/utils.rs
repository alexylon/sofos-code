use crate::error::{Result, SofosError};
use colored::Colorize;
use rand::Rng;
use std::future::Future;
use std::time::Duration;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
pub const MAX_RETRIES: u32 = 2;
pub const INITIAL_RETRY_DELAY_MS: u64 = 1000;
const JITTER_FACTOR: f64 = 0.3; // Add 0-30% random jitter

pub async fn check_response_status(response: reqwest::Response) -> Result<reqwest::Response> {
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        tracing::error!(
            status = %status,
            error = %error_text,
            "API request failed"
        );
        return Err(SofosError::Api(format!(
            "API request failed with status {}: {}",
            status, error_text
        )));
    }
    Ok(response)
}

pub fn is_retryable_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.status().is_some_and(|s| s.is_server_error())
}

/// Execute an async operation with retries and exponential backoff with jitter.
/// Returns the result of the operation or the last error after all retries exhausted.
pub async fn with_retries<F, Fut, T>(service_name: &str, operation: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = std::result::Result<T, reqwest::Error>>,
{
    let mut last_error: Option<reqwest::Error> = None;
    let mut retry_delay = Duration::from_millis(INITIAL_RETRY_DELAY_MS);

    for attempt in 0..=MAX_RETRIES {
        if attempt > 0 {
            let reason = last_error
                .as_ref()
                .map(|e| {
                    if e.is_timeout() {
                        "Request timed out".to_string()
                    } else {
                        format!("Request failed: {}", e)
                    }
                })
                .unwrap_or_else(|| "Request failed".to_string());

            // Add jitter to prevent thundering herd
            let jitter = rand::rng().random_range(0.0..JITTER_FACTOR);
            let jittered_delay = retry_delay.mul_f64(1.0 + jitter);

            tracing::warn!(
                service = service_name,
                attempt = attempt,
                max_retries = MAX_RETRIES,
                delay_ms = jittered_delay.as_millis() as u64,
                reason = %reason,
                "Retrying API request"
            );

            eprintln!(
                " {} {}, retrying in {:?}... (attempt {}/{})",
                format!("{}:", service_name).bright_yellow(),
                reason,
                jittered_delay,
                attempt,
                MAX_RETRIES
            );
            tokio::time::sleep(jittered_delay).await;
            retry_delay *= 2;
        }

        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                let is_retryable = is_retryable_error(&e);

                if attempt < MAX_RETRIES && is_retryable {
                    last_error = Some(e);
                    continue;
                } else {
                    tracing::error!(
                        service = service_name,
                        attempts = if is_retryable { attempt + 1 } else { 1 },
                        error = %e,
                        retryable = is_retryable,
                        "API request failed permanently"
                    );
                    return Err(SofosError::NetworkError(format!(
                        "{} request failed after {} attempts: {}",
                        service_name,
                        if is_retryable { attempt + 1 } else { 1 },
                        e
                    )));
                }
            }
        }
    }

    Err(last_error.map_or_else(
        || SofosError::NetworkError(format!("Unknown {} error", service_name)),
        |e| SofosError::NetworkError(format!("Failed after {} retries: {}", MAX_RETRIES, e)),
    ))
}
