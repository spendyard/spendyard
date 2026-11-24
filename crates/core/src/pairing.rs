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

    let mut pairs = Vec::new();
    let mut paired = HashSet::new();

    // 1. Close together.
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

    // 2. The receipt some days after.
    for &invoice in &invoices {
        let Some(day) = days[invoice] else {
            continue;
        };

        if paired.contains(&invoice) {
            continue;
        }

        // The vendor's next invoice of the same amount ends the wait.
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
}
