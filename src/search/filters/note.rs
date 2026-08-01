//! Note filter implementation.

use rusqlite::types::Value;

use crate::search::{Filter, FilterResult, placeholders as ph};

/// Filter over a transaction's free-form note.
///
/// Like [`super::TagFilter`], the clause is an `EXISTS` subquery keyed on
/// `{transaction_id}` alone, so it needs no extra joins and works in every
/// context that can identify a transaction.
///
/// Supports:
/// - `none` / `any` → without / with a note
/// - anything else → case-insensitive substring of the note text
///
/// Substring rather than full-text because it is the precise complement to bare
/// FTS words, which already search note text (notes are folded into
/// `transactions_fts`): `note:cba` finds that literal fragment, stemming and
/// word boundaries included.
pub struct NoteFilter;

/// `EXISTS` over a transaction's note, with `predicate` constraining `n`.
fn noted_where(predicate: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM transaction_notes n \
         WHERE n.transaction_id = {} AND {predicate})",
        ph::reference(ph::TRANSACTION_ID)
    )
}

impl Filter for NoteFilter {
    fn name(&self) -> &'static str {
        "note"
    }

    fn parse(&self, value: &str) -> FilterResult {
        if value.is_empty() {
            return FilterResult::Empty;
        }

        if value.eq_ignore_ascii_case("any") {
            return FilterResult::Valid {
                sql: noted_where("1"),
                params: Vec::new(),
            };
        }
        if value.eq_ignore_ascii_case("none") {
            return FilterResult::Valid {
                sql: format!("NOT {}", noted_where("1")),
                params: Vec::new(),
            };
        }

        let needle = value.to_lowercase().replace(['%', '_'], "");
        if needle.is_empty() {
            return FilterResult::Invalid(format!("Invalid note search: {value}"));
        }

        FilterResult::Valid {
            sql: noted_where("LOWER(n.note) LIKE ?"),
            params: vec![Value::Text(format!("%{needle}%"))],
        }
    }

    fn completions(&self, value: &str, _cursor: usize) -> Option<(Vec<String>, usize)> {
        // Only the presence keywords are suggestible; note text is free-form.
        let candidates = ["any", "none"];
        let suggestions: Vec<String> = candidates
            .iter()
            .filter(|candidate| candidate.starts_with(&value.to_lowercase()))
            .map(|candidate| (*candidate).to_string())
            .collect();
        (!suggestions.is_empty()).then_some((suggestions, 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> FilterResult {
        NoteFilter.parse(value)
    }

    fn valid(value: &str) -> (String, Vec<Value>) {
        match parse(value) {
            FilterResult::Valid { sql, params } => (sql, params),
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn empty_value_is_empty() {
        assert!(matches!(parse(""), FilterResult::Empty));
    }

    #[test]
    fn none_and_any_select_by_presence() {
        let (none_sql, none_params) = valid("none");
        assert!(none_sql.starts_with("NOT EXISTS"));
        assert!(none_params.is_empty());

        let (any_sql, any_params) = valid("any");
        assert!(any_sql.starts_with("EXISTS"));
        assert!(any_params.is_empty());
    }

    #[test]
    fn text_becomes_a_case_insensitive_substring_match() {
        let (sql, params) = valid("Acme");
        assert!(sql.contains("n.transaction_id = {transaction_id}"));
        assert!(sql.contains("LOWER(n.note) LIKE ?"));
        assert_eq!(params, vec![Value::Text("%acme%".to_string())]);
    }

    #[test]
    fn like_wildcards_in_the_value_are_stripped() {
        // Otherwise a stray `%` would silently widen the match to everything.
        assert_eq!(valid("a%c_e").1, vec![Value::Text("%ace%".to_string())]);
        assert!(matches!(parse("%%"), FilterResult::Invalid(_)));
    }

    #[test]
    fn completions_offer_the_presence_keywords() {
        let (suggestions, anchor) = NoteFilter.completions("n", 1).unwrap();
        assert_eq!(anchor, 0);
        assert_eq!(suggestions, vec!["none".to_string()]);
        assert!(NoteFilter.completions("zzz", 3).is_none());
    }
}
