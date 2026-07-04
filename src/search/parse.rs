//! Parser that combines tokenization with filter dispatch.
//!
//! Takes raw input and cursor position, returns a ParsedQuery with:
//! - Validated filter parts with SQL
//! - Regex parts with compiled patterns
//! - FTS parts ready for SQLite FTS5
//! - Cursor context for key handling

use regex::Regex;

#[cfg(test)]
use super::FilterResult;
use super::{CursorContext, Filter, ParsedQuery, QueryPart, RawToken, Span, tokenize};

/// Configuration for parsing search queries.
pub struct SearchConfig {
    /// Available filters for this search context.
    filters: Vec<Box<dyn Filter>>,
}

impl SearchConfig {
    pub fn new(filters: Vec<Box<dyn Filter>>) -> Self {
        Self { filters }
    }

    /// The standard filter set used by every transaction-oriented search:
    /// date, amount, account, and (optionally) category. This is the single
    /// registration point for built-in filters — new filters get added here
    /// and every search bar picks them up.
    ///
    /// `category_options` is `None` for contexts where a category filter is
    /// meaningless (e.g. lists of uncategorised transactions).
    pub fn standard(account_options: Vec<String>, category_options: Option<Vec<String>>) -> Self {
        use super::filters::{AccountFilter, AmountFilter, CategoryFilter, DateFilter};

        let mut filters: Vec<Box<dyn Filter>> = vec![
            Box::new(DateFilter),
            Box::new(AmountFilter),
            Box::new(AccountFilter::new(account_options)),
        ];
        if let Some(options) = category_options {
            filters.push(Box::new(CategoryFilter::new(options)));
        }
        Self::new(filters)
    }

    /// Find a filter by name or alias (internal use).
    fn find_filter(&self, name: &str) -> Option<&dyn Filter> {
        self.filters.iter().find_map(|f| {
            if f.name() == name || f.alias() == Some(name) {
                Some(f.as_ref())
            } else {
                None
            }
        })
    }

    /// Find a filter by its canonical name (for autocomplete).
    pub fn find_filter_by_name(&self, name: &str) -> Option<&dyn Filter> {
        self.filters.iter().find_map(|f| {
            if f.name() == name {
                Some(f.as_ref())
            } else {
                None
            }
        })
    }

    /// Resolve a word to a canonical filter name.
    ///
    /// Checks both canonical names and aliases.
    /// Returns the canonical name if found.
    pub fn resolve_filter_name(&self, word: &str) -> Option<&'static str> {
        self.find_filter(word).map(|f| f.name())
    }
}

