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
        /// Span of the entire token.
        span: Span,
        /// Span of just the value (after the colon).
        value_span: Span,
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
        /// Span of the entire token.
        span: Span,
    },
    /// Free-text search (everything that isn't a filter or regex).
    Fts {
        /// The text content.
        text: String,
        /// Span of the text.
        span: Span,
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
/// - **Filter**: `name:value` where name is alphanumeric and value has no whitespace
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

        // Check for regex at word boundary (pos == 0 or after whitespace)
        if chars[pos] == '/' {
            let regex_token = parse_regex(&chars, pos);
            pos = regex_token.span().end;
            tokens.push(regex_token);
            continue;
        }

        // Try to parse as filter (word:value)
        if let Some((filter_token, end)) = try_parse_filter(&chars, pos) {
            tokens.push(filter_token);
            pos = end;
            continue;
        }

        // Otherwise, it's FTS text - consume until whitespace or special token start
        let start = pos;
        let mut text = String::new();

        while pos < len && !chars[pos].is_whitespace() {
            // Check if we're about to hit a regex start (/ at word boundary after space)
            // This shouldn't happen mid-word, so just consume the character
            text.push(chars[pos]);
            pos += 1;
        }

        if !text.is_empty() {
            tokens.push(RawToken::Fts {
                text,
                span: Span::new(start, pos),
            });
        }
    }

    tokens
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

    // Parse the value: everything until whitespace
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
            } => {
                assert_eq!(name, "date");
                assert_eq!(value, "2024");
                assert_eq!(span.start, 0);
                assert_eq!(span.end, 9);
                assert_eq!(value_span.start, 5);
                assert_eq!(value_span.end, 9);
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

        assert!(matches!(&tokens[0], RawToken::Filter { name, value, .. } if name == "date" && value == "2024"));
        assert!(matches!(&tokens[1], RawToken::Whitespace { .. }));
        assert!(matches!(&tokens[2], RawToken::Filter { name, value, .. } if name == "account" && value == "ING/Orange"));
        assert!(matches!(&tokens[3], RawToken::Whitespace { .. }));
        assert!(matches!(&tokens[4], RawToken::Regex { pattern, flags, .. } if pattern == "coffee.*" && flags == "i"));
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
        assert!(matches!(&tokens[4], RawToken::Filter { name, value, .. } if name == "category" && value == "Food|Transport"));
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
        assert!(matches!(&tokens[0], RawToken::Whitespace { span } if span.start == 0 && span.end == 3));
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
    fn test_tokenize_transition_to_fuzzy() {
        // The ` ~` at end is handled by parser, not tokenizer
        // Tokenizer just sees it as FTS
        let tokens = tokenize("date:2024 coffee ~");
        assert_eq!(tokens.len(), 5);
        assert!(matches!(&tokens[4], RawToken::Fts { text, .. } if text == "~"));
    }
}
