//! The whole pipeline on a fixture, with a fake model standing in for a
//! provider.

use async_trait::async_trait;
use serde_json::json;
use spendyard::extract::{
    Accepts, ExtractError, Extraction, Extractor, ModelOutput, Prompt, Usage,
};
use spendyard::{read, understand};

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

    async fn extract(&self, prompt: &Prompt) -> Result<Extraction, ExtractError> {
        *self.seen.lock().unwrap() = Some(prompt.clone());

        let output: ModelOutput = serde_json::from_value(self.answer.clone()).unwrap();

        Ok(Extraction {
            output,
            usage: Usage {
                input: 100,
                output: 50,
            },
            model: "fake-1".to_string(),
        })
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

    let result = understand(&document, &fake).await.unwrap();

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

    assert_eq!(result.meta.extractor, "fake/model");
    assert_eq!(result.meta.model, "fake-1");

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

    let result = understand(&document, &fake).await.unwrap();

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

    let error = understand(&document, &fake).await.unwrap_err();

    assert!(matches!(error, ExtractError::NothingToRead(_)));
}