/// Parse search input with cursor position.
///
/// Returns the parsed query and cursor context for key handling.
pub fn parse(config: &SearchConfig, input: &str, cursor: usize) -> (ParsedQuery, CursorContext) {
    let raw_tokens = tokenize(input);
    let mut parts = Vec::new();
    let mut cursor_context = CursorContext::Whitespace;

    // Collect FTS tokens for combining
    let mut fts_tokens: Vec<(String, Span)> = Vec::new();
    // Whitespace between FTS tokens is deferred: the combined FTS span covers it,
    // so adding it to parts would cause overlap and wrong ordering.
    let mut pending_whitespace: Option<Span> = None;

    for token in &raw_tokens {
        match token {
            RawToken::Filter {
                name,
                value,
                span,
                value_span,
                negated,
            } => {
                flush_fts(&mut fts_tokens, cursor, &mut parts, &mut cursor_context);
                if let Some(ws_span) = pending_whitespace.take() {
                    parts.push(QueryPart::Whitespace { span: ws_span });
                }

                if let Some(filter) = config.find_filter(name) {
                    let canonical = filter.name();
                    let result = filter.parse(value);

                    // Check if cursor is in this filter's value
                    if cursor >= value_span.start && cursor <= value_span.end {
                        cursor_context = CursorContext::Filter {
                            name: canonical,
                            offset: cursor - value_span.start,
                        };
                    }

                    parts.push(QueryPart::Filter {
                        name: canonical,
                        value: value.clone(),
                        result,
                        span: *span,
                        value_span: *value_span,
                        negated: *negated,
                    });
                } else {
                    // Unknown filter — treat as FTS text. A negated unknown
                    // filter behaves like a negated FTS term over the whole
                    // `name:value` string.
                    let text = format!("{}:{}", name, value);
                    if *negated {
                        push_negated_fts(&text, *span, cursor, &mut parts, &mut cursor_context);
                    } else {
                        fts_tokens.push((text, *span));
                    }
                }
            }

            RawToken::Regex {
                original,
                pattern,
                flags,
                complete,
                span,
                negated,
            } => {
                flush_fts(&mut fts_tokens, cursor, &mut parts, &mut cursor_context);
                if let Some(ws_span) = pending_whitespace.take() {
                    parts.push(QueryPart::Whitespace { span: ws_span });
                }

                // Build regex pattern with flags
                let mut compiled_pattern = String::new();
                if flags.contains('i') {
                    compiled_pattern.push_str("(?i)");
                }
                compiled_pattern.push_str(pattern);

                // Validate regex
                let valid = *complete && Regex::new(&compiled_pattern).is_ok();

                // Check if cursor is in this regex
                if span.contains(cursor) || span.at_end(cursor) {
                    cursor_context = CursorContext::Regex {
                        offset: cursor - span.start,
                    };
                }

                parts.push(QueryPart::Regex {
                    original: original.clone(),
                    pattern: compiled_pattern,
                    valid,
                    span: *span,
                    negated: *negated,
                });
            }

            RawToken::Fts {
                text,
                span,
                negated,
            } => {
                if *negated {
                    // A negated FTS token is its own standalone part: flush any
                    // pending positive FTS run first so `coffee -tea shop` keeps
                    // "coffee" and "shop" separate from the negated "tea".
                    flush_fts(&mut fts_tokens, cursor, &mut parts, &mut cursor_context);
                    if let Some(ws_span) = pending_whitespace.take() {
                        parts.push(QueryPart::Whitespace { span: ws_span });
                    }
                    push_negated_fts(text, *span, cursor, &mut parts, &mut cursor_context);
                } else {
                    // Clear pending whitespace — the combined FTS span will cover it
                    pending_whitespace = None;
                    fts_tokens.push((text.clone(), *span));
                }
            }

            RawToken::Whitespace { span } => {
                // Don't set cursor_context here: it defaults to Whitespace,
                // and at boundaries (e.g., cursor at filter value end == whitespace start)
                // the preceding token should win.
                if fts_tokens.is_empty() {
                    // No pending FTS — safe to add directly
                    parts.push(QueryPart::Whitespace { span: *span });
                } else {
                    // Between FTS tokens — defer so we don't overlap with combined FTS span
                    pending_whitespace = Some(*span);
                }
            }
        }
    }

    flush_fts(&mut fts_tokens, cursor, &mut parts, &mut cursor_context);

    // Flush trailing whitespace (e.g., "Salary ")
    if let Some(ws_span) = pending_whitespace.take() {
        parts.push(QueryPart::Whitespace { span: ws_span });
    }

    (ParsedQuery { parts }, cursor_context)
}

/// Emit a single negated FTS token as its own standalone `QueryPart::Fts`
/// (`negated: true`). Its `span` covers the leading `-` even though `text` does
/// not, so the in-token cursor for prefix-`*` insertion is measured from the
/// first text char (one past the dash).
///
/// Known limitation: a negated multi-word quoted phrase like `-"a b"` will NOT
/// combine correctly — the tokenizer splits on whitespace, so only the `-"a`
/// token is negated. Single-word `-"asdf"` works. This is intentionally not
/// fixed here.
fn push_negated_fts(
    text: &str,
    span: Span,
    cursor: usize,
    parts: &mut Vec<QueryPart>,
    cursor_context: &mut CursorContext,
) {
    // `text` starts one char after `span.start` (the dash), so shift the cursor
    // by one when mapping it into the text.
    let text_start = span.start + 1;
    let cursor_in_text = if cursor >= text_start && cursor <= span.end {
        Some(cursor - text_start)
    } else {
        None
    };
    let query = process_fts_query(text, cursor_in_text);

    if span.contains(cursor) || span.at_end(cursor) {
        *cursor_context = CursorContext::Fts {
            offset: cursor - span.start,
        };
    }

    parts.push(QueryPart::Fts {
        original: text.to_string(),
        query,
        valid: true,
        span,
        negated: true,
    });
}

/// Flush pending FTS tokens into parts, updating cursor context if needed.
fn flush_fts(
    fts_tokens: &mut Vec<(String, Span)>,
    cursor: usize,
    parts: &mut Vec<QueryPart>,
    cursor_context: &mut CursorContext,
) {
    if fts_tokens.is_empty() {
        return;
    }
    let fts_part = combine_fts_tokens(fts_tokens, cursor);
    if let QueryPart::Fts { span, .. } = &fts_part
        && (span.contains(cursor) || span.at_end(cursor))
    {
        *cursor_context = CursorContext::Fts {
            offset: cursor - span.start,
        };
    }
    parts.push(fts_part);
    fts_tokens.clear();
}

