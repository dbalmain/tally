//! Category filter implementation.

use rusqlite::types::Value;

use super::list::{complete_pipe_segments, parse_pipe_segments};
use crate::search::{Filter, FilterResult, placeholders as ph};

/// Filter for categories.
///
/// A plain value is a start-anchored prefix over the whole path; a leading `/`
/// instead anchors after any `/` separator, matching a sub-category at any depth.
///
/// Supports:
/// - `Food` → path starts with "Food"
/// - `Food/Groceries` → path starts with "Food/Groceries"
/// - `/Groceries` → a "Groceries…" segment under any parent (after any `/`)
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
                vec![Value::Text(category_like(pattern))],
            ))
        })
    }

    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        complete_pipe_segments(&self.options, value, cursor)
    }
}

/// SQL `LIKE` pattern for one category segment.
///
/// A plain value anchors at the start of the path; a leading `/` anchors after
/// any `/` separator so it matches a sub-category at any depth:
///
/// - `Food`       → `food%`         (path starts with "Food")
/// - `Food/Gro`   → `food/gro%`     (still a start-anchored prefix)
/// - `/Groceries` → `%/groceries%`  (a "Groceries…" segment under any parent)
fn category_like(segment: &str) -> String {
    let lower = segment.to_lowercase();
    match lower.strip_prefix('/') {
        Some(rest) => format!("%/{rest}%"),
        None => format!("{lower}%"),
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
    fn test_single_is_start_anchored_prefix() {
        match parse("Food") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "LOWER({category_path}) LIKE ?");
                assert_eq!(params, vec![Value::Text("food%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_with_slash_stays_start_anchored() {
        // A `/` in the middle is literal — only a *leading* `/` unanchors.
        match parse("Food/Groceries") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "LOWER({category_path}) LIKE ?");
                assert_eq!(params, vec![Value::Text("food/groceries%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_leading_slash_matches_after_any_separator() {
        match parse("/Groceries") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "LOWER({category_path}) LIKE ?");
                assert_eq!(params, vec![Value::Text("%/groceries%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_multiple_or() {
        match parse("Food|/Restaurants") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(
                    sql,
                    "(LOWER({category_path}) LIKE ? OR LOWER({category_path}) LIKE ?)"
                );
                assert_eq!(
                    params,
                    vec![
                        Value::Text("food%".to_string()),
                        Value::Text("%/restaurants%".to_string())
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
