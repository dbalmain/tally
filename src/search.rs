use chrono::{Datelike, NaiveDate};
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher, Utf32Str,
};
use regex::Regex;

/// How to match text in descriptions
#[derive(Debug, Clone)]
pub enum TextMatch {
    /// Case-insensitive substring match (default)
    Exact(String),
    /// Fuzzy match using nucleo (~pattern)
    Fuzzy(String),
    /// Regex match (/pattern/ or /pattern/i)
    Regex { pattern: Regex, original: String },
}

impl Default for TextMatch {
    fn default() -> Self {
        TextMatch::Exact(String::new())
    }
}

impl TextMatch {
    /// Check if the text match pattern is empty.
    pub fn is_empty(&self) -> bool {
        match self {
            TextMatch::Exact(s) => s.is_empty(),
            TextMatch::Fuzzy(s) => s.is_empty(),
            TextMatch::Regex { original, .. } => original.is_empty(),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct SearchQuery {
    pub date_from: Option<NaiveDate>,
    pub date_to: Option<NaiveDate>,
    pub amount_min: Option<i64>,
    pub amount_max: Option<i64>,
    pub bank: Option<String>,
    pub account: Option<String>,
    pub category: Option<String>,
    pub text_match: TextMatch,
}

impl SearchQuery {
    /// Parse a search query string with support for filters and text matching.
    pub fn parse(input: &str) -> Self {
        let mut query = SearchQuery::default();
        let mut text_parts = Vec::new();

        for token in tokenize(input) {
            if let Some(rest) = token.strip_prefix("date:") {
                parse_date_range(rest, &mut query);
            } else if let Some(rest) = token.strip_prefix("amount:") {
                parse_amount_range(rest, &mut query);
            } else if let Some(rest) = token.strip_prefix("bank:") {
                query.bank = Some(rest.to_string());
            } else if let Some(rest) = token.strip_prefix("account:") {
                query.account = Some(rest.to_string());
            } else if let Some(rest) = token.strip_prefix("category:") {
                query.category = Some(rest.to_string());
            } else {
                text_parts.push(token);
            }
        }

        let text = text_parts.join(" ");
        query.text_match = parse_text_match(&text);
        query
    }

    /// Check if all query fields are empty.
    pub fn is_empty(&self) -> bool {
        self.date_from.is_none()
            && self.date_to.is_none()
            && self.amount_min.is_none()
            && self.amount_max.is_none()
            && self.bank.is_none()
            && self.account.is_none()
            && self.category.is_none()
            && self.text_match.is_empty()
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

fn parse_text_match(text: &str) -> TextMatch {
    if text.is_empty() {
        return TextMatch::Exact(String::new());
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

        if let Ok(regex) = Regex::new(&regex_pattern) {
            return TextMatch::Regex {
                pattern: regex,
                original: text.to_string(),
            };
        }
    }

    // Check for fuzzy: ~pattern
    if let Some(rest) = text.strip_prefix('~') {
        return TextMatch::Fuzzy(rest.to_string());
    }

    // Default: exact case-insensitive substring match (pre-lowercase for efficiency)
    TextMatch::Exact(text.to_lowercase())
}

fn parse_date_range(s: &str, query: &mut SearchQuery) {
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

fn parse_amount_range(s: &str, query: &mut SearchQuery) {
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

    /// Match haystack against the given TextMatch.
    pub fn matches(&mut self, text_match: &TextMatch, haystack: &str) -> bool {
        match text_match {
            TextMatch::Exact(pattern) => {
                if pattern.is_empty() {
                    return true;
                }
                // Pattern is already lowercased at parse time
                haystack.to_lowercase().contains(pattern)
            }
            TextMatch::Fuzzy(pattern) => self.fuzzy_matches(pattern, haystack),
            TextMatch::Regex { pattern, .. } => pattern.is_match(haystack),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_exact() {
        let q = SearchQuery::parse("coffee shop");
        assert!(matches!(q.text_match, TextMatch::Exact(ref s) if s == "coffee shop"));
        assert!(q.date_from.is_none());
    }

    #[test]
    fn test_parse_fuzzy() {
        let q = SearchQuery::parse("~cofshp");
        assert!(matches!(q.text_match, TextMatch::Fuzzy(ref s) if s == "cofshp"));
    }

    #[test]
    fn test_parse_regex() {
        let q = SearchQuery::parse("/cof.*shop/i");
        assert!(matches!(q.text_match, TextMatch::Regex { .. }));
    }

    #[test]
    fn test_parse_date_range() {
        let q = SearchQuery::parse("date:2024-01..2024-06");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        // End of June
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 6, 30).unwrap()));
    }

    #[test]
    fn test_parse_date_single_full() {
        let q = SearchQuery::parse("date:2024-03-15");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()));
    }

    #[test]
    fn test_parse_date_month() {
        // "date:2025-09" should show whole month
        let q = SearchQuery::parse("date:2025-09");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2025, 9, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2025, 9, 30).unwrap()));
    }

