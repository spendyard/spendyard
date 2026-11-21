//! Any server that speaks OpenAI's Chat Completions API: Ollama, vLLM,
//! LM Studio, llama.cpp and many hosted services. Text and images only;
//! PDFs without a text layer can't be sent.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    Accepts, Block, DEFAULT_MAX_TOKENS, ExtractError, Extractor, OutputFormat, Prompt, Reply,
    Settings, Usage, data_url, http, required_model,
};

/// Ollama's default address.
const DEFAULT_URL: &str = "http://localhost:11434/v1";

const IMAGES: &[&str] = &["image/jpeg", "image/png", "image/webp"];

pub struct OpenAiCompatible {
    transport: Arc<dyn http::Transport>,
    key: Option<String>,
    url: String,
    model: String,
    max_tokens: u32,
}

impl OpenAiCompatible {
    pub fn new(
        settings: Settings,
        transport: Arc<dyn http::Transport>,
    ) -> Result<Self, ExtractError> {
        let model = required_model(&settings)?;

        let base = settings.base_url.as_deref().unwrap_or(DEFAULT_URL);

        let compatible = OpenAiCompatible {
            transport,
            key: settings.api_key.filter(|key| !key.is_empty()),
            url: format!("{}/chat/completions", base.trim_end_matches('/')),
            model,
            max_tokens: settings.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        };

        Ok(compatible)
    }

    fn headers(&self) -> http::Headers {
        let mut headers = http::Headers::new();

        if let Some(key) = &self.key {
            headers.push(("authorization".to_string(), format!("Bearer {key}")));
        }

        headers
    }

    fn body(&self, prompt: &Prompt, format: &OutputFormat) -> Value {
        let content: Vec<Value> = prompt.blocks.iter().filter_map(content_part).collect();

        json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [
                { "role": "system", "content": prompt.system },
                { "role": "user", "content": content },
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": format.name,
                    "schema": format.schema.clone(),
                    "strict": true,
                }
            },
        })
    }
}

#[async_trait]
impl Extractor for OpenAiCompatible {
    fn name(&self) -> String {
        format!("openai-compatible/{}", self.model)
    }

    fn accepts(&self) -> Accepts {
        Accepts {
            pdf: false,
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

/// PDFs never reach here: [`Accepts::pdf`] is false, so the prompt builder
/// leaves them out.
fn content_part(block: &Block) -> Option<Value> {
    match block {
        Block::Text(text) => Some(json!({ "type": "text", "text": text })),

        Block::Image { mime, bytes, .. } => Some(json!({
            "type": "image_url",
            "image_url": { "url": data_url(mime, bytes) },
        })),

        Block::Pdf { .. } => None,
    }
}

fn parse_response(response: &Value, asked_for: &str) -> Result<Reply, ExtractError> {
    let choice = &response["choices"][0];

    if choice["finish_reason"] == "length" {
        return Err(ExtractError::Truncated);
    }

    if let Some(refusal) = choice.pointer("/message/refusal").and_then(Value::as_str) {
        return Err(ExtractError::Refused(Some(refusal.to_string())));
    }

    let text = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let usage = Usage {
        input: response
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: response
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    };

    let reply = Reply {
        text: text.to_string(),
        usage,
        model: response["model"].as_str().unwrap_or(asked_for).to_string(),
    };

    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn works_without_a_key_and_defaults_to_ollama() {
        let compatible = OpenAiCompatible::new(
            Settings {
                provider: "openai-compatible".to_string(),
                model: Some("qwen3".to_string()),
                ..Settings::default()
            },
            http::nowhere(),
        )
        .unwrap();

        assert_eq!(compatible.url, "http://localhost:11434/v1/chat/completions");
        assert!(http::header(&compatible.headers(), "authorization").is_none());

        let prompt = Prompt {
            system: "instructions",
            blocks: vec![Block::Text("hi".to_string())],
            warnings: Vec::new(),
        };

        let body = compatible.body(&prompt, &OutputFormat::receipt());

        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["response_format"]["type"], "json_schema");
    }

    #[test]
    fn reads_message_content() {
        let output = serde_json::to_string(&super::super::ModelOutput::default()).unwrap();

        let response = json!({
            "model": "qwen3",
            "choices": [{ "finish_reason": "stop", "message": { "content": output } }],
            "usage": { "prompt_tokens": 4, "completion_tokens": 2 },
        });

        let extraction = parse_response(&response, "m").unwrap();

        assert_eq!(
            extraction.usage,
            Usage {
                input: 4,
                output: 2
            }
        );
    }
}
