//! Amount filter implementation.

use rusqlite::types::Value;

use crate::search::{Filter, FilterResult, placeholders as ph};

/// Filter for amount ranges.
///
/// Amounts are entered in dollars but stored as cents. For exact matches and
/// ranges, matching is on the absolute value by default, so the same query
/// catches both debits and credits of that magnitude.
///
/// Comparisons (`>`/`<`) are always *signed* (`{amount_cents}` directly):
/// they express an ordering on the number line, so `>100` means "greater than
/// $100", never "greater in magnitude" — `-101` must not match. Use a range
/// (`100..`, `..100`) for a magnitude query.
///
/// Exact matches and ranges switch from ABS to signed when either:
/// - any value carries an explicit `+` or `-` sign, or
/// - (ranges only) an endpoint is zero — zero endpoints are meaningless under
///   ABS (`ABS(x) >= 0` matches everything), so they are reinterpreted as
///   signed rather than degenerate.
///
/// Exact-match queries are *precision-aware*: the granularity of the input
/// determines the granularity of the match. Typing `7` is a query for "any
/// $7-something", not "exactly $7.00". This matters because real-world
/// transactions rarely land on whole dollars and the old behaviour required
/// the user to know the exact cents before they could find anything. Explicit
/// ranges and comparisons stay cent-exact — the user typed those endpoints
/// deliberately, so we honour them. Signed exact matches keep the same
/// buckets on the signed axis: `-7` is "any $7-something debit", i.e.
/// `(-$8.00, -$7.00]`.
///
/// Supports:
/// - `7` → any amount in `[$7.00, $8.00)` (whole-dollar precision)
/// - `7.5` → any amount in `[$7.50, $7.60)` (10¢ precision)
/// - `7.50` → exactly $7.50 (cent precision)
/// - `100..500` → range $100.00 to $500.00 inclusive (endpoints exact, ABS)
/// - `..100` → magnitude up to $100.00 inclusive
/// - `100..` → magnitude $100.00 and above
/// - `>100` → strictly greater than $100.00 (signed; excludes all debits)
/// - `<100` → strictly less than $100.00 (signed; includes all debits)
/// - `0..` / `>0` → credits (signed); `..0` / `<0` → debits (signed)
/// - `-100..-50` → debits between -$100.00 and -$50.00 (signed)
/// - `-7` → any $7-something debit; `-7.50` → exactly -$7.50; `>-5` →
///   signed comparison
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

        // Exact match — precision-aware (see struct docs). Note: unlike ranges
        // and comparisons, a bare `0` keeps its ABS bucket ("anything under a
        // dollar, either sign") — only an explicit sign switches the axis.
        match parse_signed_amount(value) {
            Some(amount) => {
                let column = amount_column(amount.sign != Sign::Unsigned);
                let granularity = decimal_granularity(value);
                if granularity == 1 {
                    FilterResult::Valid {
                        sql: format!("{column} = ?"),
                        params: vec![Value::Integer(amount.cents)],
                    }
                } else if amount.sign == Sign::Negative {
                    // The bucket extends away from zero: `-7` covers
                    // (-$8.00, -$7.00].
                    FilterResult::Valid {
                        sql: format!("{column} > ? AND {column} <= ?"),
                        params: vec![
                            Value::Integer(amount.cents - granularity),
                            Value::Integer(amount.cents),
                        ],
                    }
                } else {
                    FilterResult::Valid {
                        sql: format!("{column} >= ? AND {column} < ?"),
                        params: vec![
                            Value::Integer(amount.cents),
                            Value::Integer(amount.cents + granularity),
                        ],
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

/// The column expression to match against: the raw signed amount, or its
/// absolute value for sign-agnostic queries.
fn amount_column(signed: bool) -> String {
    let column = ph::reference(ph::AMOUNT_CENTS);
    if signed {
        column
    } else {
        format!("ABS({column})")
    }
}

impl AmountFilter {
    fn parse_comparison(&self, value: &str, op: &str) -> FilterResult {
        match parse_signed_amount(value) {
            // Comparisons are an ordering, so they are always signed: `>100`
            // must not match a -$101 debit.
            Some(amount) => FilterResult::Valid {
                sql: format!("{} {op} ?", amount_column(true)),
                params: vec![Value::Integer(amount.cents)],
            },
            None => FilterResult::Invalid(format!("Invalid amount: {}", value)),
        }
    }

    fn parse_range(&self, from: &str, to: &str) -> FilterResult {
        let from_amount = if from.is_empty() {
            None
        } else {
            match parse_signed_amount(from) {
                Some(amount) => Some(amount),
                None => return FilterResult::Invalid(format!("Invalid start amount: {}", from)),
            }
        };

        let to_amount = if to.is_empty() {
            None
        } else {
            match parse_signed_amount(to) {
                Some(amount) => Some(amount),
                None => return FilterResult::Invalid(format!("Invalid end amount: {}", to)),
            }
        };

        // One signed or zero endpoint makes the whole range signed.
        let signed = [&from_amount, &to_amount]
            .into_iter()
            .flatten()
            .any(|amount| amount.sign != Sign::Unsigned || amount.cents == 0);
        let column = amount_column(signed);

        match (from_amount, to_amount) {
            (Some(from), Some(to)) => FilterResult::Valid {
                sql: format!("{column} >= ? AND {column} <= ?"),
                params: vec![Value::Integer(from.cents), Value::Integer(to.cents)],
            },
            (Some(from), None) => FilterResult::Valid {
                sql: format!("{column} >= ?"),
                params: vec![Value::Integer(from.cents)],
            },
            (None, Some(to)) => FilterResult::Valid {
                sql: format!("{column} <= ?"),
                params: vec![Value::Integer(to.cents)],
            },
            (None, None) => FilterResult::Empty,
        }
    }
}

/// Whether the user wrote an explicit sign, and which one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sign {
    Unsigned,
    Positive,
    Negative,
}

/// A parsed amount: sign-applied cents plus how the sign was written. The
/// sign character is kept separately from `cents` so `+0`/`-0` and bucket
/// direction don't depend on the (possibly zero) value.
struct SignedAmount {
    cents: i64,
    sign: Sign,
}

fn parse_signed_amount(s: &str) -> Option<SignedAmount> {
    let sign = match s.as_bytes().first() {
        Some(b'-') => Sign::Negative,
        Some(b'+') => Sign::Positive,
        _ => Sign::Unsigned,
    };
    let cents = parse_amount(s)?;
    Some(SignedAmount { cents, sign })
}

/// Parse an amount string to cents.
/// Supports: 100, 100.50, 100.5, and a single leading `+`/`-` applied to the
/// whole magnitude: "-100.50" → -10050.
///
/// Rejects inputs with more than 2 decimal places (e.g. "100.999"): cents are
/// the smallest unit, and silently truncating sub-cent precision tends to
/// mask data-entry mistakes rather than fix them.
pub(crate) fn parse_amount(s: &str) -> Option<i64> {
    let (negative, magnitude) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    // A second sign (e.g. "--5") would otherwise slip through the i64 parse.
    if magnitude.is_empty() || magnitude.starts_with(['+', '-']) {
        return None;
    }

    // Handle decimal amounts
    let cents = if let Some((dollars, cents_str)) = magnitude.split_once('.') {
        if cents_str.len() > 2 {
            return None;
        }
        let dollars: i64 = dollars.parse().ok()?;
        // Normalize cents to 2 digits
        let cents_str = format!("{:0<2}", cents_str);
        let cents: i64 = cents_str.parse().ok()?;
        dollars * 100 + cents
    } else {
        let dollars: i64 = magnitude.parse().ok()?;
        dollars * 100
    };
    Some(if negative { -cents } else { cents })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> FilterResult {
        AmountFilter.parse(value)
    }

    #[track_caller]
    fn assert_sql(value: &str, expected_sql: &str, expected_params: &[i64]) {
        match parse(value) {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, expected_sql, "SQL for {value:?}");
                let expected: Vec<Value> =
                    expected_params.iter().map(|&c| Value::Integer(c)).collect();
                assert_eq!(params, expected, "params for {value:?}");
            }
            other => panic!("Expected Valid for {value:?}, got {other:?}"),
        }
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
    fn test_comparisons_are_signed() {
        // `>100` is an ordering, not a magnitude test: -$101 must not match.
        assert_sql(">100", "{amount_cents} > ?", &[10000]);
        assert_sql("<50", "{amount_cents} < ?", &[5000]);
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
    fn test_zero_bounded_ranges_are_signed() {
        // ABS(x) >= 0 matches everything — a zero endpoint only makes sense
        // on the signed axis.
        assert_sql("0..", "{amount_cents} >= ?", &[0]);
        assert_sql("..0", "{amount_cents} <= ?", &[0]);
    }

    #[test]
    fn test_zero_comparisons_are_signed() {
        assert_sql(">0", "{amount_cents} > ?", &[0]);
        assert_sql("<0", "{amount_cents} < ?", &[0]);
    }

    #[test]
    fn test_negative_range() {
        assert_sql(
            "-100..-50",
            "{amount_cents} >= ? AND {amount_cents} <= ?",
            &[-10000, -5000],
        );
    }

    #[test]
    fn test_mixed_sign_range_is_fully_signed() {
        // One signed endpoint makes the whole range signed.
        assert_sql(
            "-50..100",
            "{amount_cents} >= ? AND {amount_cents} <= ?",
            &[-5000, 10000],
        );
    }

    #[test]
    fn test_open_signed_ranges() {
        assert_sql("-50..", "{amount_cents} >= ?", &[-5000]);
        assert_sql("..-50", "{amount_cents} <= ?", &[-5000]);
        assert_sql("+100..", "{amount_cents} >= ?", &[10000]);
    }

    #[test]
    fn test_signed_exact_whole_dollar_buckets_away_from_zero() {
        // "-7" is "any $7-something debit": (-$8.00, -$7.00].
        assert_sql(
            "-7",
            "{amount_cents} > ? AND {amount_cents} <= ?",
            &[-800, -700],
        );
    }

    #[test]
    fn test_signed_exact_one_decimal_buckets_away_from_zero() {
        assert_sql(
            "-7.5",
            "{amount_cents} > ? AND {amount_cents} <= ?",
            &[-760, -750],
        );
    }

    #[test]
    fn test_signed_exact_full_precision_matches_exactly() {
        assert_sql("-7.50", "{amount_cents} = ?", &[-750]);
    }

    #[test]
    fn test_positive_signed_exact_whole_dollar() {
        assert_sql(
            "+7",
            "{amount_cents} >= ? AND {amount_cents} < ?",
            &[700, 800],
        );
    }

    #[test]
    fn test_signed_comparisons() {
        assert_sql(">-5", "{amount_cents} > ?", &[-500]);
        assert_sql("<-100", "{amount_cents} < ?", &[-10000]);
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
    fn test_parse_amount_applies_sign_to_whole_magnitude() {
        // The old code applied the sign to the dollars only, so "-100.50"
        // came out as -100*100 + 50 = -9950.
        assert_eq!(parse_amount("-100.50"), Some(-10050));
        assert_eq!(parse_amount("-100.5"), Some(-10050));
        assert_eq!(parse_amount("-7"), Some(-700));
        assert_eq!(parse_amount("+7.5"), Some(750));
        assert_eq!(parse_amount("+0.99"), Some(99));
        assert_eq!(parse_amount("-"), None);
        assert_eq!(parse_amount("+"), None);
        assert_eq!(parse_amount("--5"), None);
        assert_eq!(parse_amount("+-5"), None);
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
