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

    let lopdf_text = text::tidy(&raw);

    // lopdf misses text in some fonts (Northflank's invoices lose their
    // amounts). When its text looks thin or garbled, pdf-extract reads it
    // again, and the better of the two is kept.
    let text = if looks_thin(&lopdf_text, pages) || looks_garbled(&lopdf_text) {
        let second = second_reading(bytes);

        better_text(lopdf_text, second)
    } else {
        lopdf_text
    };

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

/// Below this many visible characters per page, lopdf's text is worth a
/// second reading. A one-page receipt has several hundred.
const THIN_CHARS_PER_PAGE: usize = 200;

fn looks_thin(text: &str, pages: u32) -> bool {
    let visible = text.chars().filter(|c| !c.is_whitespace()).count();

    visible < THIN_CHARS_PER_PAGE * pages.max(1) as usize
}

/// The text as pdf-extract reads it. It panics on some malformed PDFs,
/// which only means there is no second reading.
fn second_reading(bytes: &[u8]) -> Option<String> {
    let extracted = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes));

    match extracted {
        Ok(Ok(raw)) => Some(text::tidy(&raw)),
        _ => None,
    }
}

/// The readable one with the most visible text; the first when neither is
/// readable.
fn better_text(first: String, second: Option<String>) -> String {
    let Some(second) = second else {
        return first;
    };

    let visible = |text: &str| text.chars().filter(|c| !c.is_whitespace()).count();

    let second_wins = match (looks_garbled(&first), looks_garbled(&second)) {
        (true, false) => true,
        (false, true) => false,
        _ => visible(&second) > visible(&first),
    };

    if second_wins { second } else { first }
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
    fn the_readable_longer_text_wins() {
        let short = "Invoice Paid".to_string();
        let full = "Invoice Paid Total $286.77 Total paid $286.77".to_string();
        let garbled = "Na!$ on &ar#: +NNK78P,KON.O )+, N, 4 *at$ o/ (\"r&9as$".to_string();

        assert_eq!(better_text(short.clone(), Some(full.clone())), full);
        assert_eq!(better_text(full.clone(), Some(short.clone())), full);
        assert_eq!(better_text(garbled.clone(), Some(short.clone())), short);
        assert_eq!(better_text(short.clone(), None), short);
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
