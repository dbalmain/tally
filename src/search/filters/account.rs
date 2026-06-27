//! Account filter implementation.

use rusqlite::types::Value;

use super::list::{complete_pipe_segments, parse_pipe_segments};
use crate::search::{Filter, FilterResult, placeholders as ph};

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
    pub fn new(options: Vec<String>) -> Self {
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
        parse_pipe_segments(value, |pattern| {
            // Validate against the loaded bank/account list when one exists.
            // We skip validation when options is empty (e.g. a fresh DB before
            // any imports) so the filter doesn't render red on every keystroke
            // when there's nothing to match against in the first place.
            if !self.options.is_empty()
                && let Err(msg) = validate_pattern(pattern, &self.options)
            {
                return Err(msg);
            }

            if let Some((bank, account)) = pattern.split_once('/') {
                // Bank/Account format
                if bank.is_empty() {
                    // /Account - any bank, account prefix
                    Ok((
                        format!("LOWER({}) LIKE ?", ph::reference(ph::ACCOUNT_NAME)),
                        vec![Value::Text(format!("{}%", account.to_lowercase()))],
                    ))
                } else if account.is_empty() {
                    // Bank/ - all accounts in bank
                    Ok((
                        format!("LOWER({}) LIKE ?", ph::reference(ph::BANK_NAME)),
                        vec![Value::Text(format!("{}%", bank.to_lowercase()))],
                    ))
                } else {
                    // Bank/Account - both prefixes
                    Ok((
                        format!(
                            "(LOWER({}) LIKE ? AND LOWER({}) LIKE ?)",
                            ph::reference(ph::BANK_NAME),
                            ph::reference(ph::ACCOUNT_NAME)
                        ),
                        vec![
                            Value::Text(format!("{}%", bank.to_lowercase())),
                            Value::Text(format!("{}%", account.to_lowercase())),
                        ],
                    ))
                }
            } else {
                // Bank only (prefix match)
                Ok((
                    format!("LOWER({}) LIKE ?", ph::reference(ph::BANK_NAME)),
                    vec![Value::Text(format!("{}%", pattern.to_lowercase()))],
                ))
            }
        })
    }

    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        complete_pipe_segments(&self.options, value, cursor)
    }
}

