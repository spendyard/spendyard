//! Deterministic checks on a result: sums, codes, dates, card digits, and
//! whether each quote really is in the document. A failed check is a flag,
//! not an error.

use rust_decimal::Decimal;

use crate::document::Document;
use crate::extract::schema::{LineKind, Status};
use crate::money;
use crate::result::{Flag, ReadResult};

/// Active ISO 4217 currency codes.
const CURRENCIES: &str = "AED AFN ALL AMD ANG AOA ARS AUD AWG AZN BAM BBD BDT BGN BHD BIF BMD BND \
BOB BRL BSD BTN BWP BYN BZD CAD CDF CHF CLP CNY COP CRC CUP CVE CZK DJF DKK DOP DZD EGP ERN ETB EUR \
FJD FKP GBP GEL GHS GIP GMD GNF GTQ GYD HKD HNL HTG HUF IDR ILS INR IQD IRR ISK JMD JOD JPY KES KGS \
KHR KMF KPW KRW KWD KYD KZT LAK LBP LKR LRD LSL LYD MAD MDL MGA MKD MMK MNT MOP MRU MUR MVR MWK MXN \
MYR MZN NAD NGN NIO NOK NPR NZD OMR PAB PEN PGK PHP PKR PLN PYG QAR RON RSD RUB RWF SAR SBD SCR SDG \
SEK SGD SHP SLE SOS SRD SSP STN SVC SYP SZL THB TJS TMT TND TOP TRY TTD TWD TZS UAH UGX USD UYU UZS \
VES VND VUV WST XAF XCD XOF XPF YER ZAR ZMW ZWG";

/// Rounding allowed per added-up amount.
const CENT: Decimal = Decimal::from_parts(1, 0, 0, false, 2);

/// Runs every check and adds flags to `result`.
pub fn validate(result: &mut ReadResult, document: &Document) {
    let mut flags = Vec::new();

    check_amounts(result, &mut flags);
    check_currency(result, &mut flags);
    check_dates(result, &mut flags);
    check_cards(result, &mut flags);
    check_evidence(result, document, &mut flags);
    check_models_agree(result, &mut flags);

    result.flags.extend(flags);
}

fn flag(code: &'static str, field: Option<&str>, message: impl Into<String>) -> Flag {
    Flag {
        code,
        field: field.map(str::to_string),
        message: message.into(),
    }
}

fn check_amounts(result: &ReadResult, flags: &mut Vec<Flag>) {
    let paid_or_due = result.document.is_receipt
        || result.document.is_invoice
        || result.document.status == Status::Due;

    let amount = result.amount.clone().unwrap_or_default();

    if paid_or_due && amount.total.is_none() {
        flags.push(flag(
            "missing_total",
            Some("amount.total"),
            "no total for a receipt or invoice",
        ));
    }

    let mut fields = vec![
        ("amount.total".to_string(), amount.total.clone()),
        ("amount.subtotal".to_string(), amount.subtotal.clone()),
        ("amount.tax".to_string(), amount.tax.clone()),
    ];

    for (index, line) in result.lines.iter().enumerate() {
        fields.push((format!("lines[{index}].amount"), line.amount.clone()));
        fields.push((
            format!("lines[{index}].unit_price"),
            line.unit_price.clone(),
        ));
    }

    for (index, payment) in result.payments.iter().enumerate() {
        fields.push((format!("payments[{index}].amount"), payment.amount.clone()));
    }

    for (field, value) in &fields {
        if let Some(text) = value
            && money::parse(text).is_none()
        {
            flags.push(flag(
                "invalid_amount",
                Some(field),
                format!("{text:?} is not an amount"),
            ));
        }
    }

    let total = amount.total.as_deref().and_then(money::parse);
    let subtotal = amount.subtotal.as_deref().and_then(money::parse);
    let tax = amount.tax.as_deref().and_then(money::parse);

    if let (Some(total), Some(subtotal)) = (total, subtotal) {
        let expected = subtotal + tax.unwrap_or_default();

        if (expected - total).abs() > CENT {
            flags.push(flag(
                "totals_dont_add_up",
                Some("amount.total"),
                format!(
                    "subtotal {subtotal} + tax {} is {expected}, not {total}",
                    tax.unwrap_or_default()
                ),
            ));
        }
    }

    check_lines(result, total, subtotal, tax, flags);
}

/// The lines should add up to the subtotal, or to the total less tax.
fn check_lines(
    result: &ReadResult,
    total: Option<Decimal>,
    subtotal: Option<Decimal>,
    tax: Option<Decimal>,
    flags: &mut Vec<Flag>,
) {
    let amounts: Vec<Decimal> = result
        .lines
        .iter()
        .filter_map(|line| line.amount.as_deref().and_then(money::parse))
        .collect();

    let every_line_has_an_amount = amounts.len() == result.lines.len();

    if amounts.is_empty() || !every_line_has_an_amount {
        return;
    }

    let target = subtotal.or_else(|| total.map(|total| total - tax.unwrap_or_default()));

    let Some(target) = target else {
        return;
    };

    // Discounts and credits are sometimes written as positive numbers.
    let signed: Decimal = result
        .lines
        .iter()
        .zip(&amounts)
        .map(|(line, amount)| match line.kind {
            LineKind::Discount | LineKind::Credit => -amount.abs(),
            _ => *amount,
        })
        .sum();

    let tolerance = CENT * Decimal::from(amounts.len().max(1));

    if (signed - target).abs() > tolerance {
        flags.push(flag(
            "lines_dont_add_up",
            Some("lines"),
            format!("the lines add up to {signed}, not {target}"),
        ));
    }
}

