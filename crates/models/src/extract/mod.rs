//! The extractors: each turns the core's provider-neutral [`Prompt`] into its
//! own request, and its response back into [`ModelOutput`].

mod anthropic;
mod gemini;
pub mod http;
mod openai;
mod openai_compatible;

use std::sync::Arc;

pub use http::{Headers, Transport};

// What the providers build on, as they had it beside them before the split.
#[allow(unused_imports)]
use spendyard_core::extract::*;

/// Builds the extractor `settings` describe, posting natively.
#[cfg(feature = "reqwest")]
pub fn build(settings: Settings) -> Result<Box<dyn Extractor>, ExtractError> {
    build_with(settings, http::native())
}

/// Builds the extractor `settings` describe, posting through `transport`: a
/// host's own, in WebAssembly.
pub fn build_with(
    settings: Settings,
    transport: Arc<dyn Transport>,
) -> Result<Box<dyn Extractor>, ExtractError> {
    let extractor: Box<dyn Extractor> = match settings.provider.as_str() {
        "anthropic" => Box::new(anthropic::Anthropic::new(settings, transport)?),
        "openai" => Box::new(openai::OpenAi::new(settings, transport)?),
        "gemini" => Box::new(gemini::Gemini::new(settings, transport)?),
        "openai-compatible" => Box::new(openai_compatible::OpenAiCompatible::new(
            settings, transport,
        )?),
        other => return Err(ExtractError::UnknownProvider(other.to_string())),
    };

    Ok(extractor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_need_keys_and_models() {
        let missing_key = build(Settings {
            provider: "anthropic".to_string(),
            ..Settings::default()
        });

        assert!(matches!(missing_key, Err(ExtractError::MissingKey(_))));

        let missing_model = build(Settings {
            provider: "openai".to_string(),
            api_key: Some("sk-test".to_string()),
            ..Settings::default()
        });

        assert!(matches!(missing_model, Err(ExtractError::Config(_))));

        let unknown = build(Settings {
            provider: "nope".to_string(),
            ..Settings::default()
        });

        assert!(matches!(unknown, Err(ExtractError::UnknownProvider(_))));
    }
}
