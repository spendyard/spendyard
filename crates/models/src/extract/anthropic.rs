//! Anthropic's Messages API, with structured outputs
//! (`output_config.format`).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    Accepts, Block, DEFAULT_ANTHROPIC_MODEL, DEFAULT_MAX_TOKENS, ExtractError, Extractor,
    OutputFormat, Prompt, Reply, Settings, Usage, base64, http,
};

const DEFAULT_URL: &str = "https://api.anthropic.com";

const API_VERSION: &str = "2026-10-21";

/// Server-side fallback: when the model declines, the API re-runs the
/// request on a model chosen by the refusal's category.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Models that take `fallbacks: "default"`.
const FALLBACK_MODELS: &[&str] = &[
    "claude-fable-5-2",
    "claude-fable-5-1",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
];

/// For signing in with `ANTHROPIC_AUTH_TOKEN` instead of an API key.
const OAUTH_BETA: &str = "oauth-2025-04-20";

const IMAGES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

pub struct Anthropic {
    transport: Arc<dyn http::Transport>,
    credential: Credential,
    /// An authenticated AI Gateway's token, when `url` is one.
    gateway_token: Option<String>,
    url: String,
    model: String,
    effort: Option<String>,
    max_tokens: u32,
}

enum Credential {
    ApiKey(String),
    Token(String),
}

impl Anthropic {
    pub fn new(
        settings: Settings,
        transport: Arc<dyn http::Transport>,
    ) -> Result<Self, ExtractError> {
        let credential = match (&settings.api_key, &settings.auth_token) {
            (Some(key), _) if !key.is_empty() => Credential::ApiKey(key.clone()),
            (_, Some(token)) if !token.is_empty() => Credential::Token(token.clone()),
            _ => return Err(ExtractError::MissingKey("ANTHROPIC_API_KEY".to_string())),
        };

        let base = settings.base_url.as_deref().unwrap_or(DEFAULT_URL);

        let gateway_token = settings
            .gateway_token
            .clone()
            .filter(|token| !token.is_empty());

        let anthropic = Anthropic {
            transport,
            credential,
            gateway_token,
            url: format!("{}/v1/messages", base.trim_end_matches('/')),
            model: settings
                .model
                .unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.to_string()),
            effort: settings.effort,
            max_tokens: settings.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        };

        Ok(anthropic)
    }

    fn uses_fallback(&self) -> bool {
        FALLBACK_MODELS.contains(&self.model.as_str())
    }

    fn headers(&self) -> http::Headers {
        let mut headers = http::Headers::new();

        let mut betas = Vec::new();

        match &self.credential {
            Credential::ApiKey(key) => {
                headers.push(("x-api-key".to_string(), key.clone()));
            }

            Credential::Token(token) => {
                headers.push(("authorization".to_string(), format!("Bearer {token}")));

                betas.push(OAUTH_BETA);
            }
        }

        if let Some(token) = &self.gateway_token {
            headers.push((
                "cf-aig-authorization".to_string(),
                format!("Bearer {token}"),
            ));
        }

        if self.uses_fallback() {
            betas.push(FALLBACK_BETA);
        }

        headers.push(("anthropic-version".to_string(), API_VERSION.to_string()));

        if !betas.is_empty() {
            headers.push(("anthropic-beta".to_string(), betas.join(",")));
        }

        headers
    }

    fn body(&self, prompt: &Prompt, format: &OutputFormat) -> Value {
        let content: Vec<Value> = prompt.blocks.iter().map(content_block).collect();

        let mut output_config = json!({
            "format": {
                "type": "json_schema",
                "schema": format.schema.clone(),
            }
        });

        if let Some(effort) = &self.effort {
            output_config["effort"] = json!(effort);
        }

        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": prompt.system,
            "messages": [{ "role": "user", "content": content }],
            "output_config": output_config,
        });

        if self.uses_fallback() {
            body["fallbacks"] = json!("default");
        }

        body
    }
}

#[async_trait]
impl Extractor for Anthropic {
    fn name(&self) -> String {
        format!("anthropic/{}", self.model)
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

fn content_block(block: &Block) -> Value {
    match block {
        Block::Text(text) => json!({ "type": "text", "text": text }),

        Block::Pdf { bytes, .. } => json!({
            "type": "document",
            "source": {
                "type": "base64",
                "media_type": "application/pdf",
                "data": base64(bytes),
            },
        }),

        Block::Image { mime, bytes, .. } => json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": mime,
                "data": base64(bytes),
            },
        }),
    }
}

