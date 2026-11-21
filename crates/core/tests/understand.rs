//! The whole pipeline on a fixture, with a fake model standing in for a
//! provider.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::json;
use spendyard_core::decide::{Answer, Decider, Decision, Image, Question};
use spendyard_core::extract::{
    Accepts, ExtractError, Extractor, OutputFormat, Prompt, Reply, Usage,
};
use spendyard_core::{Pipeline, read, understand};

/// Returns a fixed answer and remembers the prompt it was given.
struct Fake {
    answer: serde_json::Value,
    seen: std::sync::Mutex<Option<Prompt>>,
}

#[async_trait]
impl Extractor for Fake {
    fn name(&self) -> String {
        "fake/model".to_string()
    }

    fn accepts(&self) -> Accepts {
        Accepts::TEXT_ONLY
    }

    async fn answer(&self, prompt: &Prompt, _format: &OutputFormat) -> Result<Reply, ExtractError> {
        *self.seen.lock().unwrap() = Some(prompt.clone());

        let reply = Reply {
            text: self.answer.to_string(),
            usage: Usage {
                input: 100,
                output: 50,
            },
            model: "fake-1".to_string(),
        };

        Ok(reply)
    }
}

/// A first model with fixed answers.
struct FakeDecider {
    kind: (&'static str, f64),
    billing: (&'static str, f64),
    about_money: f64,
}

#[async_trait]
impl Decider for FakeDecider {
    fn name(&self) -> String {
        "fake-decider".to_string()
    }

    fn accepts_images(&self) -> bool {
        false
    }

    async fn decide(
        &self,
        _state: &serde_json::Value,
        questions: &[Question],
        _images: &[Image],
    ) -> Result<Decision, ExtractError> {
        assert!(
            questions
                .iter()
                .any(|question| question.name() == "document_type")
        );

        let choice = |(choice, probability): (&str, f64)| Answer::Choice {
            choice: choice.to_string(),
            probabilities: BTreeMap::from([(choice.to_string(), probability)]),
        };

        let mut decision = Decision::default();

        decision
            .answers
            .insert("document_type".to_string(), choice(self.kind));
        decision
            .answers
            .insert("billing_model".to_string(), choice(self.billing));

        decision.answers.insert(
            "about_money".to_string(),
            Answer::YesNo {
                yes: self.about_money,
            },
        );

        decision.model = "fake-decider-1".to_string();
        decision.usage = Usage {
            input: 900,
            output: 0,
        };

        Ok(decision)
    }
}

fn pipeline<'a>(extractor: &'a Fake, decider: Option<&'a FakeDecider>) -> Pipeline<'a> {
    Pipeline {
        decider: decider.map(|decider| decider as &dyn Decider),
        extractor,
        threshold: 0.85,
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));

    std::fs::read(path).unwrap()
}

/// What a good model should say about receipt.eml.
fn good_answer() -> serde_json::Value {
    json!({
        "document_type": "invoice",
        "status": "paid",
        "source_part": 2,
        "confidence": 0.95,
        "vendor": {
            "name": "Acme Cloud, Inc.",
            "domain": "acme.example",
            "processor": null,
            "account_email": "billing@fjordtech.example",
            "account_workspace": "Fjordtech AS",
            "account_customer_id": null
        },
        "payments": [{ "method": "card", "brand": "Visa", "last4": "4242", "amount": "112.40" }],
        "amount": { "total": "$112.40", "subtotal": "92.00", "tax": "20.40", "currency": "usd" },
        "billing": {
            "model": "subscription",
            "interval": "month",
            "period_start": "2026-10-19",
            "period_end": "2026-11-18",
            "next_renewal": null
        },
        "lines": [
            {
                "kind": "seat", "product": "Pro", "plan": "Pro", "description": "Pro (per seat)",
                "quantity": 4.0, "unit": "seat", "unit_price": "20.00", "amount": "80.00", "interval": "month"
            },
            {
                "kind": "usage", "product": null, "plan": null, "description": "Premium requests over quota",
                "quantity": 300.0, "unit": "request", "unit_price": "0.04", "amount": "12.00", "interval": null
            }
        ],
        "invoice_number": "A1B2C3D4-0007",
        "issued": "2026-11-19",
        "evidence": [
            { "field": "amount.total", "quote": "Amount paid\n$112.40", "part": 2 },
            { "field": "payments[0].last4", "quote": "Visa •••• 4242", "part": 2 },
            { "field": "invoice_number", "quote": "A1B2C3D4-0007", "part": 2 }
        ]
    })
}

#[tokio::test]
async fn a_good_answer_passes_every_check() {
    let document = read(&fixture("receipt.eml"), None).unwrap();

    let fake = Fake {
        answer: good_answer(),
        seen: Default::default(),
    };

    let result = understand(&document, &pipeline(&fake, None)).await.unwrap();

    assert_eq!(result.flags, [], "unexpected flags");

    assert!(result.document.is_receipt);
    assert!(result.document.is_invoice);
    assert_eq!(
        result.document.source.as_deref(),
        Some("attachment:Invoice-2291-0007.pdf")
    );

    let amount = result.amount.as_ref().unwrap();

    assert_eq!(amount.total.as_deref(), Some("112.40"));
    assert_eq!(amount.currency.as_deref(), Some("USD"));

    let billing = serde_json::to_value(&result.billing).unwrap();

    assert_eq!(billing["model"], "mixed", "seats and usage make it mixed");

    assert_eq!(
        result.vendor.as_ref().unwrap().id.as_deref(),
        Some("acme.example")
    );
    assert!(result.evidence.values().all(|evidence| evidence.checked));

    assert_eq!(result.meta.extractor.as_deref(), Some("fake/model"));
    assert_eq!(result.meta.model.as_deref(), Some("fake-1"));

    let prompt = fake.seen.lock().unwrap().clone().unwrap();

    assert!(prompt.warnings.is_empty());
}

#[tokio::test]
async fn invented_values_are_flagged() {
    let document = read(&fixture("receipt.eml"), None).unwrap();

    let mut answer = good_answer();

    // A total the document doesn't have, quoted from the real total.
    answer["amount"]["total"] = json!("121.40");

    // An invoice number and a quote that are both made up.
    answer["invoice_number"] = json!("INV-9999");
    answer["evidence"][2]["quote"] = json!("Invoice INV-9999");

    answer["payments"][0]["last4"] = json!("424");

    let fake = Fake {
        answer,
        seen: Default::default(),
    };

    let result = understand(&document, &pipeline(&fake, None)).await.unwrap();

    let codes: Vec<&str> = result.flags.iter().map(|flag| flag.code).collect();

    assert!(codes.contains(&"totals_dont_add_up"), "{codes:?}");
    assert!(codes.contains(&"quote_not_found"), "{codes:?}");
    assert!(codes.contains(&"invalid_last4"), "{codes:?}");
    assert!(codes.contains(&"quote_mismatch"), "{codes:?}");
}

#[tokio::test]
async fn a_scanned_pdf_has_nothing_for_a_text_only_model() {
    let document = read(&fixture("scanned.pdf"), Some("scanned.pdf")).unwrap();

    let fake = Fake {
        answer: good_answer(),
        seen: Default::default(),
    };

    let error = understand(&document, &pipeline(&fake, None))
        .await
        .unwrap_err();

    assert!(matches!(error, ExtractError::NothingToRead(_)));
}

#[tokio::test]
async fn a_sure_first_model_skips_the_extractor() {
    let document = read(&fixture("receipt.eml"), None).unwrap();

    let fake = Fake {
        answer: good_answer(),
        seen: Default::default(),
    };

    let decider = FakeDecider {
        kind: ("other", 0.97),
        billing: ("unknown", 0.9),
        about_money: 0.04,
    };

    let result = understand(&document, &pipeline(&fake, Some(&decider)))
        .await
        .unwrap();

    assert!(fake.seen.lock().unwrap().is_none(), "the extractor ran");
    assert!(!result.document.is_receipt);
    assert_eq!(result.meta.decider.as_deref(), Some("fake-decider"));
    assert!(result.meta.extractor.is_none());
}

#[tokio::test]
async fn an_unsure_first_model_hands_over_with_hints() {
    let document = read(&fixture("receipt.eml"), None).unwrap();

    let fake = Fake {
        answer: good_answer(),
        seen: Default::default(),
    };

    let decider = FakeDecider {
        kind: ("other", 0.6),
        billing: ("mixed", 0.9),
        about_money: 0.5,
    };

    let result = understand(&document, &pipeline(&fake, Some(&decider)))
        .await
        .unwrap();

    let prompt = fake
        .seen
        .lock()
        .unwrap()
        .clone()
        .expect("the extractor ran");

    let hints_sent = prompt.blocks.iter().any(|block| {
        matches!(block, spendyard_core::extract::Block::Text(text) if text.contains("<first-model>"))
    });

    assert!(hints_sent);
    assert!(result.document.is_receipt);
    assert!(result.decisions.is_some());
    assert_eq!(
        result.flags,
        [],
        "unsure about the type, and agrees on billing"
    );
}

#[tokio::test]
async fn a_sure_first_model_that_disagrees_is_flagged() {
    let document = read(&fixture("receipt.eml"), None).unwrap();

    let fake = Fake {
        answer: good_answer(),
        seen: Default::default(),
    };

    let decider = FakeDecider {
        kind: ("renewal_notice", 0.95),
        billing: ("subscription", 0.95),
        about_money: 0.9,
    };

    let result = understand(&document, &pipeline(&fake, Some(&decider)))
        .await
        .unwrap();

    let fields: Vec<&str> = result
        .flags
        .iter()
        .filter(|flag| flag.code == "models_disagree")
        .filter_map(|flag| flag.field.as_deref())
        .collect();

    assert_eq!(fields, ["document.type", "billing.model"]);
}
