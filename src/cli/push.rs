//! `spendyard cloud push`: what this machine read, sent to the organisation
//! it acts in (the platform's docs/push-pull.md). Each document goes as the
//! contract has it: what's known of the file, its reading as a `Reading`
//! (made by the core, as the cloud makes one), its corrections as a
//! `DocumentEdit`; then its original, when the cloud asks for it.
//!
//! What was sent is kept in `~/.spendyard/cloud.db`, by server, organisation
//! and SHA-256, so a push sends only what's new or changed since: a document
//! not sent yet, one read again, one corrected again, an original the cloud
//! still wants. `--dry-run` asks the cloud what it would do, and sends and
//! keeps nothing.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::cli::cloud::Cloud;
use crate::cli::config;
use crate::cli::store::Store;

/// Documents a call: the contract takes up to 200, and a reading's JSON is a
/// few kilobytes.
const BATCH: usize = 100;

/// Originals put at once: most are small, and each waits on the network.
const ORIGINALS_AT_ONCE: usize = 8;

/// The largest original the cloud takes.
const LARGEST_ORIGINAL: i64 = 25 * 1024 * 1024;

pub struct PushOptions {
    pub dry_run: bool,
}

/// One document of the store, as a push sends it.
struct Local {
    sha256: String,
    message_id: Option<String>,
    name: Option<String>,
    sender: Option<String>,
    received: Option<String>,
    format: Option<String>,
    size: Option<i64>,
    source: String,

    /// When its current reading was made, and the result's JSON.
    read_at: String,
    result: String,

    /// Its edit, when it has one, with when it was made.
    edit: Option<Value>,
    edit_updated: Option<String>,
}

