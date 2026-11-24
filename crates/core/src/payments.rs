//! Payments: what Spendyard counts, each once, with the evidence for it.
//! A payment collects its documents (an invoice, its receipt, the same
//! receipt forwarded twice) and its card lines into one; any of them may be
//! missing, and one is enough. The design is in the platform repository's
//! docs/cloud.md §3.1 (issue #52 there).
//!
//! Grouping goes by the rules spend and matching already have: the same
//! number ([`crate::spend`]), a paid invoice and its receipt
//! ([`crate::pairing`]), a card line and its receipt
//! ([`crate::transactions`]). Each payment keeps an ID of its own through
//! regrouping ([`keep_ids`]): one made from its evidence would change when
//! an invoice arrives after its card line.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::pairing::{Paper, pair_invoices};
use crate::spend::{
    Row, amount, counts_as_paid, is_money_back, number_key, same_vendor_and_amount,
};
use crate::transactions::Matching;

/// One piece of evidence: a document, by its ID, or a card line, by its
/// statement and line ([`crate::transactions::Charge::key`]).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Piece {
    Document(i64),
    Charge(String),
}

/// The evidence for one payment: its documents and its card lines, each
/// in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub documents: Vec<i64>,
    pub charges: Vec<String>,
}

impl Evidence {
    /// Every piece, documents first.
    pub fn pieces(&self) -> impl Iterator<Item = Piece> + '_ {
        let documents = self.documents.iter().map(|&id| Piece::Document(id));
        let charges = self.charges.iter().map(|key| Piece::Charge(key.clone()));

        documents.chain(charges)
    }

    fn from_pieces(pieces: Vec<Piece>) -> Evidence {
        let mut evidence = Evidence::default();

        for piece in pieces {
            match piece {
                Piece::Document(id) => evidence.documents.push(id),
                Piece::Charge(key) => evidence.charges.push(key),
            }
        }

        evidence.documents.sort();
        evidence.charges.sort();

        evidence
    }
}

/// One payment: its own ID, and its evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub id: String,
    pub evidence: Evidence,
}

/// A payment that became part of another: links to `from` go to `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Moved {
    pub from: String,
    pub to: String,
}

/// Payments grouped again, with what happened to the ones stored before.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Regrouped {
    pub payments: Vec<Payment>,
    /// Stored payments merged into another.
    pub moved: Vec<Moved>,
    /// Stored payments whose evidence is all gone.
    pub gone: Vec<String>,
}

/// The evidence of each payment, from the store's rows (one per document,
/// as [`crate::spend::ROWS_SQL`] gives them, and a statement's payment with
/// no receipt standing in for one, as [`crate::transactions::payment_rows`]
/// makes it) and the card lines matched to them. Rows count when they are
/// money paid or back ([`counts_as_paid`]): receipts and paid invoices,
/// refunds and credit notes paid back, not personal or ignored. Money back
/// is grouped apart from money paid: a credit note with the refund on a
/// statement for it, never with an invoice of the same amount. A card line counts when it is a purchase or a
/// refund, with its receipt or with none; one matched to a document that
/// doesn't count (personal) doesn't either.
pub fn group(rows: &[Row], matching: &Matching) -> Vec<Evidence> {
    let paid: Vec<&Row> = rows.iter().filter(|row| counts_as_paid(row)).collect();

    let mut sets = Sets::default();

    for &row in &paid {
        sets.add(piece_of(row));
    }

    let alone = join_by_number(&paid, &mut sets);

    let (back, forward): (Vec<&Row>, Vec<&Row>) =
        alone.into_iter().partition(|row| is_money_back(row));

    join_invoices_and_receipts(&forward, &mut sets);
    join_credit_notes_and_refunds(&back, &mut sets);

    let counted: BTreeSet<i64> = paid
        .iter()
        .filter(|row| row.payment.is_none())
        .map(|row| row.id)
        .collect();

    for found in &matching.transactions {
        if !found.charge.needs_receipt() {
            continue;
        }

        let piece = Piece::Charge(found.id.clone());

        match found.receipt_id {
            Some(receipt) if counted.contains(&receipt) => {
                sets.add(piece.clone());
                sets.join(&piece, &Piece::Document(receipt));
            }
            Some(_) => continue,
            None => {
                sets.add(piece);
            }
        }
    }

    sets.groups()
}

