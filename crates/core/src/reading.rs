//! A result as the contract types it, and back: `spendyard.api.v1.Reading`
//! (`proto/spendyard/api/v1/documents.proto`), in protobuf's JSON form, the
//! one way to show what was read across the CLI and the cloud.
//!
//! [`reading_of`] makes a `Reading` from a stored result, as JSON: what the
//! cloud's `GetDocument` gives and what `spendyard cloud push` sends.
//! Working from the JSON rather than [`ReadResult`] reads results stored by
//! any version, a field it doesn't have being only absent.
//!
//! [`result_of`] makes a [`ReadResult`] from a `Reading`: what the cloud
//! keeps of a pushed document, its rows made from it by its own core, as for
//! any document it reads itself.
//!
//! A card statement is a document whose reading has a `statement`: what the
//! statement says of itself (issuer, number, dates, currency, total, cards)
//! and its lines as `transactions`, which the cloud keeps as Transactions.
//!
//! Enums go by name: the core's `renewal_notice` is the contract's
//! `DOCUMENT_TYPE_RENEWAL_NOTICE`. One the other side doesn't know is
//! unspecified, or `other` where there is one.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::extract::Usage;
use crate::extract::schema::{
    Amount, BillingModel, DocumentType, Interval, Line, LineKind, Payment, PaymentMethod, Status,
};
use crate::result::{
    Account, Billing, DocumentSummary, Evidence, Flag, Meta, Period, ReadResult, Vendor,
};
use crate::statement::{Card, Charge, ChargeKind, Statement, StatementMeta};

// --- a result to a Reading -----------------------------------------------------------------------

/// A stored result, as JSON, as the contract's `Reading`, in protobuf's JSON
/// form. `read_at` is when it was read, RFC 3339: the store's, not the
/// result's.
pub fn reading_of(result: &Value, read_at: Option<&str>) -> Value {
    let mut reading = Map::new();

    let document = &result["document"];
    let document_type = text(document, "type");

    put(
        &mut reading,
        "documentType",
        enum_name("DOCUMENT_TYPE", document_type.as_deref(), Some("OTHER")),
    );

    put(&mut reading, "isReceipt", flag(document, "is_receipt"));
    put(&mut reading, "isInvoice", flag(document, "is_invoice"));
    put(
        &mut reading,
        "status",
        enum_name("PAYMENT_STATUS", text(document, "status").as_deref(), None),
    );
    put(
        &mut reading,
        "confidence",
        document.get("confidence").cloned(),
    );

    put(&mut reading, "vendor", vendor_of(&result["vendor"]));
    put(&mut reading, "amount", amount_of(&result["amount"]));
    put(&mut reading, "billing", billing_of(&result["billing"]));
    put(&mut reading, "lines", list_of(result, "lines", line_of));
    put(
        &mut reading,
        "payments",
        list_of(result, "payments", payment_of),
    );

    for (field, key) in [
        ("invoice_number", "invoiceNumber"),
        ("receipt_number", "receiptNumber"),
        ("credited_invoice", "creditedInvoice"),
        ("issued", "issued"),
    ] {
        put(&mut reading, key, text_value(result, field));
    }

    put(&mut reading, "evidence", evidence_of(&result["evidence"]));
    put(&mut reading, "flags", list_of(result, "flags", flag_of));
    put(&mut reading, "warnings", strings(&result["warnings"]));

    let meta = &result["meta"];

    put(&mut reading, "extractor", text_value(meta, "extractor"));
    put(&mut reading, "model", text_value(meta, "model"));
    put(&mut reading, "decider", text_value(meta, "decider"));
    put(
        &mut reading,
        "spendyardVersion",
        text_value(meta, "spendyard_version"),
    );
    put(&mut reading, "readAt", read_at.map(|at| json!(at)));

    put(
        &mut reading,
        "statement",
        statement_of(&result["statement"]),
    );

    Value::Object(reading)
}

fn statement_of(statement: &Value) -> Option<Value> {
    if !statement.is_object() {
        return None;
    }

    let mut out = Map::new();

    for (field, key) in [
        ("issuer", "issuer"),
        ("number", "number"),
        ("issued", "issued"),
        ("due", "due"),
        ("period_start", "periodStart"),
        ("period_end", "periodEnd"),
        ("currency", "currency"),
        ("total", "total"),
    ] {
        put(&mut out, key, text_value(statement, field));
    }

    put(
        &mut out,
        "lineCount",
        statement
            .get("line_count")
            .filter(|count| count.is_u64())
            .cloned(),
    );
    put(&mut out, "cards", list_of(statement, "cards", card_of));
    put(
        &mut out,
        "transactions",
        list_of(statement, "charges", transaction_of),
    );

    Some(Value::Object(out))
}

