//! Reading any input into a [`Document`]. No models are involved.

mod email;
mod msg;
mod pdf;

use thiserror::Error;

use crate::document::{Document, Format, Part, PartKind};
use crate::format;
use crate::text;

/// The largest input accepted.
pub const MAX_INPUT_BYTES: usize = 25 * 1024 * 1024;

/// How deep forwarded emails are followed.
pub const MAX_EMAIL_DEPTH: usize = 3;

/// Why an input couldn't be read.
#[derive(Debug, Error)]
pub enum ReadError {
    #[error("the input is empty")]
    Empty,

    #[error("the input is {size} bytes; the limit is {MAX_INPUT_BYTES}")]
    TooLarge { size: usize },

    #[error("{0:?} is not a format Spendyard reads")]
    Unsupported(Format),

    #[error("the {format:?} can't be read: {reason}")]
    Unreadable { format: Format, reason: String },
}

/// Reads `bytes` into a [`Document`]. `name` is the file name, if there is
/// one; it is used for labels only, never to decide the format.
pub fn read(bytes: &[u8], name: Option<&str>) -> Result<Document, ReadError> {
    if bytes.is_empty() {
        return Err(ReadError::Empty);
    }

    if bytes.len() > MAX_INPUT_BYTES {
        return Err(ReadError::TooLarge { size: bytes.len() });
    }

    let format = format::detect(bytes);

    match format {
        Format::Email => Ok(email::read(bytes)),
        Format::OutlookMsg => msg::read(bytes),
        Format::Unknown => Err(ReadError::Unsupported(format)),

        _ => {
            let mut warnings = Vec::new();

            let label = name.unwrap_or("input").to_string();

            let part = read_part(bytes, format, label, &mut warnings)?;

            let document = Document {
                format,
                headers: None,
                parts: vec![part],
                warnings,
            };

            Ok(document)
        }
    }
}

/// Reads a single PDF, image, HTML or text input into a [`Part`].
///
/// Emails are not parts; they are flattened by the email reader.
fn read_part(
    bytes: &[u8],
    format: Format,
    label: String,
    warnings: &mut Vec<String>,
) -> Result<Part, ReadError> {
    let mut part = Part {
        index: 0,
        label,
        kind: PartKind::Other,
        mime: format.mime().to_string(),
        bytes: bytes.to_vec(),
        size: bytes.len(),
        text: None,
        pages: None,
        inline: false,
    };

    match format {
        Format::Pdf => {
            let pdf = pdf::read(bytes)?;

            if pdf.text.is_none() {
                warnings.push(format!(
                    "{} has no text layer; it needs a model that can see",
                    part.label
                ));
            }

            part.kind = PartKind::Pdf;
            part.text = pdf.text;
            part.pages = Some(pdf.pages);
        }

        Format::Html => {
            part.kind = PartKind::Html;
            part.text = Some(text::html_to_text(bytes));
        }

        Format::Text => {
            let text = String::from_utf8_lossy(bytes);

            part.kind = PartKind::Text;
            part.text = Some(text::tidy(&text));
        }

        format if format.is_image() => {
            part.kind = PartKind::Image;
        }

        _ => {}
    }

    Ok(part)
}

/// Numbers the parts in order.
fn number(parts: &mut [Part]) {
    for (index, part) in parts.iter_mut().enumerate() {
        part.index = index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_text() {
        let document = read("Kvittering\n\n\nBeløp: kr 1 234,50  \n".as_bytes(), None).unwrap();

        assert_eq!(document.format, Format::Text);
        assert_eq!(document.parts.len(), 1);
        assert_eq!(
            document.parts[0].text_or_empty(),
            "Kvittering\n\nBeløp: kr 1 234,50"
        );
    }

    #[test]
    fn reads_an_image_without_text() {
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

        let document = read(png, Some("receipt.png")).unwrap();

        let part = &document.parts[0];

        assert_eq!(part.kind, PartKind::Image);
        assert_eq!(part.label, "receipt.png");
        assert_eq!(part.mime, "image/png");
        assert!(part.text.is_none());
    }

    #[test]
    fn rejects_empty_and_unknown_input() {
        assert!(matches!(read(b"", None), Err(ReadError::Empty)));

        let junk: Vec<u8> = (0..=255).collect();

        assert!(matches!(
            read(&junk, None),
            Err(ReadError::Unsupported(Format::Unknown))
        ));
    }
}
