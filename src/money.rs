//! Amounts as exact decimals. Money is never a float.

use std::str::FromStr;

use rust_decimal::Decimal;

/// Parses an amount written in any common way: `112.40`, `$1,234.50`,
/// `1 234,50 kr`, `1.234,50`, `(12.00)` or `-12.00`.
pub fn parse(text: &str) -> Option<Decimal> {
    let trimmed = text.trim();

    let negative = trimmed.starts_with('-')
        || trimmed.starts_with('−')
        || (trimmed.starts_with('(') && trimmed.ends_with(')'));

    let kept: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();

    if kept.is_empty() || !kept.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }

    let plain = plain_decimal(&kept);

    let value = Decimal::from_str(&plain).ok()?;

    let signed = if negative { -value } else { value };

    Some(signed)
}

/// Normalizes an amount to the form results use: `1234.50`.
pub fn normalize(text: &str) -> Option<String> {
    parse(text).map(|value| value.to_string())
}

/// Decides which of `.` and `,` is the decimal separator and drops the
/// other.
fn plain_decimal(number: &str) -> String {
    let last_dot = number.rfind('.');
    let last_comma = number.rfind(',');

    let decimal_separator = match (last_dot, last_comma) {
        (Some(dot), Some(comma)) => Some(if dot > comma { '.' } else { ',' }),

        // "1,234" is a thousand; "12,50" is twelve and a half.
        (None, Some(comma)) => {
            let digits_after = number.len() - comma - 1;

            let single = number.matches(',').count() == 1;

            (single && digits_after != 3).then_some(',')
        }

        // "1.234.567" is thousands; "12.5" and "1.234" are decimals.
        (Some(_), None) => (number.matches('.').count() == 1).then_some('.'),

        (None, None) => None,
    };

    number
        .chars()
        .filter_map(|c| match c {
            '.' | ',' if Some(c) == decimal_separator => Some('.'),
            '.' | ',' => None,
            digit => Some(digit),
        })
        .collect()
}

/// The digits of an amount, for finding it in a quote however it was
/// written: `1234.50` gives `123450`.
pub fn digits(value: &Decimal) -> String {
    let fixed = format!("{:.2}", value.abs());

    fixed.chars().filter(char::is_ascii_digit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> Option<String> {
        normalize(text)
    }

    #[test]
    fn reads_common_formats() {
        assert_eq!(n("112.40").as_deref(), Some("112.40"));
        assert_eq!(n("$1,234.50").as_deref(), Some("1234.50"));
        assert_eq!(n("1 234,50 kr").as_deref(), Some("1234.50"));
        assert_eq!(n("1.234,50").as_deref(), Some("1234.50"));
        assert_eq!(n("12,50").as_deref(), Some("12.50"));
        assert_eq!(n("1,234").as_deref(), Some("1234"));
        assert_eq!(n("(12.00)").as_deref(), Some("-12.00"));
        assert_eq!(n("-0.04").as_deref(), Some("-0.04"));
        assert_eq!(n("€0.042").as_deref(), Some("0.042"));
        assert_eq!(n("n/a"), None);
    }

    #[test]
    fn digits_ignore_separators() {
        assert_eq!(digits(&parse("1 234,5").unwrap()), "123450");
        assert_eq!(digits(&parse("-20").unwrap()), "2000");
    }
}
