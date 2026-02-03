use chrono::{Datelike, NaiveDate};
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher, Utf32Str,
};

use crate::{AccountPattern, TransactionFilter};

/// Text matching mode for DB search
#[derive(Debug, Clone, Default)]
pub enum DbTextMatch {
    #[default]
    None,
    /// Case-insensitive substring match (LIKE %pattern%)
    Substring(String),
    /// Regex match using SQLite REGEXP UDF
    Regex {
        /// The regex pattern string (with (?i) prefix if case-insensitive)
        pattern: String,
        /// The original input (e.g., "/pattern/i")
        original: String,
    },
}

impl DbTextMatch {
    pub fn is_empty(&self) -> bool {
        match self {
            DbTextMatch::None => true,
            DbTextMatch::Substring(s) => s.is_empty(),
            DbTextMatch::Regex { original, .. } => original.is_empty(),
        }
    }
}

/// DB-backed search query with structured filters and text/regex matching.
/// Parsed from user input like: `date:2024-01 amount:>100 account:ING/Orange groceries`
#[derive(Debug, Default, Clone)]
pub struct DbSearchQuery {
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    pub amount_min: Option<i64>,
    pub amount_max: Option<i64>,
    /// Account patterns (supports "Bank/Account" format, pipe-separated for OR)
    pub accounts: Vec<AccountPattern>,
    /// Category patterns (pipe-separated for OR, contains match)
    pub categories: Vec<String>,
    pub text_match: DbTextMatch,
}

impl DbSearchQuery {
    /// Parse a search query string with support for filters and text matching.
    /// Returns the query and whether it ends with ` ~` (transition to fuzzy mode).
    pub fn parse(input: &str) -> (Self, bool) {
        let (input, transition_to_fuzzy) = if let Some(stripped) = input.strip_suffix(" ~") {
            (stripped, true)
        } else {
            (input, false)
        };

        let mut query = DbSearchQuery::default();
        let mut text_parts = Vec::new();

        for token in tokenize(input) {
            if let Some(rest) = token.strip_prefix("date:").or_else(|| token.strip_prefix("d:")) {
                parse_date_range(rest, &mut query);
            } else if let Some(rest) = token.strip_prefix("amount:") {
                parse_amount_range(rest, &mut query);
            } else if let Some(rest) = token
                .strip_prefix("account:")
                .or_else(|| token.strip_prefix("a:"))
            {
                query.accounts = rest.split('|').map(AccountPattern::parse).collect();
            } else if let Some(rest) = token
                .strip_prefix("category:")
                .or_else(|| token.strip_prefix("c:"))
            {
                query.categories = rest.split('|').map(|s| s.to_string()).collect();
            } else {
                text_parts.push(token);
            }
        }

        let text = text_parts.join(" ");
        query.text_match = parse_db_text_match(&text);
        (query, transition_to_fuzzy)
    }

    /// Check if all query fields are empty.
    pub fn is_empty(&self) -> bool {
        self.date_from.is_none()
            && self.date_to.is_none()
            && self.amount_min.is_none()
            && self.amount_max.is_none()
            && self.accounts.is_empty()
            && self.categories.is_empty()
            && self.text_match.is_empty()
    }

    /// Convert to a TransactionFilter for database queries.
    pub fn to_filter(&self, limit: Option<usize>) -> TransactionFilter {
        TransactionFilter {
            from_date: self.date_from,
            to_date: self.date_to,
            amount_min: self.amount_min,
            amount_max: self.amount_max,
            account_patterns: self.accounts.clone(),
            category_patterns: self.categories.clone(),
            description_contains: match &self.text_match {
                DbTextMatch::Substring(s) => Some(s.clone()),
                _ => None,
            },
            description_regex: match &self.text_match {
                DbTextMatch::Regex { pattern, .. } => Some(pattern.clone()),
                _ => None,
            },
            limit,
            ..Default::default()
        }
    }
}