/// What was last sent of a document to this server and organisation.
#[derive(Default)]
struct Sent {
    read_at: Option<String>,
    edit_updated: Option<String>,
    original: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pushed {
    documents: Vec<PushedResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushedResult {
    sha256: String,

    #[serde(default)]
    document_id: i64,

    #[serde(default)]
    outcome: String,

    #[serde(default)]
    original_wanted: bool,

    original_upload: Option<OriginalUpload>,
}

#[derive(Deserialize)]
struct OriginalUpload {
    url: String,

    #[serde(default)]
    headers: HashMap<String, String>,
}

#[derive(Deserialize)]
struct Confirmed {
    #[serde(default)]
    kept: Vec<String>,
}

/// What a push did, counted, for the end.
#[derive(Default)]
struct Tally {
    added: usize,
    updated: usize,
    unchanged: usize,
    originals: usize,

    /// Originals the cloud wanted and this machine hasn't got: their names.
    missing: Vec<String>,
}

/// A line on stderr that says how far a push has got, rewritten in place;
/// only in a terminal, so a script or a pipe gets the summary alone.
struct Progress {
    shown: bool,
    org: String,
    total: usize,
    dry_run: bool,
}

impl Progress {
    fn new(org: &str, total: usize, dry_run: bool) -> Progress {
        Progress {
            shown: std::io::stderr().is_terminal(),
            org: org.to_string(),
            total,
            dry_run,
        }
    }

    /// `Pushing to Testing AS: 300 of 1884 documents, 250 originals`.
    fn show(&self, documents: usize, originals: usize) {
        if !self.shown {
            return;
        }

        let verb = if self.dry_run { "Asking" } else { "Pushing to" };
        let line = if self.dry_run {
            format!(
                "{verb} {}: {documents} of {} documents",
                self.org, self.total
            )
        } else {
            format!(
                "{verb} {}: {documents} of {} documents, {originals} originals",
                self.org, self.total
            )
        };

        let mut stderr = std::io::stderr();

        let _ = write!(stderr, "\r\x1b[2K{line}");
        let _ = stderr.flush();
    }

    /// A line of its own, above the progress.
    fn note(&self, line: &str) {
        self.clear();

        eprintln!("{line}");
    }

    fn clear(&self) {
        if !self.shown {
            return;
        }

        let mut stderr = std::io::stderr();

        let _ = write!(stderr, "\r\x1b[2K");
        let _ = stderr.flush();
    }
}

/// A push that stops on an error leaves no half line before it.
impl Drop for Progress {
    fn drop(&mut self) {
        self.clear();
    }
}

/// `spendyard cloud push`.
pub async fn push(options: PushOptions) -> Result<()> {
    let cloud = Cloud::connect()?;
    let store = Store::open()?;
    let sent_db = SentDb::open()?;

    let signed_in = cloud.signed_in_as();
    let place = (signed_in.server.clone(), signed_in.org.id.clone());

    let local = documents(&store)?;
    let sent = sent_db.sent(&place)?;

    let due: Vec<&Local> = local
        .iter()
        .filter(|document| is_due(&store, document, sent.get(&document.sha256)))
        .collect();

    if due.is_empty() {
        println!(
            "Nothing to push to {}: all {} documents are there as they are here",
            signed_in.org.name,
            local.len()
        );

        return Ok(());
    }

    let mut tally = Tally::default();
    let progress = Progress::new(&signed_in.org.name, due.len(), options.dry_run);
    let mut done = 0;

    progress.show(done, 0);

    for batch in due.chunks(BATCH) {
        let pushed = push_batch(&cloud, batch, options.dry_run).await?;

        let by_sha: HashMap<&str, &Local> = batch
            .iter()
            .map(|document| (document.sha256.as_str(), *document))
            .collect();

        let originals = if options.dry_run {
            Vec::new()
        } else {
            send_originals(
                &cloud, &store, &pushed, &by_sha, &mut tally, &progress, done,
            )
            .await?
        };

        for result in &pushed.documents {
            count(&mut tally, &store, result, &by_sha, options.dry_run);

            if !options.dry_run {
                let document = by_sha[result.sha256.as_str()];
                let original = !result.original_wanted || originals.contains(&result.sha256);

                sent_db.mark(&place, document, result.document_id, original)?;
            }
        }

        done += batch.len();

        progress.show(done, tally.originals);
    }

    progress.clear();

    report(&tally, &signed_in.org.name, options.dry_run);

    Ok(())
}

/// The organisation this machine acts in and how many documents the next
/// push would send it, when it's signed in: for `store info`.
pub(crate) fn waiting(store: &Store) -> Result<Option<(String, usize)>> {
    let Some(signed_in) = crate::cli::cloud::signed_in()? else {
        return Ok(None);
    };

    let place = (signed_in.server.clone(), signed_in.org.id.clone());
    let sent = SentDb::open()?.sent(&place)?;

    let due = documents(store)?
        .iter()
        .filter(|document| is_due(store, document, sent.get(&document.sha256)))
        .count();

    Ok(Some((signed_in.org.name, due)))
}

/// Whether a document has anything the cloud hasn't had from here: all of
/// it, a newer reading, a newer edit, or its original, while there's one
/// here to send (one that isn't stays the cloud's reading only, not asked
/// again on every push).
fn is_due(store: &Store, document: &Local, sent: Option<&Sent>) -> bool {
    let Some(sent) = sent else {
        return true;
    };

    let newer_reading = sent.read_at.as_deref() != Some(document.read_at.as_str());
    let newer_edit = document.edit_updated.is_some() && sent.edit_updated != document.edit_updated;
    let original_due = !sent.original && could_send_original(store, document);

    newer_reading || newer_edit || original_due
}

/// Whether there's an original here to send, at a glance: kept, or a file
/// still where it was read from. Whether it's the same bytes is for sending.
fn could_send_original(store: &Store, document: &Local) -> bool {
    let kept = store.original(&document.sha256).is_some();
    let file = document
        .source
        .strip_prefix("file:")
        .is_some_and(|path| Path::new(path).exists());

    kept || file
}

async fn push_batch(cloud: &Cloud, batch: &[&Local], dry_run: bool) -> Result<Pushed> {
    let documents: Vec<Value> = batch
        .iter()
        .map(|document| pushed_document(document))
        .collect::<Result<_>>()?;

    let request = json!({
        "orgId": cloud.signed_in_as().org.id,
        "clientVersion": spendyard_core::VERSION,
        "dryRun": dry_run,
        "documents": documents,
    });

    cloud.call("PushService/PushDocuments", request).await
}

/// A document as the contract's `PushedDocument`, in protobuf's JSON form.
fn pushed_document(document: &Local) -> Result<Value> {
    let stored: Value = serde_json::from_str(&document.result)
        .with_context(|| format!("the stored reading of {}", document.sha256))?;

    let reading = spendyard_core::reading::reading_of(&stored, Some(&document.read_at));

    let mut out = Map::new();

    out.insert("sha256".into(), json!(document.sha256));
    out.insert("reading".into(), reading);

    let texts = [
        ("messageId", &document.message_id),
        ("name", &document.name),
        ("sender", &document.sender),
        ("received", &document.received),
        ("format", &document.format),
    ];

    for (key, value) in texts {
        if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
            out.insert(key.into(), json!(value));
        }
    }

    if let Some(size) = document.size {
        out.insert("size".into(), json!(size.to_string()));
    }

    if let Some(edit) = &document.edit {
        out.insert("edit".into(), edit.clone());
    }

    Ok(Value::Object(out))
}

/// Puts each original the cloud wants where it said, then confirms them:
/// the SHA-256s it kept.
async fn send_originals(
    cloud: &Cloud,
    store: &Store,
    pushed: &Pushed,
    by_sha: &HashMap<&str, &Local>,
    tally: &mut Tally,
    progress: &Progress,
    done: usize,
) -> Result<Vec<String>> {
    // Where each wanted original is, one at a time: finding one may keep it beside the store.
    let mut sending = Vec::new();

    for result in pushed
        .documents
        .iter()
        .filter(|result| result.original_wanted)
    {
        let document = by_sha[result.sha256.as_str()];

        let Some(upload) = &result.original_upload else {
            continue;
        };

        let name = document
            .name
            .clone()
            .unwrap_or_else(|| document.sha256.clone());

        let Some(path) = original_of(store, document)? else {
            tally.missing.push(name);

            continue;
        };

        sending.push((result.sha256.clone(), name, path, upload));
    }

    // Then put, several at once.
    let client = reqwest::Client::new();

    let mut puts = futures_util::stream::iter(sending)
        .map(|(sha256, name, path, upload)| {
            let client = client.clone();

            async move {
                let taken = put_original(&client, &path, upload).await;

                (sha256, name, taken)
            }
        })
        .buffer_unordered(ORIGINALS_AT_ONCE);

    let mut put = Vec::new();

    while let Some((sha256, name, taken)) = puts.next().await {
        match taken {
            Ok(()) => put.push(sha256),

            Err(error) => progress.note(&format!(
                "spendyard: the cloud didn't take the original of {name} ({error})"
            )),
        }

        progress.show(done, tally.originals + put.len());
    }

    if put.is_empty() {
        return Ok(Vec::new());
    }

    let request = json!({ "orgId": cloud.signed_in_as().org.id, "sha256": put });
    let confirmed: Confirmed = cloud
        .call("PushService/ConfirmPushedOriginals", request)
        .await?;

    tally.originals += confirmed.kept.len();

    Ok(confirmed.kept)
}

/// Puts one original where the cloud said.
async fn put_original(
    client: &reqwest::Client,
    path: &Path,
    upload: &OriginalUpload,
) -> Result<()> {
    let bytes = std::fs::read(path)?;

    let mut request = client.put(&upload.url).body(bytes);

    for (name, value) in &upload.headers {
        request = request.header(name, value);
    }

    let response = request.send().await?;

    anyhow::ensure!(response.status().is_success(), "{}", response.status());

    Ok(())
}

/// Where a document's original is: kept beside the store, or, for one read
/// before originals were kept, the file it was read from while it's still
/// the same bytes (kept now, as it goes). None when there's none to send.
fn original_of(store: &Store, document: &Local) -> Result<Option<PathBuf>> {
    if let Some(kept) = store.original(&document.sha256) {
        return Ok(Some(kept));
    }

    let Some(path) = document.source.strip_prefix("file:").map(Path::new) else {
        return Ok(None);
    };

    let Ok(bytes) = std::fs::read(path) else {
        return Ok(None);
    };

    if hex(&Sha256::digest(&bytes)) != document.sha256 {
        return Ok(None);
    }

    if document.size.is_some_and(|size| size > LARGEST_ORIGINAL) {
        return Ok(None);
    }

    store.keep_original(&bytes).map(Some)
}

fn count(
    tally: &mut Tally,
    store: &Store,
    result: &PushedResult,
    by_sha: &HashMap<&str, &Local>,
    dry_run: bool,
) {
    match result.outcome.as_str() {
        "PUSH_OUTCOME_ADDED" => tally.added += 1,
        "PUSH_OUTCOME_UPDATED" => tally.updated += 1,
        _ => tally.unchanged += 1,
    }

    // On a dry run nothing is put: an original wanted counts as one that would go, when it's here.
    let here = by_sha
        .get(result.sha256.as_str())
        .is_some_and(|document| could_send_original(store, document));

    if dry_run && result.original_wanted && here {
        tally.originals += 1;
    }
}

fn report(tally: &Tally, org: &str, dry_run: bool) {
    let verb = if dry_run { "Would push" } else { "Pushed" };

    println!(
        "{verb} to {org}: {} added, {} updated, {} as they were; {} originals",
        tally.added, tally.updated, tally.unchanged, tally.originals
    );

    if !tally.missing.is_empty() {
        println!(
            "{} originals aren't on this machine any more, so the cloud shows their reading only:",
            tally.missing.len()
        );

        for name in &tally.missing {
            println!("  {name}");
        }
    }
}

/// Every document with a reading, its current one, and its edit.
fn documents(store: &Store) -> Result<Vec<Local>> {
    let mut statement = store.connection.prepare(
        "SELECT d.sha256, d.message_id, d.name, d.sender, d.received, d.format, d.size, d.source,
                r.created, r.json,
                e.personal, e.ignored, e.vendor_id, e.vendor_name, e.note, e.updated,
                e.date, e.type, e.total, e.currency, e.number, e.seats
         FROM documents d
         JOIN results r ON r.id = (SELECT max(id) FROM results WHERE document_id = d.id)
         LEFT JOIN edits e ON e.document_id = d.id
         ORDER BY d.id",
    )?;

    let rows = statement.query_map([], |row| {
        let edit_updated: Option<String> = row.get("updated")?;

        let edit = match &edit_updated {
            Some(updated) => Some(edit_of(row, updated)?),
            None => None,
        };

        Ok(Local {
            sha256: row.get("sha256")?,
            message_id: row.get("message_id")?,
            name: row.get("name")?,
            sender: row.get("sender")?,
            received: row.get("received")?,
            format: row.get("format")?,
            size: row.get("size")?,
            source: row.get("source")?,
            read_at: row.get("created")?,
            result: row.get("json")?,
            edit,
            edit_updated,
        })
    })?;

    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A store's edit as the contract's `DocumentEdit`, in protobuf's JSON form.
fn edit_of(row: &rusqlite::Row<'_>, updated: &str) -> rusqlite::Result<Value> {
    let mut edit = Map::new();

    if row.get::<_, Option<bool>>("personal")?.unwrap_or(false) {
        edit.insert("personal".into(), json!(true));
    }

    if row.get::<_, Option<bool>>("ignored")?.unwrap_or(false) {
        edit.insert("ignored".into(), json!(true));
    }

    let texts = [
        ("vendor_id", "vendorId"),
        ("vendor_name", "vendorName"),
        ("note", "note"),
        ("date", "date"),
        ("total", "total"),
        ("currency", "currency"),
        ("number", "number"),
    ];

    for (column, key) in texts {
        if let Some(text) = row
            .get::<_, Option<String>>(column)?
            .filter(|text| !text.is_empty())
        {
            edit.insert(key.into(), json!(text));
        }
    }

    // The core's `credit_note` is the contract's DOCUMENT_TYPE_CREDIT_NOTE.
    if let Some(kind) = row.get::<_, Option<String>>("type")? {
        edit.insert(
            "type".into(),
            json!(format!("DOCUMENT_TYPE_{}", kind.to_uppercase())),
        );
    }

    if let Some(seats) = row.get::<_, Option<f64>>("seats")? {
        edit.insert("seats".into(), json!(seats));
    }

    edit.insert("updatedAt".into(), json!(updated));

    Ok(Value::Object(edit))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `~/.spendyard/cloud.db`: what was pushed where, and where pull got to.
/// Apart from the store, so the store's tables stay the ones the cloud's
/// store has too.
pub(crate) struct SentDb {
    connection: Connection,
}

impl SentDb {
    pub(crate) fn open() -> Result<SentDb> {
        let path = config::home().join("cloud.db");

        let connection =
            Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;

        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS pushed (
                 server        TEXT NOT NULL,
                 org_id        TEXT NOT NULL,
                 sha256        TEXT NOT NULL,
                 document_id   INTEGER,
                 read_at       TEXT,
                 edit_updated  TEXT,
                 original      INTEGER NOT NULL DEFAULT 0,
                 pushed_at     TEXT NOT NULL,
                 PRIMARY KEY (server, org_id, sha256)
             );
             CREATE TABLE IF NOT EXISTS pulled (
                 server   TEXT NOT NULL,
                 org_id   TEXT NOT NULL,
                 sync_id  INTEGER NOT NULL,
                 epoch    INTEGER NOT NULL,
                 PRIMARY KEY (server, org_id)
             );",
        )?;

        Ok(SentDb { connection })
    }

    /// What was sent to this server and organisation, by SHA-256.
    fn sent(&self, (server, org_id): &(String, String)) -> Result<HashMap<String, Sent>> {
        let mut statement = self.connection.prepare(
            "SELECT sha256, read_at, edit_updated, original FROM pushed WHERE server = ?1 AND org_id = ?2",
        )?;

        let rows = statement.query_map(params![server, org_id], |row| {
            let sent = Sent {
                read_at: row.get(1)?,
                edit_updated: row.get(2)?,
                original: row.get(3)?,
            };

            Ok((row.get::<_, String>(0)?, sent))
        })?;

        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Where pull got to on this server and organisation: the sync id and
    /// its epoch; none before the first pull.
    pub(crate) fn pulled(&self, (server, org_id): &(String, String)) -> Result<Option<(i64, i64)>> {
        let found = self
            .connection
            .query_row(
                "SELECT sync_id, epoch FROM pulled WHERE server = ?1 AND org_id = ?2",
                params![server, org_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        Ok(found)
    }

    pub(crate) fn set_pulled(
        &self,
        (server, org_id): &(String, String),
        sync_id: i64,
        epoch: i64,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO pulled (server, org_id, sync_id, epoch) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (server, org_id) DO UPDATE SET sync_id = ?3, epoch = ?4",
            params![server, org_id, sync_id, epoch],
        )?;

        Ok(())
    }

    /// An edit pulled from the cloud: as sent, so the next push doesn't send
    /// it back. Only for a document pushed there before.
    pub(crate) fn edit_pulled(
        &self,
        (server, org_id): &(String, String),
        sha256: &str,
        updated: &str,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE pushed SET edit_updated = ?4 WHERE server = ?1 AND org_id = ?2 AND sha256 = ?3",
            params![server, org_id, sha256, updated],
        )?;

        Ok(())
    }

    /// A document sent: its reading and edit as they are here now, its number
    /// in the cloud, and whether the cloud has its original. Once it has, it
    /// keeps it.
    fn mark(
        &self,
        (server, org_id): &(String, String),
        document: &Local,
        document_id: i64,
        original: bool,
    ) -> Result<()> {
        let had_original: Option<bool> = self
            .connection
            .query_row(
                "SELECT original FROM pushed WHERE server = ?1 AND org_id = ?2 AND sha256 = ?3",
                params![server, org_id, document.sha256],
                |row| row.get(0),
            )
            .optional()?;

        let original = original || had_original.unwrap_or(false);

        self.connection.execute(
            "INSERT INTO pushed (server, org_id, sha256, document_id, read_at, edit_updated, original, pushed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (server, org_id, sha256) DO UPDATE SET
                 document_id = ?4, read_at = ?5, edit_updated = ?6, original = ?7, pushed_at = ?8",
            params![
                server,
                org_id,
                document.sha256,
                document_id,
                document.read_at,
                document.edit_updated,
                original,
                crate::cli::store::now(),
            ],
        )?;

        Ok(())
    }
}
