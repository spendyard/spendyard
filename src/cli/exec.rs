//! The `exec` provider: any command can be the extractor or the first
//! model. Spendyard writes the request as JSON to its standard input and
//! reads the answer as JSON from its standard output.
//!
//! This lives in the CLI, not the library, because it starts processes.
//!
//! Extractor request:
//!
//! ```json
//! { "kind": "extract", "format": "receipt", "system": "…", "schema": { … },
//!   "blocks": [ { "type": "text", "text": "…" },
//!               { "type": "pdf", "label": "…", "base64": "…" },
//!               { "type": "image", "label": "…", "mime": "image/png", "base64": "…" } ] }
//! ```
//!
//! `format` is `receipt` for a receipt or invoice and `card_statement` for
//! a page range of a card statement; the schema says what to fill in.
//!
//! Answer: the JSON the schema describes, or
//! `{ "output": { … }, "usage": { "input": 0, "output": 0 }, "model": "…" }`.
//!
//! First-model request: `{ "kind": "decide", "state": …, "questions": { … },
//! "images": [ … ] }`, in the JEV format. Answer: `{ "answers": { … },
//! "usage": { "input_tokens": 0, "output_tokens": 0 }, "model": "…" }`.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Map, Value, json};
use spendyard_core::decide::{Decider, Decision, Image, Question};
use spendyard_core::extract::{
    Accepts, Block, ExtractError, Extractor, OutputFormat, Prompt, Reply, Usage,
};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Longest a command may take.
const TIMEOUT: Duration = Duration::from_secs(300);

const IMAGES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/heic",
];

pub struct Exec {
    command: Vec<String>,
}

impl Exec {
    pub fn new(command: Option<Vec<String>>, role: &str) -> Result<Self, ExtractError> {
        let command = command.filter(|command| !command.is_empty()).ok_or_else(|| {
            ExtractError::Config(format!(
                "the exec provider needs a command: command = [\"program\", \"arg\"] under [{role}] in the config"
            ))
        })?;

        Ok(Exec { command })
    }

    fn label(&self) -> String {
        let program = std::path::Path::new(&self.command[0])
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.command[0].clone());

        format!("exec/{program}")
    }

    /// Runs the command with `request` on stdin and parses its stdout.
    async fn call(&self, request: &Value) -> Result<Value, ExtractError> {
        let mut child = Command::new(&self.command[0])
            .args(&self.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                ExtractError::Config(format!("can't run {}: {error}", self.command[0]))
            })?;

        let input =
            serde_json::to_vec(request).map_err(|error| ExtractError::Http(error.to_string()))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&input).await.map_err(|error| {
                ExtractError::Http(format!("writing to {}: {error}", self.command[0]))
            })?;
        }

        let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| {
                ExtractError::Http(format!("{} took longer than {TIMEOUT:?}", self.command[0]))
            })?
            .map_err(|error| ExtractError::Http(error.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            let tail: String = stderr
                .trim()
                .chars()
                .rev()
                .take(500)
                .collect::<String>()
                .chars()
                .rev()
                .collect();

            let error = ExtractError::Api {
                status: output.status.code().unwrap_or(-1).clamp(0, 999) as u16,
                message: format!("{} failed: {tail}", self.command[0]),
            };

            return Err(error);
        }

        serde_json::from_slice(&output.stdout).map_err(|error| {
            ExtractError::InvalidOutput(format!("{} didn't print JSON: {error}", self.command[0]))
        })
    }
}

fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn block_json(block: &Block) -> Value {
    match block {
        Block::Text(text) => json!({ "type": "text", "text": text }),

        Block::Pdf { label, bytes } => {
            json!({ "type": "pdf", "label": label, "base64": encode(bytes) })
        }

        Block::Image { label, mime, bytes } => json!({
            "type": "image",
            "label": label,
            "mime": mime,
            "base64": encode(bytes),
        }),
    }
}

