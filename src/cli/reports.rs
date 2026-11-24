//! `spendyard documents list`, `documents edit`, `report` and `subscriptions`: what the
//! store says about spending.
//!
//! Spend counts money paid: receipts and paid invoices, not failed or due
//! ones, not those marked personal or ignored, and each receipt once even
//! when it was forwarded or saved twice.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use rusqlite::params;
use rust_decimal::Decimal;
use serde_json::json;
use spendyard_core::dates::shift_date;
use spendyard_core::spend::{
    ROWS_SQL, Row, StoredRow, converted, rates_needed, report_currency, spend,
};
use spendyard_core::subscriptions::SubscriptionOptions;

use super::store::{Store, now};

/// The store's rows, newest first, their vendors grouped: the core's query
/// and rows, as the cloud has them too.
pub(crate) fn rows(store: &Store) -> Result<Vec<Row>> {
    let mut statement = store.connection.prepare(ROWS_SQL)?;

    let catalog = spendyard_core::catalog::catalog();

    let mapped = statement.query_map([], |row| {
        Ok(StoredRow {
            id: row.get("id")?,
            date: row.get("date")?,
            kind: row.get("kind")?,
            status: row.get("status")?,
            is_receipt: row.get("is_receipt")?,
            vendor_id: row.get("vendor_id")?,
            vendor_name: row.get("vendor_name")?,
            edited_vendor_name: row.get("edited_vendor_name")?,
            total: row.get("total")?,
            currency: row.get("currency")?,
            billing_model: row.get("billing_model")?,
            interval: row.get("interval")?,
            next_renewal: row.get("next_renewal")?,
            number: row.get("number")?,
            credited_invoice: row.get("credited_invoice")?,
            card: row.get("card")?,
            seats: row.get("seats")?,
            flags: row.get("flags")?,
            personal: row.get("personal")?,
            ignored: row.get("ignored")?,
            subject: row.get("subject")?,
            source: row.get("source")?,
            account_workspace: row.get("account_workspace")?,
            account_email: row.get("account_email")?,
            product: row.get("product")?,
        })
    })?;

    let stored: Vec<StoredRow> = mapped.collect::<Result<_, _>>()?;

    Ok(stored
        .into_iter()
        .map(|row| Row::from_stored(row, &catalog))
        .collect())
}

/// Makes sure the store has the rates to convert `rows` into `target`.
async fn ensure_rates(store: &Store, rows: &[Row], target: &str) -> Result<()> {
    let Some((currencies, since)) = rates_needed(rows, target) else {
        return Ok(());
    };

    match store.update_rates(&currencies, &since).await {
        Ok(added) if added > 0 => {
            eprintln!("spendyard: fetched {added} exchange rates from the ECB")
        }
        Ok(_) => {}
        Err(error) => eprintln!(
            "spendyard: no new exchange rates ({error:#}); converting with what the store has"
        ),
    }

    Ok(())
}

fn today() -> String {
    now().chars().take(10).collect()
}

pub(crate) fn money(value: Decimal) -> String {
    let rounded = value.round_dp(2);

    let text = format!("{rounded:.2}");

    let (whole, cents) = text.split_once('.').unwrap_or((&text, "00"));

    let negative = whole.starts_with('-');

    let digits: Vec<char> = whole.trim_start_matches('-').chars().collect();

    let mut grouped = String::new();

    for (index, digit) in digits.iter().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(' ');
        }

        grouped.push(*digit);
    }

    format!("{}{grouped}.{cents}", if negative { "-" } else { "" })
}

pub(crate) fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let cut: String = text.chars().take(width - 1).collect();

    format!("{cut}…")
}

/// Whether `documents list` shows a row without `--all`: receipts, invoices,
/// renewal notices, failed payments and credit notes.
pub(crate) fn listed_by_default(row: &Row) -> bool {
    row.is_receipt
        || matches!(
            row.kind.as_str(),
            "invoice" | "renewal_notice" | "payment_failed" | "credit_note"
        )
}

pub struct ReceiptsOptions {
    pub vendor: Option<String>,
    pub since: Option<String>,
    pub limit: usize,
    pub all: bool,
    pub json: bool,
}

