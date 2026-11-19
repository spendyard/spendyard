//! Outlook `.msg` files: a Compound File Binary container whose streams
//! hold the message's properties ([MS-OXMSG]).
//!
//! Only what receipts need is read: sender, recipients, subject, date,
//! message ID, the text and HTML bodies, and attachments, including
//! attached Outlook messages.

use std::io::{Cursor, Read, Seek};

use cfb::CompoundFile;

use super::{MAX_EMAIL_DEPTH, ReadError, number, read_part};
use crate::document::{Document, EmailHeaders, Format, Part, PartKind};
use crate::format;
use crate::text;

// Property IDs.
const SUBJECT: u16 = 0x0037;
const CLIENT_SUBMIT_TIME: u16 = 0x0039;
const SENDER_NAME: u16 = 0x0C1A;
const SENDER_EMAIL: u16 = 0x0C1F;
const SENDER_SMTP: u16 = 0x5D01;
const DISPLAY_TO: u16 = 0x0E04;
const BODY: u16 = 0x1000;
const HTML: u16 = 0x1013;
const MESSAGE_ID: u16 = 0x1035;
const ATTACH_DATA: u16 = 0x3701;
const ATTACH_FILENAME: u16 = 0x3704;
const ATTACH_LONG_FILENAME: u16 = 0x3707;
const ATTACH_MIME: u16 = 0x370E;

// Property types.
const STRING_8: u16 = 0x001E;
const UNICODE: u16 = 0x001F;
const BINARY: u16 = 0x0102;
const OBJECT: u16 = 0x000D;
const SYSTIME: u16 = 0x0040;

/// Size of the header before the fixed-size property entries in
/// `__properties_version1.0` of the top-level message.
const TOP_PROPERTIES_HEADER: usize = 32;

pub fn read(bytes: &[u8]) -> Result<Document, ReadError> {
    let file = CompoundFile::open(Cursor::new(bytes)).map_err(|error| ReadError::Unreadable {
        format: Format::OutlookMsg,
        reason: error.to_string(),
    })?;

    let mut reader = MsgReader {
        file,
        parts: Vec::new(),
        warnings: Vec::new(),
        forwarded: 0,
    };

    let headers = reader.headers("");

    reader.message("", "", 0);

    let mut parts = reader.parts;

    number(&mut parts);

    let document = Document {
        format: Format::OutlookMsg,
        headers: Some(headers),
        parts,
        warnings: reader.warnings,
    };

    Ok(document)
}

struct MsgReader<F> {
    file: CompoundFile<F>,
    parts: Vec<Part>,
    warnings: Vec<String>,
    forwarded: usize,
}

impl<F: Read + Seek> MsgReader<F> {
    /// Adds the body and attachments of the message stored at `storage`
    /// ("" for the top-level message).
    fn message(&mut self, storage: &str, prefix: &str, depth: usize) {
        if let Some(body) = self.body(storage, prefix) {
            self.parts.push(body);
        }

        for attachment in self.attachment_storages(storage) {
            self.attachment(&attachment, prefix, depth);
        }
    }

    fn headers(&mut self, storage: &str) -> EmailHeaders {
        let sender_name = self.string(storage, SENDER_NAME);

        let sender_address = self
            .string(storage, SENDER_SMTP)
            .or_else(|| self.string(storage, SENDER_EMAIL))
            .filter(|address| address.contains('@'));

        let from = match (sender_name, sender_address) {
            (Some(name), Some(address)) => Some(format!("{name} <{address}>")),
            (name, address) => address.or(name),
        };

        let message_id = self
            .string(storage, MESSAGE_ID)
            .map(|id| id.trim_matches(['<', '>']).to_string());

        EmailHeaders {
            from,
            to: self.string(storage, DISPLAY_TO),
            subject: self.string(storage, SUBJECT),
            date: self.time(storage, CLIENT_SUBMIT_TIME),
            message_id,
        }
    }

    /// HTML when there is HTML, otherwise the plain-text body.
    fn body(&mut self, storage: &str, prefix: &str) -> Option<Part> {
        let label = format!("{prefix}body");

        let html = self
            .binary(storage, HTML)
            .or_else(|| self.string(storage, HTML).map(String::into_bytes));

        if let Some(html) = html {
            let part = Part {
                index: 0,
                label,
                kind: PartKind::Html,
                mime: "text/html".to_string(),
                size: html.len(),
                text: Some(text::html_to_text(&html)),
                bytes: html,
                pages: None,
                inline: false,
            };

            return Some(part);
        }

        let Some(plain) = self.string(storage, BODY) else {
            self.warnings.push(format!(
                "{label}: no text or HTML body (RTF-only bodies aren't read)"
            ));

            return None;
        };

        let part = Part {
            index: 0,
            label,
            kind: PartKind::Text,
            mime: "text/plain".to_string(),
            size: plain.len(),
            text: Some(text::tidy(&plain)),
            bytes: plain.into_bytes(),
            pages: None,
            inline: false,
        };

        Some(part)
    }

