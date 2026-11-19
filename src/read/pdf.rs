//! The text layer of a PDF. No rendering and no OCR: a PDF without text is
//! returned without text, and a model that can see reads it later.

use lopdf::Document;

use super::ReadError;
use crate::document::Format;
use crate::text;

/// Fewer visible characters than this per page means there is no real text
/// layer (a scan, or just a page number).
const MIN_CHARS_PER_PAGE: usize = 20;

/// Limit on decompressed content per PDF, against zip bombs.
const MAX_DECOMPRESSED_BYTES: usize = 100 * 1024 * 1024;

pub struct Pdf {
    pub pages: u32,
    pub text: Option<String>,
}

pub fn read(bytes: &[u8]) -> Result<Pdf, ReadError> {
    let document = Document::load_mem(bytes).map_err(|error| unreadable(error.to_string()))?;

    // lopdf opens PDFs that are encrypted with an empty password. One still
    // encrypted after loading needs a password we don't have.
    if document.is_encrypted() {
        return Err(unreadable("it is encrypted with a password".to_string()));
    }

    let page_numbers: Vec<u32> = document.get_pages().keys().copied().collect();

    let pages = page_numbers.len() as u32;

    let raw = document
        .extract_text_with_limit(&page_numbers, MAX_DECOMPRESSED_BYTES)
        .unwrap_or_default();

    let text = text::tidy(&raw);

    let visible = text.chars().filter(|c| !c.is_whitespace()).count();

    let has_text_layer = visible >= MIN_CHARS_PER_PAGE * (pages.max(1) as usize).min(3);

    let pdf = Pdf {
        pages,
        text: has_text_layer.then_some(text),
    };

    Ok(pdf)
}

fn unreadable(reason: String) -> ReadError {
    ReadError::Unreadable {
        format: Format::Pdf,
        reason,
    }
}
