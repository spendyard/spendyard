//! Detecting an input's format from its first bytes. File names and
//! `Content-Type` headers are not trusted.

use crate::document::Format;

/// How much of the start of a text input is looked at for email headers or
/// HTML.
const SNIFF_LEN: usize = 4096;

/// Headers that only an email starts with. A text file starting with
/// "Subject:" alone is not enough; we need two of these.
const EMAIL_HEADERS: &[&str] = &[
    "from:",
    "to:",
    "subject:",
    "date:",
    "message-id:",
    "mime-version:",
    "received:",
    "return-path:",
    "delivered-to:",
    "content-type:",
    "x-",
    "dkim-signature:",
    "arc-",
    "authentication-results:",
    "reply-to:",
    "cc:",
];

/// Detects the format of `bytes`.
pub fn detect(bytes: &[u8]) -> Format {
    if let Some(format) = detect_binary(bytes) {
        return format;
    }

    let Some(start) = text_start(bytes) else {
        return Format::Unknown;
    };

    if looks_like_email(&start) {
        return Format::Email;
    }

    if looks_like_html(&start) {
        return Format::Html;
    }

    Format::Text
}

/// Formats with a fixed signature at the start.
fn detect_binary(bytes: &[u8]) -> Option<Format> {
    if find_pdf_header(bytes) {
        return Some(Format::Pdf);
    }

    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Format::Png);
    }

    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(Format::Jpeg);
    }

    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(Format::Gif);
    }

    let is_riff_webp = bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP";

    if is_riff_webp {
        return Some(Format::Webp);
    }

    if is_heic(bytes) {
        return Some(Format::Heic);
    }

    // Compound File Binary, the container Outlook .msg files use.
    if bytes.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        return Some(Format::OutlookMsg);
    }

    None
}

/// PDFs may have a little junk before `%PDF-`; readers allow up to 1 KB.
fn find_pdf_header(bytes: &[u8]) -> bool {
    let start = &bytes[..bytes.len().min(1024)];

    start.windows(5).any(|window| window == b"%PDF-")
}

/// HEIC and HEIF: an ISO base media `ftyp` box with an image brand.
fn is_heic(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }

    let brand = &bytes[8..12];

    matches!(
        brand,
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1" | b"heim" | b"heis"
    )
}

/// The start of the input as lowercase text, or `None` if it isn't text.
fn text_start(bytes: &[u8]) -> Option<String> {
    let start = &bytes[..bytes.len().min(SNIFF_LEN)];

    let has_nul = start.contains(&0);

    if has_nul {
        return None;
    }

    // The cut at SNIFF_LEN may split a UTF-8 character; that's fine.
    let text = String::from_utf8_lossy(start);

    let replaced = text
        .chars()
        .filter(|c| *c == char::REPLACEMENT_CHARACTER)
        .count();

    let mostly_binary = replaced > text.len() / 10;

    if mostly_binary {
        return None;
    }

    Some(text.to_lowercase())
}

/// The input starts with a block of email headers.
fn looks_like_email(start: &str) -> bool {
    let header_block = start
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .split("\r\n\r\n")
        .next()
        .unwrap_or_default();

    let mut known = 0;

    for line in header_block.lines() {
        let is_continuation = line.starts_with(' ') || line.starts_with('\t');

        if is_continuation || line.is_empty() {
            continue;
        }

        // mbox "From " separator line.
        if line.starts_with("from ") && known == 0 {
            continue;
        }

        let looks_like_header = line
            .split_once(':')
            .is_some_and(|(name, _)| !name.is_empty() && !name.contains(' '));

        if !looks_like_header {
            return false;
        }

        if EMAIL_HEADERS.iter().any(|header| line.starts_with(header)) {
            known += 1;
        }
    }

    known >= 2
}

fn looks_like_html(start: &str) -> bool {
    let trimmed = start.trim_start_matches('\u{feff}').trim_start();

    let markers = [
        "<!doctype html",
        "<html",
        "<head",
        "<body",
        "<table",
        "<div",
        "<meta",
    ];

    markers.iter().any(|marker| trimmed.starts_with(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_binary_formats() {
        assert_eq!(detect(b"%PDF-1.7\n..."), Format::Pdf);
        assert_eq!(detect(b"\x89PNG\r\n\x1a\n...."), Format::Png);
        assert_eq!(detect(b"\xff\xd8\xff\xe0...."), Format::Jpeg);
        assert_eq!(detect(b"GIF89a...."), Format::Gif);
        assert_eq!(detect(b"RIFF\0\0\0\0WEBPVP8 "), Format::Webp);
        assert_eq!(detect(b"\0\0\0\x18ftypheic\0\0\0\0"), Format::Heic);
        assert_eq!(
            detect(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1...."),
            Format::OutlookMsg
        );
    }

    #[test]
    fn detects_email() {
        let email = b"Received: from mx.example.com\r\nFrom: Acme <billing@acme.example>\r\nSubject: Your receipt\r\n\r\nThanks!";

        assert_eq!(detect(email), Format::Email);
    }

    #[test]
    fn text_with_one_header_like_line_is_text() {
        let text = b"Subject: lunch\n\nShall we?";

        assert_eq!(detect(text), Format::Text);
    }

    #[test]
    fn detects_html_and_text() {
        assert_eq!(
            detect(b"<!DOCTYPE html><html><body>Hi</body></html>"),
            Format::Html
        );
        assert_eq!(detect(b"  <html><body>Hi</body></html>"), Format::Html);
        assert_eq!(
            detect("Kvittering\nBeløp: kr 1 234,50".as_bytes()),
            Format::Text
        );
    }

    #[test]
    fn binary_junk_is_unknown() {
        let junk: Vec<u8> = (0..=255).collect();

        assert_eq!(detect(&junk), Format::Unknown);
    }
}
