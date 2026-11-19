//! The local store: `~/.spendyard/spendyard.db`, SQLite.
//!
//! Every document read is kept once (by SHA-256, and by Message-ID for
//! mail), with every result read from it; the newest result also fills
//! `receipts`, `lines` and `payments`, the columns reports use. Edits by
//! hand live apart from results, so re-reading never loses them.

pub mod rates;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use spendyard::{Document, ReadResult};

use super::config;

/// Bump with every change to the tables, and add a step to `migrate`.
const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS documents (
    id          INTEGER PRIMARY KEY,
    sha256      TEXT NOT NULL UNIQUE,
    message_id  TEXT,
    source      TEXT NOT NULL,
    name        TEXT,
    sender      TEXT,
    received    TEXT,
    format      TEXT,
    size        INTEGER,
    first_seen  TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS documents_message_id ON documents (message_id);

CREATE TABLE IF NOT EXISTS results (
    id                 INTEGER PRIMARY KEY,
    document_id        INTEGER NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    created            TEXT NOT NULL,
    spendyard_version  TEXT,
    schema_version     TEXT,
    decider            TEXT,
    extractor          TEXT,
    model              TEXT,
    json               TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS receipts (
    document_id        INTEGER PRIMARY KEY REFERENCES documents (id) ON DELETE CASCADE,
    result_id          INTEGER NOT NULL,
    type               TEXT,
    status             TEXT,
    is_receipt         INTEGER,
    is_invoice         INTEGER,
    vendor_id          TEXT,
    vendor_name        TEXT,
    processor          TEXT,
    account_email      TEXT,
    account_workspace  TEXT,
    date               TEXT,
    total              TEXT,
    tax                TEXT,
    currency           TEXT,
    billing_model      TEXT,
    interval           TEXT,
    period_start       TEXT,
    period_end         TEXT,
    next_renewal       TEXT,
    invoice_number     TEXT,
    receipt_number     TEXT,
    card_brand         TEXT,
    card_last4         TEXT,
    seats              REAL,
    flags              INTEGER
);

CREATE TABLE IF NOT EXISTS lines (
    document_id  INTEGER NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    kind         TEXT,
    product      TEXT,
    plan         TEXT,
    description  TEXT,
    quantity     REAL,
    unit         TEXT,
    unit_price   TEXT,
    amount       TEXT,
    interval     TEXT,
    PRIMARY KEY (document_id, position)
);

CREATE TABLE IF NOT EXISTS payments (
    document_id  INTEGER NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    method       TEXT,
    brand        TEXT,
    last4        TEXT,
    amount       TEXT,
    PRIMARY KEY (document_id, position)
);

CREATE TABLE IF NOT EXISTS edits (
    document_id  INTEGER PRIMARY KEY REFERENCES documents (id) ON DELETE CASCADE,
    personal     INTEGER NOT NULL DEFAULT 0,
    ignored      INTEGER NOT NULL DEFAULT 0,
    vendor_id    TEXT,
    vendor_name  TEXT,
    note         TEXT,
    updated      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rates (
    date      TEXT NOT NULL,
    currency  TEXT NOT NULL,
    per_eur   REAL NOT NULL,
    PRIMARY KEY (date, currency)
);

CREATE TABLE IF NOT EXISTS charges (
    id                 INTEGER PRIMARY KEY,
    document_id        INTEGER NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    position           INTEGER NOT NULL,
    issuer             TEXT,
    row                INTEGER,
    date               TEXT NOT NULL,
    posted             TEXT,
    merchant           TEXT NOT NULL,
    card_last4         TEXT,
    name_on_card       TEXT,
    amount             TEXT NOT NULL,
    currency           TEXT,
    original_amount    TEXT,
    original_currency  TEXT,
    rate               TEXT,
    kind               TEXT NOT NULL,
    vendor_id          TEXT,
    vendor_name        TEXT,
    receipt_id         INTEGER REFERENCES documents (id) ON DELETE SET NULL,
    UNIQUE (document_id, position)
);

CREATE INDEX IF NOT EXISTS charges_card_date ON charges (card_last4, date);

CREATE TABLE IF NOT EXISTS scans (
    key             TEXT PRIMARY KEY,
    newest          TEXT NOT NULL,
    seen_at_newest  TEXT NOT NULL,
    updated         TEXT NOT NULL
);
"#;

pub struct Store {
    pub connection: Connection,
}

/// Where a document came from.
pub struct Source<'a> {
    /// `file:<path>` or `outlook:<account>`.
    pub origin: String,

    /// A file name or a subject.
    pub name: Option<&'a str>,

    /// When it arrived, RFC 3339; the email's date otherwise.
    pub received: Option<&'a str>,
}

pub fn path() -> PathBuf {
    config::home().join("spendyard.db")
}

impl Store {
    pub fn open() -> Result<Store> {
        Store::open_at(&path())
    }

    pub fn open_at(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let connection =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;

        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(10))?;

        let store = Store { connection };

        store.migrate()?;

        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?;

        if version < SCHEMA_VERSION {
            self.connection.execute_batch(SCHEMA)?;
            self.connection
                .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }

        Ok(())
    }

    /// Whether a mail message is already in the store with a result.
    pub fn has_message(&self, message_id: &str) -> Result<bool> {
        let found: Option<i64> = self
            .connection
            .query_row(
                "SELECT d.id FROM documents d JOIN results r ON r.document_id = d.id
                 WHERE d.message_id = ?1 LIMIT 1",
                params![normalize_message_id(message_id)],
                |row| row.get(0),
            )
            .optional()?;

        Ok(found.is_some())
    }

    /// Saves a document and its result. The result becomes the document's
    /// current one. Returns the document's ID.
    pub fn save(
        &mut self,
        bytes: &[u8],
        document: &Document,
        source: &Source<'_>,
        result: &ReadResult,
    ) -> Result<i64> {
        let transaction = self.connection.transaction()?;

        let document_id = upsert_document(&transaction, bytes, document, source)?;

        let json = serde_json::to_string(result)?;

        transaction.execute(
            "INSERT INTO results (document_id, created, spendyard_version, schema_version, decider, extractor, model, json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                document_id,
                now(),
                result.meta.spendyard_version,
                result.meta.schema_version,
                result.meta.decider,
                result.meta.extractor,
                result.meta.model,
                json,
            ],
        )?;

        let result_id = transaction.last_insert_rowid();

        let fallback_date = source.received.map(str::to_string).or_else(|| {
            document
                .headers
                .as_ref()
                .and_then(|headers| headers.date.clone())
        });

        write_receipt(&transaction, document_id, result_id, result, fallback_date)?;

        write_charges(&transaction, document_id, result)?;

        transaction.commit()?;

        Ok(document_id)
    }

    /// The scan position for a mailbox folder.
    pub fn scan_state(&self, key: &str) -> Result<Option<(String, Vec<String>)>> {
        let row: Option<(String, String)> = self
            .connection
            .query_row(
                "SELECT newest, seen_at_newest FROM scans WHERE key = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        Ok(row.map(|(newest, seen)| (newest, serde_json::from_str(&seen).unwrap_or_default())))
    }

    pub fn set_scan_state(&self, key: &str, newest: &str, seen_at_newest: &[String]) -> Result<()> {
        self.connection.execute(
            "INSERT INTO scans (key, newest, seen_at_newest, updated) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (key) DO UPDATE SET newest = ?2, seen_at_newest = ?3, updated = ?4",
            params![key, newest, serde_json::to_string(seen_at_newest)?, now()],
        )?;

        Ok(())
    }
}

/// Finds the document by SHA-256, then by Message-ID (the same mail
/// saved twice, or fetched again), or inserts it.
fn upsert_document(
    transaction: &rusqlite::Transaction<'_>,
    bytes: &[u8],
    document: &Document,
    source: &Source<'_>,
) -> Result<i64> {
    let sha256 = hex(&Sha256::digest(bytes));

    let headers = document.headers.as_ref();

    let message_id = headers
        .and_then(|headers| headers.message_id.as_deref())
        .map(normalize_message_id);

    let existing: Option<i64> = transaction
        .query_row(
            "SELECT id FROM documents WHERE sha256 = ?1",
            params![sha256],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(id) = existing {
        return Ok(id);
    }

    if let Some(message_id) = &message_id {
        let by_message: Option<i64> = transaction
            .query_row(
                "SELECT id FROM documents WHERE message_id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = by_message {
            return Ok(id);
        }
    }

    let name = source
        .name
        .map(str::to_string)
        .or_else(|| headers.and_then(|headers| headers.subject.clone()));

    let received = source
        .received
        .map(str::to_string)
        .or_else(|| headers.and_then(|headers| headers.date.clone()));

    transaction.execute(
        "INSERT INTO documents (sha256, message_id, source, name, sender, received, format, size, first_seen)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            sha256,
            message_id,
            source.origin,
            name,
            headers.and_then(|headers| headers.from.clone()),
            received,
            serde_json::to_value(document.format)?.as_str().map(str::to_string),
            bytes.len() as i64,
            now(),
        ],
    )?;

    Ok(transaction.last_insert_rowid())
}

/// Replaces the document's report rows with this result's.
/// A statement's charges, replacing any read from the same document
/// before. Matches to receipts are made by `charges match`.
fn write_charges(
    transaction: &rusqlite::Transaction<'_>,
    document_id: i64,
    result: &ReadResult,
) -> Result<()> {
    let Some(statement) = &result.statement else {
        return Ok(());
    };

    transaction.execute(
        "DELETE FROM charges WHERE document_id = ?1",
        params![document_id],
    )?;

    let catalog = spendyard::catalog::catalog();

    for (position, charge) in statement.charges.iter().enumerate() {
        let vendor = catalog.by_merchant(&charge.merchant);

        let kind = serde_json::to_value(charge.kind)?
            .as_str()
            .unwrap_or("purchase")
            .to_string();

        transaction.execute(
            "INSERT INTO charges (document_id, position, issuer, row, date, posted, merchant,
                 card_last4, name_on_card, amount, currency, original_amount, original_currency,
                 rate, kind, vendor_id, vendor_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                document_id,
                position as i64,
                statement.issuer,
                charge.row,
                charge.date,
                charge.posted,
                charge.merchant,
                charge.card_last4,
                charge.name_on_card,
                charge.amount,
                statement.currency,
                charge.original_amount,
                charge.original_currency,
                charge.rate,
                kind,
                vendor.map(|vendor| vendor.id.clone()),
                vendor.map(|vendor| vendor.name.clone()),
            ],
        )?;
    }

    Ok(())
}

