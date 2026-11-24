//! The local store: `~/.spendyard/spendyard.db`, SQLite.
//!
//! Every document read is kept once (by SHA-256, and by Message-ID for
//! mail), with every result read from it; the newest result also fills
//! `receipts`, `lines` and `payments`, the columns reports use. Edits by
//! hand live apart from results, so re-reading never loses them.

pub mod rates;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use spendyard_core::payments::{Evidence, Payment, Records};
use spendyard_core::rows::{self, ChargeRow, Rows};
use spendyard_core::{Document, ReadResult};

use super::config;

/// Bump with every change to the tables, and add a step to `migrate`.
const SCHEMA_VERSION: i64 = 5;

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

-- Payments (spendyard_core::payments): which payment each piece of evidence
-- is, a document (document_id) or a statement's line (statement_id and
-- position). The grouping is worked out again from the store; what's kept is
-- each payment's ID, a number never given to another, so links to one hold
-- when its evidence changes. A deleted document leaves its row with nothing
-- in it, so the payment is still known, and found gone.
CREATE TABLE IF NOT EXISTS payment_evidence (
    payment_id    INTEGER NOT NULL,
    document_id   INTEGER REFERENCES documents (id) ON DELETE SET NULL,
    statement_id  INTEGER REFERENCES documents (id) ON DELETE SET NULL,
    position      INTEGER
);

CREATE INDEX IF NOT EXISTS payment_evidence_payment ON payment_evidence (payment_id);

-- A payment retired: moved into another (links to from_id go to to_id), or
-- gone (to_id NULL: its evidence is all gone). Its number isn't given again.
CREATE TABLE IF NOT EXISTS payment_moves (
    from_id  INTEGER PRIMARY KEY,
    to_id    INTEGER
);

CREATE TABLE IF NOT EXISTS scans (
    key             TEXT PRIMARY KEY,
    newest          TEXT NOT NULL,
    seen_at_newest  TEXT NOT NULL,
    updated         TEXT NOT NULL
);
"#;

/// The same charge from two documents: the same statement sent twice, or
/// an extract of one. Matched on card, date and amount, plus the row
/// number when the statement numbers its lines, or the merchant when not.
const SAME_CHARGE: &str = "d.document_id != c.document_id
    AND d.date = c.date
    AND d.amount = c.amount
    AND d.card_last4 IS c.card_last4
    AND ((c.row IS NOT NULL AND d.row = c.row)
         OR (c.row IS NULL AND d.row IS NULL AND d.merchant = c.merchant))";

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
            // Version 4 numbered payments with text; from 5 with numbers. Nothing filled them yet.
            if version == 4 {
                self.connection
                    .execute_batch("DROP TABLE payment_evidence; DROP TABLE payment_moves;")?;
            }

            self.connection.execute_batch(SCHEMA)?;

            // Version 2 kept a charge once per document; from 3 once in all.
            if version < 3 {
                remove_copied_charges(&self.connection)?;
            }

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

        let rows = rows::rows(result, fallback_date.as_deref());

        write_receipt(&transaction, document_id, result_id, &rows)?;

        if let Some(charges) = &rows.charges {
            write_charges(&transaction, document_id, charges)?;
        }

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

    /// The payments kept, each one's number and evidence, and the numbers
    /// retired (moved into another, or gone), which aren't given again.
    pub fn stored_payments(&self) -> Result<(Vec<Payment>, Vec<i64>)> {
        let mut by_id: BTreeMap<i64, Evidence> = BTreeMap::new();

        let mut statement = self.connection.prepare(
            "SELECT payment_id, document_id, statement_id, position FROM payment_evidence",
        )?;

        let rows = statement.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let document: Option<i64> = row.get(1)?;
            let statement: Option<i64> = row.get(2)?;
            let position: Option<i64> = row.get(3)?;

            Ok((id, document, statement, position))
        })?;

        for row in rows {
            let (id, document, statement, position) = row?;

            // A deleted document leaves its row with nothing in it: the payment is still known.
            let evidence = by_id.entry(id).or_default();

            if let Some(document) = document {
                evidence.documents.push(document);
            }

            if let (Some(statement), Some(position)) = (statement, position) {
                evidence.charges.push(format!("{statement}:{position}"));
            }
        }

        let payments = by_id
            .into_iter()
            .map(|(id, evidence)| Payment { id, evidence })
            .collect();

        let retired = self
            .connection
            .prepare("SELECT from_id FROM payment_moves")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?;

        Ok((payments, retired))
    }

    /// Keeps the payments as the core grouped them again: each one's
    /// evidence, in place of what was kept; and those retired, moved into
    /// another or gone, beside those retired before.
    pub fn save_payments(&self, records: &Records) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;

        transaction.execute("DELETE FROM payment_evidence", [])?;

        for payment in &records.payments {
            for document in &payment.evidence.documents {
                transaction.execute(
                    "INSERT INTO payment_evidence (payment_id, document_id) VALUES (?1, ?2)",
                    params![payment.id, document],
                )?;
            }

            for charge in &payment.evidence.charges {
                let Some((statement, position)) = charge.split_once(':') else {
                    continue;
                };

                transaction.execute(
                    "INSERT INTO payment_evidence (payment_id, statement_id, position) VALUES (?1, ?2, ?3)",
                    params![payment.id, statement.parse::<i64>()?, position.parse::<i64>()?],
                )?;
            }
        }

        let moved = records
            .moved
            .iter()
            .map(|moved| (moved.from, Some(moved.to)));
        let gone = records.gone.iter().map(|&id| (id, None));

        for (from, to) in moved.chain(gone) {
            transaction.execute(
                "INSERT INTO payment_moves (from_id, to_id) VALUES (?1, ?2)
                 ON CONFLICT (from_id) DO UPDATE SET to_id = ?2",
                params![from, to],
            )?;
        }

        transaction.commit()?;

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
/// Deletes every charge that another document already has.
fn remove_copied_charges(connection: &Connection) -> Result<usize> {
    let removed = connection.execute(
        &format!(
            "DELETE FROM charges WHERE id IN (
                 SELECT c.id FROM charges c
                 WHERE EXISTS (SELECT 1 FROM charges d WHERE d.id < c.id AND {SAME_CHARGE}))"
        ),
        [],
    )?;

    Ok(removed)
}

