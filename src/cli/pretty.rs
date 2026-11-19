//! Human-readable summaries for `--pretty`.

use serde::Serialize;
use spendyard::ReadResult;

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

    println!(
        "  {} · {} in / {} out · {:.1} s",
        meta.model,
        meta.tokens.input,
        meta.tokens.output,
        meta.ms as f64 / 1000.0
    );

    println!();
}

fn row(label: &str, value: &str) {
    let line = format!("  {label:<10} {value}");

    println!("{}", line.trim_end());
}
