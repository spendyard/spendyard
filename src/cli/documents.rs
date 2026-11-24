//! `spendyard documents get`: one document in the store, whole: where it came
//! from, how it was read and what was read, the corrections made to it by
//! hand, and the payments it's evidence of.

use anyhow::{Context, Result, bail};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::cli::payments;
use crate::cli::store::Store;

pub struct GetOptions {
    /// The ID `spendyard documents list` shows.
    pub id: i64,

    pub json: bool,
}

/// Where a document came from, as the store keeps it.
#[derive(Serialize)]
struct Document {
    id: i64,
    name: Option<String>,
    source: String,
    sender: Option<String>,
    received: Option<String>,
    message_id: Option<String>,
    format: Option<String>,
    size: Option<i64>,
    sha256: String,
    first_seen: String,
}

/// How it was last read.
#[derive(Serialize)]
struct Reading {
    created: String,
    decider: Option<String>,
    extractor: Option<String>,
    model: Option<String>,
    spendyard_version: Option<String>,

    #[serde(skip)]
    json: String,
}

/// What was corrected by hand (`spendyard documents edit`).
#[derive(Serialize)]
struct Edits {
    personal: bool,
    ignored: bool,
    vendor_name: Option<String>,
    note: Option<String>,
    date: Option<String>,
    kind: Option<String>,
    total: Option<String>,
    currency: Option<String>,
    number: Option<String>,
    seats: Option<f64>,
    updated: String,
}

impl Edits {
    /// Each correction in a few words, for the summary.
    fn described(&self) -> Vec<String> {
        let mut said = Vec::new();

        if self.personal {
            said.push("personal".to_string());
        }

        if self.ignored {
            said.push("ignored".to_string());
        }

        let corrections = [
            ("vendor", self.vendor_name.clone()),
            ("date", self.date.clone()),
            ("type", self.kind.clone()),
            ("total", self.total.clone()),
            ("currency", self.currency.clone()),
            ("number", self.number.clone()),
            ("seats", self.seats.map(|seats| seats.to_string())),
        ];

        for (field, value) in corrections {
            if let Some(value) = value {
                said.push(format!("{field} {value}"));
            }
        }

        if let Some(note) = &self.note {
            said.push(format!("note \"{note}\""));
        }

        said
    }
}

/// Everything `--json` prints.
#[derive(Serialize)]
struct Whole<'a> {
    document: &'a Document,
    reading: Option<&'a Reading>,
    result: Option<Value>,
    edits: Option<&'a Edits>,
    payments: &'a [i64],
}

/// Prints one stored document. Works out the payments again first, as
/// `spendyard payments` does, so the numbers shown are those it lists.
pub async fn get(options: GetOptions) -> Result<()> {
    let store = Store::open()?;

    let Some(document) = document(&store, options.id)? else {
        bail!(
            "no document {} in the store; `spendyard documents list` lists them",
            options.id
        );
    };

    let reading = last_reading(&store, options.id)?;
    let edits = edits(&store, options.id)?;

    payments::regroup(&store).await?;

    let payments = payments_of(&store, options.id)?;

    if options.json {
        let result = reading
            .as_ref()
            .map(|reading| serde_json::from_str(&reading.json))
            .transpose()?;

        let whole = Whole {
            document: &document,
            reading: reading.as_ref(),
            result,
            edits: edits.as_ref(),
            payments: &payments,
        };

        println!("{}", serde_json::to_string_pretty(&whole)?);

        return Ok(());
    }

    print_document(&document, reading.as_ref(), edits.as_ref(), &payments);

    let Some(reading) = reading else {
        println!();
        println!("Not read yet: `spendyard read` reads it again from the original");

        return Ok(());
    };

    let result: Value = serde_json::from_str(&reading.json)
        .with_context(|| format!("reading document {}'s stored result", options.id))?;

    println!();

    print_reading(&result);

    Ok(())
}

