//! Emails (RFC 822 / `.eml`): the body, the attachments, and forwarded
//! emails flattened in order.

use mail_parser::{AddressList, MessageParser, MessagePart, MessageRef, PartKind as MailPartKind};

use super::{MAX_EMAIL_DEPTH, number, read_part};
use crate::document::{Document, EmailHeaders, Format, Part, PartKind};
use crate::format;
use crate::text;

/// Reads an email. A malformed email is not an error: it falls back to
/// being read as text, with a warning.
pub fn read(bytes: &[u8]) -> Document {
    let Some(message) = MessageParser::default().parse(bytes) else {
        return as_text(bytes);
    };

    let root = message.root();

    let mut reader = Flattener::default();

    reader.message(root, "", 0);

    let mut parts = reader.parts;

    number(&mut parts);

    Document {
        format: Format::Email,
        headers: Some(headers(root)),
        parts,
        warnings: reader.warnings,
    }
}

/// Collects parts from a message and the messages forwarded inside it.
#[derive(Default)]
struct Flattener {
    parts: Vec<Part>,
    warnings: Vec<String>,
    forwarded: usize,
}

impl Flattener {
    /// Adds a message's body and attachments. `prefix` is prepended to
    /// labels, such as `forwarded:1/`.
    fn message(&mut self, message: MessageRef<'_>, prefix: &str, depth: usize) {
        if let Some(body) = body(message, prefix) {
            self.parts.push(body);
        }

        for (position, attachment) in message.attachments().enumerate() {
            self.attachment(attachment, position, prefix, depth);
        }
    }

    fn attachment(
        &mut self,
        attachment: MessagePart<'_>,
        position: usize,
        prefix: &str,
        depth: usize,
    ) {
        let name = attachment
            .attachment_name()
            .map(str::to_string)
            .unwrap_or_else(|| format!("attachment-{}", position + 1));

        let label = format!("{prefix}attachment:{name}");

        // A forwarded email that mail-parser already parsed.
        if let MailPartKind::Message(nested) = attachment.kind() {
            self.forwarded_message(nested, &label, depth);

            return;
        }

        let bytes = attachment.decoded();

        let detected = format::detect(&bytes);

        // A forwarded email sent as a plain file, such as an attached .eml.
        if detected == Format::Email {
            self.forwarded_bytes(&bytes, &label, depth);

            return;
        }

        let read = read_part(&bytes, detected, label.clone(), &mut self.warnings);

        match read {
            Ok(mut part) => {
                part.inline = attachment.is_inline() && part.kind == PartKind::Image;

                self.parts.push(part);
            }

            Err(error) => {
                self.warnings.push(format!("{label}: {error}"));
            }
        }
    }

    fn forwarded_message(&mut self, nested: MessageRef<'_>, label: &str, depth: usize) {
        if depth + 1 >= MAX_EMAIL_DEPTH {
            self.warnings.push(format!(
                "{label}: forwarded more than {MAX_EMAIL_DEPTH} levels deep; skipped"
            ));

            return;
        }

        self.forwarded += 1;

        let prefix = format!("forwarded:{}/", self.forwarded);

        self.message(nested, &prefix, depth + 1);
    }

    fn forwarded_bytes(&mut self, bytes: &[u8], label: &str, depth: usize) {
        let Some(message) = MessageParser::default().parse(bytes) else {
            self.warnings
                .push(format!("{label}: an email that couldn't be parsed"));

            return;
        };

        self.forwarded_message(message.root(), label, depth);
    }
}

/// The message body as one part: HTML when there is real HTML (receipts
/// usually look best that way), otherwise the plain text.
fn body(message: MessageRef<'_>, prefix: &str) -> Option<Part> {
    let label = format!("{prefix}body");

    let html: Vec<String> = message
        .html_body()
        .filter(|part| matches!(part.kind(), MailPartKind::Html))
        .filter_map(|part| part.text().map(|text| text.into_owned()))
        .collect();

    if !html.is_empty() {
        let html = html.join("\n");

        let part = Part {
            index: 0,
            label,
            kind: PartKind::Html,
            mime: "text/html".to_string(),
            size: html.len(),
            text: Some(text::html_to_text(html.as_bytes())),
            bytes: html.into_bytes(),
            pages: None,
            inline: false,
        };

        return Some(part);
    }

    let plain: Vec<String> = message
        .text_body()
        .filter(|part| matches!(part.kind(), MailPartKind::Text))
        .filter_map(|part| part.text().map(|text| text.into_owned()))
        .collect();

    if plain.is_empty() {
        return None;
    }

    let plain = plain.join("\n");

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

fn headers(message: MessageRef<'_>) -> EmailHeaders {
    EmailHeaders {
        from: message.from().map(addresses),
        to: message.to().map(addresses),
        subject: message.subject().map(str::to_string),
        date: message.date().map(|date| date.to_rfc3339()),
        message_id: message.message_id().map(str::to_string),
    }
}

/// `Name <address>, …`
fn addresses(list: AddressList<'_>) -> String {
    let formatted: Vec<String> = list
        .mailboxes()
        .map(|mailbox| match (mailbox.name(), mailbox.address()) {
            (Some(name), Some(address)) => format!("{name} <{address}>"),
            (None, Some(address)) => address.to_string(),
            (Some(name), None) => name.to_string(),
            (None, None) => String::new(),
        })
        .filter(|formatted| !formatted.is_empty())
        .collect();

    formatted.join(", ")
}

/// Fallback for input that looked like an email but couldn't be parsed.
fn as_text(bytes: &[u8]) -> Document {
    let text = String::from_utf8_lossy(bytes);

    let part = Part {
        index: 0,
        label: "body".to_string(),
        kind: PartKind::Text,
        mime: "text/plain".to_string(),
        bytes: bytes.to_vec(),
        size: bytes.len(),
        text: Some(text::tidy(&text)),
        pages: None,
        inline: false,
    };

    Document {
        format: Format::Email,
        headers: None,
        parts: vec![part],
        warnings: vec!["the email couldn't be parsed; read as text".to_string()],
    }
}
