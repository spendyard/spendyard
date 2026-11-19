//! The one shape every input becomes before any model sees it.

use serde::Serialize;

/// A document read from any input: an email with its attachments, a PDF, an
/// image or text.
///
/// Parts come in reading order: the email body first, then attachments,
/// with forwarded emails flattened in place.
#[derive(Debug, Clone, Serialize)]
pub struct Document {
    /// The format the input was detected as.
    pub format: Format,

    /// Set when the input is an email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<EmailHeaders>,

    pub parts: Vec<Part>,

    /// Things that went wrong but didn't stop reading, such as an
    /// attachment that couldn't be decoded.
    pub warnings: Vec<String>,
}

/// The headers of an email that matter for receipts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EmailHeaders {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,

    /// RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// One readable piece of a document.
#[derive(Debug, Clone, Serialize)]
pub struct Part {
    /// Position in [`Document::parts`].
    pub index: usize,

    /// Where the part came from: `body`, `attachment:Invoice.pdf`, or
    /// `forwarded:1/attachment:Invoice.pdf` inside a forwarded email.
    pub label: String,

    pub kind: PartKind,

    pub mime: String,

    /// The part's bytes as they came in: the HTML, the PDF, the image.
    #[serde(skip)]
    pub bytes: Vec<u8>,

    /// Size of [`Part::bytes`].
    pub size: usize,

    /// The best plain-text view: the text itself, HTML as text, or the PDF's
    /// text layer. `None` for images and for PDFs without a text layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Number of pages, for PDFs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,

    /// An image shown inside an email's HTML, usually a logo.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inline: bool,
}

/// What kind of content a part holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PartKind {
    Text,
    Html,
    Pdf,
    Image,
    Other,
}

/// The format of an input, detected from its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// RFC 822 / `.eml`.
    Email,

    /// Outlook `.msg`.
    OutlookMsg,

    Pdf,
    Png,
    Jpeg,
    Gif,
    Webp,
    Heic,
    Html,
    Text,

    /// Anything else.
    Unknown,
}

impl Format {
    /// The MIME type for the format.
    pub fn mime(self) -> &'static str {
        match self {
            Format::Email => "message/rfc822",
            Format::OutlookMsg => "application/vnd.ms-outlook",
            Format::Pdf => "application/pdf",
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Gif => "image/gif",
            Format::Webp => "image/webp",
            Format::Heic => "image/heic",
            Format::Html => "text/html",
            Format::Text => "text/plain",
            Format::Unknown => "application/octet-stream",
        }
    }

    /// Whether the format is an image.
    pub fn is_image(self) -> bool {
        matches!(
            self,
            Format::Png | Format::Jpeg | Format::Gif | Format::Webp | Format::Heic
        )
    }
}

impl Part {
    /// The part's text, or an empty string.
    pub fn text_or_empty(&self) -> &str {
        self.text.as_deref().unwrap_or_default()
    }
}
