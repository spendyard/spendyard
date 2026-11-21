//! One client for every model that speaks the JEV request format:
//! `{ model, state, questions }` in, `{ model, answers, usage }` out.
//!
//! - `jev`: TypeSafe's API (`https://api.typesafe.ai`), or Codiv's OpenJev
//!   with `base_url = "https://api.codiv.ai"` and `model = "openjev-latest"`.
//! - `clef`, `clef-flash`: Cloudflare Workers AI over REST. Takes images.
//! - `systemone`: a self-hosted server (SGLang, llama.cpp) at `base_url`.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Map, Value, json};

use spendyard_core::decide::parse_answers as parse_response;
use spendyard_core::extract::ExtractError;

use super::{Decider, Decision, Image, Question, Settings};
use crate::extract::http;

const TYPESAFE_URL: &str = "https://api.typesafe.ai";
const DEFAULT_JEV_MODEL: &str = "jev-latest";

const CLOUDFLARE_URL: &str = "https://api.cloudflare.com/client/v4";

pub struct SystemOne {
    transport: Arc<dyn http::Transport>,
    provider: String,
    url: String,
    key: Option<String>,
    model: String,
    images: bool,
}

impl SystemOne {
    pub fn new(
        settings: Settings,
        transport: Arc<dyn http::Transport>,
    ) -> Result<Self, ExtractError> {
        let key = settings.api_key.clone().filter(|key| !key.is_empty());

        let missing_key = || {
            let variable = super::key_variable(&settings.provider).unwrap_or("the API key");

            ExtractError::MissingKey(variable.to_string())
        };

        let (url, model, images) = match settings.provider.as_str() {
            "jev" => {
                key.as_ref().ok_or_else(missing_key)?;

                let base = settings.base_url.as_deref().unwrap_or(TYPESAFE_URL);

                let model = settings
                    .model
                    .clone()
                    .unwrap_or_else(|| DEFAULT_JEV_MODEL.to_string());

                (
                    format!("{}/v1/systemone", base.trim_end_matches('/')),
                    model,
                    false,
                )
            }

            "clef" | "clef-flash" => {
                key.as_ref().ok_or_else(missing_key)?;

                let account = settings
                    .account_id
                    .clone()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| ExtractError::MissingKey("CLOUDFLARE_ACCOUNT_ID".to_string()))?;

                let model = settings.provider.clone();

                let base = settings.base_url.as_deref().unwrap_or(CLOUDFLARE_URL);

                let url = format!(
                    "{}/accounts/{account}/ai/run/@cf/cloudflare/{model}",
                    base.trim_end_matches('/')
                );

                (url, model, true)
            }

            "systemone" => {
                let base = settings.base_url.clone().ok_or_else(|| {
                    ExtractError::Config(
                        "systemone needs base_url under [providers.systemone] in the config"
                            .to_string(),
                    )
                })?;

                let model = settings.model.clone().ok_or_else(|| {
                    ExtractError::Config(
                        "systemone needs a model: --decider systemone/<model>".to_string(),
                    )
                })?;

                (
                    format!("{}/v1/systemone", base.trim_end_matches('/')),
                    model,
                    false,
                )
            }

            other => {
                let error = ExtractError::Config(format!(
                    "unknown decider {other:?}; use jev, clef, clef-flash or systemone"
                ));

                return Err(error);
            }
        };

        let system_one = SystemOne {
            transport,
            provider: settings.provider,
            url,
            key,
            model,
            images,
        };

        Ok(system_one)
    }

    fn headers(&self) -> http::Headers {
        let mut headers = http::Headers::new();

        if let Some(key) = &self.key {
            headers.push(("authorization".to_string(), format!("Bearer {key}")));
        }

        headers
    }

    fn body(&self, state: &Value, questions: &[Question], images: &[Image]) -> Value {
        let questions: Map<String, Value> = questions
            .iter()
            .map(|question| (question.name().to_string(), question.to_json()))
            .collect();

        let mut body = json!({
            "model": self.model,
            "state": state,
            "questions": questions,
        });

        if self.images && !images.is_empty() {
            let encoded: Vec<Value> = images
                .iter()
                .map(|image| {
                    json!({
                        "content_type": image.mime,
                        "base64": base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                    })
                })
                .collect();

            body["images"] = Value::Array(encoded);
        }

        body
    }
}