    fn attachment(&mut self, storage: &str, prefix: &str, depth: usize) {
        let name = self
            .string(storage, ATTACH_LONG_FILENAME)
            .or_else(|| self.string(storage, ATTACH_FILENAME))
            .unwrap_or_else(|| "attachment".to_string());

        let label = format!("{prefix}attachment:{name}");

        // An attached Outlook message is a storage of its own.
        let embedded = stream_path(storage, ATTACH_DATA, OBJECT);

        if self.file.is_storage(&embedded) {
            self.embedded_message(&embedded, &label, depth);

            return;
        }

        let Some(bytes) = self.binary(storage, ATTACH_DATA) else {
            self.warnings.push(format!("{label}: no data"));

            return;
        };

        let detected = format::detect(&bytes);

        let read = read_part(&bytes, detected, label.clone(), &mut self.warnings);

        match read {
            Ok(mut part) => {
                let mime = self.string(storage, ATTACH_MIME);

                if part.kind == PartKind::Other
                    && let Some(mime) = mime
                {
                    part.mime = mime;
                }

                self.parts.push(part);
            }

            Err(error) => {
                self.warnings.push(format!("{label}: {error}"));
            }
        }
    }

    fn embedded_message(&mut self, storage: &str, label: &str, depth: usize) {
        if depth + 1 >= MAX_EMAIL_DEPTH {
            self.warnings.push(format!(
                "{label}: forwarded more than {MAX_EMAIL_DEPTH} levels deep; skipped"
            ));

            return;
        }

        self.forwarded += 1;

        let prefix = format!("forwarded:{}/", self.forwarded);

        self.message(storage, &prefix, depth + 1);
    }

    /// The attachment storages directly under `storage`, in order.
    fn attachment_storages(&self, storage: &str) -> Vec<String> {
        let root = if storage.is_empty() { "/" } else { storage };

        let Ok(entries) = self.file.read_storage(root) else {
            return Vec::new();
        };

        let mut names: Vec<String> = entries
            .filter(|entry| entry.is_storage())
            .filter(|entry| entry.name().starts_with("__attach_version1.0_#"))
            .map(|entry| entry.path().to_string_lossy().into_owned())
            .collect();

        names.sort();

        names
    }

    /// A string property, stored as UTF-16 or as 8-bit text.
    fn string(&mut self, storage: &str, id: u16) -> Option<String> {
        if let Some(bytes) = self.stream(&stream_path(storage, id, UNICODE)) {
            let (pairs, _) = bytes.as_chunks::<2>();

            let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();

            let decoded = String::from_utf16_lossy(&units);

            return non_empty(decoded.trim_end_matches('\0'));
        }

        let bytes = self.stream(&stream_path(storage, id, STRING_8))?;

        let decoded = String::from_utf8_lossy(&bytes);

        non_empty(decoded.trim_end_matches('\0'))
    }

    fn binary(&mut self, storage: &str, id: u16) -> Option<Vec<u8>> {
        self.stream(&stream_path(storage, id, BINARY))
            .filter(|bytes| !bytes.is_empty())
    }

    /// A time property from the top-level message's fixed-size property
    /// stream, as RFC 3339.
    fn time(&mut self, storage: &str, id: u16) -> Option<String> {
        let path = format!("{}/__properties_version1.0", storage.trim_end_matches('/'));

        let properties = self.stream(&path)?;

        let wanted_tag = (u32::from(id) << 16) | u32::from(SYSTIME);

        let (entries, _) = properties.get(TOP_PROPERTIES_HEADER..)?.as_chunks::<16>();

        for entry in entries {
            let tag = u32::from_le_bytes(entry[0..4].try_into().ok()?);

            if tag != wanted_tag {
                continue;
            }

            let filetime = u64::from_le_bytes(entry[8..16].try_into().ok()?);

            return filetime_to_rfc3339(filetime);
        }

        None
    }