/// A field of the stored result as text, by its path: `amount.total`. None when it's missing or null.
fn text(result: &Value, path: &str) -> Option<String> {
    let mut value = result;

    for key in path.split('.') {
        value = value.get(key)?;
    }

    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// Words that are there, joined: missing ones left out.
fn joined(words: &[Option<String>], separator: &str) -> String {
    words
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(separator)
}

/// What was read, from the stored result as it was written: older results read as well as new ones.
fn print_reading(result: &Value) {
    let vendor = match (text(result, "vendor.name"), text(result, "vendor.id")) {
        (Some(name), Some(id)) => format!("{name} ({id})"),
        (Some(name), None) => name,
        _ => "Unknown vendor".to_string(),
    };

    let kind = joined(
        &[
            text(result, "document.type"),
            text(result, "document.status"),
        ],
        ", ",
    );

    println!("{vendor} — {kind}");

    if let Some(statement) = result
        .get("statement")
        .filter(|statement| !statement.is_null())
    {
        print_statement(statement);
    } else {
        print_bill(result);
    }

    print_flags(result);
}

/// A receipt's, invoice's or other bill's fields.
fn print_bill(result: &Value) {
    if let Some(issued) = text(result, "issued") {
        println!("  Issued    {issued}");
    }

    let total = joined(
        &[
            text(result, "amount.total"),
            text(result, "amount.currency"),
        ],
        " ",
    );

    if !total.is_empty() {
        let tax = text(result, "amount.tax")
            .map(|tax| format!(", tax {tax}"))
            .unwrap_or_default();

        println!("  Total     {total}{tax}");
    }

    let period = match (
        text(result, "billing.period.start"),
        text(result, "billing.period.end"),
    ) {
        (Some(start), Some(end)) => Some(format!("{start} → {end}")),
        _ => None,
    };

    let billing = joined(
        &[
            text(result, "billing.model"),
            text(result, "billing.interval"),
            period,
        ],
        ", ",
    );

    if !billing.is_empty() {
        println!("  Billing   {billing}");
    }

    let numbers = joined(
        &[
            text(result, "invoice_number").map(|number| format!("invoice {number}")),
            text(result, "receipt_number").map(|number| format!("receipt {number}")),
        ],
        " · ",
    );

    if !numbers.is_empty() {
        println!("  Numbers   {numbers}");
    }

    for payment in list(result, "payments") {
        let card = text(payment, "last4").map(|last4| format!("••{last4}"));

        let paid = joined(
            &[
                text(payment, "method"),
                text(payment, "brand"),
                card,
                text(payment, "amount"),
            ],
            " ",
        );

        println!("  Paid      {paid}");
    }

    let lines = list(result, "lines");

    if !lines.is_empty() {
        println!("  Lines");
    }

    for line in lines {
        let what = text(line, "description")
            .or_else(|| text(line, "product"))
            .unwrap_or_default();

        let quantity = text(line, "quantity")
            .map(|quantity| format!("{} × ", quantity.trim_end_matches(".0")))
            .unwrap_or_default();

        let kind = joined(&[text(line, "kind"), text(line, "interval")], ", ");
        let amount = text(line, "amount").unwrap_or_default();

        println!("    {quantity}{what}  {amount}  ({kind})");
    }
}

/// A card statement's own summary: its lines are `spendyard transactions list`'s.
fn print_statement(statement: &Value) {
    let issuer = joined(&[text(statement, "issuer"), text(statement, "number")], " ");
    let total = joined(
        &[text(statement, "total"), text(statement, "currency")],
        " ",
    );

    println!("  Statement {issuer}");

    if let Some(issued) = text(statement, "issued") {
        println!("  Issued    {issued}");
    }

    println!("  Total     {total}");

    for card in list(statement, "cards") {
        let charges = text(card, "charges").unwrap_or_default();
        let name = text(card, "name").unwrap_or_default();

        println!(
            "  Card      ••{} {name}: {charges} charges",
            text(card, "last4").unwrap_or_default()
        );
    }

    println!("  Its lines: `spendyard transactions list`");
}

fn print_flags(result: &Value) {
    for flag in list(result, "flags") {
        let message = text(flag, "message")
            .or_else(|| text(flag, "code"))
            .unwrap_or_default();

        println!("  ! {message}");
    }
}

/// A list in the stored result; empty when it's missing.
fn list<'a>(result: &'a Value, key: &str) -> &'a [Value] {
    result
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn print_document(
    document: &Document,
    reading: Option<&Reading>,
    edits: Option<&Edits>,
    payments: &[i64],
) {
    let name = document.name.as_deref().unwrap_or("(no name)");

    println!("Document {}  {name}", document.id);

    let from = [Some(document.source.as_str()), document.sender.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");

    println!("  From      {from}");

    if let Some(received) = &document.received {
        println!("  Received  {received}");
    }

    if let Some(reading) = reading {
        // `anthropic/claude-haiku-5-5` says the model already: only an extractor that doesn't gets it after.
        let by = match (reading.extractor.as_deref(), reading.model.as_deref()) {
            (Some(extractor), Some(model)) if !extractor.ends_with(model) => {
                format!("{extractor} ({model})")
            }
            (Some(extractor), _) => extractor.to_string(),
            (None, Some(model)) => model.to_string(),
            (None, None) => "an unknown model".to_string(),
        };

        let decider = reading
            .decider
            .as_deref()
            .map(|decider| format!(", first {decider}"))
            .unwrap_or_default();

        println!("  Read      {} by {by}{decider}", reading.created);
    }

    if let Some(edits) = edits {
        let described = edits.described();

        if !described.is_empty() {
            println!("  Edited    {}", described.join(" · "));
        }
    }

    if !payments.is_empty() {
        let numbers = payments
            .iter()
            .map(|payment| format!("#{payment}"))
            .collect::<Vec<_>>()
            .join(", ");

        println!("  Payments  {numbers}");
    }
}

fn document(store: &Store, id: i64) -> Result<Option<Document>> {
    let found = store
        .connection
        .query_row(
            "SELECT id, name, source, sender, received, message_id, format, size, sha256, first_seen
             FROM documents WHERE id = ?1",
            params![id],
            |row| {
                Ok(Document {
                    id: row.get("id")?,
                    name: row.get("name")?,
                    source: row.get("source")?,
                    sender: row.get("sender")?,
                    received: row.get("received")?,
                    message_id: row.get("message_id")?,
                    format: row.get("format")?,
                    size: row.get("size")?,
                    sha256: row.get("sha256")?,
                    first_seen: row.get("first_seen")?,
                })
            },
        )
        .optional()?;

    Ok(found)
}

fn last_reading(store: &Store, id: i64) -> Result<Option<Reading>> {
    let found = store
        .connection
        .query_row(
            "SELECT created, decider, extractor, model, spendyard_version, json
             FROM results WHERE document_id = ?1
             ORDER BY id DESC LIMIT 1",
            params![id],
            |row| {
                Ok(Reading {
                    created: row.get("created")?,
                    decider: row.get("decider")?,
                    extractor: row.get("extractor")?,
                    model: row.get("model")?,
                    spendyard_version: row.get("spendyard_version")?,
                    json: row.get("json")?,
                })
            },
        )
        .optional()?;

    Ok(found)
}

fn edits(store: &Store, id: i64) -> Result<Option<Edits>> {
    let found = store
        .connection
        .query_row(
            "SELECT personal, ignored, vendor_name, note, date, type, total, currency, number, seats, updated
             FROM edits WHERE document_id = ?1",
            params![id],
            |row| {
                Ok(Edits {
                    personal: row.get("personal")?,
                    ignored: row.get("ignored")?,
                    vendor_name: row.get("vendor_name")?,
                    note: row.get("note")?,
                    date: row.get("date")?,
                    kind: row.get("type")?,
                    total: row.get("total")?,
                    currency: row.get("currency")?,
                    number: row.get("number")?,
                    seats: row.get("seats")?,
                    updated: row.get("updated")?,
                })
            },
        )
        .optional()?;

    Ok(found)
}

/// The payments it's evidence of, as a receipt or invoice or as the statement a line is on.
fn payments_of(store: &Store, id: i64) -> Result<Vec<i64>> {
    let mut statement = store.connection.prepare(
        "SELECT DISTINCT payment_id FROM payment_evidence
         WHERE document_id = ?1 OR statement_id = ?1
         ORDER BY payment_id",
    )?;

    let ids = statement
        .query_map(params![id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;

    Ok(ids)
}