/// Check whether a single `Bank`, `Bank/`, `Bank/Account`, or `/Account`
/// pattern matches at least one option in the loaded list.
///
/// Returns `Err(message)` describing which side of the slash failed, so the
/// user can tell "no such bank" from "no such account in that bank". Match
/// semantics match the SQL the filter emits: case-insensitive prefix on each
/// side of the `/`. The degenerate `/` pattern (both sides empty) returns
/// `Ok(())` because `parse` already discards it as a no-op.
fn validate_pattern(pattern: &str, options: &[String]) -> Result<(), String> {
    let (bank_part, account_part) = match pattern.split_once('/') {
        Some((b, a)) => (b, a),
        None => (pattern, ""),
    };

    // Pre-split options once. Anything missing a `/` is malformed and ignored.
    let split_options: Vec<(&str, &str)> = options
        .iter()
        .filter_map(|opt| opt.split_once('/'))
        .collect();

    let bank_lower = bank_part.to_lowercase();
    let account_lower = account_part.to_lowercase();

    if bank_part.is_empty() {
        if account_part.is_empty() {
            // "/" — both sides empty; parse() treats this as a no-op.
            return Ok(());
        }
        let any = split_options
            .iter()
            .any(|(_, a)| a.to_lowercase().starts_with(&account_lower));
        return if any {
            Ok(())
        } else {
            Err(format!("Unknown account: {}", account_part))
        };
    }

    // Bank or Bank/ or Bank/Account — bank prefix must match something.
    let bank_matches: Vec<&(&str, &str)> = split_options
        .iter()
        .filter(|(b, _)| b.to_lowercase().starts_with(&bank_lower))
        .collect();

    if bank_matches.is_empty() {
        return Err(format!("Unknown bank: {}", bank_part));
    }

    // Bank-only pattern: the bank prefix alone is enough.
    if account_part.is_empty() {
        return Ok(());
    }

    // Bank/Account — at least one bank-matching option must also have the
    // account prefix. (The same option must satisfy both — otherwise
    // "ING/Classic" would validate just because ING exists and NAB has a
    // Classic.)
    let any = bank_matches
        .iter()
        .any(|(_, a)| a.to_lowercase().starts_with(&account_lower));
    if any {
        Ok(())
    } else {
        Err(format!(
            "Unknown account: {} in {}",
            account_part, bank_part
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> AccountFilter {
        AccountFilter::new(vec![
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
    fn test_unknown_bank_returns_invalid() {
        match parse("NotABank") {
            FilterResult::Invalid(msg) => assert_eq!(msg, "Unknown bank: NotABank"),
            other => panic!("Expected Invalid, got {:?}", other),
        }
    }

    #[test]
    fn test_unknown_account_in_known_bank_returns_invalid() {
        match parse("ING/NotReal") {
            FilterResult::Invalid(msg) => assert_eq!(msg, "Unknown account: NotReal in ING"),
            other => panic!("Expected Invalid, got {:?}", other),
        }
    }

    #[test]
    fn test_unknown_account_only_returns_invalid() {
        match parse("/NotReal") {
            FilterResult::Invalid(msg) => assert_eq!(msg, "Unknown account: NotReal"),
            other => panic!("Expected Invalid, got {:?}", other),
        }
    }

    #[test]
    fn test_partial_bank_prefix_is_valid() {
        // "I" should still match "ING" — prefix semantics must survive.
        assert!(matches!(parse("I"), FilterResult::Valid { .. }));
    }

    #[test]
    fn test_case_insensitive_validation() {
        // Options use "ING"/"NAB" caps; user-typed "ing/orange" should validate.
        assert!(matches!(parse("ing/orange"), FilterResult::Valid { .. }));
    }

    #[test]
    fn test_empty_options_skips_validation() {
        // Fresh DB with no banks yet: don't paint every filter red.
        let f = AccountFilter::new(vec![]);
        assert!(matches!(
            f.parse("Anything/AtAll"),
            FilterResult::Valid { .. }
        ));
    }

    #[test]
    fn test_multi_pattern_first_invalid_fails_whole() {
        // "ING" is valid, "NotABank" is not — the whole filter fails with the
        // bad pattern's message.
        match parse("ING|NotABank") {
            FilterResult::Invalid(msg) => assert_eq!(msg, "Unknown bank: NotABank"),
            other => panic!("Expected Invalid, got {:?}", other),
        }
    }

    #[test]
    fn test_completions() {
        let f = filter();
        let (suggestions, anchor) = f.completions("ING", 3).unwrap();
        assert_eq!(anchor, 0);
        assert_eq!(
            suggestions,
            vec![
                "ING/Orange Everyday".to_string(),
                "ING/Savings Maximiser".to_string(),
                "NAB/Savings".to_string(),
            ]
        );
        assert!(suggestions.iter().any(|s| s.contains("ING")));
    }

    #[test]
    fn test_completions_multi_segment() {
        let f = filter();
        // Cursor at position 5 is at end of "N" in second segment
        let (suggestions, anchor) = f.completions("ING|N", 5).unwrap();
        assert_eq!(anchor, 4); // After the |
        assert_eq!(
            suggestions,
            vec![
                "NAB/Classic".to_string(),
                "NAB/Savings".to_string(),
                "ING/Orange Everyday".to_string(),
                "ING/Savings Maximiser".to_string(),
            ]
        );
        // Should prioritize NAB options for "N" prefix
        assert!(suggestions.iter().any(|s| s.starts_with("NAB")));
    }
}
