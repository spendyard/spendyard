//! `spendyard cloud documents …`: the documents of the organisation this
//! machine acts in, as the cloud has them: listed, one whole, and a file sent
//! to be read there, as a drop in the browser is. A member sees their own.
//!
//! A reading is shown as `documents get` shows one here: the cloud gives it
//! as the contract's `Reading`, which the core makes a result again
//! (`spendyard_core::reading::result_of`), printed by the same code.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli::cloud::Cloud;
use crate::cli::documents::describe_reading;

/// The largest file the cloud takes.
const LARGEST: u64 = 25 * 1024 * 1024;

/// How long `upload --wait` waits for the cloud to read a file.
const WAIT: Duration = Duration::from_secs(180);

pub struct ListOptions {
    pub limit: usize,
    pub search: Option<String>,
    /// `read`, `reading` or `failed`; all when none.
    pub state: Option<String>,
    /// The kind of source: `upload`, `forward`, `cli`.
    pub source: Option<String>,
    pub json: bool,
}

pub struct UploadOptions {
    pub files: Vec<PathBuf>,
    pub wait: bool,
}

#[derive(Deserialize)]
struct Listed {
    #[serde(default)]
    documents: Vec<Value>,

    #[serde(default)]
    total: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Upload {
    upload_id: String,
    url: String,

    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
struct Added {
    document: Value,

    #[serde(default)]
    seen: bool,
}

// --- list ----------------------------------------------------------------------------------------

/// `spendyard cloud documents list`: newest first, a page of 500 at a time
/// until `limit`.
pub async fn list(options: ListOptions) -> Result<()> {
    let cloud = Cloud::connect()?;

    let filter = match options.state.as_deref() {
        None => "DOCUMENT_FILTER_UNSPECIFIED",
        Some("read") => "DOCUMENT_FILTER_READ",
        Some("reading") => "DOCUMENT_FILTER_READING",
        Some("failed") => "DOCUMENT_FILTER_FAILED",
        Some(other) => bail!("--state is read, reading or failed, not {other}"),
    };

    let mut documents: Vec<Value> = Vec::new();
    let mut before = 0;
    let mut total = 0;

    while documents.len() < options.limit {
        let page_size = (options.limit - documents.len()).min(500);

        let request = json!({
            "pageSize": page_size,
            "beforeId": before,
            "filter": filter,
            "query": options.search.clone().unwrap_or_default(),
            "source": options.source.clone().unwrap_or_default(),
        });

        let page: Listed = cloud.call("DocumentService/ListDocuments", request).await?;

        total = page.total;

        let Some(last) = page
            .documents
            .last()
            .and_then(|document| document["id"].as_i64())
        else {
            break;
        };

        let full = page.documents.len() == page_size;

        documents.extend(page.documents);
        before = last;

        if !full {
            break;
        }
    }

    if options.json {
        for document in &documents {
            println!("{document}");
        }

        return Ok(());
    }

    if documents.is_empty() {
        println!(
            "No documents in {} that pick; `spendyard cloud push` sends what this machine read",
            cloud.signed_in_as().org.name
        );

        return Ok(());
    }

    for document in &documents {
        println!("{}", list_line(document));
    }

    if (documents.len() as i64) < total {
        println!("… {} of {total}; --limit shows more", documents.len());
    }

    Ok(())
}

/// One document in a line, as `documents list` prints one here.
fn list_line(document: &Value) -> String {
    let text = |key: &str| document[key].as_str().unwrap_or_default().to_string();

    let date = Some(text("date"))
        .filter(|date| !date.is_empty())
        .unwrap_or_else(|| text("received").chars().take(10).collect());

    let kind = word(&text("type"), "DOCUMENT_TYPE_");
    let vendor = document["vendor"]["name"].as_str().unwrap_or("");

    let total = [text("total"), text("currency")]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let mut marks = Vec::new();

    match document["state"].as_str() {
        Some("DOCUMENT_STATE_QUEUED") | Some("DOCUMENT_STATE_READING") => {
            marks.push("reading".to_string())
        }
        Some("DOCUMENT_STATE_FAILED") => marks.push("failed".to_string()),
        _ => {}
    }

    if document["personal"].as_bool() == Some(true) {
        marks.push("personal".to_string());
    }

    if document["ignored"].as_bool() == Some(true) {
        marks.push("set aside".to_string());
    }

    if let Some(flags) = document["flags"].as_i64().filter(|flags| *flags > 0) {
        marks.push(format!("{flags} flagged"));
    }

    let id = document["id"].as_i64().unwrap_or(0);

    format!(
        "{id:>5}  {date:<10}  {:<14} {:<26} {:>16}  {}",
        clip(&kind, 14),
        clip(vendor, 26),
        total,
        marks.join(", ")
    )
}

/// `DOCUMENT_TYPE_RENEWAL_NOTICE` as `renewal notice`.
fn word(name: &str, prefix: &str) -> String {
    name.strip_prefix(prefix)
        .unwrap_or(name)
        .to_lowercase()
        .replace('_', " ")
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let kept: String = text.chars().take(width - 1).collect();

    format!("{kept}…")
}

// --- get -----------------------------------------------------------------------------------------

/// `spendyard cloud documents get <id>`: one document whole.
pub async fn get(id: i64, json: bool) -> Result<()> {
    let cloud = Cloud::connect()?;

    let got: Value = cloud
        .call("DocumentService/GetDocument", json!({ "id": id }))
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&got)?);

