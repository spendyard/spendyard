//! A paid invoice and the receipt for the same payment, when nothing on
//! them links the two: Plain's invoice PDFs carry only the invoice number
//! and its receipt emails only the receipt number. Spend counts the pair
//! once ([`crate::spend`]); matching takes both for one charge
//! ([`crate::transactions`]). Both pair by the rules here.

use std::collections::HashSet;

use crate::dates::{SAME_PAYMENT_DAYS, day_number};

/// How many days after a paid invoice its receipt may come, when no closer
/// receipt pairs with it: paid on account, or by hand some days later. Well
/// under a month, so a monthly subscription's next receipt is never taken
/// for this one's.
pub const LATER_RECEIPT_DAYS: i64 = 14;

/// One document to pair: a paid invoice or a receipt, by its position.
pub struct Paper<'a> {
    pub invoice: bool,
    pub date: &'a str,
}

/// Paid invoices and their receipts, as `(invoice, receipt)` positions in
/// `papers`. `same(a, b)` says whether two documents are for the same
/// vendor, amount and currency. Each pairs once:
///
/// 1. a receipt at most [`SAME_PAYMENT_DAYS`] either side of the invoice,
///    the first in `papers`' order;
/// 2. then, for an invoice still alone, the closest receipt still alone
///    from its day to [`LATER_RECEIPT_DAYS`] after, and before the vendor's
///    next invoice of the same amount (that receipt is the next one's).
pub fn pair_invoices(papers: &[Paper], same: impl Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    let days: Vec<Option<i64>> = papers.iter().map(|paper| day_number(paper.date)).collect();

    let invoices: Vec<usize> = (0..papers.len()).filter(|&i| papers[i].invoice).collect();
    let receipts: Vec<usize> = (0..papers.len()).filter(|&i| !papers[i].invoice).collect();

    // Each looks only at the others within its days, found by date.
    let invoices_by_day = ByDay::new(&invoices, &days);
    let receipts_by_day = ByDay::new(&receipts, &days);

    let mut pairs = Vec::new();
    let mut paired = HashSet::new();

    // 1. Close together: the first in `papers`' order.
    for &invoice in &invoices {
        let Some(day) = days[invoice] else {
            continue;
        };

        let mut close: Vec<usize> = receipts_by_day
            .within(day - SAME_PAYMENT_DAYS, day + SAME_PAYMENT_DAYS)
            .filter(|receipt| !paired.contains(receipt))
            .collect();

        close.sort_unstable();

        let partner = close.into_iter().find(|&receipt| same(invoice, receipt));

        if let Some(receipt) = partner {
            paired.insert(invoice);
            paired.insert(receipt);
            pairs.push((invoice, receipt));
        }
    }

    // 2. The receipt some days after.
    for &invoice in &invoices {
        let Some(day) = days[invoice] else {
            continue;
        };

        if paired.contains(&invoice) {
            continue;
        }

        let last = day + LATER_RECEIPT_DAYS;

        // The vendor's next invoice of the same amount ends the wait. One
        // after the last day a receipt could be on ends nothing.
        let next_invoice = invoices_by_day
            .within(day + 1, last)
            .find(|&other| same(invoice, other))
            .and_then(|other| days[other]);

        // The closest, then the first in `papers`' order: how they're sorted.
        let partner = receipts_by_day
            .within(day, last)
            .filter(|receipt| !paired.contains(receipt))
            .filter(|&receipt| {
                next_invoice.is_none_or(|next| days[receipt].is_some_and(|d| d < next))
            })
            .find(|&receipt| same(invoice, receipt));

        if let Some(receipt) = partner {
            paired.insert(invoice);
            paired.insert(receipt);
            pairs.push((invoice, receipt));
        }
    }

    pairs
}

/// Papers (or anything else by its position) in date order, then in
/// their own order: those of a few days found without looking at the rest.
pub(crate) struct ByDay {
    sorted: Vec<(i64, usize)>,
}

impl ByDay {
    /// Those at `positions` that have a date, `days` by position.
    pub(crate) fn new(positions: &[usize], days: &[Option<i64>]) -> ByDay {
        let mut sorted: Vec<(i64, usize)> = positions
            .iter()
            .filter_map(|&at| Some((days[at]?, at)))
            .collect();

        sorted.sort_unstable();

        ByDay { sorted }
    }

    /// The positions of those from day `first` to day `last`, both
    /// included, in date order.
    pub(crate) fn within(&self, first: i64, last: i64) -> impl Iterator<Item = usize> + '_ {
        let start = self.sorted.partition_point(|&(day, _)| day < first);
        let end = self.sorted.partition_point(|&(day, _)| day <= last);

