//! `spendyard config`: where the settings live and what is in effect.

use anyhow::{Context, Result, bail};

use super::config;

/// The starter file `spendyard config init` writes.
const TEMPLATE: &str = r#"# Spendyard settings. API keys never go here; they come from the
# environment (variables named below).

[extractor]
# anthropic (ANTHROPIC_API_KEY), openai (OPENAI_API_KEY),
# gemini (GEMINI_API_KEY) or openai-compatible (Ollama, vLLM, LM Studio).
provider = "anthropic"
model = "claude-haiku-5-5"
# effort = "low"       # Anthropic only
# max_tokens = 16000

[decider]
# The cheap first model, asked before the extractor. Leave provider unset
# to send every document straight to the extractor.
# jev (TYPESAFE_API_KEY), clef or clef-flash (CLOUDFLARE_API_TOKEN and
# CLOUDFLARE_ACCOUNT_ID), or systemone (a server of your own).
# provider = "clef-flash"
# threshold = 0.85     # how sure it must be a document isn't about money

# Codiv's OpenJev instead of TypeSafe's JEV:
# [providers.jev]
# base_url = "https://api.codiv.ai"
# and model = "openjev-latest" under [decider].

# [providers.openai-compatible]
# base_url = "http://localhost:11434/v1"
"#;

pub fn show(env_files: &[std::path::PathBuf]) -> Result<()> {
    let path = config::home().join("config.toml");

    let exists = path.exists();

    let note = if exists {
        ""
    } else {
        " (not there; defaults in use — `spendyard config init` writes one)"
    };

    println!("config  {}{note}", path.display());

    for file in env_files {
        println!("env     {} (loaded)", file.display());
    }

    println!();

    let config = config::load()?;

    let extractor = config::extractor_settings(&config, None);

    let model = extractor
        .model
        .clone()
        .unwrap_or_else(|| match extractor.provider.as_str() {
            "anthropic" => spendyard_core::extract::DEFAULT_ANTHROPIC_MODEL.to_string(),
            _ => "(none set)".to_string(),
        });

    let key = spendyard_core::extract::key_variable(&extractor.provider).unwrap_or("-");

    let key_set = extractor.api_key.is_some() || extractor.auth_token.is_some();

    println!("extractor  {}/{model}", extractor.provider);
    println!(
        "           key {key} {}",
        mark(key_set || extractor.provider == "openai-compatible")
    );

    match config::decider_settings(&config, None) {
        Some(decider) => {
            let key = spendyard_core::decide::key_variable(&decider.provider).unwrap_or("-");

            println!();
            println!(
                "decider    {}{}",
                decider.provider,
                decider
                    .model
                    .as_deref()
                    .map(|model| format!("/{model}"))
                    .unwrap_or_default()
            );
            println!("           key {key} {}", mark(decider.api_key.is_some()));

            if decider.provider.starts_with("clef") {
                println!(
                    "           CLOUDFLARE_ACCOUNT_ID {}",
                    mark(decider.account_id.is_some())
                );
            }

            println!("           threshold {}", config::threshold(&config));
        }

        None => {
            println!();
            println!("decider    none (every document goes to the extractor)");
        }
    }

    Ok(())
}

pub fn init() -> Result<()> {
    let home = config::home();

    let path = home.join("config.toml");

    if path.exists() {
        bail!(
            "{} already exists; edit it, or remove it first",
            path.display()
        );
    }

    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;

    std::fs::write(&path, TEMPLATE).with_context(|| format!("writing {}", path.display()))?;

    println!("wrote {}", path.display());

    Ok(())
}

fn mark(set: bool) -> &'static str {
    if set { "set" } else { "NOT SET" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_a_valid_config() {
        let parsed: Result<config::Config, _> = toml::from_str(TEMPLATE);

        assert!(parsed.is_ok(), "{parsed:?}");
    }
}