/// Lists stored documents, newest first: receipts and invoices by
/// default, everything with `--all`.
pub fn receipts(options: ReceiptsOptions) -> Result<()> {
    let store = Store::open()?;

    let rows = rows(&store)?;

    let vendor = options.vendor.map(|vendor| vendor.to_lowercase());

    let shown: Vec<&Row> = rows
        .iter()
        .filter(|row| options.all || listed_by_default(row))
        .filter(|row| {
            vendor.as_ref().is_none_or(|vendor| {
                row.vendor_key.contains(vendor) || row.vendor_name.to_lowercase().contains(vendor)
            })
        })
        .filter(|row| {
            options
                .since
                .as_ref()
                .is_none_or(|since| row.date.as_str() >= since.as_str())
        })
        .take(options.limit)
        .collect();

    if options.json {
        for row in shown {
            println!("{}", serde_json::to_string(row)?);
        }

        return Ok(());
    }

    if shown.is_empty() {
        println!(
            "nothing stored yet; `spendyard read` and `spendyard mailbox scan` fill the store"
        );

        return Ok(());
    }

    for row in shown {
        let total = match (&row.total, &row.currency) {
            (Some(total), Some(currency)) => format!("{total} {currency}"),
            (Some(total), None) => total.clone(),
            _ => String::new(),
        };

        let mut marks = Vec::new();

        if row.personal {
            marks.push("personal");
        }

        if row.ignored {
            marks.push("ignored");
        }

        if row.flags > 0 {
            marks.push("flagged");
        }

        println!(
            "{:>5}  {:<10}  {:<14} {:<8} {:<26} {:>16}  {:<18} {}",
            row.id,
            row.date,
            clip(&row.kind, 14),
            clip(&row.status, 8),
            clip(&row.vendor_name, 26),
            total,
            clip(row.card.as_deref().unwrap_or(""), 18),
            marks.join(", ")
        );
    }

    Ok(())
}

pub struct ReportOptions {
    pub by: String,
    pub months: u32,
    pub currency: Option<String>,
    pub json: bool,
}

/// Spending per vendor or per month over the last `months` months.
pub async fn report(options: ReportOptions) -> Result<()> {
    let store = Store::open()?;

    let since = shift_date(&today(), -(options.months as i32), 0);

    let paid: Vec<Row> = spend(&rows_paid_however(&store)?)
        .into_iter()
        .filter(|row| row.date >= since)
        .collect();

    if paid.is_empty() {
        println!("no paid receipts since {since}");

        return Ok(());
    }

    let currency = report_currency(options.currency, &paid);

    ensure_rates(&store, &paid, &currency).await?;

    let mut groups: BTreeMap<String, (String, Decimal, usize, String)> = BTreeMap::new();

    let mut unconverted = 0;
    let mut no_currency = 0;

    for row in &paid {
        if row.currency.is_none() {
            no_currency += 1;

            continue;
        }

        let Some(value) = converted(&store, row, &currency) else {
            unconverted += 1;

            continue;
        };

        let key = match options.by.as_str() {
            "month" => row.date.chars().take(7).collect(),
            "vendor" => row.vendor_key.clone(),
            other => bail!("--by {other}: use vendor or month"),
        };

        let label = if options.by == "month" {
            key.clone()
        } else {
            row.vendor_name.clone()
        };

        let entry = groups
            .entry(key)
            .or_insert((label, Decimal::ZERO, 0, String::new()));

        entry.1 += value;
        entry.2 += 1;

        if row.date > entry.3 {
            entry.3 = row.date.clone();
        }
    }

    let mut ordered: Vec<(String, Decimal, usize, String)> = groups.into_values().collect();

    if options.by == "month" {
        ordered.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        ordered.sort_by_key(|group| std::cmp::Reverse(group.1));
    }

    let total: Decimal = ordered.iter().map(|group| group.1).sum();

    if options.json {
        let groups: Vec<_> = ordered
            .iter()
            .map(|(label, value, count, last)| json!({ "group": label, "total": value.round_dp(2), "receipts": count, "last": last }))
            .collect();

        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({ "currency": currency, "since": since, "by": options.by, "total": total.round_dp(2), "groups": groups })
            )?
        );

        return Ok(());
    }

    println!("Spend since {since}, in {currency}, by {}", options.by);
    println!();

    for (label, value, count, last) in &ordered {
        let share = if total > Decimal::ZERO {
            (*value / total * Decimal::from(100)).round_dp(0)
        } else {
            Decimal::ZERO
        };

        let detail = if options.by == "month" {
            String::new()
        } else {
            format!("last {last}")
        };

        println!(
            "  {:<32} {:>14} {:>4}%  {:>4} receipts  {detail}",
            clip(label, 32),
            money(*value),
            share,
            count
        );
    }

    println!();
    println!(
        "  {:<32} {:>14}         {:>4} receipts",
        "Total",
        money(total),
        paid.len() - unconverted - no_currency
    );

    if no_currency > 0 {
        println!();
        println!(
            "  {no_currency} receipts left out: no currency was read from them (`spendyard documents list --all` shows them)"
        );
    }

    if unconverted > 0 {
        println!();
        println!("  {unconverted} receipts left out: no exchange rate for their currency and date");
    }

    Ok(())
}

