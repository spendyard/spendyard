//! `~/.spendyard/config.toml` and the environment.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;
use spendyard::extract::{self, Settings};

/// What the config file holds. Everything is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub decider: DeciderConfig,

    #[serde(default)]
    pub extractor: ExtractorConfig,

    /// Per provider: `[providers.openai-compatible]`.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeciderConfig {
    /// `jev`, `clef`, `clef-flash`, `systemone` or `none`. No first model
    /// when unset.
    pub provider: Option<String>,

    pub model: Option<String>,

    /// How sure the first model must be that a document isn't about money
    /// before the extractor is skipped. Default 0.85.
    pub threshold: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractorConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub base_url: Option<String>,

    /// Read the key from this environment variable instead of the usual
    /// one.
    pub api_key_env: Option<String>,
}

/// `SPENDYARD_HOME`, or `~/.spendyard`.
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("SPENDYARD_HOME") {
        return PathBuf::from(home);
    }

    let user_home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));

    user_home.join(".spendyard")
}

/// Loads the config file, or the defaults when there is none.
pub fn load() -> Result<Config> {
    let path = home().join("config.toml");

    if !path.exists() {
        return Ok(Config::default());
    }

    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    let config = toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;

    Ok(config)
}

/// The extractor to use: `--extractor` wins over the config, which wins
/// over the default (Anthropic). `--extractor` is `provider` or
/// `provider/model`; the model may itself contain slashes.
pub fn extractor_settings(config: &Config, flag: Option<&str>) -> Settings {
    let (flag_provider, flag_model) = match flag {
        Some(value) => match value.split_once('/') {
            Some((provider, model)) => (Some(provider.to_string()), Some(model.to_string())),
            None => (Some(value.to_string()), None),
        },

        None => (None, None),
    };

    let provider = flag_provider
        .or_else(|| config.extractor.provider.clone())
        .unwrap_or_else(|| "anthropic".to_string());

    // A model in the config belongs to the configured provider only.
    let same_provider = config.extractor.provider.as_deref().unwrap_or("anthropic") == provider;

    let model = flag_model.or_else(|| {
        same_provider
            .then(|| config.extractor.model.clone())
            .flatten()
    });

    let provider_config = config.providers.get(&provider);

    let key_variable = provider_config
        .and_then(|settings| settings.api_key_env.clone())
        .or_else(|| extract::key_variable(&provider).map(str::to_string));

    let mut api_key = key_variable.and_then(|variable| env(&variable));

    if provider == "gemini" && api_key.is_none() {
        api_key = env("GOOGLE_API_KEY");
    }

    let mut base_url = provider_config.and_then(|settings| settings.base_url.clone());

    if provider == "anthropic" && base_url.is_none() {
        base_url = env("ANTHROPIC_BASE_URL");
    }

    let auth_token = (provider == "anthropic")
        .then(|| env("ANTHROPIC_AUTH_TOKEN"))
        .flatten();

    Settings {
        provider,
        model,
        api_key,
        auth_token,
        base_url,
        effort: same_provider
            .then(|| config.extractor.effort.clone())
            .flatten(),
        max_tokens: config.extractor.max_tokens,
    }
}

/// The first model to use, if any: `--decider` wins over the config.
/// `none` turns it off. `--decider` is `provider` or `provider/model`.
pub fn decider_settings(
    config: &Config,
    flag: Option<&str>,
) -> Option<spendyard::decide::Settings> {
    let (flag_provider, flag_model) = match flag {
        Some(value) => match value.split_once('/') {
            Some((provider, model)) => (Some(provider.to_string()), Some(model.to_string())),
            None => (Some(value.to_string()), None),
        },

        None => (None, None),
    };

    let provider = flag_provider.or_else(|| config.decider.provider.clone())?;

    if provider == "none" {
        return None;
    }

    let same_provider = config.decider.provider.as_deref() == Some(provider.as_str());

    let model = flag_model.or_else(|| {
        same_provider
            .then(|| config.decider.model.clone())
            .flatten()
    });

    let provider_config = config.providers.get(&provider);

    let key_variable = provider_config
        .and_then(|settings| settings.api_key_env.clone())
        .or_else(|| spendyard::decide::key_variable(&provider).map(str::to_string));

    let settings = spendyard::decide::Settings {
        api_key: key_variable.and_then(|variable| env(&variable)),
        account_id: env("CLOUDFLARE_ACCOUNT_ID"),
        base_url: provider_config.and_then(|settings| settings.base_url.clone()),
        provider,
        model,
    };

    Some(settings)
}

/// The decider threshold from the config, or the default.
pub fn threshold(config: &Config) -> f64 {
    config
        .decider
        .threshold
        .unwrap_or(spendyard::decide::DEFAULT_THRESHOLD)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn defaults_to_anthropic() {
        let settings = extractor_settings(&Config::default(), None);

        assert_eq!(settings.provider, "anthropic");
        assert_eq!(settings.model, None);
    }

    #[test]
    fn flag_overrides_config_and_drops_its_model() {
        let config = config(
            r#"
            [extractor]
            provider = "anthropic"
            model = "claude-haiku-5-5"
            "#,
        );

        let settings = extractor_settings(&config, Some("openai-compatible/meta-llama/Llama-4"));

        assert_eq!(settings.provider, "openai-compatible");
        assert_eq!(settings.model.as_deref(), Some("meta-llama/Llama-4"));

        let from_config = extractor_settings(&config, None);

        assert_eq!(from_config.model.as_deref(), Some("claude-haiku-5-5"));
    }

    #[test]
    fn provider_base_url_comes_from_the_config() {
        let config = config(
            r#"
            [providers.openai-compatible]
            base_url = "http://localhost:1234/v1"
            "#,
        );

        let settings = extractor_settings(&config, Some("openai-compatible/qwen3"));

        assert_eq!(
            settings.base_url.as_deref(),
            Some("http://localhost:1234/v1")
        );
    }

    #[test]
    fn decider_comes_from_flag_or_config() {
        assert!(decider_settings(&Config::default(), None).is_none());

        let config = config(
            r#"
            [decider]
            provider = "jev"
            model = "openjev-latest"
            threshold = 0.9

            [providers.jev]
            base_url = "https://api.codiv.ai"
            "#,
        );

        let settings = decider_settings(&config, None).unwrap();

        assert_eq!(settings.provider, "jev");
        assert_eq!(settings.model.as_deref(), Some("openjev-latest"));
        assert_eq!(settings.base_url.as_deref(), Some("https://api.codiv.ai"));
        assert_eq!(threshold(&config), 0.9);

        assert!(decider_settings(&config, Some("none")).is_none());
        assert_eq!(
            decider_settings(&config, Some("clef-flash")).unwrap().model,
            None
        );
    }

    #[test]
    fn unknown_keys_are_errors() {
        let result: Result<Config, _> = toml::from_str("[extractor]\nmodle = \"x\"\n");

        assert!(result.is_err());
    }
}
