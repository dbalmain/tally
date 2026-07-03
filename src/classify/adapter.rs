use crate::search::ParsedQuery;
use crate::{CategorySource, Result, TransactionStore, TransferSource};

use super::{
    ClassifyReport, Example, Input, PredictionSource, TransferHistory, TransferInput,
    detect_transfers, normalise, predict, train,
};

/// Detect transfers, apply saved filters, then suggest categories for the
/// remaining uncategorised transactions.
pub fn classify(store: &mut TransactionStore) -> Result<ClassifyReport> {
    let mut report = ClassifyReport::default();

    let transfer_history: Vec<_> = store
        .get_confirmed_transfer_examples()?
        .into_iter()
        .map(|example| TransferHistory {
            from_account_id: example.from_account_id,
            to_account_id: example.to_account_id,
            from_norm: normalise(&example.from_description),
            to_norm: normalise(&example.to_description),
        })
        .collect();
    // Transfers beat filters: detection must run before apply_filters(), and
    // its candidate pool is every leg without a user-confirmed category — a
    // filter or prior AI run must not permanently mask one leg of a pair
    // (create_transfer clears the unconfirmed enrichment it claims).
    let transfer_candidates: Vec<_> = store
        .get_unconfirmed_transactions(&ParsedQuery::empty(), None)?
        .into_iter()
        .map(|transaction| TransferInput {
            id: transaction.id,
            account_id: transaction.account_id,
            date: transaction.date,
            amount_cents: transaction.amount_cents,
            norm: normalise(&transaction.description),
        })
        .collect();
    let detected = detect_transfers(&transfer_candidates, &transfer_history);
    for transfer in detected {
        store.create_transfer(
            transfer.from_id,
            transfer.to_id,
            TransferSource::Auto,
            false,
            Some(transfer.confidence),
        )?;
        report.transfers += 1;
    }

    report.filtered = store.apply_filters()?;

    let examples: Vec<_> = store
        .get_confirmed_examples()?
        .into_iter()
        .map(|example| Example {
            norm: normalise(&example.description),
            amount_cents: example.amount_cents,
            date: example.date,
            category_id: example.category_id,
        })
        .collect();
    let classifier = train(&examples);
    // Uncategorised only: a suggestion never replaces an existing enrichment.
    let transactions = store.get_uncategorised_transactions(&ParsedQuery::empty(), None)?;

    for transaction in transactions {
        let input = Input {
            norm: normalise(&transaction.description),
            amount_cents: transaction.amount_cents,
            date: transaction.date,
        };
        let Some(prediction) = predict(&classifier, &input) else {
            report.unclassified += 1;
            continue;
        };

        store.set_category(
            transaction.id,
            prediction.category_id,
            CategorySource::Ai,
            false,
            Some(prediction.confidence),
        )?;
        match prediction.source {
            PredictionSource::Exact => report.exact += 1,
            PredictionSource::Recurring => report.recurring += 1,
            PredictionSource::Model => report.model += 1,
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use crate::store::TransactionStore;
    use crate::store::test_support::{insert_tx_desc, store_with_two_accounts};
    use crate::{CategorySource, FilterOverride, TransferSource};

    use super::classify;

    /// Add a category-bearing filter with the given override mode.
    fn add_filter(store: &mut TransactionStore, query: &str, category: &str, mode: FilterOverride) {
        let cat = store.get_or_create_category(category).unwrap();
        let id = store.create_filter("test", query).unwrap();
        store.set_filter_category(id, Some(cat)).unwrap();
        store.set_filter_override(id, mode).unwrap();
    }

    fn category_path(store: &TransactionStore, tx_id: i64) -> Option<String> {
        store
            .get_transaction_category(tx_id)
            .unwrap()
            .map(|category| category.path)
    }

    #[test]
    fn transfer_detection_beats_matching_filter_even_with_override_all() {
        let (_t, mut store, a1, a2) = store_with_two_accounts();
        let out = insert_tx_desc(&store, a1, "2024-06-01", "Transfer to savings", -10000);
        let into = insert_tx_desc(&store, a2, "2024-06-01", "Transfer from spending", 10000);
        add_filter(
            &mut store,
            "savings",
            "Should/NotApply",
            FilterOverride::All,
        );

        let report = classify(&mut store).unwrap();

        assert_eq!(report.transfers, 1);
        assert_eq!(report.filtered, 0);
        let transfer = store.get_transfer_for_transaction(out).unwrap().unwrap();
        assert_eq!(transfer.from_transaction_id, out);
        assert_eq!(transfer.to_transaction_id, into);
        assert_eq!(transfer.source, TransferSource::Auto);
        assert!(!transfer.confirmed);
        assert_eq!(category_path(&store, out), None);
        assert_eq!(category_path(&store, into), None);
    }

    #[test]
    fn transfer_detection_claims_leg_with_unconfirmed_enrichment() {
        let (_t, mut store, a1, a2) = store_with_two_accounts();
        let out = insert_tx_desc(&store, a1, "2024-06-01", "Transfer to savings", -10000);
        let into = insert_tx_desc(&store, a2, "2024-06-01", "Transfer from spending", 10000);
        // Stale AI suggestion from a run before the opposite leg was pulled.
        let cat = store.get_or_create_category("Bills/Misc").unwrap();
        store
            .set_category(out, cat, CategorySource::Ai, false, Some(0.6))
            .unwrap();

        let report = classify(&mut store).unwrap();

        assert_eq!(report.transfers, 1);
        assert!(store.get_transfer_for_transaction(out).unwrap().is_some());
        // create_transfer cleared the unconfirmed enrichment on the claimed leg.
        assert_eq!(category_path(&store, out), None);
        assert_eq!(category_path(&store, into), None);
    }

    #[test]
    fn transfer_detection_never_claims_user_confirmed_leg() {
        let (_t, mut store, a1, a2) = store_with_two_accounts();
        let rent = insert_tx_desc(&store, a1, "2024-06-01", "Rent", -10000);
        let refund = insert_tx_desc(&store, a2, "2024-06-01", "Rent refund", 10000);
        let cat = store.get_or_create_category("Housing/Rent").unwrap();
        store
            .set_category(rent, cat, CategorySource::Manual, true, None)
            .unwrap();

        let report = classify(&mut store).unwrap();

        assert_eq!(report.transfers, 0);
        assert!(store.get_transfer_for_transaction(rent).unwrap().is_none());
        assert!(
            store
                .get_transfer_for_transaction(refund)
                .unwrap()
                .is_none()
        );
        assert_eq!(category_path(&store, rent), Some("Housing/Rent".into()));
    }
}
