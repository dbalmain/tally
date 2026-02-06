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
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    /// The parsed parts of the query.
    pub parts: Vec<QueryPart>,
    /// Whether the query ends with ` ~` to transition to fuzzy mode.
    pub transition_to_fuzzy: bool,
}

impl ParsedQuery {
    /// Create an empty parsed query.
    pub fn empty() -> Self {
        Self {
            parts: Vec::new(),
            transition_to_fuzzy: false,
        }
    }

    /// Check if the query is empty (no meaningful content).
    pub fn is_empty(&self) -> bool {
        self.parts.iter().all(|p| matches!(p, QueryPart::Whitespace { .. }))
    }
}
