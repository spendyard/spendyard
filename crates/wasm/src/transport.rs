//! The providers' transport in a JavaScript host: its function does the
//! posting (the Worker's `fetch`), and the answer comes back here as the
//! response's status and text.
//!
//! JavaScript's promises aren't `Send`, and the providers' traits ask for it.
//! WebAssembly here has one thread, so wrapping them in `SendWrapper` is sound:
//! it would panic if they ever reached another thread, and none exists.

use std::sync::Arc;

use async_trait::async_trait;
use send_wrapper::SendWrapper;
use serde::Deserialize;
use serde_json::Value;
use spendyard_core::extract::ExtractError;
use spendyard_models::extract::http::{Headers, Transport, error_message};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

/// What the host's function answers: the response's status and text.
#[derive(Deserialize)]
struct Response {
    status: u16,
    body: String,
}

/// The host's posting function, as a transport.
pub fn host(post: js_sys::Function) -> Arc<dyn Transport> {
    Arc::new(Host {
        post: SendWrapper::new(post),
    })
}

struct Host {
    post: SendWrapper<js_sys::Function>,
}

#[async_trait]
impl Transport for Host {
    async fn post_json(
        &self,
        url: &str,
        headers: &Headers,
        body: &Value,
    ) -> Result<Value, ExtractError> {
        let call = self.post.call3(
            &JsValue::NULL,
            &JsValue::from_str(url),
            &JsValue::from_str(&serde_json::to_string(headers).unwrap_or_default()),
            &JsValue::from_str(&body.to_string()),
        );

        let promise = match call {
            Ok(value) => js_sys::Promise::resolve(&value),
            Err(error) => return Err(ExtractError::Http(describe(&error))),
        };

        let answered = SendWrapper::new(JsFuture::from(promise)).await;

        let response: Response = match answered {
            Ok(value) => {
                serde_json::from_str(&value.as_string().unwrap_or_default()).map_err(|error| {
                    ExtractError::Http(format!(
                        "the host's answer wasn't {{status, body}}: {error}"
                    ))
                })?
            }
            Err(error) => return Err(ExtractError::Http(describe(&error))),
        };

        if !(200..300).contains(&response.status) {
            return Err(ExtractError::Api {
                status: response.status,
                message: error_message(&response.body),
            });
        }

        serde_json::from_str(&response.body).map_err(|error| ExtractError::Http(error.to_string()))
    }
}

/// A JavaScript error in words.
fn describe(error: &JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(error, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| "the host's post failed".to_string())
}