/// Tokenize input handling quoted strings and backslash escapes.
/// - `key:"value with spaces"` -> `key:value with spaces`
/// - `key:value\ with\ spaces` -> `key:value with spaces`
/// - Regular whitespace-separated tokens work as before
fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_quotes = false;

    while let Some(c) = chars.next() {
        match c {
            '\\' if !in_quotes => {
                // Backslash escape: consume next char literally
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '"' => {
                in_quotes = !in_quotes;
            }
            ' ' | '\t' if !in_quotes => {
                // End of token
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => {
                current.push(c);
            }
        }
    }

    // Don't forget the last token
    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn parse_db_text_match(text: &str) -> DbTextMatch {
    if text.is_empty() {
        return DbTextMatch::None;
    }

    // Check for regex: /pattern/ or /pattern/i
    if let Some((pattern, flags)) = text.strip_prefix('/').and_then(|rest| {
        rest.rfind('/').map(|end_slash| (&rest[..end_slash], &rest[end_slash + 1..]))
    }) {
        let case_insensitive = flags.contains('i');

        let regex_pattern = if case_insensitive {
            format!("(?i){}", pattern)
        } else {
            pattern.to_string()
        };

        return DbTextMatch::Regex {
            pattern: regex_pattern,
            original: text.to_string(),
        };
    }

    // Default: case-insensitive substring match
    DbTextMatch::Substring(text.to_string())
}

fn parse_date_range(s: &str, query: &mut DbSearchQuery) {
    if let Some((from, to)) = s.split_once("..") {
        query.date_from = parse_date_start(from);
        query.date_to = parse_date_end(to);
    } else if let Some(rest) = s.strip_prefix('>') {
        query.date_from = parse_date_start(rest);
    } else if let Some(rest) = s.strip_prefix('<') {
        query.date_to = parse_date_end(rest);
    } else {
        // Single date or partial date - expand to range
        query.date_from = parse_date_start(s);
        query.date_to = parse_date_end(s);
    }
}

/// Parse date for range start (first day of month/year for partial dates)
fn parse_date_start(s: &str) -> Option<NaiveDate> {
    if s.is_empty() {
        return None;
    }
    // Full date: YYYY-MM-DD
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d);
    }
    // Partial date: YYYY-MM -> first day of month
    if let Ok(d) = NaiveDate::parse_from_str(&format!("{}-01", s), "%Y-%m-%d") {
        return Some(d);
    }
    // Year only: YYYY -> first day of year
    if let Some(year) = s.parse::<i32>().ok().filter(|y| (1900..=2100).contains(y)) {
        return NaiveDate::from_ymd_opt(year, 1, 1);
    }
    None
}

/// Parse date for range end (last day of month/year for partial dates)
fn parse_date_end(s: &str) -> Option<NaiveDate> {
    if s.is_empty() {
        return None;
    }
    // Full date: YYYY-MM-DD
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(d);
    }
    // Partial date: YYYY-MM -> last day of month
    if let Ok(first_day) = NaiveDate::parse_from_str(&format!("{}-01", s), "%Y-%m-%d") {
        return Some(last_day_of_month(first_day));
    }
    // Year only: YYYY -> last day of year
    if let Some(year) = s.parse::<i32>().ok().filter(|y| (1900..=2100).contains(y)) {
        return NaiveDate::from_ymd_opt(year, 12, 31);
    }
    None
}

/// Get the last day of the month for a given date
fn last_day_of_month(date: NaiveDate) -> NaiveDate {
    let (year, month) = (date.year(), date.month());
    if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .unwrap()
    .pred_opt()
    .unwrap()
}

fn parse_amount_range(s: &str, query: &mut DbSearchQuery) {
    if let Some((from, to)) = s.split_once("..") {
        query.amount_min = parse_cents(from);
        query.amount_max = parse_cents(to);
    } else if let Some(rest) = s.strip_prefix('>') {
        query.amount_min = parse_cents(rest);
    } else if let Some(rest) = s.strip_prefix('<') {
        query.amount_max = parse_cents(rest);
    } else {
        let amount = parse_cents(s);
        query.amount_min = amount;
        query.amount_max = amount;
    }
}

fn parse_cents(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let s = s.replace(',', "");
    if let Ok(f) = s.parse::<f64>() {
        return Some((f * 100.0).round() as i64);
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(i * 100);
    }
    None
}