    fn stream(&mut self, path: &str) -> Option<Vec<u8>> {
        if !self.file.is_stream(path) {
            return None;
        }

        let mut stream = self.file.open_stream(path).ok()?;

        let mut bytes = Vec::new();

        stream.read_to_end(&mut bytes).ok()?;

        Some(bytes)
    }
}

/// `{storage}/__substg1.0_{ID}{TYPE}`, for example `/__substg1.0_0037001F`.
fn stream_path(storage: &str, id: u16, kind: u16) -> String {
    let storage = storage.trim_end_matches('/');

    format!("{storage}/__substg1.0_{id:04X}{kind:04X}")
}

fn non_empty(text: &str) -> Option<String> {
    let trimmed = text.trim();

    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A Windows FILETIME (100 ns ticks since 1601-01-01 UTC) as RFC 3339.
fn filetime_to_rfc3339(filetime: u64) -> Option<String> {
    const TICKS_PER_SECOND: u64 = 10_000_000;
    const SECONDS_1601_TO_1970: u64 = 11_644_473_600;

    let seconds = (filetime / TICKS_PER_SECOND).checked_sub(SECONDS_1601_TO_1970)?;

    let days = (seconds / 86_400) as i64;
    let time_of_day = seconds % 86_400;

    let (year, month, day) = civil_from_days(days);

    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    let formatted = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z");

    Some(formatted)
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

    let year = year_of_era + era * 400 + i64::from(month <= 2);

    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    type MemoryFile = CompoundFile<Cursor<Vec<u8>>>;

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    fn write(file: &mut MemoryFile, path: &str, bytes: &[u8]) {
        file.create_stream(path).unwrap().write_all(bytes).unwrap();
    }

    /// Builds a small .msg in memory, laid out as Outlook does.
    fn sample_msg() -> Vec<u8> {
        let mut file = CompoundFile::create(Cursor::new(Vec::new())).unwrap();

        write(
            &mut file,
            &stream_path("", SUBJECT, UNICODE),
            &utf16("Your receipt from Acme"),
        );
        write(
            &mut file,
            &stream_path("", SENDER_NAME, UNICODE),
            &utf16("Acme Billing"),
        );
        write(
            &mut file,
            &stream_path("", SENDER_SMTP, UNICODE),
            &utf16("billing@acme.example"),
        );
        write(
            &mut file,
            &stream_path("", BODY, UNICODE),
            &utf16("Amount paid: $112.40\r\n"),
        );

        // 2026-11-19T12:00:00Z as a FILETIME.
        let filetime: u64 = (1_795_089_600 + 11_644_473_600) * 10_000_000;

        let tag = (u32::from(CLIENT_SUBMIT_TIME) << 16) | u32::from(SYSTIME);

        let mut properties = vec![0u8; TOP_PROPERTIES_HEADER];

        properties.extend_from_slice(&tag.to_le_bytes());
        properties.extend_from_slice(&[0; 4]);
        properties.extend_from_slice(&filetime.to_le_bytes());

        write(&mut file, "/__properties_version1.0", &properties);

        let attachment = "/__attach_version1.0_#00000000";

        file.create_storage(attachment).unwrap();

        write(
            &mut file,
            &stream_path(attachment, ATTACH_LONG_FILENAME, UNICODE),
            &utf16("notes.txt"),
        );

        write(
            &mut file,
            &stream_path(attachment, ATTACH_DATA, BINARY),
            b"Invoice A1B2C3D4-0007",
        );

        file.into_inner().into_inner()
    }

    #[test]
    fn reads_headers_body_and_attachment() {
        let document = read(&sample_msg()).unwrap();

        let headers = document.headers.as_ref().unwrap();

        assert_eq!(headers.subject.as_deref(), Some("Your receipt from Acme"));
        assert_eq!(
            headers.from.as_deref(),
            Some("Acme Billing <billing@acme.example>")
        );
        assert_eq!(headers.date.as_deref(), Some("2026-11-19T12:00:00Z"));

        assert_eq!(document.parts.len(), 2);

        let body = &document.parts[0];

        assert_eq!(body.label, "body");
        assert_eq!(body.text_or_empty(), "Amount paid: $112.40");

        let attachment = &document.parts[1];

        assert_eq!(attachment.index, 1);
        assert_eq!(attachment.label, "attachment:notes.txt");
        assert_eq!(attachment.text_or_empty(), "Invoice A1B2C3D4-0007");
    }

    #[test]
    fn converts_days_to_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_411), (2025, 11, 19));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }
}