        return Ok(());
    }

    for line in describe(&got) {
        println!("{line}");
    }

    Ok(())
}

/// A GetDocument answer, as lines: as `documents get` prints a document here.
fn describe(got: &Value) -> Vec<String> {
    let document = &got["document"];
    let text = |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_string();

    let mut out = vec![format!(
        "Document {}  {}",
        document["id"].as_i64().unwrap_or(0),
        text(document, "name")
    )];

    let from = [text(document, "source"), text(document, "sender")]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");

    out.push(format!("  From      {from}"));

    let received = text(document, "received");

    if !received.is_empty() {
        out.push(format!("  Received  {received}"));
    }

    match document["state"].as_str() {
        Some("DOCUMENT_STATE_FAILED") => out.push(format!(
            "  Failed    {} ({})",
            text(document, "error"),
            text(document, "errorStage")
        )),

        Some("DOCUMENT_STATE_QUEUED") | Some("DOCUMENT_STATE_READING") => {
            out.push("  Reading   not read yet".to_string())
        }

        _ => {}
    }

    let reading = &got["reading"];

    if reading.is_object() {
        let by = text(reading, "extractor");
        let at = text(reading, "readAt");

        out.push(format!("  Read      {at} by {by}"));
    }

    let edited = edited(&got["edit"]);

    if !edited.is_empty() {
        out.push(format!("  Edited    {}", edited.join(" · ")));
    }

    if let Some(payment) = document["paymentId"].as_i64().filter(|id| *id > 0) {
        out.push(format!("  Payment   #{payment}"));
    }

    if reading.is_object() {
        let result = spendyard_core::reading::result_of(reading);
        let result = serde_json::to_value(result).unwrap_or_default();

        out.push(String::new());

        describe_reading(&result, &mut out);
    }

    out
}

/// A DocumentEdit's corrections in a few words each.
fn edited(edit: &Value) -> Vec<String> {
    let mut said = Vec::new();

    for (key, words) in [
        ("personal", "personal"),
        ("ignored", "set aside"),
        ("paid", "paid"),
    ] {
        if edit[key].as_bool() == Some(true) {
            said.push(words.to_string());
        }
    }

    for (key, words) in [
        ("vendorName", "vendor"),
        ("date", "date"),
        ("total", "total"),
        ("currency", "currency"),
        ("number", "number"),
    ] {
        if let Some(value) = edit[key].as_str().filter(|value| !value.is_empty()) {
            said.push(format!("{words} {value}"));
        }
    }

    if let Some(kind) = edit["type"].as_str() {
        said.push(format!("type {}", word(kind, "DOCUMENT_TYPE_")));
    }

    if let Some(note) = edit["note"].as_str().filter(|note| !note.is_empty()) {
        said.push(format!("note \"{note}\""));
    }

    said
}

// --- upload --------------------------------------------------------------------------------------

/// `spendyard cloud documents upload <files…>`: each file sent to the cloud
/// and read there, as one dropped in the browser is. With `wait`, until it's
/// read, then what was read.
pub async fn upload(options: UploadOptions) -> Result<()> {
    let cloud = Cloud::connect()?;
    let client = reqwest::Client::new();

    for path in &options.files {
        let (id, seen) = upload_one(&cloud, &client, path).await?;

        let name = file_name(path);

        if seen {
            println!("{name}: there already, as document {id}");
        } else {
            println!("{name}: uploaded as document {id}, being read");
        }

        if options.wait && !seen {
            let got = wait_until_read(&cloud, id).await?;

            println!();

            for line in describe(&got) {
                println!("{line}");
            }

            println!();
        }
    }

    Ok(())
}