fn card_of(card: &Value) -> Value {
    let mut out = Map::new();

    for field in ["last4", "name", "brand"] {
        put(&mut out, field, text_value(card, field));
    }

    put(
        &mut out,
        "transactions",
        card.get("charges").filter(|count| count.is_u64()).cloned(),
    );

    Value::Object(out)
}

fn vendor_of(vendor: &Value) -> Option<Value> {
    if !vendor.is_object() {
        return None;
    }

    let account = &vendor["account"];
    let mut out = Map::new();

    for (field, key) in [
        ("id", "id"),
        ("name", "name"),
        ("legal_name", "legalName"),
        ("category", "category"),
        ("processor", "processor"),
    ] {
        put(&mut out, key, text_value(vendor, field));
    }

    put(&mut out, "known", flag(vendor, "known"));

    for (field, key) in [
        ("email", "accountEmail"),
        ("workspace", "accountWorkspace"),
        ("customer_id", "accountCustomerId"),
    ] {
        put(&mut out, key, text_value(account, field));
    }

    Some(Value::Object(out))
}

fn amount_of(amount: &Value) -> Option<Value> {
    if !amount.is_object() {
        return None;
    }

    let mut out = Map::new();

    for field in ["total", "subtotal", "tax", "currency"] {
        put(&mut out, field, text_value(amount, field));
    }

    Some(Value::Object(out))
}

fn billing_of(billing: &Value) -> Option<Value> {
    if !billing.is_object() {
        return None;
    }

    let mut out = Map::new();

    put(
        &mut out,
        "model",
        enum_name("BILLING_MODEL", text(billing, "model").as_deref(), None),
    );
    put(
        &mut out,
        "interval",
        enum_name(
            "INTERVAL",
            text(billing, "interval").as_deref(),
            Some("OTHER"),
        ),
    );
    put(
        &mut out,
        "periodStart",
        text_value(&billing["period"], "start"),
    );
    put(&mut out, "periodEnd", text_value(&billing["period"], "end"));
    put(&mut out, "nextRenewal", text_value(billing, "next_renewal"));

    Some(Value::Object(out))
}

fn line_of(line: &Value) -> Value {
    let mut out = Map::new();

    put(
        &mut out,
        "kind",
        enum_name("LINE_KIND", text(line, "kind").as_deref(), Some("OTHER")),
    );

    for (field, key) in [
        ("product", "product"),
        ("plan", "plan"),
        ("description", "description"),
        ("unit", "unit"),
        ("unit_price", "unitPrice"),
        ("amount", "amount"),
    ] {
        put(&mut out, key, text_value(line, field));
    }

    put(
        &mut out,
        "quantity",
        line.get("quantity").filter(|q| q.is_number()).cloned(),
    );
    put(
        &mut out,
        "interval",
        enum_name("INTERVAL", text(line, "interval").as_deref(), Some("OTHER")),
    );

    Value::Object(out)
}

fn payment_of(payment: &Value) -> Value {
    let mut out = Map::new();

    put(
        &mut out,
        "method",
        enum_name(
            "PAYMENT_METHOD",
            text(payment, "method").as_deref(),
            Some("OTHER"),
        ),
    );

    for field in ["brand", "last4", "amount"] {
        put(&mut out, field, text_value(payment, field));
    }

    Value::Object(out)
}

fn evidence_of(evidence: &Value) -> Option<Value> {
    let found = evidence.as_object()?;

    let out: Map<String, Value> = found
        .iter()
        .map(|(field, quote)| {
            let mut one = Map::new();

            put(&mut one, "quote", text_value(quote, "quote"));
            put(
                &mut one,
                "part",
                quote.get("part").filter(|p| p.is_u64()).cloned(),
            );
            put(&mut one, "checked", flag(quote, "checked"));

            (field.clone(), Value::Object(one))
        })
        .collect();

    Some(Value::Object(out))
}

fn flag_of(flag: &Value) -> Value {
    let mut out = Map::new();

    for field in ["code", "field", "message"] {
        put(&mut out, field, text_value(flag, field));
    }

    Value::Object(out)
}