fn check_currency(result: &ReadResult, flags: &mut Vec<Flag>) {
    let Some(code) = result
        .amount
        .as_ref()
        .and_then(|amount| amount.currency.as_deref())
    else {
        return;
    };

    let known = code.len() == 3 && CURRENCIES.split(' ').any(|known| known == code);

    if !known {
        flags.push(flag(
            "unknown_currency",
            Some("amount.currency"),
            format!("{code} is not an ISO 4217 code"),
        ));
    }
}

fn check_dates(result: &ReadResult, flags: &mut Vec<Flag>) {
    let mut dates = vec![("issued", result.issued.clone())];

    if let Some(billing) = &result.billing {
        dates.push(("billing.next_renewal", billing.next_renewal.clone()));

        if let Some(period) = &billing.period {
            dates.push(("billing.period.start", period.start.clone()));
            dates.push(("billing.period.end", period.end.clone()));
        }
    }

    for (field, value) in &dates {
        if let Some(text) = value
            && !is_date(text)
        {
            flags.push(flag(
                "invalid_date",
                Some(field),
                format!("{text:?} is not a YYYY-MM-DD date"),
            ));
        }
    }

    let date = |field: &str| -> Option<String> {
        dates
            .iter()
            .find(|(name, _)| *name == field)
            .and_then(|(_, value)| value.clone())
            .filter(|text| is_date(text))
    };

    // ISO dates compare correctly as strings.
    let in_order = |first: &str, second: &str, code: &'static str| match (date(first), date(second))
    {
        (Some(a), Some(b)) if b < a => Some(flag(
            code,
            Some(second),
            format!("{second} {b} is before {first} {a}"),
        )),
        _ => None,
    };

    flags.extend(in_order(
        "billing.period.start",
        "billing.period.end",
        "period_backwards",
    ));
    flags.extend(in_order(
        "issued",
        "billing.next_renewal",
        "renewal_before_issue",
    ));
}

/// `YYYY-MM-DD` with a real month and day.
fn is_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();

    let [year, month, day] = parts.as_slice() else {
        return false;
    };

    let shaped = year.len() == 4 && month.len() == 2 && day.len() == 2;

    let (Ok(year), Ok(month), Ok(day)) = (
        year.parse::<u32>(),
        month.parse::<u32>(),
        day.parse::<u32>(),
    ) else {
        return false;
    };

    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;

    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };

    shaped && (1..=days_in_month).contains(&day)
}

fn check_cards(result: &ReadResult, flags: &mut Vec<Flag>) {
    for (index, payment) in result.payments.iter().enumerate() {
        let Some(last4) = &payment.last4 else {
            continue;
        };

        let valid = last4.len() == 4 && last4.chars().all(|c| c.is_ascii_digit());

        if !valid {
            let field = format!("payments[{index}].last4");

            flags.push(flag(
                "invalid_last4",
                Some(&field),
                format!("{last4:?} is not four digits"),
            ));
        }
    }
}

/// Document types the extractor reads as money changing hands.
const MONEY_TYPES: &[&str] = &["receipt", "invoice"];

/// When the first model was sure and the extractor says otherwise, one of
/// them is wrong. Receipt and invoice count as the same here: a paid
/// invoice is both.
fn check_models_agree(result: &ReadResult, flags: &mut Vec<Flag>) {
    let Some(decisions) = &result.decisions else {
        return;
    };

    let sure = |question: &str| -> Option<String> {
        let (choice, probability) = decisions.get(question)?.chosen()?;

        (probability >= crate::decide::DEFAULT_THRESHOLD).then(|| choice.to_string())
    };

    if let Some(first) = sure("document_type") {
        let extracted = word(&result.document.kind);

        let same_side =
            MONEY_TYPES.contains(&first.as_str()) == MONEY_TYPES.contains(&extracted.as_str());

        if !same_side {
            flags.push(flag(
                "models_disagree",
                Some("document.type"),
                format!("the first model says {first}, the extractor {extracted}"),
            ));
        }
    }

    if let Some(first) = sure("billing_model")
        && first != "unknown"
        && let Some(billing) = &result.billing
    {
        let extracted = word(&billing.model);

        if extracted != "unknown" && extracted != first {
            flags.push(flag(
                "models_disagree",
                Some("billing.model"),
                format!("the first model says {first}, the extractor {extracted}"),
            ));
        }
    }
}