async fn upload_one(cloud: &Cloud, client: &reqwest::Client, path: &Path) -> Result<(i64, bool)> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;

    if bytes.len() as u64 > LARGEST {
        bail!("{} is larger than the cloud takes, 25 MB", path.display());
    }

    let request = json!({
        "name": file_name(path),
        "contentType": content_type(path),
        "size": bytes.len(),
    });

    let upload: Upload = cloud
        .call("DocumentService/CreateDocumentUpload", request)
        .await?;

    let mut put = client.put(&upload.url).body(bytes);

    for (name, value) in &upload.headers {
        put = put.header(name, value);
    }

    put.send()
        .await?
        .error_for_status()
        .with_context(|| format!("sending {}", path.display()))?;

    let added: Added = cloud
        .call(
            "DocumentService/AddDocument",
            json!({ "uploadId": upload.upload_id }),
        )
        .await?;

    let id = added.document["id"].as_i64().unwrap_or(0);

    Ok((id, added.seen))
}

/// Asks every two seconds until the document is read, or couldn't be.
async fn wait_until_read(cloud: &Cloud, id: i64) -> Result<Value> {
    let started = Instant::now();

    loop {
        let got: Value = cloud
            .call("DocumentService/GetDocument", json!({ "id": id }))
            .await?;

        let state = got["document"]["state"].as_str().unwrap_or_default();

        if matches!(state, "DOCUMENT_STATE_READ" | "DOCUMENT_STATE_FAILED") {
            return Ok(got);
        }

        if started.elapsed() > WAIT {
            bail!(
                "document {id} wasn't read within three minutes; `spendyard cloud documents get {id}` shows it once it is"
            );
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_string())
}

/// What a file is, by its extension: what the browser would say.
fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        "html" | "htm" => "text/html",
        "txt" => "text/plain",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_in_a_line() {
        let document = json!({
            "id": 412,
            "name": "slack.pdf",
            "date": "2026-07-03",
            "type": "DOCUMENT_TYPE_RENEWAL_NOTICE",
            "vendor": { "name": "Slack" },
            "total": "145.30",
            "currency": "EUR",
            "state": "DOCUMENT_STATE_READ",
            "personal": true,
            "flags": 2,
        });

        let line = list_line(&document);

        assert!(line.starts_with("  412  2026-07-03  renewal notice"));
        assert!(line.contains("Slack"));
        assert!(line.contains("145.30 EUR"));
        assert!(line.ends_with("personal, 2 flagged"));
    }

    #[test]
    fn a_document_whole_with_its_reading() {
        let got = json!({
            "document": { "id": 7, "name": "vercel.pdf", "source": "upload:u1", "state": "DOCUMENT_STATE_READ", "paymentId": 3 },
            "reading": {
                "documentType": "DOCUMENT_TYPE_RECEIPT",
                "isReceipt": true,
                "status": "PAYMENT_STATUS_PAID",
                "vendor": { "id": "vercel.com", "name": "Vercel" },
                "amount": { "total": "20.00", "currency": "USD" },
                "extractor": "anthropic/claude-haiku-5-5",
                "readAt": "2026-11-24T10:00:00Z",
            },
            "edit": { "note": "the team plan", "updatedAt": "2026-11-24T11:00:00Z" },
        });

        let lines = describe(&got);

        assert_eq!(lines[0], "Document 7  vercel.pdf");
        assert!(lines.contains(
            &"  Read      2026-11-24T10:00:00Z by anthropic/claude-haiku-5-5".to_string()
        ));
        assert!(lines.contains(&"  Edited    note \"the team plan\"".to_string()));
        assert!(lines.contains(&"  Payment   #3".to_string()));
        assert!(lines.contains(&"Vercel (vercel.com) — receipt, paid".to_string()));
        assert!(lines.contains(&"  Total     20.00 USD".to_string()));
    }

    #[test]
    fn a_file_is_what_its_extension_says() {
        assert_eq!(content_type(Path::new("a/Invoice.PDF")), "application/pdf");
        assert_eq!(content_type(Path::new("mail.eml")), "message/rfc822");
        assert_eq!(
            content_type(Path::new("data.bin")),
            "application/octet-stream"
        );
    }
}
