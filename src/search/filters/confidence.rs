//! Confidence filter implementation.

use rusqlite::types::Value;

use crate::search::{Filter, FilterResult, placeholders as ph};

/// Filter on the AI confidence score attached to a suggestion.
///
/// The score is stored as a 0.0–1.0 fraction but every UI surface shows it as
/// a rounded percentage ("85%"), so the query language speaks percentages
/// too: `confidence:<60` is "less certain than 60%". A `%` suffix is accepted
/// and means the same thing. Values above 100 are rejected — a percentage
/// above 100 is always a typo (most often a fraction typed as `0.6`, which
/// this filter reads as 0.6%).
///
/// Exact matches are *display-aware*: the granularity of the input sets the
/// size of the bucket, centred on the value, so `confidence:85` matches
/// everything that renders as "85%" rather than demanding an exact float
/// equality that would essentially never hold. Ranges and comparisons use the
/// endpoints as written.
///
/// Which score is filtered depends on what the search is over: on category
/// searches (Transactions, Todo → AI Review) it is the category suggestion's
/// confidence; on transfer searches (Transfers, Todo → Transfer Review) it is
/// the transfer's. Rows with no score at all — manual categorisations,
/// hand-marked transfers — store NULL and therefore match neither a
/// comparison nor a range; `confidence:none` selects them explicitly.
///
/// Supports:
/// - `<60` / `<60%` → confidence strictly below 60%
/// - `>80` → strictly above 80%
/// - `40..60` → between 40% and 60% inclusive; `..60` / `40..` are open-ended
/// - `85` → anything that displays as 85% (i.e. `[84.5%, 85.5%)`);
///   `85.5` → `[85.45%, 85.55%)`
/// - `none` → no score recorded (NULL); `any` → some score recorded
pub struct ConfidenceFilter;

impl Filter for ConfidenceFilter {
    fn name(&self) -> &'static str {
        "confidence"
    }

    fn alias(&self) -> Option<&'static str> {
        Some("conf")
    }

    fn parse(&self, value: &str) -> FilterResult {
        if value.is_empty() {
            return FilterResult::Empty;
        }

        let column = ph::reference(ph::AI_CONFIDENCE);

        if value.eq_ignore_ascii_case("none") {
            return FilterResult::Valid {
                sql: format!("{column} IS NULL"),
                params: Vec::new(),
            };
        }
        if value.eq_ignore_ascii_case("any") {
            return FilterResult::Valid {
                sql: format!("{column} IS NOT NULL"),
                params: Vec::new(),
            };
        }

        if let Some(rest) = value.strip_prefix('>') {
            return parse_comparison(&column, rest, ">");
        }
        if let Some(rest) = value.strip_prefix('<') {
            return parse_comparison(&column, rest, "<");
        }
        if let Some((from, to)) = value.split_once("..") {
            return parse_range(&column, from, to);
        }

        parse_exact(&column, value)
    }
}

/// A confidence percentage the user typed, converted to the stored 0.0–1.0
/// scale. `granularity` is the width of the bucket the input implies (also on
/// the stored scale): a whole percent for `85`, a tenth for `85.5`.
struct Confidence {
    fraction: f64,
    granularity: f64,
}

fn parse_percent(input: &str) -> Result<Confidence, String> {
    let text = input.strip_suffix('%').unwrap_or(input);
    let (whole, frac) = match text.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (text, ""),
    };

    // Digits only, at most two decimal places, and no bare trailing dot. A
    // sign is rejected along with everything else non-numeric: a negative
    // confidence has no meaning.
    let well_formed = !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
        && frac.len() <= 2
        && (!text.contains('.') || !frac.is_empty());
    if !well_formed {
        return Err(format!("Invalid confidence: {input}"));
    }

    let percent: f64 = text
        .parse()
        .map_err(|_| format!("Invalid confidence: {input}"))?;
    if percent > 100.0 {
        return Err(format!("Confidence is a percentage from 0 to 100: {input}"));
    }

    let percent_granularity = match frac.len() {
        0 => 1.0,
        1 => 0.1,
        _ => 0.01,
    };
    Ok(Confidence {
        fraction: percent / 100.0,
        granularity: percent_granularity / 100.0,
    })
}

fn parse_comparison(column: &str, value: &str, op: &str) -> FilterResult {
    match parse_percent(value) {
        Ok(confidence) => FilterResult::Valid {
            sql: format!("{column} {op} ?"),
            params: vec![Value::Real(confidence.fraction)],
        },
        Err(message) => FilterResult::Invalid(message),
    }
}

fn parse_exact(column: &str, value: &str) -> FilterResult {
    match parse_percent(value) {
        Ok(confidence) => {
            let half = confidence.granularity / 2.0;
            FilterResult::Valid {
                sql: format!("{column} >= ? AND {column} < ?"),
                params: vec![
                    Value::Real(confidence.fraction - half),
                    Value::Real(confidence.fraction + half),
                ],
            }
        }
        Err(message) => FilterResult::Invalid(message),
    }
}

