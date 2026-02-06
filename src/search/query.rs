//! Parsed query types representing tokenized and validated search input.

use rusqlite::types::Value;

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

    /// Convert to SQL WHERE clause and parameters.
    ///
    /// Returns (where_clause, params) for use in SQL queries.
    /// The where_clause is a fragment like "date >= ? AND amount_cents > ?".
    pub fn to_sql(&self) -> (String, Vec<Value>) {
        let mut clauses = Vec::new();
        let mut params = Vec::new();

        for part in &self.parts {
            match part {
                QueryPart::Filter {
                    result: FilterResult::Valid { sql, params: p },
                    ..
                } => {
                    clauses.push(sql.clone());
                    params.extend(p.clone());
                }
                QueryPart::Regex {
                    pattern,
                    valid: true,
                    ..
                } => {
                    clauses.push("description REGEXP ?".to_string());
                    params.push(Value::Text(pattern.clone()));
                }
                _ => {}
            }
        }

        let where_clause = if clauses.is_empty() {
            "1=1".to_string()
        } else {
            clauses.join(" AND ")
        };

        (where_clause, params)
    }

    /// Get the FTS query if present.
    ///
    /// Returns the processed FTS5 query string for use in a JOIN or MATCH clause.
    pub fn fts_query(&self) -> Option<&str> {
        self.parts.iter().find_map(|p| match p {
            QueryPart::Fts { query, .. } if !query.is_empty() => Some(query.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_filter(name: &'static str, sql: &str, params: Vec<Value>) -> QueryPart {
        QueryPart::Filter {
            name,
            value: String::new(),
            result: FilterResult::Valid {
                sql: sql.to_string(),
                params,
            },
            span: Span::new(0, 0),
            value_span: Span::new(0, 0),
        }
    }

    fn make_regex(pattern: &str) -> QueryPart {
        QueryPart::Regex {
            original: format!("/{}/", pattern),
            pattern: pattern.to_string(),
            valid: true,
            span: Span::new(0, 0),
        }
    }

    fn make_fts(query: &str) -> QueryPart {
        QueryPart::Fts {
            original: query.to_string(),
            query: query.to_string(),
            span: Span::new(0, 0),
        }
    }

    #[test]
    fn test_to_sql_empty() {
        let query = ParsedQuery::empty();
        let (sql, params) = query.to_sql();
        assert_eq!(sql, "1=1");
        assert!(params.is_empty());
    }

    #[test]
    fn test_to_sql_single_filter() {
        let query = ParsedQuery {
            parts: vec![make_filter(
                "date",
                "date >= ? AND date <= ?",
                vec![
                    Value::Text("2024-01-01".to_string()),
                    Value::Text("2024-12-31".to_string()),
                ],
            )],
            transition_to_fuzzy: false,
        };

        let (sql, params) = query.to_sql();
        assert_eq!(sql, "date >= ? AND date <= ?");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_to_sql_multiple_filters() {
        let query = ParsedQuery {
            parts: vec![
                make_filter("date", "date >= ?", vec![Value::Text("2024-01-01".to_string())]),
                QueryPart::Whitespace {
                    span: Span::new(0, 0),
                },
                make_filter("amount", "ABS(amount_cents) > ?", vec![Value::Integer(10000)]),
            ],
            transition_to_fuzzy: false,
        };

        let (sql, params) = query.to_sql();
        assert_eq!(sql, "date >= ? AND ABS(amount_cents) > ?");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_to_sql_regex() {
        let query = ParsedQuery {
            parts: vec![make_regex("(?i)coffee.*")],
            transition_to_fuzzy: false,
        };

        let (sql, params) = query.to_sql();
        assert_eq!(sql, "description REGEXP ?");
        assert_eq!(params, vec![Value::Text("(?i)coffee.*".to_string())]);
    }

    #[test]
    fn test_fts_query() {
        let query = ParsedQuery {
            parts: vec![
                make_filter("date", "date >= ?", vec![Value::Text("2024-01-01".to_string())]),
                QueryPart::Whitespace {
                    span: Span::new(0, 0),
                },
                make_fts("groceries*"),
            ],
            transition_to_fuzzy: false,
        };

        assert_eq!(query.fts_query(), Some("groceries*"));
    }

    #[test]
    fn test_fts_query_none() {
        let query = ParsedQuery {
            parts: vec![make_filter("date", "date >= ?", vec![])],
            transition_to_fuzzy: false,
        };

        assert_eq!(query.fts_query(), None);
    }
}
