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

    // A text layer whose fonts can't be decoded comes out as symbols. The
    // model is better off with the pages themselves.
    let readable = has_text_layer && !looks_garbled(&text);

    let pdf = Pdf {
        pages,
        text: readable.then_some(text),
    };

    Ok(pdf)
}

/// Symbols that are rare in real text but fill a text layer decoded with
/// the wrong font encoding (`Na!$ on &ar#` for "Name on card").
const GARBLE_SYMBOLS: &str = "!\"#$%&*+;<=>?@[\\]^_{|}~";

/// Above this many such symbols per letter, the text is taken as garbled.
/// Real receipts and emails, URLs included, stay under 0.2.
const MAX_SYMBOLS_PER_LETTER: f64 = 0.3;

/// Whether `text` looks like a text layer decoded with the wrong font
/// encoding.
pub fn looks_garbled(text: &str) -> bool {
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();

    let symbols = text.chars().filter(|c| GARBLE_SYMBOLS.contains(*c)).count();

    if letters == 0 {
        return symbols > 0;
    }

    let ratio = symbols as f64 / letters as f64;

    ratio > MAX_SYMBOLS_PER_LETTER
}

fn unreadable(reason: String) -> ReadError {
    ReadError::Unreadable {
        format: Format::Pdf,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_text_is_not_garbled() {
        let receipt = "Receipt #1234-5678\nAmount paid $112.40\nVisa ending in 4242\n\
                       https://pay.stripe.com/receipts/acct_1?s=ap&id=5";

        assert!(!looks_garbled(receipt));
    }

    #[test]
    fn wrongly_decoded_fonts_are_garbled() {
        let half = "Na!$ on &ar#: +NNK78P,KON.O )+, N, 4\n*at$ o/ (\"r&9as$: 2026-04-14\n\
                    Original a!o\"nt: 2.26 2,*";

        let symbols_only = "5#\"!$000000 !$&#+78,.)+, , 4 *$/(\"&9$ !\"2,* 5%$$";

        assert!(looks_garbled(half));
        assert!(looks_garbled(symbols_only));
    }
}
