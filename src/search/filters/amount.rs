//! Amount filter implementation.

use rusqlite::types::Value;

use crate::search::{Filter, FilterResult, placeholders as ph};

/// Filter for amount ranges.
///
/// Amounts are entered in dollars but stored as cents. Matching is on the
/// absolute value, so the same query catches both debits and credits of that
/// magnitude.
///
/// Exact-match queries are *precision-aware*: the granularity of the input
/// determines the granularity of the match. Typing `7` is a query for "any
/// $7-something", not "exactly $7.00". This matters because real-world
/// transactions rarely land on whole dollars and the old behaviour required
/// the user to know the exact cents before they could find anything. Explicit
/// ranges and comparisons stay cent-exact — the user typed those endpoints
/// deliberately, so we honour them.
///
/// Supports:
/// - `7` → any amount in `[$7.00, $8.00)` (whole-dollar precision)
/// - `7.5` → any amount in `[$7.50, $7.60)` (10¢ precision)
/// - `7.50` → exactly $7.50 (cent precision)
/// - `100..500` → range $100.00 to $500.00 inclusive (endpoints exact)
/// - `..100` → up to $100.00 inclusive
/// - `100..` → $100.00 and above
/// - `>100` → strictly greater than $100.00
/// - `<100` → strictly less than $100.00
pub struct AmountFilter;

impl Filter for AmountFilter {
    fn name(&self) -> &'static str {
        "amount"
    }

    fn alias(&self) -> Option<&'static str> {
        Some("am")
    }

    fn parse(&self, value: &str) -> FilterResult {
        if value.is_empty() {
            return FilterResult::Empty;
        }

        // Check for comparison operators
        if let Some(rest) = value.strip_prefix('>') {
            return self.parse_comparison(rest, ">");
        }
        if let Some(rest) = value.strip_prefix('<') {
            return self.parse_comparison(rest, "<");
        }

        // Check for range syntax
        if let Some((from, to)) = value.split_once("..") {
            return self.parse_range(from, to);
        }

        // Exact match — precision-aware (see struct docs).
        match parse_amount(value) {
            Some(cents) => {
                let granularity = decimal_granularity(value);
                if granularity == 1 {
                    FilterResult::Valid {
                        sql: format!("ABS({}) = ?", ph::reference(ph::AMOUNT_CENTS)),
                        params: vec![Value::Integer(cents)],
                    }
                } else {
                    FilterResult::Valid {
                        sql: format!(
                            "ABS({}) >= ? AND ABS({}) < ?",
                            ph::reference(ph::AMOUNT_CENTS),
                            ph::reference(ph::AMOUNT_CENTS)
                        ),
                        params: vec![Value::Integer(cents), Value::Integer(cents + granularity)],
                    }
                }
            }
            None => FilterResult::Invalid(format!("Invalid amount: {}", value)),
        }
    }
}

/// The smallest cent-step represented by the user's input — i.e. the size of
/// the "bucket" the exact-match query should cover.
///
/// `"7"` → 100 (a whole dollar), `"7.5"` → 10 (a dime), `"7.50"` → 1 (one
/// cent). Inputs with > 2 decimal places are rejected upstream by
/// `parse_amount`, so we never see them here.
fn decimal_granularity(s: &str) -> i64 {
    match s.split_once('.') {
        None => 100,
        Some((_, frac)) => match frac.len() {
            0 => 100,
            1 => 10,
            _ => 1,
        },
    }
}

impl AmountFilter {
    fn parse_comparison(&self, value: &str, op: &str) -> FilterResult {
        match parse_amount(value) {
            Some(cents) => {
                let sql = format!("ABS({}) {} ?", ph::reference(ph::AMOUNT_CENTS), op);
                FilterResult::Valid {
                    sql,
                    params: vec![Value::Integer(cents)],
                }
            }
            None => FilterResult::Invalid(format!("Invalid amount: {}", value)),
        }
    }

    fn parse_range(&self, from: &str, to: &str) -> FilterResult {
        let from_cents = if from.is_empty() {
            None
        } else {
            match parse_amount(from) {
                Some(cents) => Some(cents),
                None => return FilterResult::Invalid(format!("Invalid start amount: {}", from)),
            }
        };

        let to_cents = if to.is_empty() {
            None
        } else {
            match parse_amount(to) {
                Some(cents) => Some(cents),
                None => return FilterResult::Invalid(format!("Invalid end amount: {}", to)),
            }
        };

        match (from_cents, to_cents) {
            (Some(from), Some(to)) => FilterResult::Valid {
                sql: format!(
                    "ABS({}) >= ? AND ABS({}) <= ?",
                    ph::reference(ph::AMOUNT_CENTS),
                    ph::reference(ph::AMOUNT_CENTS)
                ),
                params: vec![Value::Integer(from), Value::Integer(to)],
            },
            (Some(from), None) => FilterResult::Valid {
                sql: format!("ABS({}) >= ?", ph::reference(ph::AMOUNT_CENTS)),
                params: vec![Value::Integer(from)],
            },
            (None, Some(to)) => FilterResult::Valid {
                sql: format!("ABS({}) <= ?", ph::reference(ph::AMOUNT_CENTS)),
                params: vec![Value::Integer(to)],
            },
            (None, None) => FilterResult::Empty,
        }
    }
}

