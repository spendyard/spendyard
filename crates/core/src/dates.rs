//! Calendar days as the store keeps them, `YYYY-MM-DD`: counting days
//! between them and moving them by months, without a clock or a time zone.

/// How many days a paid invoice and the receipt for the same payment may
/// be apart.
pub const SAME_PAYMENT_DAYS: i64 = 3;

/// Days since 1970-01-01 for a `YYYY-MM-DD` date, or the start of a longer
/// timestamp (Howard Hinnant's algorithm, the inverse of
/// [`civil_from_days`]).
pub fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.get(..10)?.split('-');

    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;

    let shifted_year = if month <= 2 { year - 1 } else { year };

    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;

    let month_index = if month > 2 { month - 3 } else { month + 9 };

    let day_of_year = (153 * month_index + 2) / 5 + day - 1;

    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    Some(era * 146_097 + day_of_era - 719_468)
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;

    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);

    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;

    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);

    let month_index = (5 * day_of_year + 2) / 153;

    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;

    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;

    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// `YYYY-MM-DD` for days since 1970-01-01.
pub fn date_of(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);

    format!("{year:04}-{month:02}-{day:02}")
}

/// How many days apart two dates are, either way round.
pub fn days_apart(a: &str, b: &str) -> Option<i64> {
    Some((day_number(a)? - day_number(b)?).abs())
}

/// `YYYY-MM-DD` moved by whole months, then by days. The day of the month
/// stays at most the 28th, so every month has it.
pub fn shift_date(date: &str, months: i32, days: i32) -> String {
    let parts: Vec<i32> = date
        .split('-')
        .filter_map(|part| part.parse().ok())
        .collect();

    let [year, month, day] = parts[..] else {
        return date.to_string();
    };

    let total_months = year * 12 + (month - 1) + months;

    let (year, month) = (total_months.div_euclid(12), total_months.rem_euclid(12) + 1);

    let shifted = format!("{year:04}-{month:02}-{:02}", day.min(28));

    if days == 0 {
        return shifted;
    }

    match day_number(&shifted) {
        Some(number) => date_of(number + i64::from(days)),
        None => shifted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_round_trip() {
        for date in ["1970-01-01", "2000-02-29", "2026-11-21", "1999-12-31"] {
            assert_eq!(date_of(day_number(date).unwrap()), date);
        }
    }

    #[test]
    fn shifts_by_months_and_days() {
        assert_eq!(shift_date("2026-01-31", 1, 0), "2026-02-28");
        assert_eq!(shift_date("2026-11-19", 12, 0), "2027-11-19");
        assert_eq!(shift_date("2026-01-15", -2, 0), "2025-11-15");
        assert_eq!(shift_date("2026-03-01", 0, -7), "2026-02-22");
    }

    #[test]
    fn counts_days_apart() {
        assert_eq!(days_apart("2026-11-19", "2026-11-21"), Some(2));
        assert_eq!(days_apart("2026-11-21", "2026-11-19"), Some(2));
        assert_eq!(days_apart("not a date", "2026-11-19"), None);
    }
}
