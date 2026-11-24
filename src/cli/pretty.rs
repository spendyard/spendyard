//! Human-readable summaries for `--pretty`.

use serde::Serialize;
use spendyard_core::ReadResult;
use spendyard_core::statement::{ChargeKind, Statement};

/// A serde enum as the word it serializes to: `BillingModel::OneTime` →
/// `one_time`.
fn word<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

pub fn print_result(name: &str, result: &ReadResult) {
    let document = &result.document;

    let source = document
        .source
        .as_deref()
        .map(|source| format!("  (from {source})"))
        .unwrap_or_default();

    println!(
        "{name} → {}, {}{source}",
        word(&document.kind),
        word(&document.status)
    );

    if let Some(statement) = &result.statement {
        print_statement(statement);

        return;
    }

    if let Some(vendor) = &result.vendor {
        let mut text = vendor.name.clone();

        if let Some(id) = &vendor.id {
            text.push_str(&format!(" ({id})"));
        }

        if let Some(processor) = &vendor.processor {
            text.push_str(&format!(", via {processor}"));
        }

        row("Vendor", &text);
    }

    if let Some(amount) = &result.amount
        && let Some(total) = &amount.total
    {
        let currency = amount.currency.as_deref().unwrap_or("");

        let mut text = format!("{total} {currency}");

        let breakdown: Vec<String> = [("subtotal", &amount.subtotal), ("tax", &amount.tax)]
            .iter()
            .filter_map(|(label, value)| value.as_ref().map(|value| format!("{label} {value}")))
            .collect();

        if !breakdown.is_empty() {
            text.push_str(&format!("  ({})", breakdown.join(", ")));
        }

        row("Total", text.trim_end());
    }

    let numbers: Vec<String> = [
        ("invoice", &result.invoice_number),
        ("receipt", &result.receipt_number),
        ("credits invoice", &result.credited_invoice),
    ]
    .iter()
    .filter_map(|(label, number)| number.as_ref().map(|number| format!("{label} {number}")))
    .collect();

    if !numbers.is_empty() {
        row("Number", &numbers.join(", "));
    }

    for payment in &result.payments {
        let mut text = word(&payment.method).replace('_', " ");

        if let Some(brand) = &payment.brand {
            text = brand.clone();
        }

        if let Some(last4) = &payment.last4 {
            text.push_str(&format!(" •••• {last4}"));
        }

        row("Paid with", &text);
    }

    if let Some(billing) = &result.billing {
        let mut parts = vec![word(&billing.model).replace('_', " ")];

        if let Some(interval) = &billing.interval {
            parts.push(format!("per {}", word(interval)));
        }

        if let Some(period) = &billing.period {
            let start = period.start.as_deref().unwrap_or("?");
            let end = period.end.as_deref().unwrap_or("?");

            parts.push(format!("{start} → {end}"));
        }

        if let Some(renewal) = &billing.next_renewal {
            parts.push(format!("renews {renewal}"));
        }

        row("Billing", &parts.join(", "));
    }

    if !result.lines.is_empty() {
        row("Lines", "");

        for line in &result.lines {
            let quantity = match (line.quantity, &line.unit_price) {
                (Some(quantity), Some(price)) => format!("{quantity} × {price}"),
                (Some(quantity), None) => format!("{quantity}"),
                _ => String::new(),
            };

            let amount = line.amount.as_deref().unwrap_or("");

            println!(
                "    {:<8} {:>14} {:>10}  {}",
                word(&line.kind),
                quantity,
                amount,
                line.description
            );
        }
    }

    if result.flags.is_empty() {
        row("Flags", "none");
    } else {
        row("Flags", "");

        for flag in &result.flags {
            println!("    {} {}", flag.code, flag.message);
        }
    }

    for warning in &result.warnings {
        row("Warning", warning);
    }

    let meta = &result.meta;

    let mut models = Vec::new();

    if let Some(decider) = &meta.decider {
        let tokens = meta.decider_tokens.map(|usage| usage.input).unwrap_or(0);

        models.push(format!("{decider} {tokens} in"));
    }

    match &meta.model {
        Some(model) => models.push(format!(
            "{model} {} in / {} out",
            meta.tokens.input, meta.tokens.output
        )),

        None => models.push("no extraction needed".to_string()),
    }

    println!(
        "  {} · {:.1} s",
        models.join(" → "),
        meta.ms as f64 / 1000.0
    );

    println!();
}

fn row(label: &str, value: &str) {
    let line = format!("  {label:<10} {value}");

    println!("{}", line.trim_end());
}

/// A card statement: its summary, its checks and every charge.
fn print_statement(statement: &Statement) {
    let header: Vec<String> = [
        statement.issuer.clone(),
        statement.number.clone(),
        statement
            .issued
            .as_ref()
            .map(|date| format!("issued {date}")),
        statement.due.as_ref().map(|date| format!("due {date}")),
    ]
    .into_iter()
    .flatten()
    .collect();

    if !header.is_empty() {
        row("Statement", &header.join(" · "));
    }

    let currency = statement.currency.as_deref().unwrap_or("");

    let total = statement
        .total
        .as_deref()
        .map(|total| format!(" · total {total} {currency}"))
        .unwrap_or_default();

    row("Charges", &format!("{}{total}", statement.charges.len()));

    let cards: Vec<String> = statement
        .cards
        .iter()
        .map(|card| {
            let name = card.name.as_deref().unwrap_or("no name");

            format!("•••• {} {name} ({})", card.last4, card.charges)
        })
        .collect();

    if !cards.is_empty() {
        row("Cards", &cards.join(", "));
    }

    for check in &statement.checks {
        let mark = match check.ok {
            Some(true) => "✓",
            Some(false) => "✗",
            None => "–",
        };

        row("Check", &format!("{mark} {}: {}", check.name, check.detail));
    }

    let meta = &statement.meta;

    row(
        "Read by",
        &format!(
            "{} · {} calls, {} reread · {} in, {} out · {:.1} s",
            meta.models.join(", "),
            meta.calls,
            meta.rereads,
            meta.tokens.input,
            meta.tokens.output,
            meta.ms as f64 / 1000.0
        ),
    );

    for warning in &statement.warnings {
        row("Warning", warning);
    }

    println!();

    for charge in &statement.charges {
        let number = charge
            .row
            .map(|row| format!("{row:>4}"))
            .unwrap_or_else(|| "    ".to_string());

        let original = match (&charge.original_amount, &charge.original_currency) {
            (Some(amount), Some(code)) => format!("  ({amount} {code})"),
            _ => String::new(),
        };

        let kind = match charge.kind {
            ChargeKind::Purchase => String::new(),
            other => format!("  [{}]", word(&other)),
        };

        println!(
            "{number}  {}  {:<44} {:>12}{original}{kind}",
            charge.date,
            truncate(&charge.merchant, 44),
            charge.amount
        );
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let cut: String = text.chars().take(width - 1).collect();

    format!("{cut}…")
}
