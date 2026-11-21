//! Posting JSON to a model provider, with retries for rate limits and
//! server errors.

use std::time::Duration;

use reqwest::header::HeaderMap;
use serde_json::Value;

use super::ExtractError;

/// Attempts per request: the first plus two retries.
const ATTEMPTS: u32 = 3;

/// Longest a single request may take. Extraction of a long PDF can be slow.
const TIMEOUT: Duration = Duration::from_secs(300);

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("spendyard/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("an HTTP client")
}

/// Posts `body` and returns the JSON response. Retries 408, 409, 429 and
/// 5xx responses and connection errors, waiting as `retry-after` says or
/// backing off.
pub async fn post_json(
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    body: &Value,
) -> Result<Value, ExtractError> {
    let mut attempt = 0;

    loop {
        attempt += 1;

        let sent = client
            .post(url)
            .headers(headers.clone())
            .json(body)
            .send()
            .await;

        let response = match sent {
            Ok(response) => response,

            Err(_) if attempt < ATTEMPTS => {
                tokio::time::sleep(backoff(attempt)).await;

                continue;
            }

            Err(error) => return Err(ExtractError::Http(error.to_string())),
        };

        let status = response.status();

        if status.is_success() {
            let json = response
                .json::<Value>()
                .await
                .map_err(|error| ExtractError::Http(error.to_string()))?;

            return Ok(json);
        }

        let retryable = matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error();

        if retryable && attempt < ATTEMPTS {
            let wait = retry_after(response.headers()).unwrap_or_else(|| backoff(attempt));

            tokio::time::sleep(wait).await;

            continue;
        }

        let text = response.text().await.unwrap_or_default();

        let error = ExtractError::Api {
            status: status.as_u16(),
            message: error_message(&text),
        };

        return Err(error);
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500 * 2u64.pow(attempt))
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds: u64 = headers
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;

    Some(Duration::from_secs(seconds.min(60)))
}

/// The provider's own error message when the body is JSON, the raw body
/// otherwise.
fn error_message(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();

    let message = parsed.as_ref().and_then(|json| {
        json.pointer("/error/message")
            .or_else(|| json.pointer("/message"))
            .and_then(Value::as_str)
    });

    match message {
        Some(message) => message.to_string(),
        None => body.chars().take(500).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_provider_error_messages() {
        let anthropic = r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens is too large"}}"#;

        assert_eq!(error_message(anthropic), "max_tokens is too large");
        assert_eq!(error_message("Bad Gateway"), "Bad Gateway");
    }
}
