//! OpenAI's Responses API, with a strict JSON Schema
//! (`text.format.type = "json_schema"`).

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::{
    Accepts, Block, DEFAULT_MAX_TOKENS, ExtractError, Extraction, Extractor, Prompt, Settings,
    Usage, data_url, http, parse_output, required_key, required_model, schema,
};

const DEFAULT_URL: &str = "https://api.openai.com/v1";

const IMAGES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

pub struct OpenAi {
    client: reqwest::Client,
    key: String,
    url: String,
    model: String,
    max_tokens: u32,
}

impl OpenAi {
    pub fn new(settings: Settings) -> Result<Self, ExtractError> {
        let key = required_key(&settings)?;
        let model = required_model(&settings)?;

        let base = settings.base_url.as_deref().unwrap_or(DEFAULT_URL);

        let openai = OpenAi {
            client: http::client(),
            key,
            url: format!("{}/responses", base.trim_end_matches('/')),
            model,
            max_tokens: settings.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        };

        Ok(openai)
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();

        let bearer = format!("Bearer {}", self.key);

        if let Ok(value) = HeaderValue::from_str(&bearer) {
            headers.insert("authorization", value);
        }

        headers
    }

    fn body(&self, prompt: &Prompt) -> Value {
        let content: Vec<Value> = prompt.blocks.iter().map(content_part).collect();

        json!({
            "model": self.model,
            "instructions": prompt.system,
            "input": [{ "role": "user", "content": content }],
            "max_output_tokens": self.max_tokens,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "receipt",
                    "schema": schema::model_output_schema(),
                    "strict": true,
                }
            },
        })
    }
}

#[async_trait]
impl Extractor for OpenAi {
    fn name(&self) -> String {
        format!("openai/{}", self.model)
    }

    fn accepts(&self) -> Accepts {
        Accepts {
            pdf: true,
            images: IMAGES,
        }
    }

    async fn extract(&self, prompt: &Prompt) -> Result<Extraction, ExtractError> {
        let response =
            http::post_json(&self.client, &self.url, self.headers(), &self.body(prompt)).await?;

        parse_response(&response, &self.model)
    }
}

fn content_part(block: &Block) -> Value {
    match block {
        Block::Text(text) => json!({ "type": "input_text", "text": text }),

        Block::Pdf { label, bytes } => json!({
            "type": "input_file",
            "filename": label.rsplit(':').next().unwrap_or("document.pdf"),
            "file_data": data_url("application/pdf", bytes),
        }),

        Block::Image { mime, bytes, .. } => json!({
            "type": "input_image",
            "image_url": data_url(mime, bytes),
        }),
    }
}

fn parse_response(response: &Value, asked_for: &str) -> Result<Extraction, ExtractError> {
    if response["status"] == "incomplete" {
        return Err(ExtractError::Truncated);
    }

    let contents = response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().into_iter().flatten());

    let mut text = String::new();

    for content in contents {
        match content["type"].as_str() {
            Some("output_text") => text.push_str(content["text"].as_str().unwrap_or_default()),

            Some("refusal") => {
                let reason = content["refusal"].as_str().map(str::to_string);

                return Err(ExtractError::Refused(reason));
            }

            _ => {}
        }
    }

    let usage = Usage {
        input: response
            .pointer("/usage/input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: response
            .pointer("/usage/output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    };

    let extraction = Extraction {
        output: parse_output(&text)?,
        usage,
        model: response["model"].as_str().unwrap_or(asked_for).to_string(),
    };

    Ok(extraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_strict_schema_request() {
        let openai = OpenAi::new(Settings {
            provider: "openai".to_string(),
            model: Some("some-model".to_string()),
            api_key: Some("sk-test".to_string()),
            ..Settings::default()
        })
        .unwrap();

        let prompt = Prompt {
            system: "instructions",
            blocks: vec![
                Block::Text("hello".to_string()),
                Block::Pdf {
                    label: "attachment:Invoice.pdf".to_string(),
                    bytes: b"%PDF".to_vec(),
                },
            ],
            warnings: Vec::new(),
        };

        let body = openai.body(&prompt);

        assert_eq!(body["text"]["format"]["strict"], true);

        let content = body["input"][0]["content"].as_array().unwrap();

        assert_eq!(content[1]["type"], "input_file");
        assert_eq!(content[1]["filename"], "Invoice.pdf");
        assert!(
            content[1]["file_data"]
                .as_str()
                .unwrap()
                .starts_with("data:application/pdf;base64,")
        );

        assert_eq!(openai.headers()["authorization"], "Bearer sk-test");
    }

    #[test]
    fn reads_output_text_and_refusals() {
        let output = serde_json::to_string(&super::super::ModelOutput::default()).unwrap();

        let response = json!({
            "status": "completed",
            "output": [
                { "type": "reasoning" },
                { "type": "message", "content": [{ "type": "output_text", "text": output }] },
            ],
            "usage": { "input_tokens": 10, "output_tokens": 5 },
        });

        assert!(parse_response(&response, "m").is_ok());

        let refused = json!({
            "status": "completed",
            "output": [{ "type": "message", "content": [{ "type": "refusal", "refusal": "no" }] }],
        });

        assert!(matches!(
            parse_response(&refused, "m"),
            Err(ExtractError::Refused(_))
        ));
    }
}
