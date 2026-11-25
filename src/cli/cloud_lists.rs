//! `spendyard cloud payments`, `cloud subscriptions` and `cloud vendors`:
//! what the organisation this machine acts in pays for, as the cloud works it
//! out from everything the company has, printed as `payments` and
//! `subscriptions` print what this machine read. A member sees what's theirs.
//!
//! `cloud vendors` is the company's vendors and what each costs, as the
//! dashboard's Vendors shows them; the local `vendors` is the catalog.

use std::str::FromStr;

use anyhow::{Result, bail};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli::cloud::Cloud;
use crate::cli::reports::{clip, money};

pub struct PaymentsOptions {
    /// Only those with no document: a statement's line with no receipt.
    pub missing: bool,
    /// Only money back: refunds and credits.
    pub money_back: bool,
    /// Only those whose vendor holds this.
    pub vendor: Option<String>,
    /// YYYY-MM-DD, both included.
    pub from: Option<String>,
    pub to: Option<String>,
    /// Also those marked personal or not spend.
    pub all: bool,
    pub limit: usize,
    pub json: bool,
}

pub struct VendorsOptions {
    pub search: Option<String>,
    pub category: Option<String>,
    /// Also those set aside as personal or not spend.
    pub all: bool,
    pub json: bool,
}

/// A plain decimal from the API, grouped in thousands: `1 234.50`. As it
/// came when it isn't one, and nothing for nothing.
fn amount(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    Decimal::from_str(text)
        .map(money)
        .unwrap_or_else(|_| text.to_string())
}

/// `INTERVAL_MONTH` as `month`; nothing for unspecified.
fn word(name: &str, prefix: &str) -> String {
    let word = name.strip_prefix(prefix).unwrap_or(name).to_lowercase();

    if word == "unspecified" {
        String::new()
    } else {
        word.replace('_', " ")
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

// --- payments ------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaymentsPage {
    #[serde(default)]
    payments: Vec<Value>,

    #[serde(default)]
    next_page_token: String,

    #[serde(default)]
    total: i64,
}

/// `spendyard cloud payments`: every payment once, newest first, with what
/// it has to show for it.
pub async fn payments(options: PaymentsOptions) -> Result<()> {
    if options.missing && options.money_back {
        bail!("--missing and --money-back pick different payments: one or the other");
    }

    let cloud = Cloud::connect()?;

    let filter = if options.missing {
        "PAYMENT_FILTER_NO_DOCUMENT"
    } else if options.money_back {
        "PAYMENT_FILTER_MONEY_BACK"
    } else {
        "PAYMENT_FILTER_UNSPECIFIED"
    };

    let mut payments: Vec<Value> = Vec::new();
    let mut page_token = String::new();
    let mut total = 0;

    while payments.len() < options.limit {
        let request = json!({
            "pageSize": (options.limit - payments.len()).min(500),
            "pageToken": page_token,
            "filter": filter,
            "query": options.vendor.clone().unwrap_or_default(),
            "from": options.from.clone().unwrap_or_default(),
            "to": options.to.clone().unwrap_or_default(),
            "includeAll": options.all,
        });

        let page: PaymentsPage = cloud.call("PaymentService/ListPayments", request).await?;

        total = page.total;

        payments.extend(page.payments);
        page_token = page.next_page_token;

        if page_token.is_empty() {
            break;
        }
    }

    if options.json {
        for payment in &payments {
            println!("{payment}");
        }

        return Ok(());
    }

    if payments.is_empty() {
        println!("No payments in {} that pick", cloud.signed_in_as().org.name);

        return Ok(());
    }

    for payment in &payments {
        println!("{}", payment_line(payment));
    }

    if (payments.len() as i64) < total {
        println!("… {} of {total}; --limit shows more", payments.len());
    }

    Ok(())
}

/// One payment in a line, as `payments` prints one here.
fn payment_line(payment: &Value) -> String {
    let vendor = payment["vendor"]["name"].as_str().unwrap_or("");

    format!(
        "{:>5}  {:<10}  {:<24} {:>12} {:<3}  {}",
        payment["id"].as_i64().unwrap_or(0),
        text(payment, "date"),
        clip(vendor, 24),
        amount(text(payment, "amount")),
        text(payment, "currency"),
        evidence_of(payment),
    )
}

/// What a payment has to show for it, in words: "refund, 2 documents, on a
/// statement"; "no receipt" for money paid with no document.
fn evidence_of(payment: &Value) -> String {
    let count = |key: &str| payment[key].as_array().map_or(0, Vec::len);

    let documents = count("documentIds");
    let lines = count("transactionIds");
    let kind = word(text(payment, "kind"), "PAYMENT_KIND_");

    let mut words: Vec<String> = Vec::new();

    if payment["personal"].as_bool() == Some(true) {
        words.push("personal".into());
    }

    if payment["notSpend"].as_bool() == Some(true) {
        words.push("not spend".into());
    }

    if kind != "payment" && !kind.is_empty() {
        words.push(kind.clone());
    }

    match documents {
        0 if kind == "payment" => words.push("no receipt".into()),
        0 => {}
        1 => words.push("1 document".into()),
        n => words.push(format!("{n} documents")),
    }

    match lines {
        0 => {}
        1 => words.push("on a statement".into()),
        n => words.push(format!("{n} lines on statements")),
    }

    if let Some(corrected) = payment["correctsPaymentId"].as_i64().filter(|id| *id > 0) {
        words.push(format!("credits payment {corrected}"));
    }

    words.join(", ")
}

// --- subscriptions -------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Subscriptions {
    #[serde(default)]
    subscriptions: Vec<Value>,

    #[serde(default)]
    currency: String,

    #[serde(default)]
    monthly: String,
}