pub struct FuzzyMatcher {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Default for FuzzyMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzyMatcher {
    /// Create a new fuzzy matcher instance.
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(nucleo_matcher::Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    /// Score a pattern against haystack text, returning None if no match.
    pub fn score(&mut self, pattern: &str, haystack: &str) -> Option<u32> {
        if pattern.is_empty() {
            return Some(0);
        }
        let pat = Pattern::parse(pattern, CaseMatching::Ignore, Normalization::Smart);
        self.buf.clear();
        let haystack = Utf32Str::new(haystack, &mut self.buf);
        pat.score(haystack, &mut self.matcher)
    }

    /// Check if pattern fuzzy-matches haystack.
    pub fn fuzzy_matches(&mut self, pattern: &str, haystack: &str) -> bool {
        if pattern.is_empty() {
            return true;
        }
        self.score(pattern, haystack).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_substring() {
        let (q, transition) = DbSearchQuery::parse("coffee shop");
        assert!(matches!(q.text_match, DbTextMatch::Substring(ref s) if s == "coffee shop"));
        assert!(q.date_from.is_none());
        assert!(!transition);
    }

    #[test]
    fn test_parse_regex() {
        let (q, _) = DbSearchQuery::parse("/cof.*shop/i");
        assert!(matches!(q.text_match, DbTextMatch::Regex { ref pattern, .. } if pattern == "(?i)cof.*shop"));
    }

    #[test]
    fn test_parse_regex_case_sensitive() {
        let (q, _) = DbSearchQuery::parse("/Coffee/");
        assert!(matches!(q.text_match, DbTextMatch::Regex { ref pattern, .. } if pattern == "Coffee"));
    }

    #[test]
    fn test_parse_date_range() {
        let (q, _) = DbSearchQuery::parse("date:2024-01..2024-06");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 6, 30).unwrap()));
    }

