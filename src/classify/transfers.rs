use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDate;

/// Account pair has prior confirmed-transfer history AND the candidate's
/// normalised description matches a historical leg for that pair.
const TRANSFER_HISTORY_DESC_CONFIDENCE: f64 = 0.95;
/// Account pair has prior confirmed-transfer history, descriptions differ.
const TRANSFER_HISTORY_CONFIDENCE: f64 = 0.80;
/// Structural match with NO history, but the group is an unambiguous 1:1
/// (exactly one debit + one credit, different accounts) -- only one pairing
/// possible.
const TRANSFER_UNAMBIGUOUS_CONFIDENCE: f64 = 0.70;
/// Structural match with NO history in an ambiguous group (multiple debits
/// and/or credits) -- the pairing is one of several possible guesses.
const TRANSFER_AMBIGUOUS_CONFIDENCE: f64 = 0.40;

/// One uncategorised, not-yet-transferred transaction considered for pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferInput {
    pub id: i64,
    pub account_id: i64,
    pub date: NaiveDate,
    pub amount_cents: i64,
    pub norm: String,
}

/// One leg-pair from a prior CONFIRMED transfer, used as history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferHistory {
    pub from_account_id: i64,
    pub to_account_id: i64,
    pub from_norm: String,
    pub to_norm: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectedTransfer {
    pub from_id: i64,
    pub to_id: i64,
    pub confidence: f64,
}

#[derive(Debug, Clone)]
struct ScoredPair {
    from_id: i64,
    to_id: i64,
    debit_account_id: i64,
    credit_account_id: i64,
    confidence: f64,
}

pub fn detect_transfers(
    candidates: &[TransferInput],
    history: &[TransferHistory],
) -> Vec<DetectedTransfer> {
    let history_index = build_history_index(history);
    let mut groups: BTreeMap<(NaiveDate, i64), Vec<&TransferInput>> = BTreeMap::new();

    for candidate in candidates {
        if candidate.amount_cents == 0 {
            continue;
        }
        let Some(abs_amount) = candidate.amount_cents.checked_abs() else {
            continue;
        };
        groups
            .entry((candidate.date, abs_amount))
            .or_default()
            .push(candidate);
    }

    let mut detected = Vec::new();
    for group in groups.values() {
        let debits: Vec<_> = group
            .iter()
            .copied()
            .filter(|candidate| candidate.amount_cents < 0)
            .collect();
        let credits: Vec<_> = group
            .iter()
            .copied()
            .filter(|candidate| candidate.amount_cents > 0)
            .collect();
        let unambiguous = debits.len() == 1
            && credits.len() == 1
            && debits[0].account_id != credits[0].account_id;

        let mut scored = Vec::new();
        for debit in &debits {
            for credit in &credits {
                if debit.account_id == credit.account_id {
                    continue;
                }
                scored.push(score_pair(debit, credit, unambiguous, &history_index));
            }
        }

        scored.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| a.debit_account_id.cmp(&b.debit_account_id))
                .then_with(|| a.from_id.cmp(&b.from_id))
                .then_with(|| a.credit_account_id.cmp(&b.credit_account_id))
                .then_with(|| a.to_id.cmp(&b.to_id))
        });

        let mut used = HashSet::new();
        for pair in scored {
            if used.contains(&pair.from_id) || used.contains(&pair.to_id) {
                continue;
            }
            used.insert(pair.from_id);
            used.insert(pair.to_id);
            detected.push(DetectedTransfer {
                from_id: pair.from_id,
                to_id: pair.to_id,
                confidence: pair.confidence,
            });
        }
    }

    detected
}

fn build_history_index(history: &[TransferHistory]) -> HashMap<(i64, i64), HashSet<String>> {
    let mut index = HashMap::new();
    for transfer in history {
        let key = account_pair_key(transfer.from_account_id, transfer.to_account_id);
        let descriptions = index.entry(key).or_insert_with(HashSet::new);
        descriptions.insert(transfer.from_norm.clone());
        descriptions.insert(transfer.to_norm.clone());
    }
    index
}

fn score_pair(
    debit: &TransferInput,
    credit: &TransferInput,
    unambiguous: bool,
    history_index: &HashMap<(i64, i64), HashSet<String>>,
) -> ScoredPair {
    let key = account_pair_key(debit.account_id, credit.account_id);
    let confidence = if let Some(descriptions) = history_index.get(&key) {
        if descriptions.contains(&debit.norm) || descriptions.contains(&credit.norm) {
            TRANSFER_HISTORY_DESC_CONFIDENCE
        } else {
            TRANSFER_HISTORY_CONFIDENCE
        }
    } else if unambiguous {
        TRANSFER_UNAMBIGUOUS_CONFIDENCE
    } else {
        TRANSFER_AMBIGUOUS_CONFIDENCE
    };

    ScoredPair {
        from_id: debit.id,
        to_id: credit.id,
        debit_account_id: debit.account_id,
        credit_account_id: credit.account_id,
        confidence,
    }
}

