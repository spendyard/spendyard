//! Turning HTML and messy text into clean plain text.

/// Width HTML is wrapped at. Wide, so table rows stay on one line.
const HTML_WIDTH: usize = 200;

/// HTML as plain text, keeping table rows on one line where possible.
pub fn html_to_text(html: &[u8]) -> String {
    let rendered = html2text::config::plain_no_decorate()
        .no_table_borders()
        .no_link_wrapping()
        .allow_width_overflow()
        .string_from_read(html, HTML_WIDTH);

    let text = match rendered {
        Ok(text) => text,

        // html2text only fails on I/O errors, which can't happen on a slice.
        Err(_) => String::from_utf8_lossy(html).into_owned(),
    };

    tidy(&text)
}

/// Trims trailing spaces, drops invisible characters and collapses runs of
/// blank lines.
pub fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_lines = 0;

    for line in text.lines() {
        let visible: String = line.chars().filter(|c| !is_invisible(*c)).collect();
        let line = visible.trim_end();

        if line.trim().is_empty() {
            blank_lines += 1;

            if blank_lines > 1 {
                continue;
            }

            out.push('\n');

            continue;
        }

        blank_lines = 0;

        out.push_str(line);
        out.push('\n');
    }

    out.trim().to_string()
}

/// Zero-width characters that marketing emails pad their preview text with.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{034f}' | '\u{00ad}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_collapses_blank_lines_and_trims() {
        let messy = "\n\nTotal  \n\n\n\n$112.40\u{200b}\u{034f}\n\n";

        assert_eq!(tidy(messy), "Total\n\n$112.40");
    }

    #[test]
    fn html_table_rows_stay_together() {
        let html = b"<table><tr><td>Pro (per seat)</td><td>4</td><td>$80.00</td></tr></table>";

        let text = html_to_text(html);

        let row = text
            .lines()
            .find(|line| line.contains("Pro (per seat)"))
            .expect("the row");

        assert!(row.contains("$80.00"), "{text}");
    }
}