fn transaction_of(charge: &Value) -> Value {
    let mut out = Map::new();

    put(
        &mut out,
        "row",
        charge.get("row").filter(|row| row.is_u64()).cloned(),
    );

    for (field, key) in [
        ("date", "date"),
        ("posted", "posted"),
        ("merchant", "merchant"),
        ("card_last4", "cardLast4"),
        ("name_on_card", "nameOnCard"),
        ("amount", "amount"),
        ("original_amount", "originalAmount"),
        ("original_currency", "originalCurrency"),
        ("rate", "rate"),
        ("kind", "kind"),
    ] {
        put(&mut out, key, text_value(charge, field));
    }

    Value::Object(out)
}

/// `DOCUMENT_TYPE` and `renewal_notice` as `DOCUMENT_TYPE_RENEWAL_NOTICE`;
/// None for none, or `fallback` for a name the contract may not have.
fn enum_name(prefix: &str, word: Option<&str>, fallback: Option<&str>) -> Option<Value> {
    let word = word?;

    let known = known_names(prefix).contains(&word.to_uppercase().as_str());

    let name = if known {
        word.to_uppercase()
    } else {
        fallback?.to_string()
    };

    Some(json!(format!("{prefix}_{name}")))
}

/// The names each enum of the contract has, after its prefix.
fn known_names(prefix: &str) -> &'static [&'static str] {
    match prefix {
        "DOCUMENT_TYPE" => &[
            "RECEIPT",
            "INVOICE",
            "RENEWAL_NOTICE",
            "PAYMENT_FAILED",
            "CARD_STATEMENT",
            "OTHER",
            "CREDIT_NOTE",
            "QUOTE",
            "TRIAL_ENDING",
        ],
        "PAYMENT_STATUS" => &["PAID", "DUE", "FAILED", "UPCOMING", "NONE"],
        "BILLING_MODEL" => &["SUBSCRIPTION", "USAGE", "MIXED", "ONE_TIME", "UNKNOWN"],
        "INTERVAL" => &["MONTH", "YEAR", "WEEK", "QUARTER", "OTHER"],
        "LINE_KIND" => &[
            "SEAT", "PLAN", "USAGE", "ADDON", "DISCOUNT", "CREDIT", "ONE_TIME", "OTHER",
        ],
        "PAYMENT_METHOD" => &[
            "CARD",
            "PAYPAL",
            "BANK_TRANSFER",
            "DIRECT_DEBIT",
            "APPLE_PAY",
            "GOOGLE_PAY",
            "CREDITS",
            "INVOICE",
            "OTHER",
        ],
        _ => &[],
    }
}

// --- a Reading to a result -----------------------------------------------------------------------

/// The contract's `Reading`, in protobuf's JSON form, as a [`ReadResult`]:
/// what the rows are made from. The models' token counts and timings aren't
/// in a `Reading`, so they're none.
pub fn result_of(reading: &Value) -> ReadResult {
    let document = DocumentSummary {
        kind: enum_word::<DocumentType>(reading, "documentType", "DOCUMENT_TYPE"),
        is_receipt: reading["isReceipt"].as_bool().unwrap_or(false),
        is_invoice: reading["isInvoice"].as_bool().unwrap_or(false),
        status: enum_word::<Status>(reading, "status", "PAYMENT_STATUS"),
        source: None,
        confidence: reading["confidence"].as_f64().unwrap_or(0.0),
    };

    let vendor = reading
        .get("vendor")
        .filter(|vendor| vendor.is_object())
        .map(vendor_from);

    let amount = reading
        .get("amount")
        .filter(|amount| amount.is_object())
        .map(|amount| Amount {
            total: text(amount, "total"),
            subtotal: text(amount, "subtotal"),
            tax: text(amount, "tax"),
            currency: text(amount, "currency"),
        });

    let billing = reading
        .get("billing")
        .filter(|billing| billing.is_object())
        .map(billing_from);

    let statement = reading
        .get("statement")
        .filter(|statement| statement.is_object())
        .map(|statement| statement_from(statement, reading));

    ReadResult {
        document,
        vendor,
        payments: values(&reading["payments"])
            .iter()
            .map(payment_from)
            .collect(),
        amount,
        billing,
        lines: values(&reading["lines"]).iter().map(line_from).collect(),
        invoice_number: text(reading, "invoiceNumber"),
        receipt_number: text(reading, "receiptNumber"),
        credited_invoice: text(reading, "creditedInvoice"),
        issued: text(reading, "issued"),
        evidence: evidence_from(&reading["evidence"]),
        flags: values(&reading["flags"]).iter().map(flag_from).collect(),
        warnings: strings_of(&reading["warnings"]),
        decisions: None,
        statement,
        meta: meta_from(reading),
    }
}

