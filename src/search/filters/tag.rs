//! Tag filter implementation.

use rusqlite::types::Value;

use super::list::{complete_pipe_segments, parse_pipe_segments};
use crate::normalise_tag;
use crate::search::{Filter, FilterResult, placeholders as ph};

/// Filter for `#tags`.
///
/// The clause is an `EXISTS` subquery keyed on `{transaction_id}` alone, so it
/// needs no extra joins and works unchanged in every context that can identify
/// a transaction — including both sides of a transfer search.
///
/// Supports:
/// - `work` → tagged `#work`, or any `#work/…` sub-tag
/// - `work|travel` → either (OR)
/// - `none` / `any` → untagged / tagged at all
///
/// Values are matched leniently (a leading `#` is dropped, case is ignored), so
/// `tag:#Work` finds the stored `work`.
pub struct TagFilter {
    /// Known tag names, for autocomplete.
    pub options: Vec<String>,
}

impl TagFilter {
    pub fn new(options: Vec<String>) -> Self {
        Self { options }
    }
}

/// `EXISTS` over a transaction's tags, with `predicate` constraining `tg`.
fn tagged_where(predicate: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM transaction_tags tt \
         JOIN tags tg ON tg.id = tt.tag_id \
         WHERE tt.transaction_id = {} AND {predicate})",
        ph::reference(ph::TRANSACTION_ID)
    )
}

impl Filter for TagFilter {
    fn name(&self) -> &'static str {
        "tag"
    }

    fn parse(&self, value: &str) -> FilterResult {
        if value.is_empty() {
            return FilterResult::Empty;
        }

        if value.eq_ignore_ascii_case("any") {
            return FilterResult::Valid {
                sql: tagged_where("1"),
                params: Vec::new(),
            };
        }
        if value.eq_ignore_ascii_case("none") {
            return FilterResult::Valid {
                sql: format!("NOT {}", tagged_where("1")),
                params: Vec::new(),
            };
        }

        parse_pipe_segments(value, |segment| {
            let name = normalise_tag(segment);
            if name.is_empty() {
                return Err(format!("Invalid tag: {segment}"));
            }
            // A tag matches itself or anything beneath it, so `tag:work` covers
            // `#work/travel` the way `category:Food` covers `Food/Groceries`.
            Ok((
                tagged_where("(tg.name = ? OR tg.name LIKE ?)"),
                vec![Value::Text(name.clone()), Value::Text(format!("{name}/%"))],
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

    fn filter() -> TagFilter {
        TagFilter::new(vec![
            "work".to_string(),
            "work/travel".to_string(),
            "groceries".to_string(),
        ])
    }

    fn parse(value: &str) -> FilterResult {
        filter().parse(value)
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
    fn single_tag_matches_itself_or_a_sub_tag() {
        let (sql, params) = valid("work");
        assert!(sql.contains("tt.transaction_id = {transaction_id}"));
        assert!(sql.contains("(tg.name = ? OR tg.name LIKE ?)"));
        assert_eq!(
            params,
            vec![
                Value::Text("work".to_string()),
                Value::Text("work/%".to_string())
            ]
        );
    }

    #[test]
    fn values_are_matched_leniently() {
        // A typed `#` and mixed case both canonicalise away.
        assert_eq!(valid("#Work").1, valid("work").1);
    }

    #[test]
    fn multiple_tags_are_ored() {
        let (sql, params) = valid("work|groceries");
        assert!(sql.starts_with("(EXISTS"));
        assert!(sql.contains(" OR EXISTS"));
        assert_eq!(params.len(), 4);
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
    fn completions_rank_by_fuzzy_match() {
        let (suggestions, anchor) = filter().completions("wor", 3).unwrap();
        assert_eq!(anchor, 0);
        assert_eq!(suggestions[0], "work");
        assert!(suggestions.contains(&"work/travel".to_string()));
    }
}