/// A statement's charges, replacing any read from the same document
/// before, and leaving out those another document already has (the same
/// statement sent twice). Matches to receipts are made by `charges match`.
fn write_charges(
    transaction: &rusqlite::Transaction<'_>,
    document_id: i64,
    charges: &[ChargeRow],
) -> Result<()> {
    transaction.execute(
        "DELETE FROM charges WHERE document_id = ?1",
        params![document_id],
    )?;

    for charge in charges {
        transaction.execute(
            &format!(
                "INSERT INTO charges (document_id, position, issuer, row, date, posted, merchant,
                 card_last4, name_on_card, amount, currency, original_amount, original_currency,
                 rate, kind, vendor_id, vendor_name)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
             WHERE NOT EXISTS (
                 SELECT 1 FROM charges d,
                     (SELECT ?1 AS document_id, ?5 AS date, ?10 AS amount, ?8 AS card_last4,
                             ?4 AS row, ?7 AS merchant) c
                 WHERE {SAME_CHARGE})"
            ),
            params![
                document_id,
                charge.position,
                charge.issuer,
                charge.row,
                charge.date,
                charge.posted,
                charge.merchant,
                charge.card_last4,
                charge.name_on_card,
                charge.amount,
                charge.currency,
                charge.original_amount,
                charge.original_currency,
                charge.rate,
                charge.kind,
                charge.vendor_id,
                charge.vendor_name,
            ],
        )?;
    }

    Ok(())
}