#[async_trait]
impl Decider for SystemOne {
    fn name(&self) -> String {
        if self.provider == self.model {
            return self.provider.clone();
        }

        format!("{}/{}", self.provider, self.model)
    }

    fn accepts_images(&self) -> bool {
        self.images
    }

    async fn decide(
        &self,
        state: &Value,
        questions: &[Question],
        images: &[Image],
    ) -> Result<Decision, ExtractError> {
        let body = self.body(state, questions, images);

        let response = self
            .transport
            .post_json(&self.url, &self.headers(), &body)
            .await?;

        parse_response(&response, &self.model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(provider: &str) -> Settings {
        Settings {
            provider: provider.to_string(),
            api_key: Some("key".to_string()),
            account_id: Some("acct".to_string()),
            ..Settings::default()
        }
    }

    #[test]
    fn urls_per_provider() {
        let jev = SystemOne::new(settings("jev"), http::nowhere()).unwrap();

        assert_eq!(jev.url, "https://api.typesafe.ai/v1/systemone");
        assert_eq!(jev.model, "jev-latest");
        assert_eq!(
            http::header(&jev.headers(), "authorization").unwrap(),
            "Bearer key"
        );

        let codiv = SystemOne::new(
            Settings {
                base_url: Some("https://api.codiv.ai".to_string()),
                model: Some("openjev-latest".to_string()),
                ..settings("jev")
            },
            http::nowhere(),
        )
        .unwrap();

        assert_eq!(codiv.url, "https://api.codiv.ai/v1/systemone");
        assert_eq!(codiv.name(), "jev/openjev-latest");

        let clef = SystemOne::new(settings("clef-flash"), http::nowhere()).unwrap();

        assert_eq!(
            clef.url,
            "https://api.cloudflare.com/client/v4/accounts/acct/ai/run/@cf/cloudflare/clef-flash"
        );
        assert_eq!(clef.name(), "clef-flash");
        assert!(clef.accepts_images());
    }

    #[test]
    fn keys_and_accounts_are_required() {
        let no_key = SystemOne::new(
            Settings {
                api_key: None,
                ..settings("jev")
            },
            http::nowhere(),
        );

        assert!(matches!(no_key, Err(ExtractError::MissingKey(_))));

        let no_account = SystemOne::new(
            Settings {
                account_id: None,
                ..settings("clef")
            },
            http::nowhere(),
        );

        assert!(matches!(no_account, Err(ExtractError::MissingKey(_))));
    }

    #[test]
    fn body_has_model_state_questions_and_images_for_clef() {
        let clef = SystemOne::new(settings("clef-flash"), http::nowhere()).unwrap();

        let questions = vec![Question::YesNo {
            name: "paid_by_card",
            instructions: "Card?",
            yes: "yes",
            no: "no",
        }];

        let images = vec![Image {
            mime: "image/png".to_string(),
            bytes: vec![1, 2, 3],
        }];

        let body = clef.body(&json!({ "subject": "Receipt" }), &questions, &images);

        assert_eq!(body["model"], "clef-flash");
        assert_eq!(body["questions"]["paid_by_card"]["type"], "noul");
        assert_eq!(body["images"][0]["content_type"], "image/png");

        let jev = SystemOne::new(settings("jev"), http::nowhere()).unwrap();

        assert!(
            jev.body(&json!({}), &questions, &images)
                .get("images")
                .is_none()
        );
    }

    #[test]
    fn reads_answers_wrapped_or_not() {
        let answers = json!({
            "model": "clef-flash",
            "answers": {
                "document_type": {
                    "type": "choice",
                    "choice": "invoice",
                    "confidence": 0.9,
                    "probabilities": { "invoice": 0.9, "other": 0.1 },
                },
                "paid_by_card": { "type": "noul", "noul": 0.97 },
            },
            "usage": { "input_tokens": 1500, "output_tokens": 0 },
        });

        let bare = parse_response(&answers, "x").unwrap();

        assert_eq!(bare.choice("document_type"), Some(("invoice", 0.9)));
        assert_eq!(bare.yes("paid_by_card"), Some(0.97));
        assert_eq!(bare.usage.input, 1500);

        let wrapped = parse_response(&json!({ "result": answers, "success": true }), "x").unwrap();

        assert_eq!(wrapped.model, "clef-flash");
    }
}