/// Combine multiple FTS tokens into a single FTS part.
fn combine_fts_tokens(tokens: &[(String, Span)], cursor: usize) -> QueryPart {
    let original: String = tokens
        .iter()
        .map(|(t, _)| t.as_str())
        .collect::<Vec<_>>()
        .join(" ");

    // Find overall span
    let start = tokens.first().map(|(_, s)| s.start).unwrap_or(0);
    let end = tokens.last().map(|(_, s)| s.end).unwrap_or(0);
    let span = Span::new(start, end);

    // Map cursor to position within combined text
    let cursor_in_fts = if cursor >= start && cursor <= end {
        Some(calculate_cursor_in_combined(&original, tokens, cursor))
    } else {
        None
    };

    // Process FTS query for SQLite FTS5
    let query = process_fts_query(&original, cursor_in_fts);

    QueryPart::Fts {
        original,
        query,
        valid: true,
        span,
        negated: false,
    }
}

/// Calculate cursor position within combined FTS text.
fn calculate_cursor_in_combined(combined: &str, tokens: &[(String, Span)], cursor: usize) -> usize {
    let mut combined_pos = 0;

    for (i, (text, span)) in tokens.iter().enumerate() {
        if cursor >= span.start && cursor <= span.end {
            // Cursor is in this token
            return combined_pos + (cursor - span.start);
        }
        combined_pos += text.chars().count();
        if i < tokens.len() - 1 {
            combined_pos += 1; // space between tokens
        }
    }

    combined.chars().count()
}