/// `spendyard cloud subscriptions`: the company's recurring vendors, what
/// each costs a month, and how they stand.
pub async fn subscriptions(all: bool, json: bool) -> Result<()> {
    let cloud = Cloud::connect()?;

    let found: Subscriptions = cloud
        .call(
            "SubscriptionService/ListSubscriptions",
            json!({ "includeAll": all }),
        )
        .await?;

    if json {
        for subscription in &found.subscriptions {
            println!("{subscription}");
        }

        return Ok(());
    }

    if found.subscriptions.is_empty() {
        println!("No subscriptions in {} yet", cloud.signed_in_as().org.name);

        return Ok(());
    }

    let currency = &found.currency;

    println!(
        "{:<28} {:<7} {:>12} {:>6} {:<11} {:<11} {:<17} Status",
        "Vendor",
        "Every",
        &format!("{currency}/month"),
        "Seats",
        "Last paid",
        "Next",
        "Card",
    );

    for subscription in &found.subscriptions {
        println!("{}", subscription_line(subscription));
    }

    println!();
    println!(
        "Active subscriptions: about {} {currency} a month",
        amount(&found.monthly)
    );

    Ok(())
}

/// One subscription in a line, as `subscriptions` prints one here.
fn subscription_line(subscription: &Value) -> String {
    let vendor = subscription["vendor"]["name"].as_str().unwrap_or("");
    let product = text(subscription, "product");

    let label = if product.is_empty() || product.eq_ignore_ascii_case(vendor) {
        vendor.to_string()
    } else {
        format!("{vendor} · {product}")
    };

    let seats = subscription["seats"]
        .as_f64()
        .filter(|seats| *seats > 0.0)
        .map(|seats| seats.to_string())
        .unwrap_or_default();

    format!(
        "{:<28} {:<7} {:>12} {:>6} {:<11} {:<11} {:<17} {}",
        clip(&label, 28),
        word(text(subscription, "interval"), "INTERVAL_"),
        amount(text(subscription, "perMonth")),
        seats,
        text(subscription, "lastPaid"),
        text(subscription, "nextRenewal"),
        clip(text(subscription, "card"), 17),
        word(text(subscription, "status"), "SUBSCRIPTION_STATUS_"),
    )
}

// --- vendors -------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct Vendors {
    #[serde(default)]
    vendors: Vec<Value>,

    #[serde(default)]
    currency: String,
}