pub struct SubscriptionsOptions {
    pub currency: Option<String>,
    pub json: bool,

    /// Also show subscriptions lapsed for more than a year.
    pub all: bool,
}

/// The store's rows, with an invoice a card payment paid counted as paid,
/// as one read as paid is: its statement's line matched to it, as
/// `transactions match` matches them. So spend and subscriptions are the
/// cloud's (its spending's paid however it's known), where the invoice's PDF
/// only said what was due.
fn rows_paid_however(store: &Store) -> Result<Vec<Row>> {
    use spendyard_core::transactions::{CHARGES_SQL, MATCH_RECEIPTS_SQL, match_transactions};

    let charges = super::transactions::query(store, CHARGES_SQL)?;
    let receipts = super::transactions::query(store, MATCH_RECEIPTS_SQL)?;

    let matching = match_transactions(charges, receipts, &[], store);

    let paid_by_card: std::collections::BTreeSet<i64> = matching
        .transactions
        .iter()
        .filter(|found| found.state == "matched")
        .filter_map(|found| found.receipt_id)
        .collect();

    let mut all = rows(store)?;

    for row in &mut all {
        if paid_by_card.contains(&row.id) {
            row.is_receipt = true;
        }
    }

    Ok(all)
}

/// A subscription's name in the table: its vendor, and its product or
/// account when the vendor has several.
fn label(subscription: &spendyard_core::subscriptions::Subscription) -> String {
    let apart = subscription
        .id
        .split(':')
        .skip(1)
        .any(|part| !part.is_empty());

    let detail = subscription.product.as_deref().filter(|_| apart);

    match detail {
        Some(detail) => format!("{} · {detail}", subscription.vendor),
        None => subscription.vendor.clone(),
    }
}

/// Recurring vendors: what each costs, how often, on which card, and
/// whether the last payment went through.
pub async fn subscriptions(options: SubscriptionsOptions) -> Result<()> {
    let store = Store::open()?;

    // From receipts and invoices only, as the cloud has them: a card
    // statement's payments say what was paid, not what was subscribed to, so
    // none are guessed from them nor counted into one. A vendor charging the
    // card month after month with no receipt is `transactions list --missing`.
    let all = rows_paid_however(&store)?;

    let paid = spend(&all);

    let currency = report_currency(options.currency, &paid);

    ensure_rates(&store, &paid, &currency).await?;

    let asked = SubscriptionOptions {
        currency: Some(currency.clone()),
        today: today(),
        all: options.all,
        // What people decided about subscriptions is the cloud's, for now.
        edits: Vec::new(),
    };

    let found = spendyard_core::subscriptions::subscriptions(&all, &asked, &store);

    let subscriptions = found.subscriptions;

    if options.json {
        for subscription in &subscriptions {
            println!("{}", serde_json::to_string(subscription)?);
        }

        return Ok(());
    }

    if subscriptions.is_empty() {
        println!("no recurring vendors in the store yet");

        return Ok(());
    }

    let monthly = found.monthly;

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

    for subscription in &subscriptions {
        println!(
            "{:<28} {:<7} {:>12} {:>6} {:<11} {:<11} {:<17} {}",
            clip(&label(subscription), 28),
            subscription.interval,
            subscription.per_month.map(money).unwrap_or_default(),
            subscription
                .seats
                .map(|seats| format!("{seats}"))
                .unwrap_or_default(),
            subscription.last_paid.as_deref().unwrap_or(""),
            subscription.next_renewal.as_deref().unwrap_or(""),
            clip(subscription.card.as_deref().unwrap_or(""), 17),
            subscription.status
        );
    }

    println!();
    println!(
        "Active subscriptions: about {} {currency} a month",
        money(monthly)
    );

    Ok(())
}

pub struct EditOptions {
    pub id: i64,
    pub personal: Option<bool>,
    pub ignore: Option<bool>,
    pub vendor: Option<String>,
    pub note: Option<String>,
}

/// Marks a stored receipt by hand. Edits survive re-reading.
pub fn edit(options: EditOptions) -> Result<()> {
    let store = Store::open()?;

    apply_edit(&store, &options)?;

    println!("edited {}", options.id);

    Ok(())
}

