//! `spendyard cloud pull`: corrections made in the cloud, brought to the
//! documents this machine has (the platform's docs/push-pull.md §3).
//!
//! It asks for the documents corrected since the newest correction the last
//! pull brought (`ListDocuments`, `edited_since`); the first pull asks for
//! every corrected one. Each document comes with its SHA-256 and its
//! correction (`DocumentEdit`, with when it was made):
//! one this machine has, corrected there later than here, takes the cloud's
//! correction whole. Newer here stays, for the next push to send. Documents
//! this machine hasn't got are passed over: the company's store stays the
//! cloud's.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli::cloud::Cloud;
use crate::cli::push::SentDb;
use crate::cli::store::Store;

/// How many documents a page of `ListDocuments` asks for: the most it gives.
const PAGE_SIZE: u32 = 500;

pub struct PullOptions {
    pub dry_run: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Listed {
    #[serde(default)]
    documents: Vec<Value>,
}

/// A correction, as the store keeps one.
#[derive(Debug, Clone, PartialEq, Default)]
struct Edit {
    personal: bool,
    ignored: bool,
    vendor_id: Option<String>,
    vendor_name: Option<String>,
    note: Option<String>,
    date: Option<String>,
    kind: Option<String>,
    total: Option<String>,
    currency: Option<String>,
    number: Option<String>,
    seats: Option<f64>,
    updated: String,
}

/// `spendyard cloud pull`.
pub async fn pull(options: PullOptions) -> Result<()> {
    let cloud = Cloud::connect()?;
    let store = Store::open()?;
    let sent_db = SentDb::open()?;

    let signed_in = cloud.signed_in_as();
    let place = (signed_in.server.clone(), signed_in.org.id.clone());

    let since = sent_db.pulled(&place)?;
    let documents = corrected_documents(&cloud, since.as_deref()).await?;

    // Where the next pull starts: the newest correction this one brought, or where this one started.
    let newest = documents
        .iter()
        .filter_map(|document| document["edit"]["updatedAt"].as_str())
        .max()
        .map(str::to_string)
        .or(since);

    // The newest of each: a document changed twice is the later change.
    let mut latest: BTreeMap<String, Value> = BTreeMap::new();

    for document in documents {
        if let Some(sha256) = document["sha256"].as_str() {
            latest.insert(sha256.to_string(), document);
        }
    }

    let mut corrected = 0;
    let mut newer_here = 0;

    for (sha256, document) in &latest {
        let Some(cloud_edit) = edit_from(&document["edit"]) else {
            continue;
        };

        let Some((id, name)) = local_document(&store, sha256)? else {
            continue;
        };

        let here = local_edit(&store, id)?;

        if here
            .as_ref()
            .is_some_and(|here| here.updated.as_str() >= cloud_edit.updated.as_str())
        {
            if here.as_ref() != Some(&cloud_edit) {
                newer_here += 1;
            }

            continue;
        }

        corrected += 1;

        let changes = differences(here.as_ref(), &cloud_edit);

        println!("Document {id}  {name}: {}", changes.join(", "));

        if !options.dry_run {
            write_edit(&store, id, &cloud_edit)?;

            sent_db.edit_pulled(&place, sha256, &cloud_edit.updated)?;
        }
    }

    let org = &signed_in.org.name;

    if options.dry_run {
        println!(
            "Would correct {corrected} documents from {org}; {newer_here} are corrected here since, for the next push"
        );

        return Ok(());
    }

    if let Some(newest) = &newest {
        sent_db.set_pulled(&place, newest)?;
    }

    println!(
        "Corrected {corrected} documents from {org}; {newer_here} are corrected here since, for the next push"
    );

    Ok(())
}

/// The documents corrected at or after `since` (a correction's
/// `updatedAt`), every one corrected when there's none: page after page,
/// newest first.
async fn corrected_documents(cloud: &Cloud, since: Option<&str>) -> Result<Vec<Value>> {
    let mut documents: Vec<Value> = Vec::new();
    let mut before_id = 0;

    loop {
        let mut request = json!({ "pageSize": PAGE_SIZE, "beforeId": before_id });

        // The first pull: every document corrected, the time before any was.
        request["editedSince"] = json!(since.unwrap_or("1970-01-01T00:00:00Z"));

        let page: Listed = cloud.call("DocumentService/ListDocuments", request).await?;
        let count = page.documents.len();

        before_id = page
            .documents
            .last()
            .and_then(|document| document["id"].as_i64())
            .unwrap_or(0);

        documents.extend(page.documents);

        if count < PAGE_SIZE as usize || before_id == 0 {
            break;
        }
    }

    Ok(documents)
}

/// A `DocumentEdit`, in protobuf's JSON form, as the store keeps one. None
/// when there's none, or it says only that it was paid (the CLI keeps no
/// paid marks).
fn edit_from(edit: &Value) -> Option<Edit> {
    let updated = edit["updatedAt"].as_str()?;

    let text = |key: &str| {
        edit[key]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };

    // DOCUMENT_TYPE_CREDIT_NOTE is the store's `credit_note`.
    let kind = text("type")
        .and_then(|name| name.strip_prefix("DOCUMENT_TYPE_").map(str::to_lowercase))
        .filter(|kind| kind != "unspecified");

    let edit = Edit {
        personal: edit["personal"].as_bool().unwrap_or(false),
        ignored: edit["ignored"].as_bool().unwrap_or(false),
        vendor_id: text("vendorId"),
        vendor_name: text("vendorName"),
        note: text("note"),
        date: text("date"),
        kind,
        total: text("total"),
        currency: text("currency"),
        number: text("number"),
        seats: edit["seats"].as_f64().filter(|seats| *seats != 0.0),
        updated: store_time(updated),
    };

    let only_paid = edit
        == Edit {
            updated: edit.updated.clone(),
            ..Edit::default()
        };

    (!only_paid).then_some(edit)
}

/// A time as the store writes its times: RFC 3339 in UTC, to the second.
fn store_time(at: &str) -> String {
    let second: String = at.chars().take(19).collect();

    format!("{second}Z")
}

fn local_document(store: &Store, sha256: &str) -> Result<Option<(i64, String)>> {
    let found = store
        .connection
        .query_row(
            "SELECT id, coalesce(name, '') FROM documents WHERE sha256 = ?1",
            params![sha256],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;

    Ok(found)
}

fn local_edit(store: &Store, id: i64) -> Result<Option<Edit>> {
    let found = store
        .connection
        .query_row(
            "SELECT personal, ignored, vendor_id, vendor_name, note, date, type, total, currency, number, seats, updated
             FROM edits WHERE document_id = ?1",
            params![id],
            |row| {
                Ok(Edit {
                    personal: row.get("personal")?,
                    ignored: row.get("ignored")?,
                    vendor_id: row.get("vendor_id")?,
                    vendor_name: row.get("vendor_name")?,
                    note: row.get("note")?,
                    date: row.get("date")?,
                    kind: row.get("type")?,
                    total: row.get("total")?,
                    currency: row.get("currency")?,
                    number: row.get("number")?,
                    seats: row.get("seats")?,
                    updated: row.get("updated")?,
                })
            },
        )
        .optional()?;

    Ok(found)
}

/// The cloud's correction in place of this machine's, whole.
fn write_edit(store: &Store, id: i64, edit: &Edit) -> Result<()> {
    store.connection.execute(
        "INSERT INTO edits (document_id, personal, ignored, vendor_id, vendor_name, note, date, type, total, currency, number, seats, updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (document_id) DO UPDATE SET
             personal = ?2, ignored = ?3, vendor_id = ?4, vendor_name = ?5, note = ?6, date = ?7,
             type = ?8, total = ?9, currency = ?10, number = ?11, seats = ?12, updated = ?13",
        params![
            id,
            edit.personal,
            edit.ignored,
            edit.vendor_id,
            edit.vendor_name,
            edit.note,
            edit.date,
            edit.kind,
            edit.total,
            edit.currency,
            edit.number,
            edit.seats,
            edit.updated,
        ],
    )?;

    Ok(())
}

/// What a correction changes, field by field: `personal: no → yes`.
fn differences(here: Option<&Edit>, there: &Edit) -> Vec<String> {
    let none = Edit::default();
    let here = here.unwrap_or(&none);

    let yes_no = |on: bool| if on { "yes" } else { "no" }.to_string();
    let text = |value: &Option<String>| value.clone().unwrap_or_else(|| "—".to_string());
    let seats = |value: Option<f64>| {
        value
            .map(|seats| seats.to_string())
            .unwrap_or_else(|| "—".to_string())
    };

    let fields = [
        ("personal", yes_no(here.personal), yes_no(there.personal)),
        ("set aside", yes_no(here.ignored), yes_no(there.ignored)),
        ("vendor", text(&here.vendor_name), text(&there.vendor_name)),
        ("note", text(&here.note), text(&there.note)),
        ("date", text(&here.date), text(&there.date)),
        ("type", text(&here.kind), text(&there.kind)),
        ("total", text(&here.total), text(&there.total)),
        ("currency", text(&here.currency), text(&there.currency)),
        ("number", text(&here.number), text(&there.number)),
        ("seats", seats(here.seats), seats(there.seats)),
    ];

    let changed: Vec<String> = fields
        .into_iter()
        .filter(|(_, before, after)| before != after)
        .map(|(field, before, after)| format!("{field}: {before} → {after}"))
        .collect();

    if changed.is_empty() {
        vec!["as it is here, newer".to_string()]
    } else {
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_edit_is_the_stores_correction() {
        let edit = edit_from(&json!({
            "personal": true,
            "note": "from the browser",
            "type": "DOCUMENT_TYPE_CREDIT_NOTE",
            "seats": 0,
            "updatedAt": "2026-11-24T10:00:00.250Z",
        }))
        .unwrap();

        assert!(edit.personal);
        assert_eq!(edit.note.as_deref(), Some("from the browser"));
        assert_eq!(edit.kind.as_deref(), Some("credit_note"));
        assert_eq!(edit.seats, None);
        assert_eq!(edit.updated, "2026-11-24T10:00:00Z");
    }

    #[test]
    fn only_marked_paid_is_no_correction_here() {
        let edit = edit_from(&json!({ "paid": true, "updatedAt": "2026-11-24T10:00:00Z" }));

        assert_eq!(edit, None);
        assert_eq!(edit_from(&json!({})), None);
    }

    #[test]
    fn says_what_changes_field_by_field() {
        let here = Edit {
            note: Some("old".to_string()),
            updated: "2026-11-01T10:00:00Z".to_string(),
            ..Edit::default()
        };

        let there = Edit {
            personal: true,
            note: Some("new".to_string()),
            updated: "2026-11-24T10:00:00Z".to_string(),
            ..Edit::default()
        };

        assert_eq!(
            differences(Some(&here), &there),
            vec!["personal: no → yes", "note: old → new"]
        );
    }
}
