use chrono::{Datelike, NaiveDate};
use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::{AccountPattern, TransactionFilter};

/// Regex match for DB search
#[derive(Debug, Clone, Default)]
pub struct DbRegexMatch {
    /// The regex pattern string (with (?i) prefix if case-insensitive)
    pub pattern: String,
    /// The original input (e.g., "/pattern/i")
    pub original: String,
}

/// FTS match for DB search
#[derive(Debug, Clone, Default)]
pub struct DbFtsMatch {
    /// The FTS5 query string (translated from user input)
    pub query: String,
    /// The original user input
    pub original: String,
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
    /// Regex match (optional, can coexist with FTS)
    pub regex_match: Option<DbRegexMatch>,
    /// FTS match (optional, can coexist with regex)
    pub fts_match: Option<DbFtsMatch>,
}

impl DbSearchQuery {
    /// Parse a search query string with support for filters and text matching.
    pub fn parse(input: &str) -> Self {
        Self::parse_with_cursor(input, None)
    }

    /// Parse with cursor position for implicit prefix matching.
    /// When cursor is within a text term, adds * at that position for prefix matching.
    pub fn parse_with_cursor(input: &str, cursor: Option<usize>) -> Self {
        let mut query = DbSearchQuery::default();
        let mut text_parts: Vec<String> = Vec::new();
        let mut regex_token: Option<String> = None;
        // Track (start, end) positions of text tokens in original input
        let mut text_token_ranges: Vec<(usize, usize)> = Vec::new();

        for token_info in tokenize_with_positions(input) {
            let token = token_info.token;
            if let Some(rest) = token
                .strip_prefix("date:")
                .or_else(|| token.strip_prefix("d:"))
            {
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
            } else if token.starts_with('/') && regex_token.is_none() {
                // Regex token: /pattern/ or /pattern/i
                regex_token = Some(token);
            } else {
                text_parts.push(token);
                text_token_ranges.push((token_info.start_pos, token_info.end_pos));
            }
        }

        // Parse regex if present
        if let Some(ref regex_str) = regex_token {
            query.regex_match = parse_regex_match(regex_str);
        }

        // Parse FTS from remaining text parts
        let text = text_parts.join(" ");
        if !text.is_empty() {
            // Find if cursor is within any text token, map to position in joined text
            let cursor_in_text = cursor.and_then(|c| {
                text_token_ranges
                    .iter()
                    .enumerate()
                    .find(|(_, (start, end))| c >= *start && c <= *end)
                    .map(|(idx, (start, _))| {
                        // Calculate position in joined text:
                        // sum of previous token lengths + spaces + offset within current token
                        let prev_len: usize = text_parts[..idx]
                            .iter()
                            .map(|s| s.chars().count())
                            .sum();
                        let spaces = idx; // spaces between tokens
                        let offset_in_token = c - start;
                        prev_len + spaces + offset_in_token
                    })
            });

            let fts_query = parse_fts_query(&text, cursor_in_text);
            query.fts_match = Some(DbFtsMatch {
                query: fts_query,
                original: text,
            });
        }

        query
    }

    /// Check if all query fields are empty.
    pub fn is_empty(&self) -> bool {
        self.date_from.is_none()
            && self.date_to.is_none()
            && self.amount_min.is_none()
            && self.amount_max.is_none()
            && self.accounts.is_empty()
            && self.categories.is_empty()
            && self.regex_match.is_none()
            && self.fts_match.is_none()
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
            fts_query: self.fts_match.as_ref().map(|f| f.query.clone()),
            description_regex: self.regex_match.as_ref().map(|r| r.pattern.clone()),
            limit,
            ..Default::default()
        }
    }
}

/// Token with its start and end positions (character indices) in the original input.
struct TokenWithPos {
    token: String,
    start_pos: usize,
    end_pos: usize,
}