fn vendor_from(vendor: &Value) -> Vendor {
    Vendor {
        id: text(vendor, "id"),
        name: text(vendor, "name").unwrap_or_default(),
        legal_name: text(vendor, "legalName"),
        category: text(vendor, "category"),
        known: vendor["known"].as_bool().unwrap_or(false),
        processor: text(vendor, "processor"),
        account: Account {
            email: text(vendor, "accountEmail"),
            workspace: text(vendor, "accountWorkspace"),
            customer_id: text(vendor, "accountCustomerId"),
        },
    }
}

fn billing_from(billing: &Value) -> Billing {
    let start = text(billing, "periodStart");
    let end = text(billing, "periodEnd");

    let period = (start.is_some() || end.is_some()).then_some(Period { start, end });

    Billing {
        model: enum_word::<BillingModel>(billing, "model", "BILLING_MODEL"),
        interval: optional_word::<Interval>(billing, "interval", "INTERVAL"),
        period,
        next_renewal: text(billing, "nextRenewal"),
    }
}

fn line_from(line: &Value) -> Line {
    Line {
        kind: enum_word::<LineKind>(line, "kind", "LINE_KIND"),
        product: text(line, "product"),
        plan: text(line, "plan"),
        description: text(line, "description").unwrap_or_default(),
        quantity: line["quantity"].as_f64(),
        unit: text(line, "unit"),
        unit_price: text(line, "unitPrice"),
        amount: text(line, "amount"),
        interval: optional_word::<Interval>(line, "interval", "INTERVAL"),
    }
}

fn payment_from(payment: &Value) -> Payment {
    Payment {
        method: enum_word::<PaymentMethod>(payment, "method", "PAYMENT_METHOD"),
        brand: text(payment, "brand"),
        last4: text(payment, "last4"),
        amount: text(payment, "amount"),
    }
}

fn evidence_from(evidence: &Value) -> BTreeMap<String, Evidence> {
    let Some(found) = evidence.as_object() else {
        return BTreeMap::new();
    };

    found
        .iter()
        .map(|(field, quote)| {
            let one = Evidence {
                quote: text(quote, "quote").unwrap_or_default(),
                part: quote["part"].as_u64().unwrap_or(0) as u32,
                checked: quote["checked"].as_bool().unwrap_or(false),
            };

            (field.clone(), one)
        })
        .collect()
}

fn flag_from(flag: &Value) -> Flag {
    Flag {
        code: text(flag, "code").unwrap_or_default(),
        field: text(flag, "field"),
        message: text(flag, "message").unwrap_or_default(),
    }
}

fn meta_from(reading: &Value) -> Meta {
    let version = text(reading, "spendyardVersion").unwrap_or_default();

    Meta {
        spendyard_version: version,
        schema_version: crate::SCHEMA_VERSION.to_string(),
        decider: text(reading, "decider"),
        decider_tokens: None,
        extractor: text(reading, "extractor"),
        model: text(reading, "model"),
        tokens: Usage::default(),
        ms: 0,
    }
}

/// What a statement says of itself, its cards and its lines. Its checks
/// aren't in a `Reading`: they're the reader's, done when it was read.
fn statement_from(statement: &Value, reading: &Value) -> Statement {
    let charges: Vec<Charge> = values(&statement["transactions"])
        .iter()
        .map(charge_from)
        .collect();

    Statement {
        issuer: text(statement, "issuer"),
        number: text(statement, "number"),
        issued: text(statement, "issued"),
        due: text(statement, "due"),
        period_start: text(statement, "periodStart"),
        period_end: text(statement, "periodEnd"),
        currency: text(statement, "currency"),
        total: text(statement, "total"),
        line_count: statement["lineCount"].as_u64().map(|count| count as u32),
        cards: values(&statement["cards"]).iter().map(card_from).collect(),
        charges,
        checks: Vec::new(),
        warnings: Vec::new(),
        meta: StatementMeta {
            spendyard_version: text(reading, "spendyardVersion").unwrap_or_default(),
            extractor: text(reading, "extractor").unwrap_or_default(),
            models: text(reading, "model").into_iter().collect(),
            tokens: Usage::default(),
            calls: 0,
            rereads: 0,
            ms: 0,
        },
    }
}

fn card_from(card: &Value) -> Card {
    Card {
        last4: text(card, "last4").unwrap_or_default(),
        name: text(card, "name"),
        brand: text(card, "brand"),
        charges: card["transactions"].as_u64().unwrap_or(0) as usize,
    }
}

