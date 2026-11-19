//! Reading receipts straight from a mailbox: sign-in, listing, a cheap
//! pre-filter on headers, and the normal pipeline on what passes.

pub mod accounts;
pub mod imap;
pub mod outlook;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use spendyard::ReadResult;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::models::Models;
use super::store::{Source, Store};

/// What a mailbox says about a message before it's downloaded.
#[derive(Debug, Clone, Serialize)]
pub struct Header {
    /// The provider's ID, for downloading.
    #[serde(skip)]
    pub id: String,

    /// The `Message-ID` header, the same in every copy of the message.
    pub message_id: Option<String>,

    pub subject: String,
    pub from: String,

    /// RFC 3339.
    pub received: String,

    #[serde(skip)]
    pub preview: String,

    pub has_attachments: bool,
}

/// Words that suggest money changed hands, in the languages seen so far.
/// The pre-filter is generous: a false match costs one download and a
/// cheap first-model call; a miss loses a receipt.
const MONEY_WORDS: &[&str] = &[
    "receipt",
    "invoice",
    "payment",
    "paid",
    "charged",
    "charge",
    "billing",
    "bill",
    "order",
    "subscription",
    "renew",
    "renewal",
    "trial",
    "statement",
    "refund",
    "purchase",
    "plan",
    "kvittering",
    "faktura",
    "betaling",
    "abonnement",
    "fornyelse",
    "ordre",
    "kjøp",
    "kvitto",
    "rechnung",
    "quittung",
    "zahlung",
    "bestellung",
    "facture",
    "reçu",
];

/// Sender address parts typical of billing mail.
const BILLING_SENDERS: &[&str] = &[
    "billing",
    "invoice",
    "receipt",
    "payment",
    "accounts",
    "stripe.com",
    "paddle.com",
    "lemonsqueezy",
    "fastspring",
    "chargebee",
    "apple.com",
    "paypal",
];

/// Whether a message is worth downloading. Looks only at the header,
/// sender and preview.
pub fn likely_about_money(header: &Header) -> bool {
    let text = format!("{} {}", header.subject, header.preview).to_lowercase();

    let has_word = MONEY_WORDS.iter().any(|word| text.contains(word));

    let billing_sender = BILLING_SENDERS
        .iter()
        .any(|part| header.from.contains(part));

    let has_amount = text.contains('$')
        || text.contains('€')
        || text.contains('£')
        || text.contains(" kr")
        || text.contains("nok")
        || text.contains("usd")
        || text.contains("eur");

    has_word || billing_sender || (header.has_attachments && has_amount)
}

pub struct ScanOptions {
    pub account: Option<String>,
    pub folder: Option<String>,

    /// `YYYY-MM-DD`. Default: where the last scan stopped, or 30 days ago.
    pub since: Option<String>,

    pub limit: usize,

    /// Skip the pre-filter and read every message.
    pub all: bool,

    /// List what would be read, without downloading or calling models.
    pub dry_run: bool,

    /// Also save each message read as `.eml` here.
    pub save: Option<PathBuf>,

    /// Read messages again even when the store already has them.
    pub again: bool,

    pub concurrency: usize,
}

/// One message's outcome.
pub struct Scanned {
    pub header: Header,
    pub result: Result<ReadResult>,
}

/// A message read: its bytes and document, kept to save in the store.
struct Read {
    mime: Vec<u8>,
    document: spendyard::Document,
    result: ReadResult,
}

pub struct ScanReport {
    /// `outlook` or `imap`.
    pub provider: &'static str,

    pub account: String,
    pub folder: String,
    pub since: String,
    pub listed: usize,
    pub candidates: Vec<Header>,

    /// Likely about money, but already in the store.
    pub already_stored: usize,

    pub scanned: Vec<Scanned>,
}