#[async_trait]
impl Extractor for Exec {
    fn name(&self) -> String {
        self.label()
    }

    fn accepts(&self) -> Accepts {
        Accepts {
            pdf: true,
            images: IMAGES,
        }
    }

    async fn answer(&self, prompt: &Prompt, format: &OutputFormat) -> Result<Reply, ExtractError> {
        let blocks: Vec<Value> = prompt.blocks.iter().map(block_json).collect();

        let request = json!({
            "kind": "extract",
            "format": format.name,
            "system": prompt.system,
            "schema": format.schema,
            "blocks": blocks,
        });

        let answer = self.call(&request).await?;

        // Either the output itself, or wrapped with usage and model.
        let wrapped = answer.get("output").is_some();

        let output_json = if wrapped {
            answer["output"].clone()
        } else {
            answer.clone()
        };

        let usage = Usage {
            input: answer
                .pointer("/usage/input")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: answer
                .pointer("/usage/output")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        };

        let model = answer["model"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| self.label());

        let reply = Reply {
            text: output_json.to_string(),
            usage,
            model,
        };

        Ok(reply)
    }
}

#[async_trait]
impl Decider for Exec {
    fn name(&self) -> String {
        self.label()
    }

    fn accepts_images(&self) -> bool {
        true
    }

    async fn decide(
        &self,
        state: &Value,
        questions: &[Question],
        images: &[Image],
    ) -> Result<Decision, ExtractError> {
        let questions: Map<String, Value> = questions
            .iter()
            .map(|question| (question.name().to_string(), question.to_json()))
            .collect();

        let images: Vec<Value> = images
            .iter()
            .map(|image| json!({ "content_type": image.mime, "base64": encode(&image.bytes) }))
            .collect();

        let request = json!({
            "kind": "decide",
            "state": state,
            "questions": questions,
            "images": images,
        });

        let answer = self.call(&request).await?;

        spendyard_core::decide::parse_answers(&answer, &self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Exec {
        Exec::new(
            Some(vec!["sh".to_string(), "-c".to_string(), script.to_string()]),
            "extractor",
        )
        .unwrap()
    }

    #[test]
    fn needs_a_command() {
        assert!(Exec::new(None, "extractor").is_err());
        assert!(Exec::new(Some(Vec::new()), "decider").is_err());
    }

    #[tokio::test]
    async fn extracts_through_a_command() {
        let output =
            serde_json::to_string(&spendyard_core::extract::ModelOutput::default()).unwrap();

        // Reads the request, checks it, and prints a wrapped answer.
        let script = format!(
            "grep -q '\"kind\":\"extract\"' && echo '{{\"output\": {output}, \"usage\": {{\"input\": 5, \"output\": 2}}, \"model\": \"tiny\"}}'"
        );

        let exec = shell(&script);

        let prompt = Prompt {
            system: "instructions",
            blocks: vec![Block::Text("hello".to_string())],
            warnings: Vec::new(),
        };

        let extraction = exec.extract(&prompt).await.unwrap();

        assert_eq!(extraction.model, "tiny");
        assert_eq!(
            extraction.usage,
            Usage {
                input: 5,
                output: 2
            }
        );
    }

    #[tokio::test]
    async fn decides_through_a_command() {
        let exec = shell(
            "cat > /dev/null; echo '{\"answers\": {\"about_money\": {\"type\": \"noul\", \"noul\": 0.1}}, \"model\": \"rules\"}'",
        );

        let decision = exec.decide(&json!({}), &[], &[]).await.unwrap();

        assert_eq!(decision.yes("about_money"), Some(0.1));
        assert_eq!(decision.model, "rules");
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_stderr() {
        let exec = shell("cat > /dev/null; echo 'no model loaded' >&2; exit 3");

        let error = exec.decide(&json!({}), &[], &[]).await.unwrap_err();

        assert!(error.to_string().contains("no model loaded"), "{error}");
    }
}
