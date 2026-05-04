//! Account filter implementation.

use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use rusqlite::types::Value;

use crate::search::{Filter, FilterResult};

/// Filter for accounts using Bank/Account format.
///
/// Supports:
/// - `ING` → bank prefix
/// - `ING/` → all accounts in bank
/// - `ING/Orange` → bank + account prefix
/// - `/Savings` → any bank, account prefix
/// - `ING|NAB` → multiple banks (OR)
pub struct AccountFilter {
    /// Available account options in "Bank/Account" format.
    pub options: Vec<String>,
}

impl AccountFilter {
    pub fn new(banks: &[(i64, String)], accounts: &[(i64, i64, String)]) -> Self {
        let mut options = Vec::new();
        for (_account_id, bank_id, account_name) in accounts {
            if let Some((_, bank_name)) = banks.iter().find(|(id, _)| id == bank_id) {
                options.push(format!("{}/{}", bank_name, account_name));
            }
        }
        Self { options }
    }

    /// Create from pre-formatted options (for testing).
    pub fn with_options(options: Vec<String>) -> Self {
        Self { options }
    }
}

impl Filter for AccountFilter {
    fn name(&self) -> &'static str {
        "account"
    }

    fn alias(&self) -> Option<&'static str> {
        Some("a")
    }

    fn parse(&self, value: &str) -> FilterResult {
        if value.is_empty() {
            return FilterResult::Empty;
        }

        let mut clauses = Vec::new();
        let mut params = Vec::new();

        for pattern in value.split('|') {
            if pattern.is_empty() {
                continue;
            }

            if let Some((bank, account)) = pattern.split_once('/') {
                // Bank/Account format
                if bank.is_empty() {
                    // /Account - any bank, account prefix
                    clauses.push("LOWER({account_name}) LIKE ?".to_string());
                    params.push(Value::Text(format!("{}%", account.to_lowercase())));
                } else if account.is_empty() {
                    // Bank/ - all accounts in bank
                    clauses.push("LOWER({bank_name}) LIKE ?".to_string());
                    params.push(Value::Text(format!("{}%", bank.to_lowercase())));
                } else {
                    // Bank/Account - both prefixes
                    clauses.push(
                        "(LOWER({bank_name}) LIKE ? AND LOWER({account_name}) LIKE ?)".to_string(),
                    );
                    params.push(Value::Text(format!("{}%", bank.to_lowercase())));
                    params.push(Value::Text(format!("{}%", account.to_lowercase())));
                }
            } else {
                // Bank only (prefix match)
                clauses.push("LOWER({bank_name}) LIKE ?".to_string());
                params.push(Value::Text(format!("{}%", pattern.to_lowercase())));
            }
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
                *pos += seg.chars().count() + 1; // +1 for the |
                Some((start, seg))
            })
            .collect();

        // Find which segment the cursor is in
        let (anchor_offset, current_segment) = segments
            .iter()
            .find(|(start, seg)| cursor >= *start && cursor <= start + seg.chars().count())
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

    fn filter() -> AccountFilter {
        AccountFilter::with_options(vec![
            "ING/Orange Everyday".to_string(),
            "ING/Savings Maximiser".to_string(),
            "NAB/Classic".to_string(),
            "NAB/Savings".to_string(),
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
    fn test_bank_only() {
        match parse("ING") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "LOWER({bank_name}) LIKE ?");
                assert_eq!(params, vec![Value::Text("ing%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_bank_slash() {
        match parse("ING/") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "LOWER({bank_name}) LIKE ?");
                assert_eq!(params, vec![Value::Text("ing%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_bank_account() {
        match parse("ING/Orange") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(
                    sql,
                    "(LOWER({bank_name}) LIKE ? AND LOWER({account_name}) LIKE ?)"
                );
                assert_eq!(
                    params,
                    vec![
                        Value::Text("ing%".to_string()),
                        Value::Text("orange%".to_string())
                    ]
                );
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_account_only() {
        match parse("/Savings") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(sql, "LOWER({account_name}) LIKE ?");
                assert_eq!(params, vec![Value::Text("savings%".to_string())]);
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_multiple_or() {
        match parse("ING|NAB") {
            FilterResult::Valid { sql, params } => {
                assert_eq!(
                    sql,
                    "(LOWER({bank_name}) LIKE ? OR LOWER({bank_name}) LIKE ?)"
                );
                assert_eq!(
                    params,
                    vec![
                        Value::Text("ing%".to_string()),
                        Value::Text("nab%".to_string())
                    ]
                );
            }
            _ => panic!("Expected Valid"),
        }
    }

    #[test]
    fn test_completions() {
        let f = filter();
        let (suggestions, anchor) = f.completions("ING", 3).unwrap();
        assert_eq!(anchor, 0);
        assert!(suggestions.iter().any(|s| s.contains("ING")));
    }

    #[test]
    fn test_completions_multi_segment() {
        let f = filter();
        // Cursor at position 5 is at end of "N" in second segment
        let (suggestions, anchor) = f.completions("ING|N", 5).unwrap();
        assert_eq!(anchor, 4); // After the |
        // Should prioritize NAB options for "N" prefix
        assert!(suggestions.iter().any(|s| s.starts_with("NAB")));
    }
}
