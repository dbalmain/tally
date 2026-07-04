//! Tokenizer for search input.
//!
//! Splits input into raw tokens: filters, regex, FTS text, and whitespace.
//! Preserves spans for cursor positioning and validity display.

use super::query::Span;

/// A raw token before filter parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawToken {
    /// A filter token like `date:2024` or `account:ING/Orange`.
    Filter {
        /// The filter name (e.g., "date", "d", "account").
        name: String,
        /// The value after the colon.
        value: String,
        /// Span of the entire token (includes a leading `-` when negated).
        span: Span,
        /// Span of just the value (after the colon).
        value_span: Span,
        /// Whether the token is negated by a leading `-`.
        negated: bool,
    },
    /// A regex token like `/pattern/flags`.
    Regex {
        /// The full original text including slashes and flags.
        original: String,
        /// The pattern between slashes.
        pattern: String,
        /// The flags after the closing slash (e.g., "i", "gi").
        flags: String,
        /// Whether the regex is complete (has closing slash).
        complete: bool,
        /// Span of the entire token (includes a leading `-` when negated).
        span: Span,
        /// Whether the token is negated by a leading `-`.
        negated: bool,
    },
    /// Free-text search (everything that isn't a filter or regex).
    Fts {
        /// The text content (never includes the leading `-` of a negation).
        text: String,
        /// Span of the text (includes a leading `-` when negated).
        span: Span,
        /// Whether the token is negated by a leading `-`.
        negated: bool,
    },
    /// Whitespace between tokens.
    Whitespace {
        /// Span of the whitespace.
        span: Span,
    },
}

impl RawToken {
    pub fn span(&self) -> Span {
        match self {
            RawToken::Filter { span, .. } => *span,
            RawToken::Regex { span, .. } => *span,
            RawToken::Fts { span, .. } => *span,
            RawToken::Whitespace { span } => *span,
        }
    }
}

/// Tokenize search input into raw tokens.
///
/// Rules:
/// - **Negation**: a leading `-` at a word boundary, immediately followed by a
///   non-whitespace char, negates the token that follows (`-coffee`,
///   `-category:Food`, `-/re/`). A lone `-` (followed by whitespace or
///   end-of-input) is literal FTS text, not negation. A `-` *inside* a value
///   (`amount:-50`) is untouched.
/// - **Filter**: `name:value` where value has no whitespace
/// - **Regex**: `/` at word boundary, content until unescaped `/`, then flags until whitespace
/// - **FTS**: Everything else (whitespace-separated, quotes for phrases)
/// - **Whitespace**: Preserved between tokens for cursor positioning
pub fn tokenize(input: &str) -> Vec<RawToken> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut pos = 0;

    while pos < len {
        // Skip and record whitespace
        if chars[pos].is_whitespace() {
            let start = pos;
            while pos < len && chars[pos].is_whitespace() {
                pos += 1;
            }
            tokens.push(RawToken::Whitespace {
                span: Span::new(start, pos),
            });
            continue;
        }

        // Detect a leading `-` that negates the following token. A lone `-`
        // (next char is whitespace or end-of-input) is literal FTS, so it falls
        // through to the normal path below with `negated: false`.
        let dash = pos;
        let negated = chars[pos] == '-' && chars.get(pos + 1).is_some_and(|c| !c.is_whitespace());
        let token_start = if negated { pos + 1 } else { pos };

        // Check for regex at word boundary (pos == 0 or after whitespace)
        if chars[token_start] == '/' {
            let mut regex_token = parse_regex(&chars, token_start);
            pos = regex_token.span().end;
            if negated {
                rewrite_span_start(&mut regex_token, dash);
                set_negated(&mut regex_token);
            }
            tokens.push(regex_token);
            continue;
        }

        // Try to parse as filter (word:value)
        if let Some((mut filter_token, end)) = try_parse_filter(&chars, token_start) {
            pos = end;
            if negated {
                rewrite_span_start(&mut filter_token, dash);
                set_negated(&mut filter_token);
            }
            tokens.push(filter_token);
            continue;
        }

        // Otherwise, it's FTS text - consume until whitespace. When negated, the
        // text starts after the dash but the span covers the dash so highlighting
        // includes `-token`.
        let mut text = String::new();
        pos = token_start;
        while pos < len && !chars[pos].is_whitespace() {
            text.push(chars[pos]);
            pos += 1;
        }

        if negated || !text.is_empty() {
            let span_start = if negated { dash } else { token_start };
            tokens.push(RawToken::Fts {
                text,
                span: Span::new(span_start, pos),
                negated,
            });
        }
    }

    tokens
}