/// Tokenize input handling quoted strings, backslash escapes, and regex literals.
/// Returns tokens with their positions for cursor tracking.
///
/// Regex tokens start with `/` at a word boundary and consume until closing `/` (with optional flags),
/// allowing unescaped spaces within the regex. Use `\/` to include a literal `/` in the pattern.
fn tokenize_with_positions(input: &str) -> Vec<TokenWithPos> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_start = 0;
    let mut current_end = 0;
    let mut chars = input.chars().peekable();
    let mut in_quotes = false;
    let mut in_regex = false;
    let mut regex_closed = false; // After closing /, consuming flags
    let mut pos = 0;

    while let Some(c) = chars.next() {
        match c {
            '/' if !in_quotes && !in_regex && current.is_empty() => {
                // Start of regex token (at word boundary since current is empty)
                current_start = pos;
                current.push(c);
                pos += 1;
                current_end = pos;
                in_regex = true;
                regex_closed = false;
            }
            '\\' if in_regex && !regex_closed => {
                // Backslash inside regex - consume it and the next char literally
                current.push(c);
                pos += 1;
                if let Some(next) = chars.next() {
                    current.push(next);
                    pos += 1;
                }
                current_end = pos;
            }
            '/' if in_regex && !regex_closed => {
                // Closing slash of regex (not escaped, handled above)
                current.push(c);
                pos += 1;
                current_end = pos;
                regex_closed = true;
            }
            c if in_regex && regex_closed => {
                // After closing /, only consume valid flag chars (i, g, m, s, etc.)
                if c.is_ascii_alphabetic() {
                    current.push(c);
                    pos += 1;
                    current_end = pos;
                } else {
                    // End of regex token, push it and handle this char
                    tokens.push(TokenWithPos {
                        token: std::mem::take(&mut current),
                        start_pos: current_start,
                        end_pos: current_end,
                    });
                    in_regex = false;
                    regex_closed = false;

                    // Handle the current char (space ends, otherwise start new token)
                    if c == ' ' || c == '\t' {
                        pos += 1;
                        current_start = pos;
                    } else {
                        current_start = pos;
                        current.push(c);
                        pos += 1;
                        current_end = pos;
                    }
                }
            }
            _ if in_regex => {
                // Inside regex (before closing /), consume everything including spaces
                current.push(c);
                pos += 1;
                current_end = pos;
            }
            '\\' if !in_quotes => {
                // Backslash escape: consume next char literally
                if let Some(next) = chars.next() {
                    if current.is_empty() {
                        current_start = pos;
                    }
                    current.push(next);
                    pos += 2;
                    current_end = pos;
                } else {
                    pos += 1;
                }
            }
            '"' => {
                in_quotes = !in_quotes;
                pos += 1;
                // Update end if we're building a token
                if !current.is_empty() {
                    current_end = pos;
                }
            }
            ' ' | '\t' if !in_quotes => {
                // End of token
                if !current.is_empty() {
                    tokens.push(TokenWithPos {
                        token: std::mem::take(&mut current),
                        start_pos: current_start,
                        end_pos: current_end,
                    });
                }
                pos += 1;
                current_start = pos;
            }
            _ => {
                if current.is_empty() {
                    current_start = pos;
                }
                current.push(c);
                pos += 1;
                current_end = pos;
            }
        }
    }

    // Don't forget the last token
    if !current.is_empty() {
        tokens.push(TokenWithPos {
            token: current,
            start_pos: current_start,
            end_pos: current_end,
        });
    }

    tokens
}

/// Parse a regex token like /pattern/ or /pattern/i into DbRegexMatch.
/// Returns None for empty patterns (e.g., `//` or `//i`).
fn parse_regex_match(text: &str) -> Option<DbRegexMatch> {
    // Check for regex: /pattern/ or /pattern/i
    text.strip_prefix('/').and_then(|rest| {
        rest.rfind('/').and_then(|end_slash| {
            let pattern = &rest[..end_slash];

            // Ignore empty patterns
            if pattern.is_empty() {
                return None;
            }

            let flags = &rest[end_slash + 1..];
            let case_insensitive = flags.contains('i');

            let regex_pattern = if case_insensitive {
                format!("(?i){}", pattern)
            } else {
                pattern.to_string()
            };

            Some(DbRegexMatch {
                pattern: regex_pattern,
                original: text.to_string(),
            })
        })
    })
}