/// Process FTS query for SQLite FTS5.
///
/// - Adds implicit prefix `*` at cursor position
/// - Balances unclosed parentheses
fn process_fts_query(text: &str, cursor: Option<usize>) -> String {
    let mut result = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut in_quote = false;
    let mut paren_depth: i32 = 0;

    for (i, &c) in chars.iter().enumerate() {
        result.push(c);

        if c == '"' {
            in_quote = !in_quote;
        } else if !in_quote {
            if c == '(' {
                paren_depth += 1;
            } else if c == ')' {
                paren_depth = paren_depth.saturating_sub(1);
            }
        }

        // Add implicit prefix at cursor if at word boundary
        if let Some(cur) = cursor
            && i + 1 == cur
            && !in_quote
        {
            // Check if cursor is at a word boundary (end of word)
            let at_word_end = i + 1 >= len
                || chars
                    .get(i + 1)
                    .map(|c| c.is_whitespace() || *c == ')')
                    .unwrap_or(true);
            let is_word_char = c.is_alphanumeric();
            let not_already_prefix = c != '*';

            if at_word_end && is_word_char && not_already_prefix {
                result.push('*');
            }
        }
    }

    // Close unclosed parens
    for _ in 0..paren_depth {
        result.push(')');
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::types::Value;

    use crate::search::{
        AccountFilter, AmountFilter, CategoryFilter, DateFilter, SqlContext, placeholders as ph,
    };

    fn test_config() -> SearchConfig {
        SearchConfig::new(vec![
            Box::new(DateFilter),
            Box::new(AmountFilter),
            Box::new(AccountFilter::new(vec![
                "ING/Orange".to_string(),
                "NAB/Classic".to_string(),
            ])),
            Box::new(CategoryFilter::new(vec![
                "Food".to_string(),
                "Transport".to_string(),
            ])),
        ])
    }

    fn test_sql_ctx() -> SqlContext {
        SqlContext::new()
            .with(ph::BANK_NAME, "t.bank_name")
            .with(ph::ACCOUNT_NAME, "t.account_name")
            .with(ph::CATEGORY_PATH, "c.path")
    }

    #[test]
    fn test_parse_empty() {
        let config = test_config();
        let (query, context) = parse(&config, "", 0);
        assert!(query.is_empty());
        assert!(matches!(context, CursorContext::Whitespace));
    }

    #[test]
    fn test_parse_filter() {
        let config = test_config();
        let (query, _) = parse(&config, "date:2024", 9);

        assert_eq!(query.parts.len(), 1);
        match &query.parts[0] {
            QueryPart::Filter {
                name,
                value,
                result,
                ..
            } => {
                assert_eq!(*name, "date");
                assert_eq!(value, "2024");
                assert!(matches!(result, FilterResult::Valid { .. }));
            }
            _ => panic!("Expected Filter"),
        }
    }

    #[test]
    fn test_parse_filter_alias() {
        let config = test_config();
        let (query, _) = parse(&config, "d:2024", 6);

        match &query.parts[0] {
            QueryPart::Filter { name, .. } => {
                assert_eq!(*name, "date"); // Canonical name, not alias
            }
            _ => panic!("Expected Filter"),
        }
    }

    #[test]
    fn test_parse_regex() {
        let config = test_config();
        let (query, _) = parse(&config, "/coffee.*/i", 11);

        match &query.parts[0] {
            QueryPart::Regex { pattern, valid, .. } => {
                assert_eq!(pattern, "(?i)coffee.*");
                assert!(*valid);
            }
            _ => panic!("Expected Regex"),
        }
    }

    #[test]
    fn test_parse_fts() {
        let config = test_config();
        let (query, _) = parse(&config, "groceries", 9);

        // Should have FTS part
        let fts_parts: Vec<_> = query
            .parts
            .iter()
            .filter(|p| matches!(p, QueryPart::Fts { .. }))
            .collect();
        assert_eq!(fts_parts.len(), 1);

        match fts_parts[0] {
            QueryPart::Fts {
                original, query, ..
            } => {
                assert_eq!(original, "groceries");
                assert_eq!(query, "groceries*"); // Implicit prefix at cursor
            }
            _ => panic!("Expected FTS"),
        }
    }

    #[test]
    fn test_parse_combined() {
        let config = test_config();
        let (query, _) = parse(&config, "date:2024 account:ING /coffee.*/i groceries", 43);

        // Count each type
        let filter_count = query
            .parts
            .iter()
            .filter(|p| matches!(p, QueryPart::Filter { .. }))
            .count();
        let regex_count = query
            .parts
            .iter()
            .filter(|p| matches!(p, QueryPart::Regex { .. }))
            .count();
        let fts_count = query
            .parts
            .iter()
            .filter(|p| matches!(p, QueryPart::Fts { .. }))
            .count();

        assert_eq!(filter_count, 2);
        assert_eq!(regex_count, 1);
        assert_eq!(fts_count, 1);
    }

    #[test]
    fn test_render_account_single_value() {
        let config = test_config();
        let (query, _) = parse(&config, "account:ING/Orange", 18);
        let rendered = query.render(&test_sql_ctx());

        assert_eq!(
            rendered.where_clause,
            "(LOWER(t.bank_name) LIKE ? AND LOWER(t.account_name) LIKE ?)"
        );
        assert_eq!(
            rendered.params,
            vec![
                Value::Text("ing%".to_string()),
                Value::Text("orange%".to_string()),
            ]
        );
    }

    #[test]
    fn test_render_account_multi_value_or() {
        let config = test_config();
        let (query, _) = parse(&config, "account:ING|NAB", 15);
        let rendered = query.render(&test_sql_ctx());

        assert_eq!(
            rendered.where_clause,
            "(LOWER(t.bank_name) LIKE ? OR LOWER(t.bank_name) LIKE ?)"
        );
        assert_eq!(
            rendered.params,
            vec![
                Value::Text("ing%".to_string()),
                Value::Text("nab%".to_string()),
            ]
        );
    }

    #[test]
    fn test_render_category_single_value() {
        let config = test_config();
        let (query, _) = parse(&config, "category:Food", 13);
        let rendered = query.render(&test_sql_ctx());

        assert_eq!(rendered.where_clause, "LOWER(c.path) LIKE ?");
        assert_eq!(rendered.params, vec![Value::Text("food%".to_string())]);
    }

    #[test]
    fn test_render_category_multi_value_or() {
        let config = test_config();
        let (query, _) = parse(&config, "category:Food|Transport", 23);
        let rendered = query.render(&test_sql_ctx());

        assert_eq!(
            rendered.where_clause,
            "(LOWER(c.path) LIKE ? OR LOWER(c.path) LIKE ?)"
        );
        assert_eq!(
            rendered.params,
            vec![
                Value::Text("food%".to_string()),
                Value::Text("transport%".to_string()),
            ]
        );
    }

    #[test]
    fn test_cursor_context_filter() {
        let config = test_config();
        let (_, context) = parse(&config, "date:2024", 7); // Cursor in "2024"

        match context {
            CursorContext::Filter { name, offset } => {
                assert_eq!(name, "date");
                assert_eq!(offset, 2); // Position within "2024"
            }
            _ => panic!("Expected Filter context"),
        }
    }

    #[test]
    fn test_cursor_context_fts() {
        let config = test_config();
        let (_, context) = parse(&config, "date:2024 coffee", 14); // Cursor in "coffee"

        match context {
            CursorContext::Fts { offset } => {
                assert!(offset > 0);
            }
            _ => panic!("Expected FTS context, got {:?}", context),
        }
    }

    #[test]
    fn test_cursor_context_filter_before_fts() {
        let config = test_config();
        // Cursor at end of filter value when followed by FTS
        let (_, context) = parse(&config, "account:ING coffee", 11);
        match context {
            CursorContext::Filter { name, offset } => {
                assert_eq!(name, "account");
                assert_eq!(offset, 3); // After "ING"
            }
            _ => panic!("Expected Filter context, got {:?}", context),
        }
    }

    #[test]
    fn test_cursor_context_fts_before_filter() {
        let config = test_config();
        // Cursor in FTS that appears before a filter
        let (_, context) = parse(&config, "coffee date:2024", 4);
        match context {
            CursorContext::Fts { .. } => {}
            _ => panic!("Expected FTS context, got {:?}", context),
        }
    }

    #[test]
    fn test_unknown_filter() {
        let config = test_config();
        let (query, _) = parse(&config, "unknown:value", 13);

        // Unknown filters are treated as FTS text
        let fts_parts: Vec<_> = query
            .parts
            .iter()
            .filter(|p| matches!(p, QueryPart::Fts { .. }))
            .collect();
        assert_eq!(fts_parts.len(), 1);

        match fts_parts[0] {
            QueryPart::Fts { original, .. } => {
                assert_eq!(original, "unknown:value");
            }
            _ => panic!("Expected FTS"),
        }
    }

    /// Collect the parsed FTS parts as `(query, negated)` in order.
    fn fts_parts(query: &super::ParsedQuery) -> Vec<(String, bool)> {
        query
            .parts
            .iter()
            .filter_map(|p| match p {
                QueryPart::Fts { query, negated, .. } => Some((query.clone(), *negated)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn test_parse_negated_fts_standalone() {
        let config = test_config();
        // Cursor at 0 so no implicit prefix is added to any token.
        let (query, _) = parse(&config, "coffee -tea shop", 0);
        // "coffee" (positive), "tea" (negated, standalone), "shop" (positive).
        assert_eq!(
            fts_parts(&query),
            vec![
                ("coffee".to_string(), false),
                ("tea".to_string(), true),
                ("shop".to_string(), false),
            ]
        );
    }

    #[test]
    fn test_parse_two_negated_fts() {
        let config = test_config();
        let (query, _) = parse(&config, "-coffee -tea", 0);
        // Two standalone negated parts.
        assert_eq!(
            fts_parts(&query),
            vec![("coffee".to_string(), true), ("tea".to_string(), true)]
        );
    }

    #[test]
    fn test_parse_negated_filter_propagates() {
        let config = test_config();
        let (query, _) = parse(&config, "-category:Food", 0);
        assert_eq!(query.parts.len(), 1);
        match &query.parts[0] {
            QueryPart::Filter { name, negated, .. } => {
                assert_eq!(*name, "category");
                assert!(negated);
            }
            _ => panic!("Expected Filter"),
        }
    }

    #[test]
    fn test_parse_negated_regex_propagates() {
        let config = test_config();
        let (query, _) = parse(&config, "-/coffee/i", 0);
        assert_eq!(query.parts.len(), 1);
        match &query.parts[0] {
            QueryPart::Regex {
                pattern, negated, ..
            } => {
                assert_eq!(pattern, "(?i)coffee");
                assert!(negated);
            }
            _ => panic!("Expected Regex"),
        }
    }

    #[test]
    fn test_parse_negated_fts_cursor_context() {
        let config = test_config();
        // Cursor inside "-tea" (span covers the dash) → FTS context.
        let (_, context) = parse(&config, "-tea", 2);
        match context {
            CursorContext::Fts { .. } => {}
            _ => panic!("Expected FTS context, got {:?}", context),
        }
    }

    #[test]
    fn test_process_fts_query_prefix() {
        // Cursor at end adds prefix
        assert_eq!(process_fts_query("coffee", Some(6)), "coffee*");

        // Cursor in middle doesn't add prefix
        assert_eq!(process_fts_query("coffee", Some(3)), "coffee");

        // No cursor, no prefix
        assert_eq!(process_fts_query("coffee", None), "coffee");
    }

    #[test]
    fn test_process_fts_query_paren_balance() {
        assert_eq!(process_fts_query("(foo", None), "(foo)");
        assert_eq!(process_fts_query("((nested", None), "((nested))");
        assert_eq!(process_fts_query("(foo)", None), "(foo)");
    }
}