/// Rewrite a token's span start (used to extend a negated token's span back
/// over the leading `-`).
fn rewrite_span_start(token: &mut RawToken, start: usize) {
    match token {
        RawToken::Filter { span, .. }
        | RawToken::Regex { span, .. }
        | RawToken::Fts { span, .. } => span.start = start,
        RawToken::Whitespace { span } => span.start = start,
    }
}

/// Mark a token as negated.
fn set_negated(token: &mut RawToken) {
    match token {
        RawToken::Filter { negated, .. }
        | RawToken::Regex { negated, .. }
        | RawToken::Fts { negated, .. } => *negated = true,
        RawToken::Whitespace { .. } => {}
    }
}

/// Try to parse a filter token starting at pos.
/// Returns the token and the end position, or None if not a valid filter.
fn try_parse_filter(chars: &[char], start: usize) -> Option<(RawToken, usize)> {
    let len = chars.len();
    let mut pos = start;

    // Parse the name: must be alphanumeric (allows shortcuts like "d" or "a")
    let name_start = pos;
    while pos < len && (chars[pos].is_alphanumeric() || chars[pos] == '_') {
        pos += 1;
    }

    // Must have at least one character and be followed by ':'
    if pos == name_start || pos >= len || chars[pos] != ':' {
        return None;
    }

    let name: String = chars[name_start..pos].iter().collect();
    pos += 1; // consume the ':'
    let value_start = pos;

    // Parse the value: everything until whitespace.
    while pos < len && !chars[pos].is_whitespace() {
        pos += 1;
    }

    let value: String = chars[value_start..pos].iter().collect();

    Some((
        RawToken::Filter {
            name,
            value,
            span: Span::new(start, pos),
            value_span: Span::new(value_start, pos),
            negated: false,
        },
        pos,
    ))
}