fn write_receipt(
    transaction: &rusqlite::Transaction<'_>,
    document_id: i64,
    result_id: i64,
    result: &ReadResult,
    fallback_date: Option<String>,
) -> Result<()> {
    transaction.execute(
        "DELETE FROM receipts WHERE document_id = ?1",
        params![document_id],
    )?;
    transaction.execute(
        "DELETE FROM lines WHERE document_id = ?1",
        params![document_id],
    )?;
    transaction.execute(
        "DELETE FROM payments WHERE document_id = ?1",
        params![document_id],
    )?;

    let word = |value: Value| value.as_str().map(str::to_string);

    let vendor = result.vendor.as_ref();
    let amount = result.amount.as_ref();
    let billing = result.billing.as_ref();
    let card = result
        .payments
        .iter()
        .find(|payment| payment.last4.is_some());

    let date = result
        .issued
        .clone()
        .or_else(|| fallback_date.map(|date| date.chars().take(10).collect()));

    let seats: Option<f64> = {
        let quantities: Vec<f64> = result
            .lines
            .iter()
            .filter(|line| line.kind == spendyard::extract::schema::LineKind::Seat)
            .filter_map(|line| line.quantity)
            .collect();

        (!quantities.is_empty()).then(|| quantities.iter().sum())
    };

    transaction.execute(
        "INSERT INTO receipts (
            document_id, result_id, type, status, is_receipt, is_invoice,
            vendor_id, vendor_name, processor, account_email, account_workspace,
            date, total, tax, currency, billing_model, interval, period_start, period_end, next_renewal,
            invoice_number, receipt_number, card_brand, card_last4, seats, flags
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
        params![
            document_id,
            result_id,
            word(serde_json::to_value(result.document.kind)?),
            word(serde_json::to_value(result.document.status)?),
            result.document.is_receipt,
            result.document.is_invoice,
            vendor.and_then(|vendor| vendor.id.clone()),
            vendor.map(|vendor| vendor.name.clone()),
            vendor.and_then(|vendor| vendor.processor.clone()),
            vendor.and_then(|vendor| vendor.account.email.clone()),
            vendor.and_then(|vendor| vendor.account.workspace.clone()),
            date,
            amount.and_then(|amount| amount.total.clone()),
            amount.and_then(|amount| amount.tax.clone()),
            amount.and_then(|amount| amount.currency.clone()),
            billing.and_then(|billing| word(serde_json::to_value(billing.model).ok()?)),
            billing.and_then(|billing| billing.interval).and_then(|interval| word(serde_json::to_value(interval).ok()?)),
            billing.and_then(|billing| billing.period.as_ref()).and_then(|period| period.start.clone()),
            billing.and_then(|billing| billing.period.as_ref()).and_then(|period| period.end.clone()),
            billing.and_then(|billing| billing.next_renewal.clone()),
            result.invoice_number,
            result.receipt_number,
            card.and_then(|payment| payment.brand.clone()),
            card.and_then(|payment| payment.last4.clone()),
            seats,
            result.flags.len() as i64,
        ],
    )?;

    for (position, line) in result.lines.iter().enumerate() {
        transaction.execute(
            "INSERT INTO lines (document_id, position, kind, product, plan, description, quantity, unit, unit_price, amount, interval)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                document_id,
                position as i64,
                word(serde_json::to_value(line.kind)?),
                line.product,
                line.plan,
                line.description,
                line.quantity,
                line.unit,
                line.unit_price,
                line.amount,
                line.interval.and_then(|interval| word(serde_json::to_value(interval).ok()?)),
            ],
        )?;
    }

    for (position, payment) in result.payments.iter().enumerate() {
        transaction.execute(
            "INSERT INTO payments (document_id, position, method, brand, last4, amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                document_id,
                position as i64,
                word(serde_json::to_value(payment.method)?),
                payment.brand,
                payment.last4,
                payment.amount,
            ],
        )?;
    }

    Ok(())
}