fn parse_range(column: &str, from: &str, to: &str) -> FilterResult {
    let from = if from.is_empty() {
        None
    } else {
        match parse_percent(from) {
            Ok(confidence) => Some(confidence),
            Err(_) => return FilterResult::Invalid(format!("Invalid start confidence: {from}")),
        }
    };
    let to = if to.is_empty() {
        None
    } else {
        match parse_percent(to) {
            Ok(confidence) => Some(confidence),
            Err(_) => return FilterResult::Invalid(format!("Invalid end confidence: {to}")),
        }
    };

    match (from, to) {
        (Some(from), Some(to)) => FilterResult::Valid {
            sql: format!("{column} >= ? AND {column} <= ?"),
            params: vec![Value::Real(from.fraction), Value::Real(to.fraction)],
        },
        (Some(from), None) => FilterResult::Valid {
            sql: format!("{column} >= ?"),
            params: vec![Value::Real(from.fraction)],
        },
        (None, Some(to)) => FilterResult::Valid {
            sql: format!("{column} <= ?"),
            params: vec![Value::Real(to.fraction)],
        },
        (None, None) => FilterResult::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> FilterResult {
        ConfidenceFilter.parse(value)
    }

    #[track_caller]
    fn assert_sql(value: &str, expected_sql: &str, expected_params: &[f64]) {
        match parse(value) {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, expected_sql, "SQL for {value:?}");
                assert_eq!(params.len(), expected_params.len(), "params for {value:?}");
                for (actual, expected) in params.iter().zip(expected_params) {
                    let Value::Real(actual) = actual else {
                        panic!("expected a REAL param for {value:?}, got {actual:?}");
                    };
                    assert!(
                        (actual - expected).abs() < 1e-9,
                        "param for {value:?}: {actual} != {expected}"
                    );
                }
            }
            other => panic!("Expected Valid for {value:?}, got {other:?}"),
        }
    }

    #[test]
    fn empty_value_is_ignored() {
        assert!(matches!(parse(""), FilterResult::Empty));
    }

    #[test]
    fn comparisons_convert_percent_to_fraction() {
        // The headline case: "less certain than 60%".
        assert_sql("<60", "{ai_confidence} < ?", &[0.6]);
        assert_sql(">80", "{ai_confidence} > ?", &[0.8]);
    }

    #[test]
    fn percent_suffix_is_accepted() {
        assert_sql("<60%", "{ai_confidence} < ?", &[0.6]);
        assert_sql(
            "40%..60%",
            "{ai_confidence} >= ? AND {ai_confidence} <= ?",
            &[0.4, 0.6],
        );
    }

    #[test]
    fn range_endpoints_are_inclusive() {
        assert_sql(
            "40..60",
            "{ai_confidence} >= ? AND {ai_confidence} <= ?",
            &[0.4, 0.6],
        );
    }

    #[test]
    fn open_ended_ranges() {
        assert_sql("40..", "{ai_confidence} >= ?", &[0.4]);
        assert_sql("..60", "{ai_confidence} <= ?", &[0.6]);
        assert!(matches!(parse(".."), FilterResult::Empty));
    }

    #[test]
    fn exact_whole_percent_matches_the_displayed_value() {
        // "85" is a query for rows that render as 85%, not for the exact float.
        assert_sql(
            "85",
            "{ai_confidence} >= ? AND {ai_confidence} < ?",
            &[0.845, 0.855],
        );
    }

    #[test]
    fn exact_input_narrows_with_added_decimals() {
        assert_sql(
            "85.5",
            "{ai_confidence} >= ? AND {ai_confidence} < ?",
            &[0.8545, 0.8555],
        );
        assert_sql(
            "85.25",
            "{ai_confidence} >= ? AND {ai_confidence} < ?",
            &[0.85245, 0.85255],
        );
    }

    #[test]
    fn presence_tests_target_null() {
        assert_sql("none", "{ai_confidence} IS NULL", &[]);
        assert_sql("any", "{ai_confidence} IS NOT NULL", &[]);
        assert_sql("None", "{ai_confidence} IS NULL", &[]);
    }

    #[test]
    fn above_one_hundred_is_rejected() {
        // Catches a fraction typed with the decimal point in the wrong place
        // as well as plain nonsense.
        assert!(matches!(parse("101"), FilterResult::Invalid(_)));
        assert!(matches!(parse("<600"), FilterResult::Invalid(_)));
    }

    #[test]
    fn malformed_values_are_rejected() {
        assert!(matches!(parse("abc"), FilterResult::Invalid(_)));
        assert!(matches!(parse("-60"), FilterResult::Invalid(_)));
        assert!(matches!(parse("60."), FilterResult::Invalid(_)));
        assert!(matches!(parse("60.123"), FilterResult::Invalid(_)));
        assert!(matches!(parse("<"), FilterResult::Invalid(_)));
        assert!(matches!(parse("40..abc"), FilterResult::Invalid(_)));
    }

    #[test]
    fn sub_one_percent_values_are_taken_literally() {
        // `0.6` is 0.6%, not 60% — the filter's scale is percent throughout.
        assert_sql("<0.6", "{ai_confidence} < ?", &[0.006]);
    }
}