fn charge_from(line: &Value) -> Charge {
    let kind: ChargeKind = text(line, "kind")
        .and_then(|kind| serde_json::from_value(json!(kind)).ok())
        .unwrap_or_default();

    Charge {
        row: line["row"].as_u64().map(|row| row as u32),
        date: text(line, "date").unwrap_or_default(),
        posted: text(line, "posted"),
        merchant: text(line, "merchant").unwrap_or_default(),
        card_last4: text(line, "cardLast4"),
        name_on_card: text(line, "nameOnCard"),
        amount: text(line, "amount").unwrap_or_default(),
        original_amount: text(line, "originalAmount"),
        original_currency: text(line, "originalCurrency"),
        rate: text(line, "rate"),
        kind,
    }
}

/// An enum of the core's from the contract's name for it: the prefix off,
/// lower case. Its default when there's none, or the core doesn't know it.
fn enum_word<T: DeserializeOwned + Default>(value: &Value, key: &str, prefix: &str) -> T {
    optional_word(value, key, prefix).unwrap_or_default()
}

/// As [`enum_word`], None when there's none (or it's unspecified).
fn optional_word<T: DeserializeOwned>(value: &Value, key: &str, prefix: &str) -> Option<T> {
    let name = text(value, key)?;
    let word = name.strip_prefix(prefix)?.strip_prefix('_')?.to_lowercase();

    if word == "unspecified" {
        return None;
    }

    serde_json::from_value(json!(word)).ok()
}

// --- JSON, both ways -----------------------------------------------------------------------------

/// A field's text: None when it's missing, null or empty.
fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn text_value(value: &Value, key: &str) -> Option<Value> {
    text(value, key).map(Value::String)
}

/// A field's boolean when it's true: protobuf's JSON leaves out a false one.
fn flag(value: &Value, key: &str) -> Option<Value> {
    value
        .get(key)
        .filter(|on| on.as_bool() == Some(true))
        .cloned()
}