/// `<abc@example.com>` and `abc@example.com` are the same Message-ID.
pub fn normalize_message_id(message_id: &str) -> String {
    message_id.trim().trim_matches(['<', '>']).to_lowercase()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The current time, RFC 3339 in UTC, to the second.
pub fn now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);

    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);

    let time = seconds % 86_400;

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
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

    fn temporary_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "spendyard-test-{}-{}",
            std::process::id(),
            rand_suffix()
        ));

        let store = Store::open_at(&dir.join("test.db")).unwrap();

        (store, dir)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn saves_once_per_document_and_keeps_every_result() {
        let (mut store, dir) = temporary_store();

        let bytes = fixture("receipt.eml");

        let document = spendyard::read(&bytes, None).unwrap();

        let result = spendyard::result::not_about_money(
            spendyard::extract::schema::DocumentType::Other,
            0.9,
        );

        let source = Source {
            origin: "file:receipt.eml".to_string(),
            name: None,
            received: None,
        };

        let first = store.save(&bytes, &document, &source, &result).unwrap();
        let second = store.save(&bytes, &document, &source, &result).unwrap();

        assert_eq!(first, second);

        let results: i64 = store
            .connection
            .query_row("SELECT count(*) FROM results", [], |row| row.get(0))
            .unwrap();

        assert_eq!(results, 2);

        let receipts: i64 = store
            .connection
            .query_row("SELECT count(*) FROM receipts", [], |row| row.get(0))
            .unwrap();

        assert_eq!(receipts, 1);

        assert!(
            store
                .has_message("<receipt-2291-0007@acme.example>")
                .unwrap()
        );

        let name: String = store
            .connection
            .query_row("SELECT name FROM documents", [], |row| row.get(0))
            .unwrap();

        assert_eq!(name, "Your receipt from Acme Cloud, Inc. #2291-0007");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn scan_state_round_trips() {
        let (store, dir) = temporary_store();

        assert!(store.scan_state("outlook:a:Inbox").unwrap().is_none());

        store
            .set_scan_state(
                "outlook:a:Inbox",
                "2026-11-19T09:00:00Z",
                &["x".to_string()],
            )
            .unwrap();

        let (newest, seen) = store.scan_state("outlook:a:Inbox").unwrap().unwrap();

        assert_eq!(newest, "2026-11-19T09:00:00Z");
        assert_eq!(seen, ["x"]);

        std::fs::remove_dir_all(dir).ok();
    }
}