/// Scans a signed-in mailbox: `outlook` or `imap`. Models are only needed
/// when not a dry run. With a store, it continues where the last scan
/// stopped, skips messages already stored, and saves what it reads.
pub async fn scan(
    provider: &str,
    options: ScanOptions,
    models: Option<Arc<Models>>,
    store: Option<&mut Store>,
) -> Result<ScanReport> {
    let account = accounts::find(provider, options.account.as_deref())?;

    match provider {
        "outlook" => scan_outlook(account, options, models, store).await,
        "imap" => scan_imap(account, options, models, store).await,
        other => anyhow::bail!("unknown mailbox {other:?}; use outlook or imap"),
    }
}

async fn scan_outlook(
    account: accounts::Account,
    options: ScanOptions,
    models: Option<Arc<Models>>,
    mut store: Option<&mut Store>,
) -> Result<ScanReport> {
    let session = Arc::new(outlook::Session::open(&account).await?);

    let folder_label = options
        .folder
        .clone()
        .unwrap_or_else(|| "all folders".to_string());

    let state_key = format!("{}:{folder_label}", account.key());

    let previous = match &store {
        Some(store) => store.scan_state(&state_key)?,
        None => None,
    };

    let since = match (&options.since, &previous) {
        (Some(date), _) => date_to_rfc3339(date),
        (None, Some((newest, _))) => newest.clone(),
        (None, None) => days_ago(30),
    };

    let already_seen: Vec<String> = match (&options.since, &previous) {
        (None, Some((_, seen))) => seen.clone(),
        _ => Vec::new(),
    };

    let headers = session
        .list(options.folder.as_deref(), &since, options.limit)
        .await?;

    let cut_short = headers.len() >= options.limit;

    let fresh: Vec<Header> = headers
        .iter()
        .filter(|header| !already_seen.contains(&header.id))
        .cloned()
        .collect();

    let (candidates, already_stored) = pick(&fresh, &options, store.as_deref())?;

    let mut report = ScanReport {
        provider: "outlook",
        account: account.address.clone(),
        folder: folder_label,
        since,
        listed: headers.len(),
        candidates: candidates.clone(),
        already_stored,
        scanned: Vec::new(),
    };

    if options.dry_run {
        return Ok(report);
    }

    // Graph serves many downloads at once.
    let limit = Arc::new(Semaphore::new(options.concurrency.max(1)));

    let mut tasks = JoinSet::new();

    for (position, header) in candidates.into_iter().enumerate() {
        let session = Arc::clone(&session);
        let limit = Arc::clone(&limit);

        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;

            let mime = session.mime(&header.id).await;

            (position, header, mime)
        });
    }

    let mut downloaded = Vec::new();

    while let Some(joined) = tasks.join_next().await {
        downloaded.push(joined?);
    }

    downloaded.sort_by_key(|(position, _, _)| *position);

    let downloaded = downloaded
        .into_iter()
        .map(|(_, header, mime)| (header, mime))
        .collect();

    report.scanned =
        read_and_keep(downloaded, &account, &options, models, store.as_deref_mut()).await?;

    // Remember where this scan got to, unless it was a one-off window or
    // the listing was cut short (older messages would be skipped next time).
    if options.since.is_none()
        && !cut_short
        && let Some(newest) = headers.first()
        && let Some(store) = store
    {
        let seen_at_newest: Vec<String> = headers
            .iter()
            .filter(|header| header.received == newest.received)
            .map(|header| header.id.clone())
            .collect();

        store.set_scan_state(&state_key, &newest.received, &seen_at_newest)?;
    }

    Ok(report)
}

