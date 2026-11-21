//! The core for a JavaScript host (Spendyard Cloud's Worker), through
//! `wasm-bindgen`. Answers are JSON text, as the CLI prints them, so the host
//! reads exactly what `spendyard read` would. Each export is a plain function
//! here too, which builds natively, so the same answer can be checked both
//! ways (`examples/read.rs`, `tests/compare.mjs`).
//!
//! More of the core comes out as the cloud needs it: the steps either side of
//! a model call, and the rows its store keeps (spendyard/platform#13).

use wasm_bindgen::prelude::wasm_bindgen;

/// Reads a document: `{"document": …}`, or `{"error": "…"}` when it can't be
/// read. The document is what `spendyard parts` shows: its format, parts and
/// text.
pub fn read_json(bytes: &[u8]) -> String {
    let answer = match spendyard_core::read(bytes, None) {
        Ok(document) => serde_json::json!({ "document": document }),
        Err(error) => serde_json::json!({ "error": error.to_string() }),
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