        self.sorted[start..end.max(start)].iter().map(|&(_, at)| at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn papers(list: &[(bool, &'static str)]) -> Vec<Paper<'static>> {
        list.iter()
            .map(|&(invoice, date)| Paper { invoice, date })
            .collect()
    }

    #[test]
    fn pairs_close_together_first() {
        let papers = papers(&[(true, "2026-11-01"), (false, "2026-11-02")]);

        assert_eq!(pair_invoices(&papers, |_, _| true), [(0, 1)]);
    }

    #[test]
    fn pairs_a_receipt_some_days_after() {
        let papers = papers(&[(true, "2026-11-01"), (false, "2026-11-10")]);

        assert_eq!(pair_invoices(&papers, |_, _| true), [(0, 1)]);
    }

    #[test]
    fn never_a_receipt_before_the_invoice_or_weeks_after() {
        let before = papers(&[(true, "2026-11-10"), (false, "2026-11-01")]);
        let weeks = papers(&[(true, "2026-11-01"), (false, "2026-11-20")]);

        assert!(pair_invoices(&before, |_, _| true).is_empty());
        assert!(pair_invoices(&weeks, |_, _| true).is_empty());
    }

    #[test]
    fn a_receipt_after_the_next_invoice_is_the_next_ones() {
        // Billed weekly; this week's receipt is missing.
        let papers = papers(&[
            (true, "2026-11-01"),
            (true, "2026-11-08"),
            (false, "2026-11-12"),
        ]);

        // The receipt is the second invoice's, not the first's.
        assert_eq!(pair_invoices(&papers, |_, _| true), [(1, 2)]);
    }

    #[test]
    fn close_pairs_are_made_before_later_ones() {
        // Monthly, an invoice and a receipt each month, October's receipt missing.
        let papers = papers(&[
            (true, "2026-10-01"),
            (true, "2026-11-01"),
            (false, "2026-11-01"),
        ]);

        assert_eq!(pair_invoices(&papers, |_, _| true), [(1, 2)]);
    }

    #[test]
    fn only_the_same_vendor_and_amount() {
        let papers = papers(&[(true, "2026-11-01"), (false, "2026-11-01")]);

        assert!(pair_invoices(&papers, |_, _| false).is_empty());
    }

    /// [`pair_invoices`] as it was before: every invoice against every
    /// other paper.
    fn pair_all_pairs(
        papers: &[Paper],
        same: impl Fn(usize, usize) -> bool,
    ) -> Vec<(usize, usize)> {
        let days: Vec<Option<i64>> = papers.iter().map(|paper| day_number(paper.date)).collect();

        let invoices: Vec<usize> = (0..papers.len()).filter(|&i| papers[i].invoice).collect();
        let receipts: Vec<usize> = (0..papers.len()).filter(|&i| !papers[i].invoice).collect();

        let mut pairs = Vec::new();
        let mut paired = HashSet::new();

        for &invoice in &invoices {
            let Some(day) = days[invoice] else {
                continue;
            };

            let partner = receipts.iter().copied().find(|&receipt| {
                !paired.contains(&receipt)
                    && days[receipt].is_some_and(|other| (other - day).abs() <= SAME_PAYMENT_DAYS)
                    && same(invoice, receipt)
            });

            if let Some(receipt) = partner {
                paired.insert(invoice);
                paired.insert(receipt);
                pairs.push((invoice, receipt));
            }
        }

        for &invoice in &invoices {
            let Some(day) = days[invoice] else {
                continue;
            };

            if paired.contains(&invoice) {
                continue;
            }

            let next_invoice = invoices
                .iter()
                .filter(|&&other| other != invoice && same(invoice, other))
                .filter_map(|&other| days[other])
                .filter(|&other| other > day)
                .min();

            let partner = receipts
                .iter()
                .copied()
                .filter(|&receipt| !paired.contains(&receipt) && same(invoice, receipt))
                .filter_map(|receipt| Some((receipt, days[receipt]? - day)))
                .filter(|&(receipt, after)| {
                    (0..=LATER_RECEIPT_DAYS).contains(&after)
                        && next_invoice.is_none_or(|next| days[receipt].is_some_and(|d| d < next))
                })
                .min_by_key(|&(_, after)| after)
                .map(|(receipt, _)| receipt);

            if let Some(receipt) = partner {
                paired.insert(invoice);
                paired.insert(receipt);
                pairs.push((invoice, receipt));
            }
        }

        pairs
    }

    /// `count` papers over `days` days, a few vendors and amounts (`kinds`
    /// of them): which are the same, and their dates, some missing. The
    /// same every run.
    fn random_papers(
        seed: u64,
        count: usize,
        days: i64,
        kinds: u64,
    ) -> (Vec<u64>, Vec<String>, Vec<bool>) {
        let mut state = seed;

        let mut roll = |sides: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);

            (state >> 33) % sides
        };

        let mut kinds_of = Vec::new();
        let mut dates = Vec::new();
        let mut invoices = Vec::new();

        for _ in 0..count {
            kinds_of.push(roll(kinds));
            invoices.push(roll(2) == 0);

            let day = 20_000 + roll(days as u64) as i64;
            let date = if roll(20) == 0 {
                String::new()
            } else {
                crate::dates::date_of(day)
            };

            dates.push(date);
        }

        (kinds_of, dates, invoices)
    }

    #[test]
    fn pairs_by_date_as_pairing_every_two_did() {
        for seed in 0..200 {
            let (kinds, dates, invoices) = random_papers(seed, 80, 90, 4);

            let papers: Vec<Paper> = dates
                .iter()
                .zip(&invoices)
                .map(|(date, &invoice)| Paper { invoice, date })
                .collect();

            let same = |a: usize, b: usize| kinds[a] == kinds[b];

            let paired = pair_invoices(&papers, same);

            assert!(!paired.is_empty(), "seed {seed}: some pair");
            assert_eq!(paired, pair_all_pairs(&papers, same), "seed {seed}");
        }
    }

    /// How many times pairing asks whether two papers are the same, for a
    /// store of `count` papers over as many days.
    fn questions_for(count: usize) -> usize {
        let (kinds, dates, invoices) = random_papers(3, count, count as i64, 50);

        let papers: Vec<Paper> = dates
            .iter()
            .zip(&invoices)
            .map(|(date, &invoice)| Paper { invoice, date })
            .collect();

        let asked = std::cell::Cell::new(0);

        pair_invoices(&papers, |a, b| {
            asked.set(asked.get() + 1);

            kinds[a] == kinds[b]
        });

        asked.get()
    }

    #[test]
    fn four_times_the_papers_asks_about_four_times_as_much() {
        let once = questions_for(2_000);
        let four_times = questions_for(8_000);

        // Every two would ask about sixteen times as much.
        assert!(four_times < once * 6, "{once} questions, then {four_times}");
    }
}
