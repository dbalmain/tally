//! Parsed query types representing tokenized and validated search input.

use super::filter::FilterResult;

/// A span of characters in the original input string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Start character index (inclusive).
    pub start: usize,
    /// End character index (exclusive).
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Check if a cursor position is within this span.
    pub fn contains(&self, pos: usize) -> bool {
        pos >= self.start && pos < self.end
    }

    /// Check if a cursor position is at the end of this span.
    pub fn at_end(&self, pos: usize) -> bool {
        pos == self.end
    }
}

/// A parsed part of the search query.
#[derive(Debug, Clone)]
pub enum QueryPart {
    /// A structured filter like `date:2024` or `account:ING/Orange`.
    Filter {
        /// Canonical filter name (always "date", not "d").
        name: &'static str,
        /// The raw value string after the colon.
        value: String,
        /// Result of parsing the value.
        result: FilterResult,
        /// Span of the entire filter token (e.g., "date:2024").
        span: Span,
        /// Span of just the value part (e.g., "2024").
        value_span: Span,
    },
    /// A regex pattern like `/coffee.*/i`.
    Regex {
        /// The original input (e.g., "/pattern/i").
        original: String,
        /// The compiled pattern (e.g., "(?i)pattern").
        pattern: String,
        /// Whether the regex is valid.
        valid: bool,
        /// Span of the regex token.
        span: Span,
    },
    /// Full-text search terms (everything that isn't a filter or regex).
    Fts {
        /// The original text from user input.
        original: String,
        /// The processed FTS5 query (with prefix * added, parens balanced).
        query: String,
        /// Span of the FTS text.
        span: Span,
    },
    /// Whitespace between tokens (preserved for cursor positioning).
    Whitespace {
        /// Span of the whitespace.
        span: Span,
    },
}

impl QueryPart {
    /// Get the span of this query part.
    pub fn span(&self) -> Span {
        match self {
            QueryPart::Filter { span, .. } => *span,
            QueryPart::Regex { span, .. } => *span,
            QueryPart::Fts { span, .. } => *span,
            QueryPart::Whitespace { span } => *span,
        }
    }
}

/// A fully parsed search query.
#[derive(Debug, Clone, Default)]
pub struct ParsedQuery {
    /// The parsed parts of the query.
    pub parts: Vec<QueryPart>,
}

impl ParsedQuery {
    /// Create an empty parsed query.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Check if the query is empty (no meaningful content).
    pub fn is_empty(&self) -> bool {
        self.parts
            .iter()
            .all(|p| matches!(p, QueryPart::Whitespace { .. }))
    }

    /// Get the FTS query if present.
    ///
    /// Returns the processed FTS5 query string for use in a JOIN or MATCH clause.
    pub fn fts_query(&self) -> Option<&str> {
        self.fts_queries().into_iter().next()
    }

    /// Get all FTS queries in input order.
    pub fn fts_queries(&self) -> Vec<&str> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                QueryPart::Fts { query, .. } if !query.is_empty() => Some(query.as_str()),
                _ => None,
            })
            .collect()
    }

    /// First validation error to show the user, with cursor priority.
    ///
    /// Returns the message of an invalid filter or regex containing the
    /// cursor; if no invalid part is under the cursor, returns the leftmost
    /// invalid part's message. Returns `None` if everything parses cleanly.
    pub fn error_at_cursor(&self, cursor: usize) -> Option<&str> {
        // Cursor-priority pass.
        for part in &self.parts {
            if !part_span_contains_cursor(part, cursor) {
                continue;
            }
            if let Some(msg) = part_error_message(part) {
                return Some(msg);
            }
        }
        // Fall back to the leftmost invalid part.
        self.parts.iter().find_map(part_error_message)
    }
}

fn part_span_contains_cursor(part: &QueryPart, cursor: usize) -> bool {
    let span = part.span();
    span.contains(cursor) || span.at_end(cursor)
}

fn part_error_message(part: &QueryPart) -> Option<&str> {
    match part {
        QueryPart::Filter {
            result: FilterResult::Invalid(msg),
            ..
        } => Some(msg.as_str()),
        QueryPart::Regex { valid: false, .. } => Some("Invalid regex pattern"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(name: &'static str, value: &str, result: FilterResult, start: usize) -> QueryPart {
        let end = start + name.len() + 1 + value.len();
        let value_start = start + name.len() + 1;
        QueryPart::Filter {
            name,
            value: value.to_string(),
            result,
            span: Span::new(start, end),
            value_span: Span::new(value_start, end),
        }
    }

    fn regex(text: &str, valid: bool, start: usize) -> QueryPart {
        QueryPart::Regex {
            original: text.to_string(),
            pattern: text.trim_matches('/').to_string(),
            valid,
            span: Span::new(start, start + text.len()),
        }
    }

    fn ws(start: usize, end: usize) -> QueryPart {
        QueryPart::Whitespace {
            span: Span::new(start, end),
        }
    }

    #[test]
    fn error_at_cursor_returns_none_when_all_valid() {
        let q = ParsedQuery {
            parts: vec![filter(
                "date",
                "2024",
                FilterResult::Valid {
                    sql: String::new(),
                    params: vec![],
                },
                0,
            )],
        };
        assert_eq!(q.error_at_cursor(0), None);
        assert_eq!(q.error_at_cursor(5), None);
    }

    #[test]
    fn error_at_cursor_prefers_cursor_position_over_leftmost() {
        // Two invalid filters; cursor is inside the second one.
        let q = ParsedQuery {
            parts: vec![
                filter("date", "x", FilterResult::Invalid("bad date".into()), 0),
                ws(6, 7),
                filter("amount", "y", FilterResult::Invalid("bad amount".into()), 7),
            ],
        };
        // Cursor in second filter's span returns its message.
        assert_eq!(q.error_at_cursor(10), Some("bad amount"));
        // Cursor in first filter's span returns its message.
        assert_eq!(q.error_at_cursor(2), Some("bad date"));
        // Cursor in whitespace falls back to the leftmost invalid.
        assert_eq!(q.error_at_cursor(6), Some("bad date"));
    }

    #[test]
    fn error_at_cursor_reports_invalid_regex() {
        let q = ParsedQuery {
            parts: vec![regex("/[/", false, 0)],
        };
        assert_eq!(q.error_at_cursor(1), Some("Invalid regex pattern"));
    }

    #[test]
    fn error_at_cursor_skips_valid_parts_under_cursor() {
        // Cursor inside the valid filter; falls back to the leftmost
        // invalid part further along.
        let q = ParsedQuery {
            parts: vec![
                filter(
                    "date",
                    "2024",
                    FilterResult::Valid {
                        sql: String::new(),
                        params: vec![],
                    },
                    0,
                ),
                ws(9, 10),
                filter(
                    "amount",
                    "x",
                    FilterResult::Invalid("bad amount".into()),
                    10,
                ),
            ],
        };
        assert_eq!(q.error_at_cursor(3), Some("bad amount"));
    }
}