fn values(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn strings_of(value: &Value) -> Vec<String> {
    values(value)
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn strings(value: &Value) -> Option<Value> {
    let found = strings_of(value);

    (!found.is_empty()).then(|| json!(found))
}

fn list_of(value: &Value, key: &str, each: fn(&Value) -> Value) -> Option<Value> {
    list_values(&value[key], each)
}

fn list_values(list: &Value, each: fn(&Value) -> Value) -> Option<Value> {
    let made: Vec<Value> = values(list).iter().map(each).collect();

    (!made.is_empty()).then_some(Value::Array(made))
}

/// Sets `key` when there's something to set: protobuf's JSON leaves out what's empty.
fn put(out: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        out.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::not_about_money;
    use crate::rows::rows;
    use crate::statement::{ChargeOutput, StatementOutput, assemble};

    /// A receipt with something in every part a `Reading` carries.
    fn receipt() -> ReadResult {
        let mut result = not_about_money(DocumentType::CreditNote, 0.92);

        result.document.is_invoice = true;
        result.document.status = Status::Paid;

        result.vendor = Some(Vendor {
            id: Some("slack.com".to_string()),
            name: "Slack".to_string(),
            legal_name: Some("Slack Technologies Limited".to_string()),
            category: Some("communication".to_string()),
            known: true,
            processor: None,
            account: Account {
                email: Some("ada@fjordtech.no".to_string()),
                workspace: Some("fjordtech".to_string()),
                customer_id: None,
            },
        });

        result.amount = Some(Amount {
            total: Some("-145.30".to_string()),
            subtotal: None,
            tax: Some("0.00".to_string()),
            currency: Some("EUR".to_string()),
        });

        result.billing = Some(Billing {
            model: BillingModel::Subscription,
            interval: Some(Interval::Month),
            period: Some(Period {
                start: Some("2026-06-17".to_string()),
                end: Some("2026-07-16".to_string()),
            }),
            next_renewal: None,
        });

        result.lines = vec![Line {
            kind: LineKind::Seat,
            product: Some("Slack".to_string()),
            plan: Some("Pro".to_string()),
            description: "Pro monthly, 18 seats".to_string(),
            quantity: Some(18.0),
            unit: Some("seat".to_string()),
            unit_price: Some("8.25".to_string()),
            amount: Some("148.50".to_string()),
            interval: Some(Interval::Month),
        }];

        result.payments = vec![Payment {
            method: PaymentMethod::Card,
            brand: Some("mastercard".to_string()),
            last4: Some("8239".to_string()),
            amount: Some("145.30".to_string()),
        }];

        result.invoice_number = Some("SBIE 4181730".to_string());
        result.credited_invoice = Some("SBIE 4100000".to_string());
        result.issued = Some("2026-07-03".to_string());

        result.evidence.insert(
            "amount.total".to_string(),
            Evidence {
                quote: "€145.30".to_string(),
                part: 0,
                checked: true,
            },
        );

        result.flags = vec![Flag {
            code: "lines_dont_add_up".to_string(),
            field: Some("lines".to_string()),
            message: "the lines add up to 148.50, not 145.30".to_string(),
        }];

        result.meta.extractor = Some("anthropic/claude-haiku-5-5".to_string());
        result.meta.model = Some("claude-haiku-5-5".to_string());

        result
    }

    /// The result as a store keeps it, as a `Reading`, and back.
    fn round_trip(result: &ReadResult) -> (Value, ReadResult) {
        let stored = serde_json::to_value(result).unwrap();
        let reading = reading_of(&stored, Some("2026-11-24T10:00:00Z"));

        (reading.clone(), result_of(&reading))
    }

    #[test]
    fn a_receipt_makes_the_same_rows_after_going_through_a_reading() {
        let result = receipt();
        let (reading, back) = round_trip(&result);

        assert_eq!(
            rows(&back, Some("2026-07-03")),
            rows(&result, Some("2026-07-03"))
        );

        assert_eq!(back.flags, result.flags);
        assert_eq!(back.evidence["amount.total"].quote, "€145.30");

        // The contract's names, in protobuf's JSON form.
        assert_eq!(reading["documentType"], "DOCUMENT_TYPE_CREDIT_NOTE");
        assert_eq!(reading["status"], "PAYMENT_STATUS_PAID");
        assert_eq!(reading["billing"]["interval"], "INTERVAL_MONTH");
        assert_eq!(reading["lines"][0]["unitPrice"], "8.25");
        assert_eq!(reading["vendor"]["accountWorkspace"], "fjordtech");
        assert_eq!(reading["readAt"], "2026-11-24T10:00:00Z");
        assert!(reading.get("statement").is_none());
    }

    #[test]
    fn a_statement_makes_the_same_transactions_after_going_through_a_reading() {
        let mut result = not_about_money(DocumentType::CardStatement, 0.85);

        let line = |row: u32, merchant: &str, amount: &str| ChargeOutput {
            row: Some(row),
            date: "2026-04-14".to_string(),
            merchant: merchant.to_string(),
            card_last4: Some("3776".to_string()),
            amount: amount.to_string(),
            ..ChargeOutput::default()
        };

        result.statement = Some(assemble(vec![StatementOutput {
            is_card_statement: true,
            issuer: Some("DNB Corporate Mastercard".to_string()),
            number: Some("706366374".to_string()),
            currency: Some("NOK".to_string()),
            total: Some("2192.07".to_string()),
            charges: vec![
                line(1, "VERCEL INC., COVINA, US", "192.07"),
                line(2, "SLACK T0123, DUBLIN, IE", "2000.00"),
            ],
            ..StatementOutput::default()
        }]));

        let (reading, back) = round_trip(&result);

        assert_eq!(rows(&back, None), rows(&result, None));

        let statement = &reading["statement"];

        assert_eq!(statement["issuer"], "DNB Corporate Mastercard");
        assert_eq!(statement["currency"], "NOK");
        assert_eq!(
            statement["transactions"][1]["merchant"],
            "SLACK T0123, DUBLIN, IE"
        );
        assert_eq!(statement["cards"][0]["last4"], "3776");
        assert_eq!(
            back.statement.unwrap().cards,
            result.statement.unwrap().cards
        );
    }

    #[test]
    fn names_neither_side_knows_are_unspecified_or_other() {
        let stored = json!({
            "document": { "type": "something_new", "status": "maybe" },
            "lines": [{ "kind": "mystery", "description": "?" }],
        });

        let reading = reading_of(&stored, None);
        let back = result_of(&json!({ "documentType": "DOCUMENT_TYPE_FROM_THE_FUTURE" }));

        assert_eq!(reading["documentType"], "DOCUMENT_TYPE_OTHER");
        assert!(reading.get("status").is_none());
        assert_eq!(reading["lines"][0]["kind"], "LINE_KIND_OTHER");
        assert_eq!(back.document.kind, DocumentType::Other);
    }
}