/// The piece a row is: a statement's payment standing in for a receipt is
/// its card line, any other row its document.
pub(crate) fn piece_of(row: &Row) -> Piece {
    match &row.payment {
        Some(charge) => Piece::Charge(charge.clone()),
        None => Piece::Document(row.id),
    }
}

/// Joins rows with the same number (the same receipt saved twice), and
/// gives the first of each with its number, and those without one.
fn join_by_number<'a>(rows: &[&'a Row], sets: &mut Sets) -> Vec<&'a Row> {
    let mut first_by_number: BTreeMap<String, Piece> = BTreeMap::new();
    let mut alone = Vec::new();

    for &row in rows {
        let Some(key) = number_key(row) else {
            alone.push(row);

            continue;
        };

        match first_by_number.get(&key) {
            Some(first) => sets.join(first, &piece_of(row)),
            None => {
                first_by_number.insert(key, piece_of(row));
                alone.push(row);
            }
        }
    }

    alone
}

/// Joins each paid invoice with its receipt, by [`pair_invoices`].
fn join_invoices_and_receipts(rows: &[&Row], sets: &mut Sets) {
    let papers: Vec<Paper> = rows
        .iter()
        .map(|row| Paper {
            invoice: row.kind == "invoice",
            date: &row.date,
        })
        .collect();

    let same = |a: usize, b: usize| same_vendor_and_amount(rows[a], rows[b]);

    for (invoice, receipt) in pair_invoices(&papers, same) {
        let invoice = piece_of(rows[invoice]);
        let receipt = piece_of(rows[receipt]);

        sets.join(&invoice, &receipt);
    }
}

/// Joins each credit note paid back with the refund on a statement for the
/// same money: the same vendor, currency and amount, whichever sign each
/// was written with, a few days apart ([`pair_invoices`], the credit note
/// in the invoice's place).
fn join_credit_notes_and_refunds(rows: &[&Row], sets: &mut Sets) {
    let papers: Vec<Paper> = rows
        .iter()
        .map(|row| Paper {
            invoice: row.kind == "credit_note",
            date: &row.date,
        })
        .collect();

    let size = |row: &Row| {
        row.total
            .as_deref()
            .and_then(amount)
            .map(|total| total.abs())
    };

    let same = |a: usize, b: usize| {
        let (a, b) = (rows[a], rows[b]);

        a.vendor_key == b.vendor_key && a.currency == b.currency && size(a) == size(b)
    };

    for (credit_note, refund) in pair_invoices(&papers, same) {
        sets.join(&piece_of(rows[credit_note]), &piece_of(rows[refund]));
    }
}

/// Gives each group the ID of the stored payment it shares the most
/// evidence with, and a new one (`new_id`) when it shares none. The most
/// shared is decided first, so when a payment splits the larger part keeps
/// its ID; when two merge, one keeps its ID and the other is [`Moved`] to
/// it. Ties go to the stored payment listed first.
pub fn keep_ids(
    groups: Vec<Evidence>,
    stored: &[Payment],
    mut new_id: impl FnMut() -> String,
) -> Regrouped {
    let shared = shared_evidence(&groups, stored);

    let mut ids: Vec<Option<String>> = vec![None; groups.len()];
    let mut kept: BTreeSet<usize> = BTreeSet::new();

    for &(_, group, old) in &shared {
        if ids[group].is_some() || kept.contains(&old) {
            continue;
        }

        ids[group] = Some(stored[old].id.clone());
        kept.insert(old);
    }

    let ids: Vec<String> = ids
        .into_iter()
        .map(|id| id.unwrap_or_else(&mut new_id))
        .collect();

    let mut regrouped = Regrouped::default();

    for (old, payment) in stored.iter().enumerate() {
        if kept.contains(&old) {
            continue;
        }

        // The group it shares the most with, as `shared` is sorted.
        let into = shared
            .iter()
            .find(|&&(_, _, other)| other == old)
            .map(|&(_, group, _)| ids[group].clone());

        match into {
            Some(to) => regrouped.moved.push(Moved {
                from: payment.id.clone(),
                to,
            }),
            None => regrouped.gone.push(payment.id.clone()),
        }
    }

    regrouped.payments = groups
        .into_iter()
        .zip(ids)
        .map(|(evidence, id)| Payment { id, evidence })
        .collect();

    regrouped
}

