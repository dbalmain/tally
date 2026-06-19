//! Local transfer detection and category classification.
//!
//! [`detect_transfers`], [`train`], and [`predict`] are pure in-memory
//! operations. [`classify`] is the storage adapter used by the CLI.

mod adapter;
mod similarity;
mod svm;
mod tfidf;
mod transfers;

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use chrono::NaiveDate;
use regex::Regex;

use self::svm::LinearSvm;
use self::tfidf::Tfidf;

pub use adapter::classify;
pub use similarity::{SIMILARITY_THRESHOLD, SimilarityIndex};
pub use transfers::{TransferHistory, TransferInput, detect_transfers};

const EXACT_CONFIDENCE: f64 = 0.99;
const RECURRING_HIGH_CONFIDENCE: f64 = 0.95;
const RECURRING_LOW_CONFIDENCE: f64 = 0.40;

/// Counts produced by one local classification run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClassifyReport {
    pub transfers: usize,
    pub exact: usize,
    pub recurring: usize,
    pub model: usize,
    pub unclassified: usize,
}

/// Confirmed training row consumed by the pure classifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Example {
    pub norm: String,
    pub amount_cents: i64,
    pub date: NaiveDate,
    pub category_id: i64,
}

/// Uncategorized row consumed by the pure classifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub norm: String,
    pub amount_cents: i64,
    pub date: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredictionSource {
    Exact,
    Recurring,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prediction {
    pub category_id: i64,
    pub confidence: f64,
    pub source: PredictionSource,
}

struct HistoryEntry {
    date: NaiveDate,
    amount_cents: i64,
    category_id: i64,
}

struct BillerHistory {
    entries: Vec<HistoryEntry>,
    unambiguous: bool,
}

/// Trained, in-memory category classifier with no storage or IO dependencies.
pub struct Classifier {
    history: HashMap<String, BillerHistory>,
    tfidf: Option<Tfidf>,
    svm: Option<LinearSvm>,
}

/// Normalize a bank description into a merchant-like matching key.
pub fn normalise(description: &str) -> String {
    static CARD_DETAILS: OnceLock<Regex> = OnceLock::new();
    static REFERENCE: OnceLock<Regex> = OnceLock::new();
    static NOISE: OnceLock<Regex> = OnceLock::new();
    static DIGITS: OnceLock<Regex> = OnceLock::new();
    static PUNCTUATION: OnceLock<Regex> = OnceLock::new();
    static WHITESPACE: OnceLock<Regex> = OnceLock::new();

    let mut text = description.to_lowercase();
    text = CARD_DETAILS
        .get_or_init(|| {
            Regex::new(
                r"\b(?:(?:visa|mastercard|debit|credit)\s+)?card\s*(?:ending\s+(?:in\s+)?)?(?:x+|\*+)?\s*\d{2,}\b",
            )
            .unwrap()
        })
        .replace_all(&text, " ")
        .into_owned();
    text = REFERENCE
        .get_or_init(|| {
            Regex::new(
                r"\b(?:ref(?:erence)?|txn|transaction)\s*(?:no|number)?\s*[:#-]?\s*[a-z0-9*-]*\d[a-z0-9*-]*\b",
            )
            .unwrap()
        })
        .replace_all(&text, " ")
        .into_owned();
    text = NOISE
        .get_or_init(|| {
            Regex::new(
                r"\b(?:visa|mastercard|debit|credit|card|eftpos|pos|purchase|ref|reference|txn|transaction)\b",
            )
            .unwrap()
        })
        .replace_all(&text, " ")
        .into_owned();
    text = DIGITS
        .get_or_init(|| Regex::new(r"\d+").unwrap())
        .replace_all(&text, " ")
        .into_owned();
    text = PUNCTUATION
        .get_or_init(|| Regex::new(r"[^a-z\s]+").unwrap())
        .replace_all(&text, " ")
        .into_owned();
    WHITESPACE
        .get_or_init(|| Regex::new(r"\s+").unwrap())
        .replace_all(text.trim(), " ")
        .into_owned()
}

/// Train the pure temporal and description model pipeline.
pub fn train(examples: &[Example]) -> Classifier {
    let mut grouped: HashMap<String, Vec<HistoryEntry>> = HashMap::new();
    for example in examples.iter().filter(|example| !example.norm.is_empty()) {
        grouped
            .entry(example.norm.clone())
            .or_default()
            .push(HistoryEntry {
                date: example.date,
                amount_cents: example.amount_cents,
                category_id: example.category_id,
            });
    }

    let history = grouped
        .into_iter()
        .map(|(norm, mut entries)| {
            entries.sort_by_key(|entry| entry.date);
            let unambiguous = entries
                .iter()
                .map(|entry| entry.category_id)
                .collect::<HashSet<_>>()
                .len()
                == 1;
            (
                norm,
                BillerHistory {
                    entries,
                    unambiguous,
                },
            )
        })
        .collect();

    let documents: Vec<_> = examples
        .iter()
        .map(|example| example.norm.as_str())
        .collect();
    let tfidf = Tfidf::fit(&documents);
    let svm = tfidf.as_ref().and_then(|tfidf| {
        let rows: Vec<_> = documents
            .iter()
            .map(|document| tfidf.transform(document))
            .collect();
        let labels: Vec<_> = examples.iter().map(|example| example.category_id).collect();
        LinearSvm::train(&rows, &labels, tfidf.len())
    });

    Classifier {
        history,
        tfidf,
        svm,
    }
}

/// Predict one input using confirmed history first, then the linear model.
pub fn predict(classifier: &Classifier, input: &Input) -> Option<Prediction> {
    if input.norm.is_empty() {
        return None;
    }

    if let Some(history) = classifier.history.get(&input.norm) {
        if let Some(entry) = history
            .entries
            .iter()
            .rev()
            .find(|entry| entry.date < input.date && entry.amount_cents == input.amount_cents)
        {
            return Some(Prediction {
                category_id: entry.category_id,
                confidence: EXACT_CONFIDENCE,
                source: PredictionSource::Exact,
            });
        }

        if let Some(entry) = history
            .entries
            .iter()
            .rev()
            .find(|entry| entry.date < input.date)
        {
            return Some(Prediction {
                category_id: entry.category_id,
                confidence: if history.unambiguous {
                    RECURRING_HIGH_CONFIDENCE
                } else {
                    RECURRING_LOW_CONFIDENCE
                },
                source: PredictionSource::Recurring,
            });
        }
    }

    let row = classifier.tfidf.as_ref()?.transform(&input.norm);
    let (category_id, margin) = classifier.svm.as_ref()?.predict(&row);
    let sigmoid = 1.0 / (1.0 + (-f64::from(margin.max(0.0))).exp());
    Some(Prediction {
        category_id,
        confidence: 0.5 + 0.4 * (2.0 * sigmoid - 1.0),
        source: PredictionSource::Model,
    })
}

#[cfg(test)]
mod tests;