/// Parse a regex token starting at the opening '/'.
fn parse_regex(chars: &[char], start: usize) -> RawToken {
    let len = chars.len();
    let mut pos = start + 1; // skip opening '/'
    let mut pattern = String::new();
    let mut complete = false;

    // Parse pattern until unescaped '/'
    while pos < len {
        if chars[pos] == '\\' && pos + 1 < len {
            // Escaped character - include both in pattern
            pattern.push(chars[pos]);
            pattern.push(chars[pos + 1]);
            pos += 2;
        } else if chars[pos] == '/' {
            // Closing slash
            pos += 1;
            complete = true;
            break;
        } else {
            pattern.push(chars[pos]);
            pos += 1;
        }
    }

    // Parse flags (only if we found closing slash)
    let mut flags = String::new();
    if complete {
        while pos < len && chars[pos].is_ascii_alphabetic() {
            flags.push(chars[pos]);
            pos += 1;
        }
    }

    let original: String = chars[start..pos].iter().collect();

    RawToken::Regex {
        original,
        pattern,
        flags,
        complete,
        span: Span::new(start, pos),
        negated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_simple_fts() {
        let tokens = tokenize("hello world");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(&tokens[0], RawToken::Fts { text, .. } if text == "hello"));
        assert!(matches!(&tokens[1], RawToken::Whitespace { .. }));
        assert!(matches!(&tokens[2], RawToken::Fts { text, .. } if text == "world"));
    }

    #[test]
    fn test_tokenize_filter() {
        let tokens = tokenize("date:2024");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Filter {
                name,
                value,
                span,
                value_span,
                negated,
            } => {
                assert_eq!(name, "date");
                assert_eq!(value, "2024");
                assert_eq!(span.start, 0);
                assert_eq!(span.end, 9);
                assert_eq!(value_span.start, 5);
                assert_eq!(value_span.end, 9);
                assert!(!negated);
            }
            _ => panic!("Expected Filter token"),
        }
    }

    #[test]
    fn test_tokenize_filter_shortcut() {
        let tokens = tokenize("d:2024-01");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Filter { name, value, .. } => {
                assert_eq!(name, "d");
                assert_eq!(value, "2024-01");
            }
            _ => panic!("Expected Filter token"),
        }
    }

    #[test]
    fn test_tokenize_filter_with_slash() {
        // account:ING/Orange - the / should not start a regex
        let tokens = tokenize("account:ING/Orange");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Filter { name, value, .. } => {
                assert_eq!(name, "account");
                assert_eq!(value, "ING/Orange");
            }
            _ => panic!("Expected Filter token"),
        }
    }

    #[test]
    fn test_tokenize_regex() {
        let tokens = tokenize("/coffee.*/i");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Regex {
                original,
                pattern,
                flags,
                complete,
                ..
            } => {
                assert_eq!(original, "/coffee.*/i");
                assert_eq!(pattern, "coffee.*");
                assert_eq!(flags, "i");
                assert!(*complete);
            }
            _ => panic!("Expected Regex token"),
        }
    }

    #[test]
    fn test_tokenize_regex_incomplete() {
        let tokens = tokenize("/coffee");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Regex {
                pattern, complete, ..
            } => {
                assert_eq!(pattern, "coffee");
                assert!(!*complete);
            }
            _ => panic!("Expected Regex token"),
        }
    }

    #[test]
    fn test_tokenize_regex_with_escaped_slash() {
        let tokens = tokenize(r"/foo\/bar/");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Regex {
                pattern, complete, ..
            } => {
                assert_eq!(pattern, r"foo\/bar");
                assert!(*complete);
            }
            _ => panic!("Expected Regex token"),
        }
    }

    #[test]
    fn test_tokenize_combined() {
        let tokens = tokenize("date:2024 account:ING/Orange /coffee.*/i groceries");

        // date:2024, space, account:..., space, regex, space, fts
        assert_eq!(tokens.len(), 7);

        assert!(
            matches!(&tokens[0], RawToken::Filter { name, value, .. } if name == "date" && value == "2024")
        );
        assert!(matches!(&tokens[1], RawToken::Whitespace { .. }));
        assert!(
            matches!(&tokens[2], RawToken::Filter { name, value, .. } if name == "account" && value == "ING/Orange")
        );
        assert!(matches!(&tokens[3], RawToken::Whitespace { .. }));
        assert!(
            matches!(&tokens[4], RawToken::Regex { pattern, flags, .. } if pattern == "coffee.*" && flags == "i")
        );
        assert!(matches!(&tokens[5], RawToken::Whitespace { .. }));
        assert!(matches!(&tokens[6], RawToken::Fts { text, .. } if text == "groceries"));
    }

    #[test]
    fn test_tokenize_preserves_spans() {
        let tokens = tokenize("d:2024 coffee");
        // d:2024 = chars 0-6, space = 6-7, coffee = 7-13

        assert_eq!(tokens[0].span(), Span::new(0, 6));
        assert_eq!(tokens[1].span(), Span::new(6, 7));
        assert_eq!(tokens[2].span(), Span::new(7, 13));
    }

    #[test]
    fn test_tokenize_multiple_filters() {
        let tokens = tokenize("date:2024 amount:>100 category:Food|Transport");
        assert_eq!(tokens.len(), 5); // 3 filters + 2 whitespaces

        assert!(matches!(&tokens[0], RawToken::Filter { name, .. } if name == "date"));
        assert!(matches!(&tokens[2], RawToken::Filter { name, .. } if name == "amount"));
        assert!(
            matches!(&tokens[4], RawToken::Filter { name, value, .. } if name == "category" && value == "Food|Transport")
        );
    }

    #[test]
    fn test_tokenize_fts_with_quotes() {
        // Tokenizer splits on whitespace - FTS5 will see the quotes and handle phrase matching
        // The tokens are: "\"exact", " ", "phrase\""
        let tokens = tokenize("\"exact phrase\"");
        assert_eq!(tokens.len(), 3);
        assert!(matches!(&tokens[0], RawToken::Fts { text, .. } if text == "\"exact"));
        assert!(matches!(&tokens[1], RawToken::Whitespace { .. }));
        assert!(matches!(&tokens[2], RawToken::Fts { text, .. } if text == "phrase\""));
    }

    #[test]
    fn test_tokenize_fts_with_or() {
        // FTS with OR - OR is part of the FTS text
        let tokens = tokenize("coffee OR tea");
        assert_eq!(tokens.len(), 5);
        assert!(matches!(&tokens[0], RawToken::Fts { text, .. } if text == "coffee"));
        assert!(matches!(&tokens[2], RawToken::Fts { text, .. } if text == "OR"));
        assert!(matches!(&tokens[4], RawToken::Fts { text, .. } if text == "tea"));
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = tokenize("");
        assert!(tokens.is_empty());
    }

    #[test]
    fn test_tokenize_only_whitespace() {
        let tokens = tokenize("   ");
        assert_eq!(tokens.len(), 1);
        assert!(
            matches!(&tokens[0], RawToken::Whitespace { span } if span.start == 0 && span.end == 3)
        );
    }

    #[test]
    fn test_tokenize_filter_empty_value() {
        let tokens = tokenize("date:");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Filter { name, value, .. } => {
                assert_eq!(name, "date");
                assert_eq!(value, "");
            }
            _ => panic!("Expected Filter token"),
        }
    }

    #[test]
    fn test_tokenize_negated_fts() {
        // `-coffee`: text is "coffee" (no dash), span covers the dash.
        let tokens = tokenize("-coffee");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Fts {
                text,
                span,
                negated,
            } => {
                assert_eq!(text, "coffee");
                assert!(negated);
                assert_eq!(*span, Span::new(0, 7));
            }
            _ => panic!("Expected Fts token"),
        }
    }

    #[test]
    fn test_tokenize_negated_quoted_fts() {
        // Single-word quoted token negates; span covers the dash.
        let tokens = tokenize("-\"asdf\"");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Fts {
                text,
                span,
                negated,
            } => {
                assert_eq!(text, "\"asdf\"");
                assert!(negated);
                assert_eq!(*span, Span::new(0, 7));
            }
            _ => panic!("Expected Fts token"),
        }
    }

    #[test]
    fn test_tokenize_negated_filter() {
        let tokens = tokenize("-category:Food");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Filter {
                name,
                value,
                span,
                value_span,
                negated,
            } => {
                assert_eq!(name, "category");
                assert_eq!(value, "Food");
                assert!(negated);
                // Span covers the dash; value_span is unchanged.
                assert_eq!(*span, Span::new(0, 14));
                assert_eq!(*value_span, Span::new(10, 14));
            }
            _ => panic!("Expected Filter token"),
        }
    }

    #[test]
    fn test_tokenize_negated_regex() {
        let tokens = tokenize("-/re/i");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Regex {
                pattern,
                flags,
                span,
                negated,
                ..
            } => {
                assert_eq!(pattern, "re");
                assert_eq!(flags, "i");
                assert!(negated);
                assert_eq!(*span, Span::new(0, 6));
            }
            _ => panic!("Expected Regex token"),
        }
    }

    #[test]
    fn test_tokenize_lone_dash_is_literal() {
        // A lone `-` at end-of-input is literal FTS, not negation.
        let tokens = tokenize("-");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Fts { text, negated, .. } => {
                assert_eq!(text, "-");
                assert!(!negated);
            }
            _ => panic!("Expected Fts token"),
        }
    }

    #[test]
    fn test_tokenize_dash_then_space_is_literal() {
        // `- foo`: the `-` is literal FTS (next char is whitespace).
        let tokens = tokenize("- foo");
        assert_eq!(tokens.len(), 3);
        match &tokens[0] {
            RawToken::Fts { text, negated, .. } => {
                assert_eq!(text, "-");
                assert!(!negated);
            }
            _ => panic!("Expected Fts token"),
        }
        assert!(matches!(&tokens[1], RawToken::Whitespace { .. }));
        assert!(
            matches!(&tokens[2], RawToken::Fts { text, negated, .. } if text == "foo" && !negated)
        );
    }

    #[test]
    fn test_tokenize_signed_amount_not_negated() {
        // `amount:-50`: the `-` is inside the value, so the token is NOT negated.
        let tokens = tokenize("amount:-50");
        assert_eq!(tokens.len(), 1);
        match &tokens[0] {
            RawToken::Filter {
                name,
                value,
                negated,
                ..
            } => {
                assert_eq!(name, "amount");
                assert_eq!(value, "-50");
                assert!(!negated);
            }
            _ => panic!("Expected Filter token"),
        }
    }

    #[test]
    fn test_tokenize_transition_to_fuzzy() {
        // The ` ~` at end is handled by parser, not tokenizer
        // Tokenizer just sees it as FTS
        let tokens = tokenize("date:2024 coffee ~");
        assert_eq!(tokens.len(), 5);
        assert!(matches!(&tokens[4], RawToken::Fts { text, .. } if text == "~"));
    }
}