/// Replaces the document's `receipts`, `lines` and `payments` with these.
fn write_receipt(
    transaction: &rusqlite::Transaction<'_>,
    document_id: i64,
    result_id: i64,
    rows: &Rows,
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

    let receipt = &rows.receipt;

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
            receipt.r#type,
            receipt.status,
            receipt.is_receipt,
            receipt.is_invoice,
            receipt.vendor_id,
            receipt.vendor_name,
            receipt.processor,
            receipt.account_email,
            receipt.account_workspace,
            receipt.date,
            receipt.total,
            receipt.tax,
            receipt.currency,
            receipt.billing_model,
            receipt.interval,
            receipt.period_start,
            receipt.period_end,
            receipt.next_renewal,
            receipt.invoice_number,
            receipt.receipt_number,
            receipt.card_brand,
            receipt.card_last4,
            receipt.seats,
            receipt.flags,
        ],
    )?;

    for line in &rows.lines {
        transaction.execute(
            "INSERT INTO lines (document_id, position, kind, product, plan, description, quantity, unit, unit_price, amount, interval)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                document_id,
                line.position,
                line.kind,
                line.product,
                line.plan,
                line.description,
                line.quantity,
                line.unit,
                line.unit_price,
                line.amount,
                line.interval,
            ],
        )?;
    }

    for payment in &rows.payments {
        transaction.execute(
            "INSERT INTO payments (document_id, position, method, brand, last4, amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                document_id,
                payment.position,
                payment.method,
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

// Calendar days, as the core counts them.
pub use spendyard_core::dates::civil_from_days;

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
            "{}/crates/core/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn keeps_payments_and_their_numbers_when_their_document_is_deleted() {
        let (mut store, dir) = temporary_store();

        let bytes = fixture("receipt.eml");
        let document = spendyard_core::read(&bytes, None).unwrap();

        let result = spendyard_core::result::not_about_money(
            spendyard_core::extract::schema::DocumentType::Other,
            0.9,
        );

        let source = Source {
            origin: "file:receipt.eml".to_string(),
            name: None,
            received: None,
        };

        let id = store.save(&bytes, &document, &source, &result).unwrap();

        let kept = Records {
            payments: vec![Payment {
                id: 1,
                evidence: Evidence {
                    documents: vec![id],
                    charges: vec![format!("{id}:4")],
                },
            }],
            ..Records::default()
        };

        store.save_payments(&kept).unwrap();

        let (payments, retired) = store.stored_payments().unwrap();

        assert_eq!(payments, kept.payments);
        assert!(retired.is_empty());

        // Its document deleted: the payment is still known, with nothing to show for it.
        store
            .connection
            .execute("DELETE FROM documents WHERE id = ?1", params![id])
            .unwrap();

        let (payments, _) = store.stored_payments().unwrap();

        assert_eq!(payments.len(), 1);
        assert!(
            payments[0].evidence.documents.is_empty() && payments[0].evidence.charges.is_empty()
        );

        // Found gone: retired, its number not given again.
        let gone = Records {
            gone: vec![1],
            ..Records::default()
        };

        store.save_payments(&gone).unwrap();

        let (payments, retired) = store.stored_payments().unwrap();

        assert!(payments.is_empty());
        assert_eq!(retired, [1]);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn saves_once_per_document_and_keeps_every_result() {
        let (mut store, dir) = temporary_store();

        let bytes = fixture("receipt.eml");

        let document = spendyard_core::read(&bytes, None).unwrap();

        let result = spendyard_core::result::not_about_money(
            spendyard_core::extract::schema::DocumentType::Other,
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
    fn a_statement_sent_twice_keeps_its_charges_once() {
        use spendyard_core::statement::{ChargeOutput, StatementOutput, assemble};

        let (mut store, dir) = temporary_store();

        let charge = |row: u32, amount: &str| ChargeOutput {
            row: Some(row),
            date: "2026-04-14".to_string(),
            merchant: "VERCEL INC., COVINA, US".to_string(),
            card_last4: Some("3776".to_string()),
            amount: amount.to_string(),
            ..ChargeOutput::default()
        };

        let whole = StatementOutput {
            is_card_statement: true,
            charges: vec![charge(1, "22.01"), charge(2, "192.07"), charge(3, "192.07")],
            ..StatementOutput::default()
        };

        let extract = StatementOutput {
            is_card_statement: true,
            charges: vec![charge(2, "192.07")],
            ..StatementOutput::default()
        };

        let mut result = spendyard_core::result::not_about_money(
            spendyard_core::extract::schema::DocumentType::CardStatement,
            0.9,
        );

        let source = Source {
            origin: "file:statement.pdf".to_string(),
            name: None,
            received: None,
        };

        for (bytes, output) in [
            (b"first copy".to_vec(), whole.clone()),
            (b"second copy".to_vec(), whole),
            (b"an extract".to_vec(), extract),
        ] {
            let document = spendyard_core::read(&bytes, None).unwrap();

            result.statement = Some(assemble(vec![output]));

            store.save(&bytes, &document, &source, &result).unwrap();
        }

        let charges: i64 = store
            .connection
            .query_row("SELECT count(*) FROM charges", [], |row| row.get(0))
            .unwrap();

        assert_eq!(charges, 3);

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
