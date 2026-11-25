//! `spendyard store …`: the local store as a whole, `~/.spendyard`'s
//! `spendyard.db` with the originals beside it: what's in it, a backup of it,
//! bringing one another version made up to this one, and wiping it to start
//! again.
//!
//! None of them opens the store as `Store::open` does, upgrading or refusing
//! it: `info` and `backup` work on a store another version made too, which is
//! when they're wanted most, before a wipe.
//!
//! What's kept when the store goes: the config and model keys, the
//! mailboxes' and the cloud's sign-ins, local vendors, and backups. What goes
//! with it: the originals, where each mailbox scan got to (in the store), and
//! what was pushed and pulled (`cloud.db`), which is about this store.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;

use crate::cli::config;
use crate::cli::store::{Brought, BroughtReading, SCHEMA_VERSION, Store, now};

/// The files of the store, in its home folder.
struct Files {
    home: PathBuf,
}

impl Files {
    fn of(home: &Path) -> Files {
        Files {
            home: home.to_path_buf(),
        }
    }

    fn database(&self) -> PathBuf {
        self.home.join("spendyard.db")
    }

    fn originals(&self) -> PathBuf {
        self.home.join("originals")
    }

    /// What was pushed and pulled: about this store, so it goes with it.
    fn sent(&self) -> PathBuf {
        self.home.join("cloud.db")
    }

    fn backups(&self) -> PathBuf {
        self.home.join("backups")
    }

    /// A database and the files SQLite keeps beside it while it's in use.
    fn with_journal(database: &Path) -> Vec<PathBuf> {
        ["", "-wal", "-shm"]
            .iter()
            .map(|suffix| PathBuf::from(format!("{}{suffix}", database.display())))
            .collect()
    }
}

/// What the store holds, as `store info` tells it.
#[derive(Debug, Serialize)]
pub(crate) struct Info {
    path: PathBuf,

    /// The store's version: SCHEMA_VERSION when this spendyard made it.
    version: i64,

    /// The database's size, its journal with it, in bytes.
    size: u64,

    originals: Originals,

    /// Counts; none when another version made the store, whose tables may be others.
    contents: Option<Contents>,

    /// Where each mailbox's scan got to.
    scans: Vec<Scan>,
}

#[derive(Debug, Serialize)]
struct Originals {
    path: PathBuf,
    count: usize,
    size: u64,
}

#[derive(Debug, Serialize)]
struct Contents {
    documents: i64,
    /// Every reading kept, read again ones too.
    readings: i64,
    /// Documents corrected by hand.
    corrected: i64,
    /// Lines of card statements.
    transactions: i64,
    payments: i64,
    /// Documents with no original kept: read before originals were.
    without_original: i64,
}

#[derive(Debug, Serialize)]
struct Scan {
    mailbox: String,
    newest: String,
    updated: String,
}

// --- info ----------------------------------------------------------------------------------------

/// What's in the store at `home`; None when there's none.
pub(crate) fn info_of(home: &Path) -> Result<Option<Info>> {
    let files = Files::of(home);
    let database = files.database();

    if !database.exists() {
        return Ok(None);
    }

    let connection =
        Connection::open(&database).with_context(|| format!("opening {}", database.display()))?;

    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;

    let size = Files::with_journal(&database)
        .iter()
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum();

    let (count, originals_size, kept) = originals_in(&files.originals());

    let ours = version == SCHEMA_VERSION;

    let contents = if ours {
        Some(contents_of(&connection, &kept)?)
    } else {
        None
    };

    let scans = if ours {
        scans_of(&connection)?
    } else {
        Vec::new()
    };

    Ok(Some(Info {
        path: database,
        version,
        size,
        originals: Originals {
            path: files.originals(),
            count,
            size: originals_size,
        },
        contents,
        scans,
    }))
}