/// Parse user FTS input into FTS5 query syntax.
///
/// Passthrough to FTS5 with minimal modification:
/// - `term1 term2` → AND (implicit, FTS5 default)
/// - `term*` → prefix match
/// - `"phrase"` → exact phrase
/// - `foo OR bar` → native FTS5 OR syntax
/// - `(group1) OR (group2)` → grouping with OR
///
/// Modifications:
/// - If `cursor_pos` is Some and points to end of a term, inserts `*` for live prefix matching
/// - Auto-balances unclosed parentheses to prevent query errors
fn parse_fts_query(input: &str, cursor_pos: Option<usize>) -> String {
    let mut result = String::new();
    let mut in_quotes = false;
    let mut open_parens = 0;

    for (i, c) in input.chars().enumerate() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                result.push(c);
            }
            '(' if !in_quotes => {
                open_parens += 1;
                result.push(c);
            }
            ')' if !in_quotes => {
                if open_parens > 0 {
                    open_parens -= 1;
                }
                result.push(c);
            }
            _ => {
                result.push(c);
            }
        }

        // Check if we should insert * after this character
        if let Some(cursor) = cursor_pos {
            if i + 1 == cursor && !in_quotes {
                // Cursor is right after this character
                let next_char = input.chars().nth(i + 1);
                let at_word_boundary =
                    next_char.map_or(true, |nc| nc.is_whitespace() || nc == ')');
                if at_word_boundary && c.is_alphanumeric() {
                    result.push('*');
                }
            }
        }
    }

    // Auto-balance unclosed parentheses
    for _ in 0..open_parens {
        result.push(')');
    }

    result
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
    fn test_parse_simple_fts() {
        let q = DbSearchQuery::parse("coffee shop");
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee shop");
        assert!(q.date_from.is_none());
    }

    #[test]
    fn test_parse_regex() {
        let q = DbSearchQuery::parse("/cof.*shop/i");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, "(?i)cof.*shop");
        assert!(q.fts_match.is_none());
    }

    #[test]
    fn test_parse_regex_case_sensitive() {
        let q = DbSearchQuery::parse("/Coffee/");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, "Coffee");
        assert!(q.fts_match.is_none());
    }

    #[test]
    fn test_parse_regex_with_fts() {
        // Regex and FTS can coexist
        let q = DbSearchQuery::parse("/pattern/i groceries");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, "(?i)pattern");
        assert_eq!(q.fts_match.as_ref().unwrap().query, "groceries");

        // Filter converts both
        let filter = q.to_filter(None);
        assert_eq!(filter.description_regex, Some("(?i)pattern".to_string()));
        assert_eq!(filter.fts_query, Some("groceries".to_string()));
    }

    #[test]
    fn test_parse_regex_with_spaces() {
        // Regex can contain unescaped spaces
        let q = DbSearchQuery::parse("/coffee shop/i");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, "(?i)coffee shop");
        assert!(q.fts_match.is_none());

        // Regex with spaces followed by FTS
        let q = DbSearchQuery::parse("/coffee shop/i groceries");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, "(?i)coffee shop");
        assert_eq!(q.fts_match.as_ref().unwrap().query, "groceries");

        // Regex with spaces followed by filter
        let q = DbSearchQuery::parse("/coffee shop/ date:2024");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, "coffee shop");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
    }

    #[test]
    fn test_parse_empty_regex_ignored() {
        // Empty regex is ignored
        let q = DbSearchQuery::parse("//");
        assert!(q.regex_match.is_none());

        // Empty regex with flag is ignored
        let q = DbSearchQuery::parse("//i");
        assert!(q.regex_match.is_none());

        // Empty regex with FTS still parses FTS
        let q = DbSearchQuery::parse("// groceries");
        assert!(q.regex_match.is_none());
        assert_eq!(q.fts_match.as_ref().unwrap().query, "groceries");
    }

    #[test]
    fn test_parse_regex_escaped_slash() {
        // Escaped slash inside regex
        let q = DbSearchQuery::parse(r"/foo\/bar/");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, r"foo\/bar");

        // Escaped slash with spaces
        let q = DbSearchQuery::parse(r"/path\/to\/file/i");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, r"(?i)path\/to\/file");

        // Escaped slash followed by FTS
        let q = DbSearchQuery::parse(r"/a\/b/ coffee");
        assert_eq!(q.regex_match.as_ref().unwrap().pattern, r"a\/b");
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee");
    }

    #[test]
    fn test_parse_date_range() {
        let q = DbSearchQuery::parse("date:2024-01..2024-06");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
        assert_eq!(
            q.date_to,
            Some(NaiveDate::from_ymd_opt(2024, 6, 30).unwrap())
        );
    }

    #[test]
    fn test_parse_date_single_full() {
        let q = DbSearchQuery::parse("date:2024-03-15");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap())
        );
        assert_eq!(
            q.date_to,
            Some(NaiveDate::from_ymd_opt(2024, 3, 15).unwrap())
        );
    }

    #[test]
    fn test_parse_date_month() {
        let q = DbSearchQuery::parse("date:2025-09");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2025, 9, 1).unwrap())
        );
        assert_eq!(
            q.date_to,
            Some(NaiveDate::from_ymd_opt(2025, 9, 30).unwrap())
        );
    }

    #[test]
    fn test_parse_date_february_leap_year() {
        let q = DbSearchQuery::parse("date:2024-02");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2024, 2, 1).unwrap())
        );
        assert_eq!(
            q.date_to,
            Some(NaiveDate::from_ymd_opt(2024, 2, 29).unwrap())
        );
    }

    #[test]
    fn test_parse_date_year_only() {
        let q = DbSearchQuery::parse("date:2024");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
        assert_eq!(
            q.date_to,
            Some(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap())
        );
    }

    #[test]
    fn test_parse_amount_range() {
        let q = DbSearchQuery::parse("amount:50..200");
        assert_eq!(q.amount_min, Some(5000));
        assert_eq!(q.amount_max, Some(20000));
    }

    #[test]
    fn test_parse_amount_greater() {
        let q = DbSearchQuery::parse("amount:>100");
        assert_eq!(q.amount_min, Some(10000));
        assert!(q.amount_max.is_none());
    }

    #[test]
    fn test_parse_negative_amount() {
        let q = DbSearchQuery::parse("amount:-50");
        assert_eq!(q.amount_min, Some(-5000));
        assert_eq!(q.amount_max, Some(-5000));
    }

    #[test]
    fn test_parse_combined() {
        let q = DbSearchQuery::parse("date:2024-01 amount:>100 account:Chase/ groceries");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
        assert_eq!(
            q.date_to,
            Some(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap())
        );
        assert_eq!(q.amount_min, Some(10000));
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "Chase");
        assert_eq!(q.accounts[0].account_prefix, Some("".to_string()));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "groceries");
    }

    #[test]
    fn test_fuzzy_matcher() {
        let mut m = FuzzyMatcher::new();
        assert!(m.fuzzy_matches("ctysd", "CITYSIDE BANK"));
        assert!(m.fuzzy_matches("ctysd", "cityside"));
        assert!(m.fuzzy_matches("", "anything"));
        assert!(!m.fuzzy_matches("xyz", "cityside"));
    }

    fn tokenize(input: &str) -> Vec<String> {
        tokenize_with_positions(input)
            .into_iter()
            .map(|t| t.token)
            .collect()
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
        let q = DbSearchQuery::parse(r#"account:"ING/Orange Everyday""#);
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "ING");
        assert_eq!(
            q.accounts[0].account_prefix,
            Some("Orange Everyday".to_string())
        );
    }

    #[test]
    fn test_parse_account_backslash() {
        let q = DbSearchQuery::parse(r"account:ING/Orange\ Everyday");
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "ING");
        assert_eq!(
            q.accounts[0].account_prefix,
            Some("Orange Everyday".to_string())
        );
    }

    #[test]
    fn test_parse_account_bank_only() {
        let q = DbSearchQuery::parse("account:St coffee");
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "St");
        assert_eq!(q.accounts[0].account_prefix, None);
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee");
    }

    #[test]
    fn test_parse_account_multiple() {
        let q = DbSearchQuery::parse(r#"account:"ING/Orange"|"St George/Savings""#);
        assert_eq!(q.accounts.len(), 2);
        assert_eq!(q.accounts[0].bank_prefix, "ING");
        assert_eq!(q.accounts[0].account_prefix, Some("Orange".to_string()));
        assert_eq!(q.accounts[1].bank_prefix, "St George");
        assert_eq!(q.accounts[1].account_prefix, Some("Savings".to_string()));
    }

    #[test]
    fn test_parse_category_multiple() {
        let q = DbSearchQuery::parse("category:Food|Transport");
        assert_eq!(q.categories, vec!["Food", "Transport"]);
    }

    #[test]
    fn test_parse_shortcuts() {
        // d: for date
        let q = DbSearchQuery::parse("d:2024-01");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
        assert_eq!(
            q.date_to,
            Some(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap())
        );

        // a: for account
        let q = DbSearchQuery::parse("a:ING/Orange");
        assert_eq!(q.accounts.len(), 1);
        assert_eq!(q.accounts[0].bank_prefix, "ING");
        assert_eq!(q.accounts[0].account_prefix, Some("Orange".to_string()));

        // c: for category
        let q = DbSearchQuery::parse("c:Food|Transport");
        assert_eq!(q.categories, vec!["Food", "Transport"]);

        // Combined shortcuts
        let q = DbSearchQuery::parse("d:2024 a:Chase c:Food coffee");
        assert_eq!(
            q.date_from,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
        assert_eq!(q.accounts[0].bank_prefix, "Chase");
        assert_eq!(q.categories, vec!["Food"]);
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee");
    }

    #[test]
    fn test_to_filter() {
        let q = DbSearchQuery::parse("date:2024-01 amount:>100 account:Chase/ groceries");
        let filter = q.to_filter(Some(500));

        assert_eq!(
            filter.from_date,
            Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
        assert_eq!(
            filter.to_date,
            Some(NaiveDate::from_ymd_opt(2024, 1, 31).unwrap())
        );
        assert_eq!(filter.amount_min, Some(10000));
        assert_eq!(filter.account_patterns.len(), 1);
        assert_eq!(filter.account_patterns[0].bank_prefix, "Chase");
        assert_eq!(filter.fts_query, Some("groceries".to_string()));
        assert_eq!(filter.limit, Some(500));
    }

    #[test]
    fn test_to_filter_regex() {
        let q = DbSearchQuery::parse("/coffee.*/i");
        let filter = q.to_filter(None);

        assert!(filter.fts_query.is_none());
        assert_eq!(filter.description_regex, Some("(?i)coffee.*".to_string()));
    }

    #[test]
    fn test_parse_fts_query() {
        // Simple terms (implicit AND) - no cursor
        assert_eq!(parse_fts_query("AAMI mar", None), "AAMI mar");

        // Prefix match
        assert_eq!(parse_fts_query("mar*", None), "mar*");

        // Phrase match
        assert_eq!(parse_fts_query("\"AAMI mar\"", None), "\"AAMI mar\"");

        // Native FTS5 OR syntax (passthrough)
        assert_eq!(parse_fts_query("foo OR bar", None), "foo OR bar");
        assert_eq!(
            parse_fts_query("(foo bar) OR (baz qux)", None),
            "(foo bar) OR (baz qux)"
        );

        // Pipe is passed through (not translated)
        assert_eq!(parse_fts_query("(foo|bar)", None), "(foo|bar)");
        assert_eq!(parse_fts_query("foo|bar", None), "foo|bar");

        // Quoted phrase with pipe preserved
        assert_eq!(parse_fts_query("\"foo|bar\"", None), "\"foo|bar\"");
    }

    #[test]
    fn test_parse_fts_query_auto_balance_parens() {
        // Unclosed paren gets auto-closed
        assert_eq!(parse_fts_query("(foo", None), "(foo)");
        assert_eq!(parse_fts_query("(foo bar", None), "(foo bar)");
        assert_eq!(parse_fts_query("((nested", None), "((nested))");

        // Already balanced - no change
        assert_eq!(parse_fts_query("(foo)", None), "(foo)");
        assert_eq!(parse_fts_query("(a) (b)", None), "(a) (b)");

        // Parens in quotes don't count
        assert_eq!(parse_fts_query("\"(quoted\"", None), "\"(quoted\"");
    }

    #[test]
    fn test_parse_fts_query_implicit_prefix() {
        // Cursor at end of last term -> adds *
        assert_eq!(parse_fts_query("coffee", Some(6)), "coffee*");
        assert_eq!(parse_fts_query("coffee shop", Some(11)), "coffee shop*");

        // Cursor at end of first term -> adds * there
        assert_eq!(parse_fts_query("aam oct", Some(3)), "aam* oct");

        // No prefix after explicit *, quote, or paren
        assert_eq!(parse_fts_query("coffee*", Some(7)), "coffee*");
        assert_eq!(parse_fts_query("\"coffee\"", Some(8)), "\"coffee\"");

        // Cursor not at word boundary -> no prefix
        assert_eq!(parse_fts_query("coffee", Some(3)), "coffee");

        // Close-paren is treated as word boundary for prefix matching
        // Cursor at position 4 is after "Tree", before ")"
        assert_eq!(parse_fts_query("(Tree)", Some(5)), "(Tree*)");
        assert_eq!(parse_fts_query("(foo bar)", Some(8)), "(foo bar*)");

        // Cursor inside unclosed paren group - still gets prefix and auto-close
        assert_eq!(parse_fts_query("(Tree", Some(5)), "(Tree*)");
    }

    #[test]
    fn test_parse_with_cursor_implicit_prefix() {
        // Cursor at end of term -> adds prefix
        let q = DbSearchQuery::parse_with_cursor("coffee", Some(6));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee*");

        // Cursor at end of first term in multi-word -> adds prefix there
        let q = DbSearchQuery::parse_with_cursor("aam oct", Some(3));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "aam* oct");

        // Cursor in middle of term -> no prefix
        let q = DbSearchQuery::parse_with_cursor("coffee", Some(3));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee");

        // Cursor after space (not at end of term) -> no prefix
        let q = DbSearchQuery::parse_with_cursor("coffee ", Some(7));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee");

        // No cursor -> no prefix (programmatic use)
        let q = DbSearchQuery::parse("coffee");
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee");

        // With filter, cursor at end of text term
        let q = DbSearchQuery::parse_with_cursor("d:2024 coffee", Some(13));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "coffee*");

        // With filter, cursor at end of first text term
        let q = DbSearchQuery::parse_with_cursor("d:2024 aam oct", Some(10));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "aam* oct");

        // Cursor within token before close-paren: (Treehous_) where _ is cursor at pos 9
        // (=0, T=1, r=2, e=3, e=4, h=5, o=6, u=7, s=8, )=9
        let q = DbSearchQuery::parse_with_cursor("(Treehous)", Some(9));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "(Treehous*)");

        // Cursor within token in middle of word
        let q = DbSearchQuery::parse_with_cursor("(Tree)", Some(3));
        assert_eq!(q.fts_match.as_ref().unwrap().query, "(Tree)");
    }
}
