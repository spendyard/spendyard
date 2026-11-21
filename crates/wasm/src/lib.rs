//! The core for a JavaScript host (Spendyard Cloud's Worker), through
//! `wasm-bindgen`. Answers are JSON text, as the CLI prints them, so the host
//! reads exactly what `spendyard read` would.
//!
//! - [`read`]: a document's format, parts and text (`spendyard parts`).
//! - [`understand`]: the whole pipeline (`spendyard read`), with the CLI's
//!   providers and prompts, posting through a function the host gives: the
//!   Worker's `fetch` to AI Gateway and Workers AI. With the result come the
//!   rows a store keeps from it.
//!
//! [`read_json`] builds natively too, so the same answer can be checked both
//! ways (`examples/read.rs`, `tests/compare.mjs`).

mod transport;

use serde::Deserialize;
use serde_json::{Value, json};
use spendyard_core::{Pipeline, decide, extract};
use wasm_bindgen::prelude::wasm_bindgen;

/// Reads a document: `{"document": …}`, or `{"error": "…"}` when it can't be
/// read. The document is what `spendyard parts` shows: its format, parts and
/// text.
pub fn read_json(bytes: &[u8]) -> String {
    let answer = match spendyard_core::read(bytes, None) {
        Ok(document) => json!({ "document": document }),
        Err(error) => json!({ "error": error.to_string() }),
    };

    answer.to_string()
}

/// [`read_json`], for JavaScript.
#[wasm_bindgen]
pub fn read(bytes: &[u8]) -> String {
    read_json(bytes)
}

/// The core's version: `meta.spendyard_version` in its results.
#[wasm_bindgen]
pub fn version() -> String {
    spendyard_core::VERSION.to_string()
}

/// What [`understand`] reads with, as JSON.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    /// The capable model: `{"provider": "anthropic", "model": …, "apiKey": …, "baseUrl": …}`.
    extractor: ModelOptions,

    /// The cheap first model, if any: `{"provider": "clef-flash", "accountId": …, "apiKey": …}`.
    decider: Option<ModelOptions>,

    /// How sure the first model must be that a document isn't about money
    /// before the extractor is skipped; the CLI's default otherwise.
    threshold: Option<f64>,

    /// A file name, which helps tell a format apart.
    name: Option<String>,

    /// When it arrived, RFC 3339: a receipt that doesn't say when it was
    /// issued is dated by it.
    received: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelOptions {
    provider: String,
    model: Option<String>,
    api_key: Option<String>,
    account_id: Option<String>,
    base_url: Option<String>,
    max_tokens: Option<u32>,
}

/// Reads a document with the whole pipeline, as `spendyard read` does, posting
/// to the models through `post`. Its answer is JSON: `{"document": …,
/// "result": …, "rows": …}`, or `{"error": "…", "stage": …}` when it couldn't
/// read it (`stage` is `options`, `read` or `understand`).
///
/// `post(url, headers, body)` takes the URL, the headers as JSON (`[[name,
/// value], …]`) and the body as JSON text, and returns a promise of
/// `{"status": 200, "body": "…"}`: the response's status and text. It
/// rejects only when no response came.
#[wasm_bindgen]
pub async fn understand(bytes: Vec<u8>, options: String, post: js_sys::Function) -> String {
    understand_with(bytes, &options, transport::host(post))
        .await
        .to_string()
}

async fn understand_with(
    bytes: Vec<u8>,
    options: &str,
    transport: std::sync::Arc<dyn spendyard_models::extract::Transport>,
) -> Value {
    let options: Options = match serde_json::from_str(options) {
        Ok(options) => options,
        Err(error) => return json!({ "error": error.to_string(), "stage": "options" }),
    };

    let document = match spendyard_core::read(&bytes, options.name.as_deref()) {
        Ok(document) => document,
        Err(error) => return json!({ "error": error.to_string(), "stage": "read" }),
    };

    let extractor = match spendyard_models::extract::build_with(
        extractor_settings(options.extractor),
        transport.clone(),
    ) {
        Ok(extractor) => extractor,
        Err(error) => return json!({ "error": error.to_string(), "stage": "options" }),
    };

    let decider = match options.decider.map(decider_settings) {
        Some(settings) => match spendyard_models::decide::build_with(settings, transport) {
            Ok(decider) => Some(decider),
            Err(error) => return json!({ "error": error.to_string(), "stage": "options" }),
        },
        None => None,
    };

    let pipeline = Pipeline {
        decider: decider.as_deref(),
        extractor: extractor.as_ref(),
        threshold: options.threshold.unwrap_or(decide::DEFAULT_THRESHOLD),
    };

    let result = match spendyard_core::understand(&document, &pipeline).await {
        Ok(result) => result,
        Err(error) => return json!({ "error": error.to_string(), "stage": "understand" }),
    };

    let headers = document.headers.as_ref();

    let received = options
        .received
        .clone()
        .or_else(|| headers.and_then(|headers| headers.date.clone()));

    let rows = spendyard_core::rows::rows(&result, received.as_deref());

    json!({
        "document": {
            "format": document.format,
            "from": headers.and_then(|headers| headers.from.clone()),
            "subject": headers.and_then(|headers| headers.subject.clone()),
            "date": headers.and_then(|headers| headers.date.clone()),
            "messageId": headers.and_then(|headers| headers.message_id.clone()),
        },
        "result": result,
        "rows": rows,
    })
}

fn extractor_settings(options: ModelOptions) -> extract::Settings {
    extract::Settings {
        provider: options.provider,
        model: options.model,
        api_key: options.api_key,
        base_url: options.base_url,
        max_tokens: options.max_tokens,
        ..extract::Settings::default()
    }
}

fn decider_settings(options: ModelOptions) -> decide::Settings {
    decide::Settings {
        provider: options.provider,
        model: options.model,
        api_key: options.api_key,
        account_id: options.account_id,
        base_url: options.base_url,
    }
}