/// How many originals, their size, and their names (their SHA-256s).
fn originals_in(folder: &Path) -> (usize, u64, std::collections::HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return (0, 0, Default::default());
    };

    let mut size = 0;
    let mut names = std::collections::HashSet::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();

        // A file still being written is `.<sha256>.<pid>.partial`.
        if name.starts_with('.') {
            continue;
        }

        size += entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        names.insert(name);
    }

    (names.len(), size, names)
}

fn contents_of(
    connection: &Connection,
    kept: &std::collections::HashSet<String>,
) -> Result<Contents> {
    let count = |sql: &str| -> Result<i64> { Ok(connection.query_row(sql, [], |row| row.get(0))?) };

    let mut statement = connection.prepare("SELECT sha256 FROM documents")?;

    let without_original = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(Result::ok)
        .filter(|sha256| !kept.contains(sha256))
        .count() as i64;

    Ok(Contents {
        documents: count("SELECT count(*) FROM documents")?,
        readings: count("SELECT count(*) FROM results")?,
        corrected: count("SELECT count(*) FROM edits")?,
        transactions: count("SELECT count(*) FROM transactions")?,
        payments: count("SELECT count(DISTINCT payment_id) FROM payment_evidence")?,
        without_original,
    })
}

fn scans_of(connection: &Connection) -> Result<Vec<Scan>> {
    let mut statement =
        connection.prepare("SELECT key, newest, updated FROM scans ORDER BY key")?;

    let scans = statement
        .query_map([], |row| {
            Ok(Scan {
                mailbox: row.get(0)?,
                newest: row.get(1)?,
                updated: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(scans)
}

/// `spendyard store info`.
pub async fn info(json: bool) -> Result<()> {
    let home = config::home();

    let Some(info) = info_of(&home)? else {
        println!(
            "No store yet at {}: `spendyard read` and `spendyard mailbox scan` make one",
            Files::of(&home).database().display()
        );

        return Ok(());
    };

    // What the next push would send, when this machine is signed in and the store is this version's.
    let waiting = if info.contents.is_some() {
        crate::cli::push::waiting(&Store::open()?)?
    } else {
        None
    };

    if json {
        let mut whole = serde_json::to_value(&info)?;

        if let Some((org, due)) = &waiting {
            whole["push"] = json!({ "organisation": org, "waiting": due });
        }

        println!("{}", serde_json::to_string_pretty(&whole)?);

        return Ok(());
    }

    for line in describe(&info) {
        println!("{line}");
    }

    // Signed in: how many the next push would send (new, read again or corrected here, or an original still wanted).
    if let Some((org, due)) = waiting {
        if due == 0 {
            println!("  Cloud     {org}: everything here is pushed");
        } else {
            let documents = plural(due as i64, "document");

            println!(
                "  Cloud     {org}: `spendyard cloud push` would send {documents}, new or changed here"
            );
        }
    }

    Ok(())
}

/// The store in lines, as `store info` and `store wipe` tell it.
fn describe(info: &Info) -> Vec<String> {
    let mut out = vec![format!("Store     {}", info.path.display())];

    out.push(format!(
        "  Version {} · {}",
        info.version,
        size_in_words(info.size)
    ));

    out.push(format!(
        "  Originals {} · {}, in {}",
        plural(info.originals.count as i64, "file"),
        size_in_words(info.originals.size),
        info.originals.path.display()
    ));

    let Some(contents) = &info.contents else {
        out.push(format!(
            "  Made by another version of spendyard (this one reads version {SCHEMA_VERSION}): `spendyard store upgrade` brings it up to this one, `spendyard store wipe` starts a new one"
        ));

        return out;
    };

    out.push(format!(
        "  Documents {}, {} kept; {} corrected by hand",
        contents.documents,
        plural(contents.readings, "reading"),
        contents.corrected
    ));

    out.push(format!(
        "  Statements' lines {}; payments {}",
        contents.transactions, contents.payments
    ));

    if contents.without_original > 0 {
        out.push(format!(
            "  {} without their original: read before originals were kept",
            plural(contents.without_original, "document")
        ));
    }

    for scan in &info.scans {
        out.push(format!(
            "  Scanned   {} up to {}, on {}",
            scan.mailbox, scan.newest, scan.updated
        ));
    }

    out
}

fn plural(count: i64, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

/// `239 MB`, `4.6 KB`.
fn size_in_words(bytes: u64) -> String {
    let units = ["bytes", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;

    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} bytes")
    } else if size < 10.0 {
        format!("{size:.1} {}", units[unit])
    } else {
        format!("{size:.0} {}", units[unit])
    }
}

// --- backup --------------------------------------------------------------------------------------

/// A backup's file name, by when it was made: `spendyard-2026-11-25T10-00-00Z.tar.gz`.
fn backup_name() -> String {
    format!("spendyard-{}.tar.gz", now().replace(':', "-"))
}

/// Backs the store at `home` up to `archive`: its database and what was
/// pushed, each copied as SQLite copies a database in use, its originals,
/// and what it is (`manifest.json`). A `.tar.gz`.
pub(crate) fn backup_to(home: &Path, archive: &Path) -> Result<()> {
    let files = Files::of(home);

    if !files.database().exists() {
        bail!("no store to back up at {}", files.database().display());
    }

    if archive.exists() {
        bail!("{} is there already", archive.display());
    }

    if let Some(parent) = archive
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }

    // Beside the archive, named by it: two backups at once never share it.
    let name = archive
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let staging = archive.with_file_name(format!(".{name}.staging"));

    let _ = std::fs::remove_dir_all(&staging);

    std::fs::create_dir_all(&staging)?;

    let made = write_archive(&files, &staging, archive);

    let _ = std::fs::remove_dir_all(&staging);

    if made.is_err() {
        let _ = std::fs::remove_file(archive);
    }

    made
}

fn write_archive(files: &Files, staging: &Path, archive: &Path) -> Result<()> {
    // Copies made by SQLite, so a store in use is copied whole, as it is at one moment.
    copy_database(&files.database(), &staging.join("spendyard.db"))?;

    if files.sent().exists() {
        copy_database(&files.sent(), &staging.join("cloud.db"))?;
    }

    let info = info_of(&files.home)?.context("the store went while it was backed up")?;

    let manifest = json!({
        "spendyard_version": spendyard_core::VERSION,
        "store_version": info.version,
        "made": now(),
        "documents": info.contents.as_ref().map(|contents| contents.documents),
        "originals": info.originals.count,
    });

    std::fs::write(
        staging.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    let file =
        std::fs::File::create(archive).with_context(|| format!("making {}", archive.display()))?;

    // Fast: the originals are mostly PDFs and pictures, compressed already, which harder work hardly shrinks.
    let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::fast()));

    tar.append_path_with_name(staging.join("manifest.json"), "manifest.json")?;
    tar.append_path_with_name(staging.join("spendyard.db"), "spendyard.db")?;

    if staging.join("cloud.db").exists() {
        tar.append_path_with_name(staging.join("cloud.db"), "cloud.db")?;
    }

    if files.originals().exists() {
        tar.append_dir_all("originals", files.originals())?;
    }

    tar.into_inner()?.finish()?.flush()?;

    Ok(())
}

/// A database copied by SQLite (`VACUUM INTO`): whole, and compacted.
fn copy_database(from: &Path, to: &Path) -> Result<()> {
    let connection =
        Connection::open(from).with_context(|| format!("opening {}", from.display()))?;

    connection
        .execute("VACUUM INTO ?1", [to.to_string_lossy()])
        .with_context(|| format!("copying {}", from.display()))?;

    Ok(())
}

/// `spendyard store backup [path]`: the store and its originals in one
/// archive; in the current folder when no path is given.
pub fn backup(path: Option<PathBuf>) -> Result<()> {
    let home = config::home();
    let archive = path.unwrap_or_else(|| PathBuf::from(backup_name()));

    backup_to(&home, &archive)?;

    let size = std::fs::metadata(&archive)
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    println!(
        "Backed up {} to {} ({})",
        Files::of(&home).database().display(),
        archive.display(),
        size_in_words(size)
    );

    Ok(())
}

// --- upgrade -------------------------------------------------------------------------------------

/// What an upgrade brought.
#[derive(Debug)]
pub(crate) struct Upgraded {
    pub from: i64,
    pub documents: usize,
    pub readings: usize,
    pub corrected: i64,
}

/// The tables copied as they are, by the columns both stores have: what's
/// decided by hand or kept for later, not made from the readings.
const COPIED: &[&str] = &[
    "edits",
    "scans",
    "rates",
    "payment_evidence",
    "payment_moves",
];

/// Brings the store at `home`, made by another version, up to this one: a
/// new store built beside it from what every version keeps the same way
/// (documents and their readings, the core's JSON), its rows made again by
/// this version's core; corrections, scans, rates and payment numbers
/// copied; then swapped in. The originals and cloud.db stay as they are.
/// Backed up first, to `backup`.
pub(crate) async fn upgrade_at(home: &Path, backup: &Path) -> Result<Upgraded> {
    let files = Files::of(home);
    let database = files.database();

    let info = info_of(home)?.context("no store to upgrade")?;

    if info.version == SCHEMA_VERSION {
        bail!("the store is this version's already ({SCHEMA_VERSION})");
    }

    if info.version > SCHEMA_VERSION {
        bail!(
            "a newer spendyard made the store (version {}, this one reads {SCHEMA_VERSION}): upgrade spendyard",
            info.version
        );
    }

    backup_to(home, backup)?;

    let building = home.join(format!(".upgrade-{}.db", std::process::id()));

    for path in Files::with_journal(&building) {
        let _ = std::fs::remove_file(path);
    }

    let built = build(&database, &building).await;

    let upgraded = match built {
        Ok(upgraded) => upgraded,

        Err(error) => {
            for path in Files::with_journal(&building) {
                let _ = std::fs::remove_file(path);
            }

            return Err(error.context("not upgraded: the store is as it was"));
        }
    };

    // The old one out, the new one in: the backup has the old one.
    for path in Files::with_journal(&database) {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }

    std::fs::rename(&building, &database)?;

    for path in Files::with_journal(&building) {
        let _ = std::fs::remove_file(path);
    }

    Ok(Upgraded {
        from: info.version,
        ..upgraded
    })
}

/// The new store at `building`, from the old one at `old`.
async fn build(old: &Path, building: &Path) -> Result<Upgraded> {
    let reader = Connection::open(old).with_context(|| format!("opening {}", old.display()))?;

    let mut store = Store::open_at(building)?;

    let mut readings = readings_of(&reader)?;
    let documents = documents_of(&reader)?;

    let mut brought = 0;
    let mut kept_readings = 0;

    for mut document in documents {
        document.readings = readings.remove(&document.id).unwrap_or_default();
        document.fallback_date = fallback_date(&store, &document);

        kept_readings += document.readings.len();

        store.bring(&document)?;

        brought += 1;
    }

    copy_tables(&store, old)?;

    // Statements' lines matched to their receipts again, as `transactions match` does.
    crate::cli::transactions::match_all(&store).await?;

    let corrected: i64 = store
        .connection
        .query_row("SELECT count(*) FROM edits", [], |row| row.get(0))?;

    // All of it in the database file, nothing left in its journal, before it's moved.
    store
        .connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;

    drop(store);

    Ok(Upgraded {
        from: 0,
        documents: brought,
        readings: kept_readings,
        corrected,
    })
}

/// The old store's documents, oldest first, without their readings yet.
fn documents_of(reader: &Connection) -> Result<Vec<Brought>> {
    let mut statement = reader.prepare(
        "SELECT id, sha256, message_id, source, name, sender, received, format, size, first_seen
         FROM documents ORDER BY id",
    )?;

    let documents = statement
        .query_map([], |row| {
            Ok(Brought {
                id: row.get(0)?,
                sha256: row.get(1)?,
                message_id: row.get(2)?,
                source: row.get(3)?,
                name: row.get(4)?,
                sender: row.get(5)?,
                received: row.get(6)?,
                format: row.get(7)?,
                size: row.get(8)?,
                first_seen: row.get(9)?,
                readings: Vec::new(),
                fallback_date: None,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    Ok(documents)
}

/// Every reading of the old store, by document, oldest first.
fn readings_of(reader: &Connection) -> Result<std::collections::HashMap<i64, Vec<BroughtReading>>> {
    let mut statement = reader.prepare(
        "SELECT document_id, created, spendyard_version, schema_version, decider, extractor, model, json
         FROM results ORDER BY id",
    )?;

    let mut by_document: std::collections::HashMap<i64, Vec<BroughtReading>> = Default::default();

    let rows = statement.query_map([], |row| {
        let reading = BroughtReading {
            created: row.get(1)?,
            spendyard_version: row.get(2)?,
            schema_version: row.get(3)?,
            decider: row.get(4)?,
            extractor: row.get(5)?,
            model: row.get(6)?,
            json: row.get(7)?,
        };

        Ok((row.get::<_, i64>(0)?, reading))
    })?;

    for row in rows {
        let (document_id, reading) = row?;

        by_document.entry(document_id).or_default().push(reading);
    }

    Ok(by_document)
}

/// When a document arrived, for a reading that doesn't say when it was
/// issued, as `save` takes it: its `received`, else an email's own date,
/// from its original (kept, or where it was read from).
fn fallback_date(store: &Store, document: &Brought) -> Option<String> {
    if document.received.is_some() {
        return document.received.clone();
    }

    if document.format.as_deref() != Some("email") {
        return None;
    }

    let original = store.original(&document.sha256).or_else(|| {
        document
            .source
            .strip_prefix("file:")
            .map(PathBuf::from)
            .filter(|path| path.exists())
    })?;

    let bytes = std::fs::read(original).ok()?;
    let read = spendyard_core::read(&bytes, None).ok()?;

    read.headers.and_then(|headers| headers.date)
}

/// The tables decided by hand or kept for later, copied from the old store
/// by the columns both have.
fn copy_tables(store: &Store, old: &Path) -> Result<()> {
    store
        .connection
        .execute("ATTACH DATABASE ?1 AS old", [old.to_string_lossy()])?;

    for table in COPIED {
        let ours = columns_of(&store.connection, "main", table)?;
        let theirs = columns_of(&store.connection, "old", table)?;

        let both: Vec<&String> = ours
            .iter()
            .filter(|column| theirs.contains(column))
            .collect();

        if both.is_empty() {
            continue;
        }

        let list = both
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");

        store.connection.execute_batch(&format!(
            "INSERT OR IGNORE INTO main.{table} ({list}) SELECT {list} FROM old.{table};"
        ))?;
    }

    store.connection.execute_batch("DETACH DATABASE old;")?;

    Ok(())
}

/// A table's columns in one of the attached databases; none when it hasn't the table.
fn columns_of(connection: &Connection, schema: &str, table: &str) -> Result<Vec<String>> {
    let mut statement = connection.prepare(&format!("PRAGMA {schema}.table_info({table})"))?;

    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;

    Ok(columns)
}

/// `spendyard store upgrade`: a store another version made, brought up to this one.
pub async fn upgrade() -> Result<()> {
    let home = config::home();
    let backup = Files::of(&home).backups().join(backup_name());

    let upgraded = upgrade_at(&home, &backup).await?;

    println!(
        "Upgraded the store from version {} to {SCHEMA_VERSION}: {} with {}, {} corrected by hand; its rows made again by this version",
        upgraded.from,
        plural(upgraded.documents as i64, "document"),
        plural(upgraded.readings as i64, "reading"),
        upgraded.corrected
    );

    println!("The store as it was is backed up to {}", backup.display());

    Ok(())
}

// --- wipe ----------------------------------------------------------------------------------------

/// Deletes the store at `home`, its originals and what was pushed and
/// pulled: what `store wipe` does once it's been asked. Everything else in
/// `home` stays. Whether there was anything to delete.
pub(crate) fn wipe_at(home: &Path) -> Result<bool> {
    let files = Files::of(home);
    let mut deleted = false;

    let databases = [files.database(), files.sent()];

    for path in databases
        .iter()
        .flat_map(|database| Files::with_journal(database))
    {
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;

            deleted = true;
        }
    }

    if files.originals().exists() {
        std::fs::remove_dir_all(files.originals())
            .with_context(|| format!("deleting {}", files.originals().display()))?;

        deleted = true;
    }

    Ok(deleted)
}

/// `spendyard store wipe`: the store, its originals and what was pushed and
/// pulled, gone for good, after saying what goes and asking (or `yes`).
/// With `backup`, a backup in `~/.spendyard/backups/` first.
pub async fn wipe(yes: bool, backup: bool) -> Result<()> {
    let home = config::home();
    let files = Files::of(&home);

    let Some(info) = info_of(&home)? else {
        // No database; perhaps originals or cloud.db left from one.
        if wipe_at(&home)? {
            println!("Wiped what was left of a store in {}", home.display());
        } else {
            println!(
                "No store at {}: nothing to wipe",
                files.database().display()
            );
        }

        return Ok(());
    };

    println!("This deletes, for good:");

    for line in describe(&info) {
        println!("  {line}");
    }

    println!(
        "  and what was pushed and pulled ({})",
        files.sent().display()
    );
    println!();
    println!(
        "It keeps the config, model keys, mailbox and cloud sign-ins, local vendors and backups. Reading again calls the models again."
    );

    if crate::cli::cloud::signed_in()?.is_some() {
        println!("What was pushed to the cloud stays there.");
    }

    let kept_at = if backup {
        let archive = files.backups().join(backup_name());

        backup_to(&home, &archive)?;

        println!("Backed up to {} first.", archive.display());

        Some(archive)
    } else {
        None
    };

    if !yes {
        confirm()?;
    }

    wipe_at(&home)?;

    match kept_at {
        Some(archive) => println!("Wiped. The backup is {}", archive.display()),
        None => println!("Wiped. The next read or scan starts a new store."),
    }

    Ok(())
}

/// Asks to type `wipe`; refuses when there's no one to ask.
fn confirm() -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("not wiped: nobody to ask here; `spendyard store wipe --yes` wipes without asking");
    }

    print!("Type \"wipe\" to delete it: ");

    std::io::stdout().flush()?;

    let mut answer = String::new();

    std::io::stdin().lock().read_line(&mut answer)?;

    if answer.trim() != "wipe" {
        bail!("not wiped");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::store::Source;

    /// A home folder of its own, with a store holding one document and its
    /// original, a corrected one, and what was pushed; and the config and a
    /// mailbox's sign-in beside them.
    fn home_with_a_store(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "spendyard-store-test-{name}-{}",
            std::process::id()
        ));

        let _ = std::fs::remove_dir_all(&home);

        std::fs::create_dir_all(&home).unwrap();

        let mut store = Store::open_at(&home.join("spendyard.db")).unwrap();

        let bytes = std::fs::read(format!(
            "{}/crates/core/tests/fixtures/receipt.eml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();

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

        store
            .connection
            .execute(
                "INSERT INTO edits (document_id, personal, updated) VALUES (?1, 1, '2026-11-25T10:00:00Z')",
                [id],
            )
            .unwrap();

        drop(store);

        Connection::open(home.join("cloud.db"))
            .unwrap()
            .execute_batch("CREATE TABLE pushed (sha256 TEXT);")
            .unwrap();

        std::fs::write(home.join("config.toml"), "# the config\n").unwrap();
        std::fs::write(home.join("accounts.json"), "[]\n").unwrap();

        home
    }

    #[test]
    fn says_what_the_store_holds() {
        let home = home_with_a_store("info");
        let info = info_of(&home).unwrap().unwrap();
        let contents = info.contents.as_ref().unwrap();

        assert_eq!(info.version, SCHEMA_VERSION);
        assert_eq!(info.originals.count, 1);
        assert_eq!(contents.documents, 1);
        assert_eq!(contents.readings, 1);
        assert_eq!(contents.corrected, 1);
        assert_eq!(contents.without_original, 0);

        let lines = describe(&info).join("\n");

        assert!(lines.contains("Documents 1, 1 reading kept; 1 corrected by hand"));
        assert!(lines.contains("Originals 1 file"));

        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn says_what_it_can_of_another_versions_store() {
        let home = home_with_a_store("old");

        Connection::open(home.join("spendyard.db"))
            .unwrap()
            .pragma_update(None, "user_version", 7)
            .unwrap();

        let info = info_of(&home).unwrap().unwrap();

        assert_eq!(info.version, 7);
        assert!(info.contents.is_none());
        assert!(
            describe(&info)
                .join("\n")
                .contains("Made by another version")
        );

        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn backs_the_store_up_whole_in_one_archive() {
        let home = home_with_a_store("backup");
        let archive = home.join("backups").join("a.tar.gz");

        backup_to(&home, &archive).unwrap();

        let file = std::fs::File::open(&archive).unwrap();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));

        let names: Vec<String> = tar
            .entries()
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert!(names.contains(&"manifest.json".to_string()));
        assert!(names.contains(&"spendyard.db".to_string()));
        assert!(names.contains(&"cloud.db".to_string()));
        assert!(
            names
                .iter()
                .any(|name| name.starts_with("originals/") && name.len() > "originals/".len() + 60)
        );

        // Not twice to the same file.
        assert!(backup_to(&home, &archive).is_err());

        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn upgrades_an_older_store_keeping_its_readings_and_corrections() {
        let home = home_with_a_store("upgrade");
        let database = home.join("spendyard.db");

        // Its receipts row as this version makes it, to compare with what the upgrade makes again.
        let row_of = |path: &Path| -> String {
            Connection::open(path)
                .unwrap()
                .query_row("SELECT json_array(type, status, vendor_name, date, total, currency) FROM receipts", [], |row| row.get(0))
                .unwrap()
        };

        let before = row_of(&database);

        // As version 7 had it: statements' lines in `charges`.
        let old = Connection::open(&database).unwrap();

        old.execute_batch("ALTER TABLE transactions RENAME TO charges;")
            .unwrap();
        old.pragma_update(None, "user_version", 7).unwrap();

        drop(old);

        let backup = home.join("backups").join("before-upgrade.tar.gz");
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let upgraded = runtime.block_on(upgrade_at(&home, &backup)).unwrap();

        assert_eq!(upgraded.from, 7);
        assert_eq!(upgraded.documents, 1);
        assert_eq!(upgraded.readings, 1);
        assert_eq!(upgraded.corrected, 1);
        assert!(backup.exists());

        let info = info_of(&home).unwrap().unwrap();

        assert_eq!(info.version, SCHEMA_VERSION);
        assert_eq!(info.contents.unwrap().corrected, 1);
        assert_eq!(row_of(&database), before);

        // Once is enough: this version's store isn't upgraded again.
        let again = runtime
            .block_on(upgrade_at(
                &home,
                &home.join("backups").join("again.tar.gz"),
            ))
            .unwrap_err();

        assert!(again.to_string().contains("this version's already"));
        assert!(!home.join(".upgrade-0.db").exists());

        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn wipes_the_store_and_keeps_the_rest() {
        let home = home_with_a_store("wipe");
        let archive = home.join("backups").join("before.tar.gz");

        backup_to(&home, &archive).unwrap();

        assert!(wipe_at(&home).unwrap());

        assert!(!home.join("spendyard.db").exists());
        assert!(!home.join("originals").exists());
        assert!(!home.join("cloud.db").exists());

        assert!(home.join("config.toml").exists());
        assert!(home.join("accounts.json").exists());
        assert!(archive.exists());

        assert!(info_of(&home).unwrap().is_none());
        assert!(!wipe_at(&home).unwrap());

        std::fs::remove_dir_all(home).ok();
    }
}