/// Parse an amount string to cents.
/// Supports: 100, 100.50, 100.5
///
/// Rejects inputs with more than 2 decimal places (e.g. "100.999"): cents are
/// the smallest unit, and silently truncating sub-cent precision tends to
/// mask data-entry mistakes rather than fix them.
pub(crate) fn parse_amount(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }

    // Handle decimal amounts
    if let Some((dollars, cents_str)) = s.split_once('.') {
        if cents_str.len() > 2 {
            return None;
        }
        let dollars: i64 = dollars.parse().ok()?;
        // Normalize cents to 2 digits
        let cents_str = format!("{:0<2}", cents_str);
        let cents: i64 = cents_str.parse().ok()?;
        Some(dollars * 100 + cents)
    } else {
        let dollars: i64 = s.parse().ok()?;
        Some(dollars * 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> FilterResult {
        AmountFilter.parse(value)
    }

    #[test]
    fn test_empty() {
        assert!(matches!(parse(""), FilterResult::Empty));
    }

    #[test]
    fn test_whole_dollar_input_expands_to_one_dollar_range() {
        // "100" means "any $100-something", not exactly $100.00 — most
        // transactions aren't on whole dollars.
        match parse("100") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) >= ? AND ABS({amount_cents}) < ?");
                assert_eq!(params, vec![Value::Integer(10000), Value::Integer(10100)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_one_decimal_input_expands_to_ten_cent_range() {
        match parse("7.5") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) >= ? AND ABS({amount_cents}) < ?");
                assert_eq!(params, vec![Value::Integer(750), Value::Integer(760)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_full_precision_input_matches_exactly() {
        // Two decimal places = the user said the exact cents, so we honour it.
        match parse("100.50") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) = ?");
                assert_eq!(params, vec![Value::Integer(10050)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_zero_dollars_expands_to_under_one_dollar() {
        // "0" → everything under $1.
        match parse("0") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) >= ? AND ABS({amount_cents}) < ?");
                assert_eq!(params, vec![Value::Integer(0), Value::Integer(100)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_zero_with_one_decimal_expands_to_ten_cent_range() {
        match parse("0.9") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) >= ? AND ABS({amount_cents}) < ?");
                assert_eq!(params, vec![Value::Integer(90), Value::Integer(100)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_greater_than() {
        match parse(">100") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) > ?");
                assert_eq!(params, vec![Value::Integer(10000)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_less_than() {
        match parse("<50") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) < ?");
                assert_eq!(params, vec![Value::Integer(5000)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_range() {
        match parse("100..500") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) >= ? AND ABS({amount_cents}) <= ?");
                assert_eq!(params, vec![Value::Integer(10000), Value::Integer(50000)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_open_end_range() {
        match parse("100..") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) >= ?");
                assert_eq!(params, vec![Value::Integer(10000)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_open_start_range() {
        match parse("..100") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "ABS({amount_cents}) <= ?");
                assert_eq!(params, vec![Value::Integer(10000)]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_invalid() {
        assert!(matches!(parse("abc"), FilterResult::Invalid(_)));
        assert!(matches!(parse("100.abc"), FilterResult::Invalid(_)));
    }

    #[test]
    fn test_parse_amount() {
        assert_eq!(parse_amount("100"), Some(10000));
        assert_eq!(parse_amount("100.50"), Some(10050));
        assert_eq!(parse_amount("100.5"), Some(10050));
        assert_eq!(parse_amount("0.99"), Some(99));
        assert_eq!(parse_amount(""), None);
        assert_eq!(parse_amount("abc"), None);
    }

    #[test]
    fn test_parse_amount_rejects_sub_cent_precision() {
        // Cents are the smallest unit; reject rather than truncate so typos
        // surface instead of silently losing the trailing digits.
        assert_eq!(parse_amount("100.999"), None);
        assert_eq!(parse_amount("0.001"), None);
        assert_eq!(parse_amount("1.234567"), None);
    }
}