/// The word a serde enum serializes to: `BillingModel::OneTime` → `one_time`.
fn word<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Every quote must be in the document's text. The values that matter most
/// must have a quote, and the quote must contain the value.
fn check_evidence(result: &mut ReadResult, document: &Document, flags: &mut Vec<Flag>) {
    let texts: Vec<Option<String>> = document
        .parts
        .iter()
        .map(|part| part.text.as_deref().map(normalize_for_search))
        .collect();

    let has_any_text = texts.iter().any(Option::is_some);

    // The model also sees the email headers, and may quote them (a vendor's
    // domain from the From address).
    let headers = document
        .headers
        .as_ref()
        .map(|headers| {
            let fields = [
                ("From", &headers.from),
                ("To", &headers.to),
                ("Subject", &headers.subject),
                ("Date", &headers.date),
            ];

            let joined: Vec<String> = fields
                .iter()
                .filter_map(|(name, value)| value.as_ref().map(|value| format!("{name}: {value}")))
                .collect();

            normalize_for_search(&joined.join(" "))
        })
        .unwrap_or_default();

    for (field, evidence) in result.evidence.iter_mut() {
        let quote = normalize_for_search(&evidence.quote);

        if quote.is_empty() {
            continue;
        }

        let in_part = texts
            .get(evidence.part as usize)
            .and_then(Option::as_ref)
            .is_some_and(|text| text.contains(&quote));

        let anywhere = in_part
            || headers.contains(&quote)
            || texts.iter().flatten().any(|text| text.contains(&quote));

        evidence.checked = anywhere;

        let part_has_text = texts
            .get(evidence.part as usize)
            .is_some_and(Option::is_some);

        // A quote from an image or a scanned PDF can't be checked.
        if !anywhere && part_has_text {
            flags.push(flag(
                "quote_not_found",
                Some(field),
                format!("{:?} is not in the document", evidence.quote),
            ));
        }
    }

    for (field, value) in key_values(result) {
        match result.evidence.get(&field) {
            None if has_any_text => {
                flags.push(flag("no_evidence", Some(&field), "no quote for this value"));
            }

            Some(evidence) if !quote_contains(&evidence.quote, &value) => {
                flags.push(flag(
                    "quote_mismatch",
                    Some(&field),
                    format!("{:?} doesn't show {value}", evidence.quote),
                ));
            }

            _ => {}
        }
    }
}

/// The values that must have evidence: the total, card digits and the
/// invoice number.
fn key_values(result: &ReadResult) -> Vec<(String, String)> {
    let mut values = Vec::new();

    if let Some(total) = result
        .amount
        .as_ref()
        .and_then(|amount| amount.total.clone())
    {
        values.push(("amount.total".to_string(), total));
    }

    for (index, payment) in result.payments.iter().enumerate() {
        if let Some(last4) = &payment.last4 {
            values.push((format!("payments[{index}].last4"), last4.clone()));
        }
    }

    if let Some(number) = &result.invoice_number {
        values.push(("invoice_number".to_string(), number.clone()));
    }

    if let Some(number) = &result.receipt_number {
        values.push(("receipt_number".to_string(), number.clone()));
    }

    values
}

/// Whether a quote shows a value. Amounts are compared by their digits, so
/// `$1,234.50` shows `1234.50`.
fn quote_contains(quote: &str, value: &str) -> bool {
    if let Some(amount) = money::parse(value)
        && value.chars().any(|c| c == '.')
    {
        let quote_digits: String = quote.chars().filter(char::is_ascii_digit).collect();

        let whole_digits: String = format!("{}", amount.abs().trunc())
            .chars()
            .filter(char::is_ascii_digit)
            .collect();

        return quote_digits.contains(&money::digits(&amount))
            || quote_digits.contains(&whole_digits);
    }

    normalize_for_search(quote).contains(&normalize_for_search(value))
}

/// Letters and digits only, lowercased. Models copy quotes with small
/// changes in spacing and punctuation ("PENNYSYR 0096" for
/// "PENNYSYR-0096", a line break for a space); those shouldn't count as
/// made up, while different words or numbers still do.
fn normalize_for_search(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert!(is_date("2026-11-19"));
        assert!(is_date("2028-02-29"));
        assert!(!is_date("2026-02-29"));
        assert!(!is_date("2026-13-01"));
        assert!(!is_date("19.11.2026"));
    }

    #[test]
    fn quotes_show_amounts_however_written() {
        assert!(quote_contains("Amount paid $1,234.50", "1234.50"));
        assert!(quote_contains("Beløp: kr 1 234,50", "1234.50"));
        assert!(quote_contains("Total: 20 USD", "20.00"));
        assert!(!quote_contains("Amount paid $112.40", "121.40"));
        assert!(quote_contains("Visa •••• 4242", "4242"));
    }

    #[test]
    fn search_ignores_case_spacing_and_punctuation() {
        assert_eq!(
            normalize_for_search("Amount\n  paid\u{a0}$112.40 "),
            normalize_for_search("amount paid 112,40")
        );

        assert_eq!(
            normalize_for_search("PENNYSYR 0096"),
            normalize_for_search("PENNYSYR-0096")
        );
        assert_eq!(normalize_for_search("Beløp"), "beløp");
        assert_ne!(
            normalize_for_search("$112.40"),
            normalize_for_search("$121.40")
        );
    }
}