    #[test]
    fn test_parse_date_single_full() {
        let (q, _) = DbSearchQuery::parse("date:2024-03-15");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()));
    }

    #[test]
    fn test_parse_date_month() {
        let (q, _) = DbSearchQuery::parse("date:2025-09");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2025, 9, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2025, 9, 30).unwrap()));
    }

    #[test]
    fn test_parse_date_february_leap_year() {
        let (q, _) = DbSearchQuery::parse("date:2024-02");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 2, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()));
    }

    #[test]
    fn test_parse_date_year_only() {
        let (q, _) = DbSearchQuery::parse("date:2024");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap()));
    }

    #[test]
    fn test_parse_amount_range() {
        let (q, _) = DbSearchQuery::parse("amount:50..200");
        assert_eq!(q.amount_min, Some(5000));
        assert_eq!(q.amount_max, Some(20000));
    }

    #[test]
    fn test_parse_amount_greater() {
        let (q, _) = DbSearchQuery::parse("amount:>100");
        assert_eq!(q.amount_min, Some(10000));
        assert!(q.amount_max.is_none());
    }

    #[test]
    fn test_parse_negative_amount() {
        let (q, _) = DbSearchQuery::parse("amount:-50");
        assert_eq!(q.amount_min, Some(-5000));
        assert_eq!(q.amount_max, Some(-5000));
    }

    #[test]
    fn test_parse_combined() {
        let (q, _) = DbSearchQuery::parse("date:2024-01 amount:>100 account:Chase/ groceries");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap()));
        assert_eq!(q.amount_min, Some(10000));
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "Chase");
        assert_eq!(q.accounts[0].account_prefix, Some("".to_string()));
        assert!(matches!(q.text_match, DbTextMatch::Substring(ref s) if s == "groceries"));
    }

    #[test]
    fn test_parse_transition_to_fuzzy() {
        let (q, transition) = DbSearchQuery::parse("date:2024 coffee ~");
        assert!(transition);
        assert!(matches!(q.text_match, DbTextMatch::Substring(ref s) if s == "coffee"));
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
    }

    #[test]
    fn test_parse_no_transition_without_space() {
        let (_, transition) = DbSearchQuery::parse("coffee~");
        assert!(!transition);
    }

    #[test]
    fn test_fuzzy_matcher() {
        let mut m = FuzzyMatcher::new();
        assert!(m.fuzzy_matches("ctysd", "CITYSIDE BANK"));
        assert!(m.fuzzy_matches("ctysd", "cityside"));
        assert!(m.fuzzy_matches("", "anything"));
        assert!(!m.fuzzy_matches("xyz", "cityside"));
    }

    #[test]
    fn test_tokenize_simple() {
        let tokens = tokenize("hello world");
        assert_eq!(tokens, vec!["hello", "world"]);
    }

    #[test]
    fn test_tokenize_quoted() {
        let tokens = tokenize(r#"account:"Orange Everyday" groceries"#);
        assert_eq!(tokens, vec!["account:Orange Everyday", "groceries"]);
    }

    #[test]
    fn test_tokenize_backslash() {
        let tokens = tokenize(r"account:Orange\ Everyday groceries");
        assert_eq!(tokens, vec!["account:Orange Everyday", "groceries"]);
    }

    #[test]
    fn test_parse_account_quoted() {
        let (q, _) = DbSearchQuery::parse(r#"account:"ING/Orange Everyday""#);
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "ING");
        assert_eq!(q.accounts[0].account_prefix, Some("Orange Everyday".to_string()));
    }

    #[test]
    fn test_parse_account_backslash() {
        let (q, _) = DbSearchQuery::parse(r"account:ING/Orange\ Everyday");
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "ING");
        assert_eq!(q.accounts[0].account_prefix, Some("Orange Everyday".to_string()));
    }

    #[test]
    fn test_parse_account_bank_only() {
        let (q, _) = DbSearchQuery::parse("account:St coffee");
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "St");
        assert_eq!(q.accounts[0].account_prefix, None);
        assert!(matches!(q.text_match, DbTextMatch::Substring(ref s) if s == "coffee"));
    }

    #[test]
    fn test_parse_account_multiple() {
        let (q, _) = DbSearchQuery::parse(r#"account:"ING/Orange"|"St George/Savings""#);
        assert_eq!(q.accounts.len(), 2);
        assert_eq!(q.accounts[0].bank_prefix, "ING");
        assert_eq!(q.accounts[0].account_prefix, Some("Orange".to_string()));
        assert_eq!(q.accounts[1].bank_prefix, "St George");
        assert_eq!(q.accounts[1].account_prefix, Some("Savings".to_string()));
    }

    #[test]
    fn test_parse_category_multiple() {
        let (q, _) = DbSearchQuery::parse("category:Food|Transport");
        assert_eq!(q.categories, vec!["Food", "Transport"]);
    }

    #[test]
    fn test_parse_shortcuts() {
        // d: for date
        let (q, _) = DbSearchQuery::parse("d:2024-01");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap()));

        // a: for account
        let (q, _) = DbSearchQuery::parse("a:ING/Orange");
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "ING");
        assert_eq!(q.accounts[0].account_prefix, Some("Orange".to_string()));

        // c: for category
        let (q, _) = DbSearchQuery::parse("c:Food|Transport");
        assert_eq!(q.categories, vec!["Food", "Transport"]);

        // Combined shortcuts
        let (q, _) = DbSearchQuery::parse("d:2024 a:Chase c:Food coffee");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert_eq!(q.accounts[0].bank_prefix, "Chase");
        assert_eq!(q.categories, vec!["Food"]);
        assert!(matches!(q.text_match, DbTextMatch::Substring(ref s) if s == "coffee"));
    }

    #[test]
    fn test_to_filter() {
        let (q, _) = DbSearchQuery::parse("date:2024-01 amount:>100 account:Chase/ groceries");
        let filter = q.to_filter(Some(500));

        assert_eq!(filter.from_date, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert_eq!(filter.to_date, Some(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap()));
        assert_eq!(filter.amount_min, Some(10000));
        assert_eq!(filter.account_patterns.len(), 1);
        assert_eq!(filter.account_patterns[0].bank_prefix, "Chase");
        assert_eq!(filter.description_contains, Some("groceries".to_string()));
        assert_eq!(filter.limit, Some(500));
    }

    #[test]
    fn test_to_filter_regex() {
        let (q, _) = DbSearchQuery::parse("/coffee.*/i");
        let filter = q.to_filter(None);

        assert!(filter.description_contains.is_none());
        assert_eq!(filter.description_regex, Some("(?i)coffee.*".to_string()));
    }
}
