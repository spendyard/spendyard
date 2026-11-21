//! Google's Gemini API (`generateContent`), with a JSON Schema response
//! (`responseJsonSchema`).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    Accepts, Block, DEFAULT_MAX_TOKENS, ExtractError, Extractor, OutputFormat, Prompt, Reply,
    Settings, Usage, base64, http, required_key, required_model,
};

const DEFAULT_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Gemini also reads HEIC, which most phones save photos as.
const IMAGES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/webp",
    "image/heic",
    "image/heif",
];

pub struct Gemini {
    transport: Arc<dyn http::Transport>,
    key: String,
    url: String,
    model: String,
    max_tokens: u32,
}

impl Gemini {
    pub fn new(
        settings: Settings,
        transport: Arc<dyn http::Transport>,
    ) -> Result<Self, ExtractError> {
        let key = required_key(&settings)?;
        let model = required_model(&settings)?;

        let base = settings.base_url.as_deref().unwrap_or(DEFAULT_URL);

        let gemini = Gemini {
            transport,
            key,
            url: format!(
                "{}/models/{model}:generateContent",
                base.trim_end_matches('/')
            ),
            model,
            max_tokens: settings.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        };

        Ok(gemini)
    }

    fn headers(&self) -> http::Headers {
        vec![("x-goog-api-key".to_string(), self.key.clone())]
    }

    fn body(&self, prompt: &Prompt, format: &OutputFormat) -> Value {
        let parts: Vec<Value> = prompt.blocks.iter().map(part).collect();

        json!({
            "systemInstruction": { "parts": [{ "text": prompt.system }] },
            "contents": [{ "role": "user", "parts": parts }],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseJsonSchema": format.schema.clone(),
                "maxOutputTokens": self.max_tokens,
            },
        })
    }
}

#[async_trait]
impl Extractor for Gemini {
    fn name(&self) -> String {
        format!("gemini/{}", self.model)
    }

    fn accepts(&self) -> Accepts {
        Accepts {
            pdf: true,
            images: IMAGES,
        }
    }

    async fn answer(&self, prompt: &Prompt, format: &OutputFormat) -> Result<Reply, ExtractError> {
        let response = self
            .transport
            .post_json(&self.url, &self.headers(), &self.body(prompt, format))
            .await?;

        parse_response(&response, &self.model)
    }
}

fn part(block: &Block) -> Value {
    match block {
        Block::Text(text) => json!({ "text": text }),

        Block::Pdf { bytes, .. } => json!({
            "inlineData": { "mimeType": "application/pdf", "data": base64(bytes) },
        }),

        Block::Image { mime, bytes, .. } => json!({
            "inlineData": { "mimeType": mime, "data": base64(bytes) },
        }),
    }
}

fn parse_response(response: &Value, asked_for: &str) -> Result<Reply, ExtractError> {
    if let Some(reason) = response
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(ExtractError::Refused(Some(reason.to_string())));
    }

    let candidate = &response["candidates"][0];

    match candidate["finishReason"].as_str() {
        Some("MAX_TOKENS") => return Err(ExtractError::Truncated),

        Some(reason @ ("SAFETY" | "RECITATION" | "PROHIBITED_CONTENT" | "BLOCKLIST")) => {
            return Err(ExtractError::Refused(Some(reason.to_string())));
        }

        _ => {}
    }

    let text: String = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part["thought"] != true)
        .filter_map(|part| part["text"].as_str())
        .collect();

    let usage = Usage {
        input: response
            .pointer("/usageMetadata/promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: response
            .pointer("/usageMetadata/candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    };

    let reply = Reply {
        text: text.to_string(),
        usage,
        model: response["modelVersion"]
            .as_str()
            .unwrap_or(asked_for)
            .to_string(),
    };

    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_json_schema_request() {
        let gemini = Gemini::new(
            Settings {
                provider: "gemini".to_string(),
                model: Some("some-model".to_string()),
                api_key: Some("key".to_string()),
                ..Settings::default()
            },
            http::nowhere(),
        )
        .unwrap();

        assert!(gemini.url.ends_with("/models/some-model:generateContent"));

        let prompt = Prompt {
            system: "instructions",
            blocks: vec![Block::Image {
                label: "photo".to_string(),
                mime: "image/heic".to_string(),
                bytes: vec![0],
            }],
            warnings: Vec::new(),
        };

        let body = gemini.body(&prompt, &OutputFormat::receipt());

        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        assert_eq!(
            body["contents"][0]["parts"][0]["inlineData"]["mimeType"],
            "image/heic"
        );
        assert_eq!(
            http::header(&gemini.headers(), "x-goog-api-key").unwrap(),
            "key"
        );
    }

    #[test]
    fn reads_text_and_stops() {
        let output = serde_json::to_string(&super::super::ModelOutput::default()).unwrap();

        let response = json!({
            "candidates": [{ "content": { "parts": [{ "text": output }] }, "finishReason": "STOP" }],
            "usageMetadata": { "promptTokenCount": 7, "candidatesTokenCount": 3 },
        });

        let extraction = parse_response(&response, "m").unwrap();

        assert_eq!(
            extraction.usage,
            Usage {
                input: 7,
                output: 3
            }
        );

        let cut = json!({ "candidates": [{ "finishReason": "MAX_TOKENS" }] });

        assert!(matches!(
            parse_response(&cut, "m"),
            Err(ExtractError::Truncated)
        ));
    }
}