async fn scan_imap(
    account: accounts::Account,
    options: ScanOptions,
    models: Option<Arc<Models>>,
    mut store: Option<&mut Store>,
) -> Result<ScanReport> {
    let server = account
        .server
        .clone()
        .ok_or_else(|| anyhow::anyhow!("{} has no IMAP server; sign in again", account.key()))?;

    let password = accounts::token(&account)?;

    let mut session = imap::connect(&server, &account.address, &password).await?;

    let folder = match &options.folder {
        Some(folder) => folder.clone(),
        None => session.default_folder().await?,
    };

    let state_key = format!("{}:{folder}", account.key());

    let cursor = match (&options.since, &store) {
        (None, Some(store)) => store.scan_state(&state_key)?.map(|(cursor, _)| cursor),
        _ => None,
    };

    let since_date = options
        .since
        .clone()
        .unwrap_or_else(|| days_ago(30).chars().take(10).collect());

    let (headers, cursor_after, cut_short) = session
        .list(&folder, cursor.as_deref(), &since_date, options.limit)
        .await?;

    let (candidates, already_stored) = pick(&headers, &options, store.as_deref())?;

    let since_label = match &cursor {
        Some(_) => "the last scan".to_string(),
        None => since_date.clone(),
    };

    let mut report = ScanReport {
        provider: "imap",
        account: account.address.clone(),
        folder: folder.clone(),
        since: since_label,
        listed: headers.len(),
        candidates: candidates.clone(),
        already_stored,
        scanned: Vec::new(),
    };

    if options.dry_run {
        session.logout().await;

        return Ok(report);
    }

    // One IMAP connection answers one command at a time: download first,
    // then read the messages in parallel.
    let mut downloaded = Vec::new();

    for header in candidates {
        let mime = session.mime(&header.id).await;

        downloaded.push((header, mime));
    }

    session.logout().await;

    report.scanned =
        read_and_keep(downloaded, &account, &options, models, store.as_deref_mut()).await?;

    if options.since.is_none()
        && !cut_short
        && let Some(cursor_after) = cursor_after
        && let Some(store) = store
    {
        store.set_scan_state(&state_key, &cursor_after, &[])?;
    }

    Ok(report)
}

/// The messages worth reading: likely about money (or all, with `--all`),
/// and not already in the store (unless `--again`). Returns them and how
/// many were skipped as stored.
fn pick(
    headers: &[Header],
    options: &ScanOptions,
    store: Option<&Store>,
) -> Result<(Vec<Header>, usize)> {
    let mut candidates = Vec::new();
    let mut already_stored = 0;

    for header in headers
        .iter()
        .filter(|header| options.all || likely_about_money(header))
    {
        let stored = match (store, &header.message_id) {
            (Some(store), Some(message_id)) if !options.again => store.has_message(message_id)?,
            _ => false,
        };

        if stored {
            already_stored += 1;
        } else {
            candidates.push(header.clone());
        }
    }

    Ok((candidates, already_stored))
}

/// Reads downloaded messages through the pipeline, several at a time,
/// saves each to the store, and keeps copies with `--save`.
async fn read_and_keep(
    downloaded: Vec<(Header, Result<Vec<u8>>)>,
    account: &accounts::Account,
    options: &ScanOptions,
    models: Option<Arc<Models>>,
    mut store: Option<&mut Store>,
) -> Result<Vec<Scanned>> {
    let models = models.expect("models for a scan that reads");

    if let Some(dir) = &options.save {
        std::fs::create_dir_all(dir)?;
    }

    let limit = Arc::new(Semaphore::new(options.concurrency.max(1)));

    let mut tasks = JoinSet::new();

    for (position, (header, mime)) in downloaded.into_iter().enumerate() {
        let models = Arc::clone(&models);
        let limit = Arc::clone(&limit);
        let save = options.save.clone();

        tasks.spawn(async move {
            let _permit = limit.acquire_owned().await;

            let outcome = match mime {
                Ok(mime) => read_message(mime, &header, &models, save.as_deref()).await,
                Err(error) => Err(error),
            };

            (position, header, outcome)
        });
    }

    let mut finished = Vec::new();

    while let Some(joined) = tasks.join_next().await {
        finished.push(joined?);
    }

    finished.sort_by_key(|(position, _, _)| *position);

    let mut scanned = Vec::new();

    for (_, header, outcome) in finished {
        let result = match outcome {
            Ok(read) => {
                if let Some(store) = store.as_deref_mut() {
                    let source = Source {
                        origin: account.key(),
                        name: Some(&header.subject),
                        received: Some(&header.received),
                    };

                    store.save(&read.mime, &read.document, &source, &read.result)?;
                }

                Ok(read.result)
            }

            Err(error) => Err(error),
        };

        scanned.push(Scanned { header, result });
    }

    Ok(scanned)
}

