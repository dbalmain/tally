//! Category filter implementation.

use rusqlite::types::Value;

use super::list::{complete_pipe_segments, parse_pipe_segments};
use crate::search::{Filter, FilterResult, placeholders as ph};

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
    pub fn new(options: Vec<String>) -> Self {
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
        parse_pipe_segments(value, |pattern| {
            Ok((
                format!("LOWER({}) LIKE ?", ph::reference(ph::CATEGORY_PATH)),
                vec![Value::Text(format!("%{}%", pattern.to_lowercase()))],
            ))
        })
    }

    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        complete_pipe_segments(&self.options, value, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> CategoryFilter {
        CategoryFilter::new(vec![
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
                assert_eq!(sql, "LOWER({category_path}) LIKE ?");
                assert_eq!(params, vec![Value::Text("%food%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_with_slash() {
        match parse("Food/Groceries") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "LOWER({category_path}) LIKE ?");
                assert_eq!(params, vec![Value::Text("%food/groceries%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_multiple_or() {
        match parse("Food|Transport") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(
                    sql,
                    "(LOWER({category_path}) LIKE ? OR LOWER({category_path}) LIKE ?)"
                );
                assert_eq!(
                    params,
                    vec![
                        Value::Text("%food%".to_string()),
                        Value::Text("%transport%".to_string())
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
        assert_eq!(
            suggestions,
            vec![
                "Food".to_string(),
                "Food/Groceries".to_string(),
                "Food/Restaurants".to_string(),
            ]
        );
        assert!(suggestions.iter().any(|s| s.starts_with("Food")));
    }

    #[test]
    fn test_completions_multi_segment() {
        let f = filter();
        // Cursor at position 6 is at end of "T" in second segment
        let (suggestions, anchor) = f.completions("Food|T", 6).unwrap();
        assert_eq!(anchor, 5); // After the |
        assert_eq!(
            suggestions,
            vec![
                "Transport".to_string(),
                "Transport/Fuel".to_string(),
                "Food/Restaurants".to_string(),
                "Utilities".to_string(),
            ]
        );
        // Should prioritize Transport for "T" prefix
        assert!(suggestions.iter().any(|s| s.starts_with("Transport")));
    }
}