/// Every group and stored payment with evidence in common, as
/// `(pieces shared, group, stored)`: the most shared first, then the stored
/// payment listed first, then the group.
fn shared_evidence(groups: &[Evidence], stored: &[Payment]) -> Vec<(usize, usize, usize)> {
    let mut owner: BTreeMap<Piece, usize> = BTreeMap::new();

    for (old, payment) in stored.iter().enumerate() {
        for piece in payment.evidence.pieces() {
            owner.insert(piece, old);
        }
    }

    let mut counts: BTreeMap<(usize, usize), usize> = BTreeMap::new();

    for (group, evidence) in groups.iter().enumerate() {
        for piece in evidence.pieces() {
            if let Some(&old) = owner.get(&piece) {
                *counts.entry((group, old)).or_default() += 1;
            }
        }
    }

    let mut shared: Vec<(usize, usize, usize)> = counts
        .into_iter()
        .map(|((group, old), count)| (count, group, old))
        .collect();

    shared.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)).then(a.1.cmp(&b.1)));

    shared
}

/// Pieces joined into groups: a union-find over the pieces added.
#[derive(Default)]
struct Sets {
    index: BTreeMap<Piece, usize>,
    pieces: Vec<Piece>,
    parent: Vec<usize>,
}

impl Sets {
    fn add(&mut self, piece: Piece) -> usize {
        if let Some(&at) = self.index.get(&piece) {
            return at;
        }

        let at = self.pieces.len();

        self.index.insert(piece.clone(), at);
        self.pieces.push(piece);
        self.parent.push(at);

        at
    }

    fn root(&mut self, mut at: usize) -> usize {
        while self.parent[at] != at {
            self.parent[at] = self.parent[self.parent[at]];
            at = self.parent[at];
        }

        at
    }

    /// Joins the groups of two pieces already added.
    fn join(&mut self, a: &Piece, b: &Piece) {
        let (Some(&a), Some(&b)) = (self.index.get(a), self.index.get(b)) else {
            return;
        };

        let (a, b) = (self.root(a), self.root(b));

        if a != b {
            self.parent[b] = a;
        }
    }