fn parse_response(response: &Value, asked_for: &str) -> Result<Reply, ExtractError> {
    let stop_reason = response["stop_reason"].as_str().unwrap_or_default();

    if stop_reason == "refusal" {
        let reason = response
            .pointer("/stop_details/explanation")
            .and_then(Value::as_str)
            .map(str::to_string);

        return Err(ExtractError::Refused(reason));
    }

    if stop_reason == "max_tokens" {
        return Err(ExtractError::Truncated);
    }

    let text: String = response["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect();

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

    let model = response["model"].as_str().unwrap_or(asked_for).to_string();

    let reply = Reply {
        text: text.to_string(),
        usage,
        model,
    };

    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extractor(model: &str) -> Anthropic {
        Anthropic::new(
            Settings {
                provider: "anthropic".to_string(),
                model: Some(model.to_string()),
                api_key: Some("sk-ant-test".to_string()),
                effort: Some("low".to_string()),
                ..Settings::default()
            },
            http::nowhere(),
        )
        .unwrap()
    }

    fn prompt() -> Prompt {
        Prompt {
            system: "instructions",
            blocks: vec![
                Block::Text("<part index=\"0\">Total $5</part>".to_string()),
                Block::Pdf {
                    label: "scan.pdf".to_string(),
                    bytes: b"%PDF-1.7".to_vec(),
                },
            ],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn builds_a_structured_output_request_with_fallback() {
        let anthropic = extractor("claude-opus-5-5");

        let body = anthropic.body(&prompt(), &OutputFormat::receipt());

        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["system"], "instructions");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["fallbacks"], "default");

        let content = body["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[1]["type"], "document");
        assert_eq!(content[1]["source"]["media_type"], "application/pdf");

        let headers = anthropic.headers();

        assert_eq!(http::header(&headers, "x-api-key").unwrap(), "sk-ant-test");
        assert_eq!(
            http::header(&headers, "anthropic-version").unwrap(),
            API_VERSION
        );
        assert_eq!(
            http::header(&headers, "anthropic-beta").unwrap(),
            FALLBACK_BETA
        );
    }

    #[test]
    fn an_authenticated_gateway_gets_its_token() {
        let through = Anthropic::new(
            Settings {
                provider: "anthropic".to_string(),
                api_key: Some("sk-ant-test".to_string()),
                base_url: Some(
                    "https://gateway.ai.cloudflare.com/v1/acct/gw/anthropic".to_string(),
                ),
                gateway_token: Some("cf-gateway".to_string()),
                ..Settings::default()
            },
            http::nowhere(),
        )
        .unwrap();

        let headers = through.headers();

        assert_eq!(
            through.url,
            "https://gateway.ai.cloudflare.com/v1/acct/gw/anthropic/v1/messages"
        );
        assert_eq!(http::header(&headers, "x-api-key").unwrap(), "sk-ant-test");
        assert_eq!(
            http::header(&headers, "cf-aig-authorization").unwrap(),
            "Bearer cf-gateway"
        );

        // Straight to Anthropic: no gateway, no token.
        assert_eq!(
            http::header(
                &extractor("claude-haiku-5-5").headers(),
                "cf-aig-authorization"
            ),
            None
        );
    }

    #[test]
    fn no_fallback_for_other_models() {
        let anthropic = extractor("claude-haiku-5-5");

        assert!(
            anthropic
                .body(&prompt(), &OutputFormat::receipt())
                .get("fallbacks")
                .is_none()
        );
        assert!(http::header(&anthropic.headers(), "anthropic-beta").is_none());
    }

    #[test]
    fn reads_text_usage_and_model() {
        let output = serde_json::to_string(&super::super::ModelOutput::default()).unwrap();

        let response = json!({
            "model": "claude-opus-5-5",
            "stop_reason": "end_turn",
            "content": [
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": output },
            ],
            "usage": { "input_tokens": 1200, "output_tokens": 300 },
        });

        let extraction = parse_response(&response, "x").unwrap();

        assert_eq!(
            extraction.usage,
            Usage {
                input: 1200,
                output: 300
            }
        );
        assert_eq!(extraction.model, "claude-opus-5-5");
    }

    #[test]
    fn refusals_and_cut_offs_are_errors() {
        let refused = json!({
            "stop_reason": "refusal",
            "stop_details": { "type": "refusal", "category": null, "explanation": "no" },
            "content": [],
        });

        assert!(matches!(
            parse_response(&refused, "x"),
            Err(ExtractError::Refused(_))
        ));

        let cut = json!({ "stop_reason": "max_tokens", "content": [] });

        assert!(matches!(
            parse_response(&cut, "x"),
            Err(ExtractError::Truncated)
        ));
    }
}