    #[test]
    fn test_parse_date_february_leap_year() {
        let q = SearchQuery::parse("date:2024-02");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 2, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()));
    }

    #[test]
    fn test_parse_date_year_only() {
        let q = SearchQuery::parse("date:2024");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap()));
    }

    #[test]
    fn test_parse_amount_range() {
        let q = SearchQuery::parse("amount:50..200");
        assert_eq!(q.amount_min, Some(5000));
        assert_eq!(q.amount_max, Some(20000));
    }

    #[test]
    fn test_parse_amount_greater() {
        let q = SearchQuery::parse("amount:>100");
        assert_eq!(q.amount_min, Some(10000));
        assert!(q.amount_max.is_none());
    }

    #[test]
    fn test_parse_negative_amount() {
        let q = SearchQuery::parse("amount:-50");
        assert_eq!(q.amount_min, Some(-5000));
        assert_eq!(q.amount_max, Some(-5000));
    }

    #[test]
    fn test_parse_combined() {
        let q = SearchQuery::parse("date:2024-01 amount:>100 bank:Chase groceries");
        assert_eq!(q.date_from, Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert_eq!(q.date_to, Some(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap()));
        assert_eq!(q.amount_min, Some(10000));
        assert_eq!(q.bank, Some("Chase".to_string()));
        assert!(matches!(q.text_match, TextMatch::Exact(ref s) if s == "groceries"));
    }

    #[test]
    fn test_matcher_exact() {
        let mut m = FuzzyMatcher::new();
        let exact = TextMatch::Exact("cityside".to_string());
        assert!(m.matches(&exact, "CITYSIDE BANK"));
        assert!(m.matches(&exact, "cityside"));
        assert!(!m.matches(&exact, "city side")); // not a substring match
    }

    #[test]
    fn test_matcher_fuzzy() {
        let mut m = FuzzyMatcher::new();
        let fuzzy = TextMatch::Fuzzy("ctysd".to_string());
        assert!(m.matches(&fuzzy, "CITYSIDE BANK"));
        assert!(m.matches(&fuzzy, "cityside"));
    }

    #[test]
    fn test_matcher_regex() {
        let mut m = FuzzyMatcher::new();
        let regex = TextMatch::Regex {
            pattern: Regex::new("(?i)ci\\w+de").unwrap(),
            original: "/ci\\w+de/i".to_string(),
        };
        assert!(m.matches(&regex, "Cityside"));
        assert!(m.matches(&regex, "CITYSIDE"));
        assert!(!m.matches(&regex, "city"));
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
        let q = SearchQuery::parse(r#"account:"Orange Everyday""#);
        assert_eq!(q.account, Some("Orange Everyday".to_string()));
    }

    #[test]
    fn test_parse_account_backslash() {
        let q = SearchQuery::parse(r"account:Orange\ Everyday");
        assert_eq!(q.account, Some("Orange Everyday".to_string()));
    }

    #[test]
    fn test_parse_bank_quoted_with_text() {
        let q = SearchQuery::parse(r#"bank:"My Bank" coffee"#);
        assert_eq!(q.bank, Some("My Bank".to_string()));
        assert!(matches!(q.text_match, TextMatch::Exact(ref s) if s == "coffee"));
    }
}