    /// Each group's evidence, in the order of each group's first piece.
    fn groups(mut self) -> Vec<Evidence> {
        let mut by_root: BTreeMap<usize, Vec<Piece>> = BTreeMap::new();
        let mut order: Vec<usize> = Vec::new();

        for at in 0..self.pieces.len() {
            let root = self.root(at);

            if !by_root.contains_key(&root) {
                order.push(root);
            }

            by_root
                .entry(root)
                .or_default()
                .push(self.pieces[at].clone());
        }

        order
            .into_iter()
            .filter_map(|root| by_root.remove(&root))
            .map(Evidence::from_pieces)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;
    use crate::spend::{StoredRow, spend};
    use crate::transactions::{Charge, Transaction};

    fn row(id: i64, date: &str, vendor: &str, total: &str) -> StoredRow {
        StoredRow {
            id,
            date: date.into(),
            kind: "receipt".into(),
            status: "paid".into(),
            is_receipt: 1,
            vendor_name: vendor.into(),
            total: Some(total.into()),
            currency: Some("USD".into()),
            ..StoredRow::default()
        }
    }

    fn invoice(id: i64, date: &str, vendor: &str, total: &str) -> StoredRow {
        StoredRow {
            kind: "invoice".into(),
            ..row(id, date, vendor, total)
        }
    }

    fn rows(stored: Vec<StoredRow>) -> Vec<Row> {
        stored
            .into_iter()
            .map(|row| Row::from_stored(row, &catalog()))
            .collect()
    }

    /// A card line on statement 90, matched to `receipt` or to none.
    fn line(position: i64, kind: &str, receipt: Option<i64>) -> Transaction {
        let charge = Charge {
            document_id: 90,
            position,
            kind: kind.into(),
            ..Charge::default()
        };

        Transaction {
            id: charge.key(),
            charge,
            state: if receipt.is_some() {
                "matched"
            } else {
                "missing"
            }
            .into(),
            receipt_id: receipt,
            by_hand: false,
        }
    }

    fn matching(lines: Vec<Transaction>) -> Matching {
        Matching {
            transactions: lines,
            ..Matching::default()
        }
    }

    fn evidence(documents: &[i64], charges: &[&str]) -> Evidence {
        Evidence {
            documents: documents.to_vec(),
            charges: charges.iter().map(|key| key.to_string()).collect(),
        }
    }

    fn payment(id: &str, documents: &[i64], charges: &[&str]) -> Payment {
        Payment {
            id: id.into(),
            evidence: evidence(documents, charges),
        }
    }

    /// New IDs `new 1`, `new 2`, … in the order asked for.
    fn counter() -> impl FnMut() -> String {
        let mut next = 0;

        move || {
            next += 1;

            format!("new {next}")
        }
    }

    #[test]
    fn an_invoice_its_receipt_and_the_card_line_are_one_payment() {
        let rows = rows(vec![
            row(2, "2026-11-03", "Vercel", "20.00"),
            invoice(1, "2026-11-01", "Vercel", "20.00"),
        ]);

        let matching = matching(vec![line(4, "purchase", Some(2))]);

        assert_eq!(group(&rows, &matching), [evidence(&[1, 2], &["90:4"])]);
    }

    #[test]
    fn the_same_receipt_saved_twice_is_one_payment() {
        let mut email = row(2, "2026-11-19", "Vercel", "20.00");

        email.number = Some("2291-0007".into());

        let mut pdf = row(1, "2026-11-19", "Vercel", "20.00");

        pdf.number = Some("2291 0007".into());

        let rows = rows(vec![email, pdf]);

        assert_eq!(group(&rows, &Matching::default()), [evidence(&[1, 2], &[])]);
    }

    #[test]
    fn a_card_line_with_no_receipt_is_a_payment_and_a_fee_is_none() {
        let matching = matching(vec![line(1, "purchase", None), line(2, "fee", None)]);

        assert_eq!(group(&[], &matching), [evidence(&[], &["90:1"])]);
    }

    #[test]
    fn a_card_line_matched_to_a_personal_receipt_is_personal_too() {
        let mut personal = row(1, "2026-11-19", "Netflix", "15.49");

        personal.personal = 1;

        let rows = rows(vec![personal]);
        let matching = matching(vec![line(1, "purchase", Some(1))]);

        assert!(group(&rows, &matching).is_empty());
    }

    #[test]
    fn groups_documents_as_spend_counts_them() {
        let rows = rows(vec![
            row(4, "2026-11-10", "Fjordbyrå AS", "300.00"),
            row(3, "2026-11-01", "Vercel", "20.00"),
            invoice(2, "2026-11-01", "Vercel", "20.00"),
            invoice(1, "2026-10-01", "Vercel", "20.00"),
            invoice(0, "2026-11-01", "Fjordbyrå AS", "300.00"),
        ]);

        let groups = group(&rows, &Matching::default());

        assert_eq!(groups.len(), spend(&rows).len());
        assert_eq!(
            groups,
            [
                evidence(&[0, 4], &[]),
                evidence(&[2, 3], &[]),
                evidence(&[1], &[])
            ]
        );
    }

    #[test]
    fn a_payment_keeps_its_id_when_its_invoice_comes_after_its_card_line() {
        let stored = [payment("p1", &[], &["90:4"])];
        let groups = vec![evidence(&[7], &["90:4"])];

        let regrouped = keep_ids(groups, &stored, counter());

        assert_eq!(regrouped.payments, [payment("p1", &[7], &["90:4"])]);
        assert!(regrouped.moved.is_empty() && regrouped.gone.is_empty());
    }

    #[test]
    fn when_two_payments_merge_one_keeps_its_id_and_the_other_moves_to_it() {
        let stored = [payment("p1", &[], &["90:4"]), payment("p2", &[8], &[])];
        let groups = vec![evidence(&[7, 8], &["90:4"])];

        let regrouped = keep_ids(groups, &stored, counter());

        assert_eq!(regrouped.payments, [payment("p1", &[7, 8], &["90:4"])]);
        assert_eq!(
            regrouped.moved,
            [Moved {
                from: "p2".into(),
                to: "p1".into()
            }]
        );
    }

    #[test]
    fn when_a_payment_splits_the_larger_part_keeps_its_id() {
        let stored = [payment("p1", &[1, 2], &["90:4"])];
        let groups = vec![evidence(&[2], &[]), evidence(&[1], &["90:4"])];

        let regrouped = keep_ids(groups, &stored, counter());

        assert_eq!(
            regrouped.payments,
            [payment("new 1", &[2], &[]), payment("p1", &[1], &["90:4"])]
        );
    }

    #[test]
    fn a_payment_whose_evidence_is_gone_is_gone() {
        let stored = [payment("p1", &[1], &[])];

        let regrouped = keep_ids(vec![evidence(&[2], &[])], &stored, counter());

        assert_eq!(regrouped.payments, [payment("new 1", &[2], &[])]);
        assert_eq!(regrouped.gone, ["p1"]);
    }
}
