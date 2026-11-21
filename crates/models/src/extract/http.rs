//! Posting JSON to a model provider: a [`Transport`], so the providers run
//! wherever there's a way to POST. Natively it's [`Reqwest`], which retries
//! rate limits and server errors; in WebAssembly the host gives one
//! (spendyard-wasm: the Worker's `fetch`).

#[cfg(feature = "reqwest")]
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::ExtractError;

/// A request's headers, as name and value.
pub type Headers = Vec<(String, String)>;

/// Something that POSTs JSON and gets JSON back: the providers' only way out.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Posts `body` to `url` and returns the response's JSON. A response that
    /// isn't a success is an [`ExtractError::Api`] with the provider's own
    /// message ([`error_message`]); a request that never got one is
    /// [`ExtractError::Http`].
    async fn post_json(
        &self,
        url: &str,
        headers: &Headers,
        body: &Value,
    ) -> Result<Value, ExtractError>;
}

/// The transport natively: reqwest, with retries.
#[cfg(feature = "reqwest")]
pub fn native() -> Arc<dyn Transport> {
    Arc::new(Reqwest::new())
}

/// Posts with reqwest, retrying 408, 409, 429 and 5xx responses and
/// connection errors, waiting as `retry-after` says or backing off.
#[cfg(feature = "reqwest")]
pub struct Reqwest {
    client: reqwest::Client,
}

#[cfg(feature = "reqwest")]
mod native {
    use std::time::Duration;

    use async_trait::async_trait;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    use serde_json::Value;

    use super::{ExtractError, Headers, Reqwest, Transport, error_message};

    /// Attempts per request: the first plus two retries.
    const ATTEMPTS: u32 = 3;

    /// Longest a single request may take. Extraction of a long PDF can be slow.
    const TIMEOUT: Duration = Duration::from_secs(300);

    impl Reqwest {
        pub fn new() -> Reqwest {
            let client = reqwest::Client::builder()
                .timeout(TIMEOUT)
                .user_agent(concat!("spendyard/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("an HTTP client");

            Reqwest { client }
        }
    }

    impl Default for Reqwest {
        fn default() -> Self {
            Reqwest::new()
        }
    }

    /// reqwest's headers; a name or value it won't take is left out.
    fn header_map(headers: &Headers) -> HeaderMap {
        let mut map = HeaderMap::new();

        for (name, value) in headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::try_from(name.as_str()),
                HeaderValue::from_str(value),
            ) {
                map.insert(name, value);
            }
        }

        map
    }

    #[async_trait]
    impl Transport for Reqwest {
        async fn post_json(
            &self,
            url: &str,
            headers: &Headers,
            body: &Value,
        ) -> Result<Value, ExtractError> {
            let headers = header_map(headers);

            let mut attempt = 0;

            loop {
                attempt += 1;

                let sent = self
                    .client
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

                let retryable =
                    matches!(status.as_u16(), 408 | 409 | 429) || status.is_server_error();

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
}

/// A header's value by name, ignoring case: for tests of what providers send.
pub fn header<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// A transport that goes nowhere: every post fails. For tests of providers that
/// look at what they'd send, without sending it.
pub fn nowhere() -> std::sync::Arc<dyn Transport> {
    struct Nowhere;

    #[async_trait]
    impl Transport for Nowhere {
        async fn post_json(
            &self,
            url: &str,
            _: &Headers,
            _: &Value,
        ) -> Result<Value, ExtractError> {
            Err(ExtractError::Http(format!("nowhere to post to {url}")))
        }
    }

    std::sync::Arc::new(Nowhere)
}

/// The provider's own error message when the body is JSON, the raw body
/// otherwise: for a [`Transport`]'s [`ExtractError::Api`].
pub fn error_message(body: &str) -> String {
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
