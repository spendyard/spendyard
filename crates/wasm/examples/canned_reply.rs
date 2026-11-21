//! Writes tests/fixtures/anthropic-reply.json: what Anthropic answers for a
//! receipt (Vercel, 20 USD, a Visa ending 3776), in the extraction format the
//! core asks for. For hosts' tests of `understand` that don't call a model
//! (tests/understand.mjs here, the platform's). Run again when the format
//! changes: `cargo run -p spendyard-wasm --example canned_reply`.

use spendyard_core::extract::ModelOutput;
use spendyard_core::extract::schema::{
    Amount, DocumentType, Line, LineKind, Payment, PaymentMethod, Status, Vendor,
};

fn main() {
    let output = ModelOutput {
        document_type: DocumentType::Receipt,
        status: Status::Paid,
        confidence: 0.95,
        vendor: Vendor {
            name: "Vercel".to_string(),
            domain: Some("vercel.com".to_string()),
            ..Vendor::default()
        },
        payments: vec![Payment {
            method: PaymentMethod::Card,
            brand: Some("visa".to_string()),
            last4: Some("3776".to_string()),
            amount: Some("20.00".to_string()),
        }],
        amount: Amount {
            total: Some("20.00".to_string()),
            currency: Some("USD".to_string()),
            ..Amount::default()
        },
        lines: vec![Line {
            kind: LineKind::Seat,
            product: Some("Vercel Pro".to_string()),
            description: "Pro seat".to_string(),
            quantity: Some(1.0),
            amount: Some("20.00".to_string()),
            ..Line::default()
        }],
        receipt_number: Some("2291-0007".to_string()),
        issued: Some("2026-11-19".to_string()),
        ..ModelOutput::default()
    };

    let reply = serde_json::json!({
        "model": "claude-haiku-5-5",
        "stop_reason": "end_turn",
        "content": [{ "type": "text", "text": serde_json::to_string(&output).unwrap() }],
        "usage": { "input_tokens": 1200, "output_tokens": 300 },
    });

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/anthropic-reply.json"
    );

    std::fs::write(path, serde_json::to_string_pretty(&reply).unwrap() + "\n").unwrap();

    println!("wrote {path}");
}