/// Corrects a stored document by hand, as `documents edit` does.
pub(crate) fn apply_edit(store: &Store, options: &EditOptions) -> Result<()> {
    let exists: i64 = store.connection.query_row(
        "SELECT count(*) FROM documents WHERE id = ?1",
        params![options.id],
        |row| row.get(0),
    )?;

    if exists == 0 {
        bail!(
            "no document {} in the store; `spendyard documents list` lists them",
            options.id
        );
    }

    store.connection.execute(
        "INSERT INTO edits (document_id, updated) VALUES (?1, ?2) ON CONFLICT (document_id) DO NOTHING",
        params![options.id, now()],
    )?;

    if let Some(personal) = options.personal {
        store.connection.execute(
            "UPDATE edits SET personal = ?2 WHERE document_id = ?1",
            params![options.id, personal],
        )?;
    }

    if let Some(ignore) = options.ignore {
        store.connection.execute(
            "UPDATE edits SET ignored = ?2 WHERE document_id = ?1",
            params![options.id, ignore],
        )?;
    }

    if let Some(vendor) = &options.vendor {
        store.connection.execute(
            "UPDATE edits SET vendor_name = ?2 WHERE document_id = ?1",
            params![options.id, vendor],
        )?;
    }

    if let Some(note) = &options.note {
        store.connection.execute(
            "UPDATE edits SET note = ?2 WHERE document_id = ?1",
            params![options.id, note],
        )?;
    }

    store.connection.execute(
        "UPDATE edits SET updated = ?2 WHERE document_id = ?1",
        params![options.id, now()],
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invoice_a_card_payment_paid_is_paid() {
        use spendyard_core::extract::schema::{
            Amount, DocumentType, Payment, PaymentMethod, Status,
        };
        use spendyard_core::result::{Account, Vendor, not_about_money};
        use spendyard_core::statement::{ChargeOutput, StatementOutput, assemble};

        use crate::cli::store::Source;

        let dir = std::env::temp_dir().join(format!(
            "spendyard-paid-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        let _ = std::fs::remove_dir_all(&dir);

        let mut store = Store::open_at(&dir.join("test.db")).unwrap();

        // Vercel's invoice, read as due: its PDF says what's owed, not that it was paid.
        let mut invoice = not_about_money(DocumentType::Invoice, 0.9);

        invoice.document.is_invoice = true;
        invoice.document.status = Status::Due;
        invoice.issued = Some("2026-04-13".to_string());

        invoice.vendor = Some(Vendor {
            id: Some("vercel.com".to_string()),
            name: "Vercel".to_string(),
            legal_name: None,
            category: None,
            known: true,
            processor: None,
            account: Account::default(),
        });

        invoice.amount = Some(Amount {
            total: Some("192.07".to_string()),
            currency: Some("USD".to_string()),
            ..Amount::default()
        });

        invoice.payments = vec![Payment {
            method: PaymentMethod::Card,
            last4: Some("3776".to_string()),
            ..Payment::default()
        }];

        // The card statement with the line that paid it.
        let mut statement = not_about_money(DocumentType::CardStatement, 0.9);

        statement.statement = Some(assemble(vec![StatementOutput {
            is_card_statement: true,
            currency: Some("USD".to_string()),
            charges: vec![ChargeOutput {
                row: Some(1),
                date: "2026-04-14".to_string(),
                merchant: "VERCEL INC., COVINA, US".to_string(),
                card_last4: Some("3776".to_string()),
                amount: "192.07".to_string(),
                ..ChargeOutput::default()
            }],
            ..StatementOutput::default()
        }]));

        let source = Source {
            origin: "file:test".to_string(),
            name: None,
            received: None,
        };

        let mut save = |bytes: &[u8], result| {
            let document = spendyard_core::read(bytes, None).unwrap();

            store.save(bytes, &document, &source, result).unwrap()
        };

        let invoice_id = save(b"vercel's invoice", &invoice);

        save(b"the card statement", &statement);

        let as_read = rows(&store).unwrap();
        let paid = rows_paid_however(&store).unwrap();

        let is_paid = |all: &[Row]| {
            all.iter()
                .find(|row| row.id == invoice_id)
                .unwrap()
                .is_receipt
        };

        assert!(!is_paid(&as_read));
        assert!(is_paid(&paid));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dates_shift() {
        assert_eq!(shift_date("2026-11-19", 1, 0), "2026-12-19");
        assert_eq!(shift_date("2026-12-19", 1, 0), "2027-01-19");
        assert_eq!(shift_date("2026-01-31", 1, 0), "2026-02-28");
        assert_eq!(shift_date("2026-11-19", -12, 0), "2025-11-19");
        assert_eq!(shift_date("2026-03-03", 0, -7), "2026-02-24");
    }

    #[test]
    fn money_groups_thousands() {
        assert_eq!(money(Decimal::new(123456789, 2)), "1 234 567.89");
        assert_eq!(money(Decimal::new(500, 2)), "5.00");
    }
}
