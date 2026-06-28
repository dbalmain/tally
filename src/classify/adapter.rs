use crate::search::ParsedQuery;
use crate::{CategorySource, Result, TransactionStore, TransferSource};

use super::{
    ClassifyReport, Example, Input, PredictionSource, TransferHistory, TransferInput,
    detect_transfers, normalise, predict, train,
};

/// Classify all currently uncategorized, non-transfer transactions in a collection.
pub fn classify(store: &mut TransactionStore) -> Result<ClassifyReport> {
    let mut report = ClassifyReport {
        filtered: store.apply_filters()?,
        ..Default::default()
    };

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
    let transfer_candidates: Vec<_> = store
        .get_uncategorised_transactions(&ParsedQuery::empty(), None)?
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
