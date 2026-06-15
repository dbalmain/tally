use std::path::Path;

use crate::search::ParsedQuery;
use crate::{CategorySource, Result, TransactionStore};

use super::{ClassifyReport, Example, Input, PredictionSource, normalise, predict, train};

/// Classify all currently uncategorized, non-transfer transactions in a collection.
pub fn classify(store: &mut TransactionStore, _collection_root: &Path) -> Result<ClassifyReport> {
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
    let mut report = ClassifyReport::default();

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