/// `spendyard cloud vendors`: the company's vendors, what each cost the
/// last year and costs a month, as Vendors shows them.
pub async fn vendors(options: VendorsOptions) -> Result<()> {
    let cloud = Cloud::connect()?;

    let request = json!({
        "query": options.search.clone().unwrap_or_default(),
        "category": options.category.clone().unwrap_or_default(),
        "includeAll": options.all,
    });

    let found: Vendors = cloud.call("VendorService/ListVendors", request).await?;

    if options.json {
        for vendor in &found.vendors {
            println!("{vendor}");
        }

        return Ok(());
    }

    if found.vendors.is_empty() {
        println!("No vendors in {} that pick", cloud.signed_in_as().org.name);

        return Ok(());
    }

    let currency = &found.currency;

    println!(
        "{:<28} {:<16} {:>14} {:>12} {:>8} {:<11} Subscriptions",
        "Vendor",
        "Category",
        &format!("{currency} a year"),
        &format!("{currency}/month"),
        "Payments",
        "Last paid",
    );

    for vendor in &found.vendors {
        println!("{}", vendor_line(vendor));
    }

    Ok(())
}

/// One vendor in a line: what it cost the last year, a month now, how many
/// payments, when last, how many subscriptions; set aside ones marked.
fn vendor_line(vendor: &Value) -> String {
    let mut name = text(vendor, "name").to_string();

    if vendor["personal"].as_bool() == Some(true) {
        name.push_str(" (personal)");
    } else if vendor["notSpend"].as_bool() == Some(true) {
        name.push_str(" (not spend)");
    }

    let subscriptions = vendor["subscriptions"].as_i64().unwrap_or(0);

    format!(
        "{:<28} {:<16} {:>14} {:>12} {:>8} {:<11} {}",
        clip(&name, 28),
        clip(text(vendor, "category"), 16),
        amount(text(vendor, "spentYear")),
        amount(text(vendor, "monthly")),
        vendor["payments"].as_i64().unwrap_or(0),
        text(vendor, "lastPaid"),
        if subscriptions > 0 {
            subscriptions.to_string()
        } else {
            String::new()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_payment_in_a_line_with_what_shows_for_it() {
        let paid = json!({
            "id": 1408, "kind": "PAYMENT_KIND_PAYMENT", "vendor": { "name": "Tailscale" },
            "date": "2026-10-19", "amount": "1234.50", "currency": "USD",
            "documentIds": [1883, 1886], "transactionIds": ["90:4"],
        });

        let alone = json!({ "id": 9, "kind": "PAYMENT_KIND_PAYMENT", "date": "2026-10-01", "amount": "20.00", "currency": "NOK", "transactionIds": ["90:5"] });
        let refund = json!({ "id": 10, "kind": "PAYMENT_KIND_REFUND", "amount": "-45.00", "documentIds": [3], "correctsPaymentId": 8 });

        assert!(payment_line(&paid).contains("1 234.50 USD  2 documents, on a statement"));
        assert!(payment_line(&alone).ends_with("no receipt, on a statement"));
        assert!(payment_line(&refund).ends_with("refund, 1 document, credits payment 8"));
    }

    #[test]
    fn a_subscription_and_a_vendor_in_a_line() {
        let subscription = json!({
            "vendor": { "name": "Slack" }, "product": "Pro", "interval": "INTERVAL_MONTH",
            "status": "SUBSCRIPTION_STATUS_PAYMENT_FAILED", "perMonth": "305.50", "seats": 37,
            "lastPaid": "2026-09-17", "card": "mastercard 8239",
        });

        let vendor = json!({
            "name": "Slack", "category": "communication", "spentYear": "3210.00",
            "monthly": "305.50", "payments": 12, "lastPaid": "2026-09-17", "subscriptions": 1, "personal": true,
        });

        let line = subscription_line(&subscription);

        assert!(line.starts_with("Slack · Pro"));
        assert!(line.contains("month"));
        assert!(line.ends_with("payment failed"));

        let line = vendor_line(&vendor);

        assert!(line.starts_with("Slack (personal)"));
        assert!(line.contains("3 210.00"));
        assert!(line.ends_with(" 1"));
    }
}