fn account_pair_key(left: i64, right: i64) -> (i64, i64) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    fn tx(
        id: i64,
        account_id: i64,
        date_value: &str,
        amount_cents: i64,
        norm: &str,
    ) -> TransferInput {
        TransferInput {
            id,
            account_id,
            date: date(date_value),
            amount_cents,
            norm: norm.to_string(),
        }
    }

    fn history(
        from_account_id: i64,
        to_account_id: i64,
        from_norm: &str,
        to_norm: &str,
    ) -> TransferHistory {
        TransferHistory {
            from_account_id,
            to_account_id,
            from_norm: from_norm.to_string(),
            to_norm: to_norm.to_string(),
        }
    }

    #[test]
    fn unambiguous_same_day_opposite_amount_detects_transfer() {
        let detected = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "from savings"),
                tx(2, 20, "2025-01-01", 5000, "to checking"),
            ],
            &[],
        );

        assert_eq!(
            detected,
            vec![DetectedTransfer {
                from_id: 1,
                to_id: 2,
                confidence: TRANSFER_UNAMBIGUOUS_CONFIDENCE,
            }]
        );
    }

    #[test]
    fn same_account_opposite_amount_is_not_a_transfer() {
        let detected = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "debit"),
                tx(2, 10, "2025-01-01", 5000, "credit"),
            ],
            &[],
        );

        assert!(detected.is_empty());
    }

    #[test]
    fn different_days_are_not_paired() {
        let detected = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "debit"),
                tx(2, 20, "2025-01-02", 5000, "credit"),
            ],
            &[],
        );

        assert!(detected.is_empty());
    }

    #[test]
    fn transaction_is_used_at_most_once() {
        let detected = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "debit"),
                tx(2, 20, "2025-01-01", 5000, "credit a"),
                tx(3, 30, "2025-01-01", 5000, "credit b"),
            ],
            &[],
        );

        assert_eq!(detected.len(), 1);
        assert_eq!(detected[0].from_id, 1);
        assert_eq!(detected[0].to_id, 2);
        assert_eq!(detected[0].confidence, TRANSFER_AMBIGUOUS_CONFIDENCE);
    }

    #[test]
    fn history_raises_confidence_and_description_match_raises_it_further() {
        let prior = [history(20, 10, "old debit", "old credit")];
        let no_description_match = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "new debit"),
                tx(2, 20, "2025-01-01", 5000, "new credit"),
            ],
            &prior,
        );

        assert_eq!(no_description_match.len(), 1);
        assert_eq!(
            no_description_match[0].confidence,
            TRANSFER_HISTORY_CONFIDENCE
        );

        let description_match = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "old debit"),
                tx(2, 20, "2025-01-01", 5000, "new credit"),
            ],
            &prior,
        );

        assert_eq!(description_match.len(), 1);
        assert_eq!(
            description_match[0].confidence,
            TRANSFER_HISTORY_DESC_CONFIDENCE
        );
    }

    #[test]
    fn history_backed_pair_is_chosen_first_in_ambiguous_group() {
        let detected = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "debit a"),
                tx(2, 30, "2025-01-01", -5000, "debit b"),
                tx(3, 20, "2025-01-01", 5000, "credit a"),
                tx(4, 40, "2025-01-01", 5000, "credit b"),
            ],
            &[history(10, 20, "old debit", "old credit")],
        );

        assert_eq!(
            detected,
            vec![
                DetectedTransfer {
                    from_id: 1,
                    to_id: 3,
                    confidence: TRANSFER_HISTORY_CONFIDENCE,
                },
                DetectedTransfer {
                    from_id: 2,
                    to_id: 4,
                    confidence: TRANSFER_AMBIGUOUS_CONFIDENCE,
                },
            ]
        );
    }

    #[test]
    fn confidence_tiers_have_expected_ordering() {
        let ambiguous = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "debit"),
                tx(2, 20, "2025-01-01", 5000, "credit a"),
                tx(3, 30, "2025-01-01", 5000, "credit b"),
            ],
            &[],
        )[0]
        .confidence;
        let unambiguous = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "debit"),
                tx(2, 20, "2025-01-01", 5000, "credit"),
            ],
            &[],
        )[0]
        .confidence;
        let history_backed = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "debit"),
                tx(2, 20, "2025-01-01", 5000, "credit"),
            ],
            &[history(10, 20, "old debit", "old credit")],
        )[0]
        .confidence;
        let history_with_description = detect_transfers(
            &[
                tx(1, 10, "2025-01-01", -5000, "old debit"),
                tx(2, 20, "2025-01-01", 5000, "credit"),
            ],
            &[history(10, 20, "old debit", "old credit")],
        )[0]
        .confidence;

        assert!(unambiguous > ambiguous);
        assert!(history_backed > unambiguous);
        assert!(history_with_description > history_backed);
    }
}
