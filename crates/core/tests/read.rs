//! Reading the fixtures in `tests/fixtures`.

use spendyard_core::{Format, PartKind, read};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));

    std::fs::read(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

#[test]
fn pdf_with_a_text_layer() {
    let document = read(&fixture("invoice.pdf"), Some("invoice.pdf")).unwrap();

    assert_eq!(document.format, Format::Pdf);
    assert!(document.warnings.is_empty());

    let part = &document.parts[0];

    assert_eq!(part.kind, PartKind::Pdf);
    assert_eq!(part.pages, Some(1));

    let text = part.text_or_empty();

    for expected in [
        "A1B2C3D4-0007",
        "$112.40",
        "Visa •••• 4242",
        "Beløp: kr 1 234,50",
    ] {
        assert!(
            text.contains(expected),
            "{expected:?} missing from:\n{text}"
        );
    }
}

#[test]
fn scanned_pdf_has_no_text_and_says_so() {
    let document = read(&fixture("scanned.pdf"), Some("scanned.pdf")).unwrap();

    let part = &document.parts[0];

    assert_eq!(part.kind, PartKind::Pdf);
    assert!(part.text.is_none());
    assert_eq!(document.warnings.len(), 1);
}

#[test]
fn image() {
    let document = read(&fixture("receipt.jpg"), Some("receipt.jpg")).unwrap();

    assert_eq!(document.format, Format::Jpeg);
    assert_eq!(document.parts[0].kind, PartKind::Image);
    assert_eq!(document.parts[0].mime, "image/jpeg");
}

#[test]
fn html_table_rows_keep_their_amounts() {
    let document = read(&fixture("invoice.html"), None).unwrap();

    let text = document.parts[0].text_or_empty();

    let row = text
        .lines()
        .find(|line| line.contains("Pro (per seat)"))
        .unwrap_or_else(|| panic!("no seat line in:\n{text}"));

    assert!(row.contains("$80.00"), "{row}");
}

#[test]
fn receipt_email() {
    let document = read(&fixture("receipt.eml"), None).unwrap();

    assert_eq!(document.format, Format::Email);

    let headers = document.headers.as_ref().unwrap();

    assert_eq!(
        headers.from.as_deref(),
        Some("Acme Cloud <invoice+statements@acme.example>")
    );
    assert_eq!(headers.date.as_deref(), Some("2026-11-19T09:30:00Z"));
    assert_eq!(
        headers.message_id.as_deref(),
        Some("receipt-2291-0007@acme.example")
    );

    let labels: Vec<&str> = document
        .parts
        .iter()
        .map(|part| part.label.as_str())
        .collect();

    assert_eq!(
        labels,
        [
            "body",
            "attachment:attachment-1",
            "attachment:Invoice-2291-0007.pdf"
        ]
    );

    let body = &document.parts[0];

    assert_eq!(body.kind, PartKind::Html, "HTML wins over the text body");
    assert!(body.text_or_empty().contains("Visa •••• 4242"));

    let logo = &document.parts[1];

    assert!(logo.inline);

    let invoice = &document.parts[2];

    assert_eq!(invoice.kind, PartKind::Pdf);
    assert!(
        invoice
            .text_or_empty()
            .contains("Premium requests over quota")
    );
}

#[test]
fn forwarded_email_is_flattened_in_order() {
    let document = read(&fixture("forwarded.eml"), None).unwrap();

    let headers = document.headers.as_ref().unwrap();

    assert_eq!(
        headers.subject.as_deref(),
        Some("Fwd: Your receipt from Acme Cloud, Inc. #2291-0007")
    );

    let labels: Vec<&str> = document
        .parts
        .iter()
        .map(|part| part.label.as_str())
        .collect();

    assert_eq!(
        labels,
        [
            "body",
            "forwarded:1/body",
            "forwarded:1/attachment:attachment-1",
            "forwarded:1/attachment:Invoice-2291-0007.pdf",
        ]
    );

    let indexes: Vec<usize> = document.parts.iter().map(|part| part.index).collect();

    assert_eq!(indexes, [0, 1, 2, 3]);
}

#[test]
fn file_name_never_decides_the_format() {
    let document = read(&fixture("invoice.pdf"), Some("invoice.txt")).unwrap();

    assert_eq!(document.format, Format::Pdf);
}