async fn read_message(
    mime: Vec<u8>,
    header: &Header,
    models: &Models,
    save: Option<&std::path::Path>,
) -> Result<Read> {
    if let Some(dir) = save {
        let name = format!(
            "{}_{}.eml",
            header.received.get(..10).unwrap_or("unknown"),
            safe_name(&header.subject)
        );

        std::fs::write(dir.join(name), &mime)?;
    }

    let document = spendyard::read(&mime, None)?;

    let result = spendyard::understand(&document, &models.pipeline()).await?;

    Ok(Read {
        mime,
        document,
        result,
    })
}

/// A JSON line for one scanned message: the result plus where it came from.
pub fn result_line(report: &ScanReport, scanned: &Scanned, result: &ReadResult) -> Value {
    let mut line = serde_json::to_value(result).unwrap_or_default();

    line["source"] = json!({
        "mailbox": report.provider,
        "account": report.account,
        "message_id": scanned.header.message_id,
        "received": scanned.header.received,
        "from": scanned.header.from,
        "subject": scanned.header.subject,
    });

    line
}

fn safe_name(subject: &str) -> String {
    let kept: String = subject
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    let squeezed = kept
        .split('-')
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    squeezed.chars().take(60).collect()
}

/// `2026-01-01` → `2026-01-01T00:00:00Z`; full timestamps pass through.
fn date_to_rfc3339(date: &str) -> String {
    if date.contains('T') {
        return date.to_string();
    }

    format!("{date}T00:00:00Z")
}

/// Midnight UTC `days` days ago, as RFC 3339.
fn days_ago(days: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);

    let day = (now / 86_400).saturating_sub(days) as i64;

    let (year, month, date) = civil_from_days(day);

    format!("{year:04}-{month:02}-{date:02}T00:00:00Z")
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;

    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);

    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;

    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);

    let month_index = (5 * day_of_year + 2) / 153;

    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;

    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;

    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(subject: &str, from: &str, preview: &str, attachments: bool) -> Header {
        Header {
            id: "x".to_string(),
            message_id: None,
            subject: subject.to_string(),
            from: from.to_string(),
            received: "2026-11-19T09:00:00Z".to_string(),
            preview: preview.to_string(),
            has_attachments: attachments,
        }
    }

    #[test]
    fn pre_filter_keeps_receipts_and_drops_chatter() {
        assert!(likely_about_money(&header(
            "Your receipt from Anthropic",
            "invoice+statements@mail.anthropic.com",
            "",
            false
        )));
        assert!(likely_about_money(&header(
            "Kvittering fra groundcover",
            "noam@groundcover.com",
            "",
            false
        )));
        assert!(likely_about_money(&header(
            "Fieldmade, your plan has renewed",
            "feedback@slack.com",
            "",
            true
        )));
        assert!(likely_about_money(&header(
            "Hello",
            "noreply@stripe.com",
            "",
            false
        )));

        assert!(!likely_about_money(&header(
            "Standup notes",
            "kari@fjordtech.example",
            "Agenda for Monday",
            false
        )));
        assert!(!likely_about_money(&header(
            "Your login code",
            "notifications@linear.app",
            "Code 482-913",
            false
        )));
    }

    #[test]
    fn dates() {
        assert_eq!(date_to_rfc3339("2026-01-01"), "2026-01-01T00:00:00Z");
        assert_eq!(civil_from_days(20_411), (2025, 11, 19));
        assert!(days_ago(0).ends_with("T00:00:00Z"));
    }

    #[test]
    fn safe_names() {
        assert_eq!(
            safe_name("Your receipt from Render #2406-8350"),
            "Your-receipt-from-Render-2406-8350"
        );
    }
}
