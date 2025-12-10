use crate::error::{Result, SofosError};

pub async fn check_response_status(response: reqwest::Response) -> Result<reqwest::Response> {
    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();
        return Err(SofosError::Api(format!(
            "API request failed with status {}: {}",
            status, error_text
        )));
    }
    Ok(response)
}
