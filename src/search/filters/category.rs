//! Category filter implementation.

use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use rusqlite::types::Value;

use crate::search::{Filter, FilterResult};

/// Filter for categories.
///
/// Supports:
/// - `Food` → contains "Food"
/// - `Food/Groceries` → contains "Food/Groceries"
/// - `Food|Transport` → multiple (OR)
pub struct CategoryFilter {
    /// Available category paths.
    pub options: Vec<String>,
}

impl CategoryFilter {
    pub fn new(categories: &[(i64, String)]) -> Self {
        Self {
            options: categories.iter().map(|(_, path)| path.clone()).collect(),
        }
    }

    /// Create from pre-formatted options (for testing).
    pub fn with_options(options: Vec<String>) -> Self {
        Self { options }
    }
}

impl Filter for CategoryFilter {
    fn name(&self) -> &'static str {
        "category"
    }

    fn alias(&self) -> Option<&'static str> {
        Some("c")
    }

    fn parse(&self, value: &str) -> FilterResult {
        if value.is_empty() {
            return FilterResult::Empty;
        }

        // Split by | for OR
        let patterns: Vec<&str> = value.split('|').collect();
        let mut clauses = Vec::new();
        let mut params = Vec::new();

        for pattern in patterns {
            if pattern.is_empty() {
                continue;
            }

            clauses.push("category_path LIKE ?".to_string());
            params.push(Value::Text(format!("%{}%", pattern)));
        }

        if clauses.is_empty() {
            return FilterResult::Empty;
        }

        let sql = if clauses.len() == 1 {
            clauses.into_iter().next().unwrap()
        } else {
            format!("({})", clauses.join(" OR "))
        };

        FilterResult::Valid { sql, params }
    }

    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        // Find the segment containing the cursor
        let segments: Vec<(usize, &str)> = value
            .split('|')
            .scan(0, |pos, seg| {
                let start = *pos;
                *pos += seg.len() + 1; // +1 for the |
                Some((start, seg))
            })
            .collect();

        // Find which segment the cursor is in
        let (anchor_offset, current_segment) = segments
            .iter()
            .find(|(start, seg)| cursor >= *start && cursor <= start + seg.len())
            .map(|(start, seg)| (*start, *seg))
            .unwrap_or((0, value));

        // Get other segments to exclude from completions
        let other_segments: Vec<&str> = segments
            .iter()
            .filter(|(start, _)| *start != anchor_offset)
            .map(|(_, seg)| *seg)
            .collect();

        // Fuzzy match against options
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::new(
            current_segment,
            CaseMatching::Ignore,
            Normalization::Smart,
            nucleo_matcher::pattern::AtomKind::Fuzzy,
        );

        let mut scored: Vec<(u32, &String)> = self
            .options
            .iter()
            .filter(|opt| !other_segments.contains(&opt.as_str()))
            .filter_map(|opt| {
                let mut buf = Vec::new();
                let haystack = Utf32Str::new(opt, &mut buf);
                pattern
                    .score(haystack, &mut matcher)
                    .map(|score| (score, opt))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));

        let suggestions: Vec<String> = scored.into_iter().map(|(_, s)| s.clone()).collect();

        if suggestions.is_empty() {
            None
        } else {
            Some((suggestions, anchor_offset))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> CategoryFilter {
        CategoryFilter::with_options(vec![
            "Food".to_string(),
            "Food/Groceries".to_string(),
            "Food/Restaurants".to_string(),
            "Transport".to_string(),
            "Transport/Fuel".to_string(),
            "Utilities".to_string(),
        ])
    }

    fn parse(value: &str) -> FilterResult {
        filter().parse(value)
    }

    #[test]
    fn test_empty() {
        assert!(matches!(parse(""), FilterResult::Empty));
    }

    #[test]
    fn test_single() {
        match parse("Food") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "category_path LIKE ?");
                assert_eq!(params, vec![Value::Text("%Food%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_with_slash() {
        match parse("Food/Groceries") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "category_path LIKE ?");
                assert_eq!(params, vec![Value::Text("%Food/Groceries%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_multiple_or() {
        match parse("Food|Transport") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "(category_path LIKE ? OR category_path LIKE ?)");
                assert_eq!(
                    params,
                    vec![
                        Value::Text("%Food%".to_string()),
                        Value::Text("%Transport%".to_string())
                    ]
                );
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_completions() {
        let f = filter();
        let (suggestions, anchor) = f.completions("Foo", 3).unwrap();
        assert_eq!(anchor, 0);
        assert!(suggestions.iter().any(|s| s.starts_with("Food")));
    }

    #[test]
    fn test_completions_multi_segment() {
        let f = filter();
        // Cursor at position 6 is at end of "T" in second segment
        let (suggestions, anchor) = f.completions("Food|T", 6).unwrap();
        assert_eq!(anchor, 5); // After the |
        // Should prioritize Transport for "T" prefix
        assert!(suggestions.iter().any(|s| s.starts_with("Transport")));
    }
}
