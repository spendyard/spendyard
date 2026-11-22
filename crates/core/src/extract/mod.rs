//! The capable second model: reads a document and fills in [`ModelOutput`].
//!
//! The providers are spendyard-models': each turns a provider-neutral
//! [`Prompt`] into its own request and its response back into
//! [`ModelOutput`]. A host can call a model itself (the cloud does) with the
//! same prompt, and read its answer with [`parse_output`].

pub mod prompt;
pub mod schema;

use async_trait::async_trait;
use base64::Engine;
use serde_json::Value;
use thiserror::Error;

pub use prompt::{Accepts, Block, Prompt};
pub use schema::ModelOutput;

/// Why an extraction failed.
#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("{0} is not set")]
    MissingKey(String),

    #[error("unknown extractor {0:?}; use anthropic, openai, gemini or openai-compatible")]
    UnknownProvider(String),

    #[error("{0}")]
    Config(String),

    #[error("request failed: {0}")]
    Http(String),

    #[error("the provider answered {status}: {message}")]
    Api { status: u16, message: String },

    #[error("the model declined to answer{}", refusal_reason(.0))]
    Refused(Option<String>),

    #[error("the answer was cut off at the token limit")]
    Truncated,

    #[error("the answer wasn't valid: {0}")]
    InvalidOutput(String),

    #[error("nothing in the document could be sent to this extractor: {0}")]
    NothingToRead(String),
}

fn refusal_reason(reason: &Option<String>) -> String {
    match reason {
        Some(reason) => format!(": {reason}"),
        None => String::new(),
    }
}

/// Tokens a call used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
}

/// The model's answer and what it cost.
#[derive(Debug, Clone)]
pub struct Extraction {
    pub output: ModelOutput,
    pub usage: Usage,

    /// The model that actually answered, which may differ from the one
    /// asked for when a provider falls back.
    pub model: String,
}

/// The JSON shape a model is asked to answer in: a name (some providers
/// want one) and a strict schema.
#[derive(Debug, Clone)]
pub struct OutputFormat {
    pub name: &'static str,
    pub schema: Value,
}

impl OutputFormat {
    /// What a receipt or invoice is read into: [`ModelOutput`].
    pub fn receipt() -> OutputFormat {
        OutputFormat {
            name: "receipt",
            schema: schema::model_output_schema(),
        }
    }
}

/// A model's answer as JSON text, before it is parsed, and what it cost.
#[derive(Debug, Clone)]
pub struct Reply {
    pub text: String,
    pub usage: Usage,

    /// The model that actually answered.
    pub model: String,
}

/// A model that reads documents.
#[async_trait]
pub trait Extractor: Send + Sync {
    /// `provider/model`, as reported in results.
    fn name(&self) -> String;

    /// What it can read besides text.
    fn accepts(&self) -> Accepts;

    /// Answers `prompt` with JSON in `format`.
    async fn answer(&self, prompt: &Prompt, format: &OutputFormat) -> Result<Reply, ExtractError>;

    /// Reads a receipt or invoice into [`ModelOutput`].
    async fn extract(&self, prompt: &Prompt) -> Result<Extraction, ExtractError> {
        let reply = self.answer(prompt, &OutputFormat::receipt()).await?;

        let extraction = Extraction {
            output: parse_output(&reply.text)?,
            usage: reply.usage,
            model: reply.model,
        };

        Ok(extraction)
    }
}

/// Settings for building an extractor. Keys are read from the environment
/// by the caller and passed in; the library never reads the environment.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    /// `anthropic`, `openai`, `gemini` or `openai-compatible`.
    pub provider: String,

    /// The model. Anthropic has a default; the others need one named.
    pub model: Option<String>,

    pub api_key: Option<String>,

    /// Sent as `Authorization: Bearer` instead of an API key (Anthropic).
    pub auth_token: Option<String>,

    /// Override the provider's URL, such as `http://localhost:11434/v1` for
    /// Ollama.
    pub base_url: Option<String>,

    /// A token for the Cloudflare AI Gateway `base_url` is, when it asks for
    /// one: sent as `cf-aig-authorization: Bearer` (Anthropic).
    pub gateway_token: Option<String>,

    /// Anthropic `output_config.effort`: low, medium, high, xhigh or max.
    pub effort: Option<String>,

    pub max_tokens: Option<u32>,
}

/// The default model for Anthropic.
pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-5-5";

/// Generous enough for an invoice with many lines.
pub const DEFAULT_MAX_TOKENS: u32 = 16_000;

/// Every provider's name, for help text and errors.
pub const PROVIDERS: &[&str] = &["anthropic", "openai", "gemini", "openai-compatible"];

/// The environment variable each provider's key is read from.
pub fn key_variable(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "openai-compatible" => Some("OPENAI_COMPATIBLE_API_KEY"),
        _ => None,
    }
}

/// Parses a model's JSON text into [`ModelOutput`].
pub fn parse_output(text: &str) -> Result<ModelOutput, ExtractError> {
    parse_json(text)
}

/// Parses a model's JSON text, with or without a code fence, into `T`.
pub fn parse_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, ExtractError> {
    let trimmed = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    serde_json::from_str(trimmed).map_err(|error| ExtractError::InvalidOutput(error.to_string()))
}

/// Bytes as standard base64, for images and PDFs in a request.
pub fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Bytes as a `data:` URL, for providers that take files that way.
pub fn data_url(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64(bytes))
}

/// The model `settings` name, or an error saying how to name one.
pub fn required_model(settings: &Settings) -> Result<String, ExtractError> {
    let model = settings.model.clone().filter(|model| !model.is_empty());

    model.ok_or_else(|| {
        ExtractError::Config(format!(
            "{} needs a model: --extractor {}/<model>, or model = \"…\" under [extractor] in the config",
            settings.provider, settings.provider
        ))
    })
}

/// The API key in `settings`, or an error naming the variable it comes from.
pub fn required_key(settings: &Settings) -> Result<String, ExtractError> {
    let variable = key_variable(&settings.provider).unwrap_or("the API key");

    let key = settings.api_key.clone().filter(|key| !key.is_empty());

    key.ok_or_else(|| ExtractError::MissingKey(variable.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_output_with_or_without_a_code_fence() {
        let json = serde_json::to_string(&ModelOutput::default()).unwrap();

        assert!(parse_output(&json).is_ok());
        assert!(parse_output(&format!("```json\n{json}\n```")).is_ok());
        assert!(parse_output("not json").is_err());
    }
}
