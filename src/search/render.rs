//! SQL rendering for parsed queries.
//!
//! Filters declare their SQL using named placeholders like `{date}` or
//! `{bank_name}`. A [`SqlContext`] maps those placeholders to real SQL
//! references at the call site, so the same filter SQL can be reused across
//! different table aliases (transactions, transfer from-side, transfer to-side).
//!
//! [`ParsedQuery::render`](crate::search::ParsedQuery::render) walks the parsed
//! parts, substitutes placeholders, and returns a [`Rendered`] WHERE fragment
//! with parameters. Filter parts whose placeholders are not supported by the
//! context are dropped (e.g. category filters dropped on transfer queries).

use std::collections::HashMap;

use rusqlite::types::Value;

use super::FilterResult;
use super::query::{ParsedQuery, QueryPart};

/// Maps placeholder names like `"date"` to SQL column references like `"t.date"`.
///
/// A SQL template containing `{date}` will be substituted with `t.date`.
/// Templates referencing an unknown placeholder are reported as unsupported by
/// [`SqlContext::render_template`] and dropped by [`ParsedQuery::render`].
#[derive(Debug, Clone, Default)]
pub struct SqlContext {
    columns: HashMap<&'static str, String>,
}

impl SqlContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `{name}` in templates to the given SQL fragment.
    pub fn with(mut self, name: &'static str, sql: impl Into<String>) -> Self {
        self.columns.insert(name, sql.into());
        self
    }

    /// Substitute `{name}` placeholders in `template`. Returns `None` if any
    /// placeholder is missing from the context (signal to drop the clause).
    pub fn render_template(&self, template: &str) -> Option<String> {
        let mut out = String::with_capacity(template.len());
        let mut chars = template.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '{' {
                out.push(c);
                continue;
            }
            // Allow literal '{' via "{{".
            if chars.peek() == Some(&'{') {
                chars.next();
                out.push('{');
                continue;
            }
            let mut name = String::new();
            let mut closed = false;
            for nc in chars.by_ref() {
                if nc == '}' {
                    closed = true;
                    break;
                }
                name.push(nc);
            }
            if !closed {
                // Unterminated placeholder — treat as literal to avoid
                // silent SQL corruption.
                out.push('{');
                out.push_str(&name);
                continue;
            }
            let sql = self.columns.get(name.as_str())?;
            out.push_str(sql);
        }
        Some(out)
    }
}

/// Result of rendering a [`ParsedQuery`] against a [`SqlContext`].
///
/// Internal to the crate: store.rs consumes this directly via `and_prefix()`
/// and `.params`; nothing outside the workspace needs to name it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Rendered {
    /// WHERE fragment, e.g. `"t.date >= ? AND t.date <= ?"`. Empty if there are
    /// no applicable clauses.
    pub(crate) where_clause: String,
    /// Bound parameters in order matching `?` placeholders in `where_clause`.
    pub(crate) params: Vec<Value>,
}

impl Rendered {
    pub(crate) fn is_empty(&self) -> bool {
        self.where_clause.is_empty()
    }

    /// Return the WHERE clause prefixed with " AND " for splicing into an
    /// existing WHERE expression. Empty if there are no clauses.
    pub(crate) fn and_prefix(&self) -> String {
        if self.where_clause.is_empty() {
            String::new()
        } else {
            format!(" AND {}", self.where_clause)
        }
    }
}

impl ParsedQuery {
    /// Render the parsed query against a context.
    ///
    /// - [`QueryPart::Filter`] with a `Valid` result is rendered if all its
    ///   placeholders are supported; otherwise dropped.
    /// - [`QueryPart::Regex`] (valid) renders as `regexp(?, {description})` if
    ///   `{description}` is supported.
    /// - [`QueryPart::Fts`] substitutes `{fts_match}` if the context provides
    ///   it. The substituted SQL is expected to contain exactly one `?` for
    ///   the FTS pattern (e.g. `"transactions_fts MATCH ?"` for a single-table
    ///   query, or a side-scoped subquery for a join).
    pub(crate) fn render(&self, ctx: &SqlContext) -> Rendered {
        let mut clauses = Vec::new();
        let mut params = Vec::new();
        self.render_into(ctx, &mut clauses, &mut params);
        Rendered {
            where_clause: clauses.join(" AND "),
            params,
        }
    }

    /// Render a transfer query: once against the from-side context, once
    /// against the to-side context, OR the results together.
    ///
    /// A transfer matches if EITHER side satisfies the query. Each side renders
    /// independently against its own table aliases; FTS uses the per-side
    /// `{fts_match}` form so the subquery scopes to that side. Parameters are
    /// appended in order: lhs then rhs.
    pub(crate) fn render_transfers(&self, lhs: &SqlContext, rhs: &SqlContext) -> Rendered {
        let l = self.render(lhs);
        let r = self.render(rhs);
        match (l.is_empty(), r.is_empty()) {
            (true, true) => Rendered::default(),
            (false, true) => l,
            (true, false) => r,
            (false, false) => {
                let mut params = l.params;
                params.extend(r.params);
                Rendered {
                    where_clause: format!("(({}) OR ({}))", l.where_clause, r.where_clause),
                    params,
                }
            }
        }
    }

    fn render_into(&self, ctx: &SqlContext, clauses: &mut Vec<String>, params: &mut Vec<Value>) {
        for part in &self.parts {
            match part {
                QueryPart::Filter {
                    result:
                        FilterResult::Valid {
                            sql,
                            params: filter_params,
                        },
                    ..
                } => {
                    let Some(rendered) = ctx.render_template(sql) else {
                        continue;
                    };
                    clauses.push(rendered);
                    params.extend(filter_params.iter().cloned());
                }
                QueryPart::Regex {
                    pattern,
                    valid: true,
                    ..
                } => {
                    let Some(rendered) = ctx.render_template("regexp(?, {description})") else {
                        continue;
                    };
                    clauses.push(rendered);
                    params.push(Value::Text(pattern.clone()));
                }
                QueryPart::Fts {
                    query, valid: true, ..
                } if !query.is_empty() => {
                    let Some(rendered) = ctx.render_template("{fts_match}") else {
                        continue;
                    };
                    clauses.push(rendered);
                    params.push(Value::Text(query.clone()));
                }
                _ => {}
            }
        }
    }

    /// Returns true if any filter part references the given placeholder.
    ///
    /// Useful for deciding whether to add an optional JOIN (e.g. category).
    pub fn uses_placeholder(&self, placeholder: &str) -> bool {
        self.parts.iter().any(|p| match p {
            QueryPart::Filter {
                result: FilterResult::Valid { sql, .. },
                ..
            } => template_uses(sql, placeholder),
            QueryPart::Regex { valid: true, .. } => placeholder == "description",
            QueryPart::Fts {
                query, valid: true, ..
            } if !query.is_empty() => placeholder == "fts_match",
            _ => false,
        })
    }
}

fn template_uses(template: &str, placeholder: &str) -> bool {
    let needle_open = format!("{{{}", placeholder);
    let mut rest = template;
    while let Some(idx) = rest.find(&needle_open) {
        let after = &rest[idx + needle_open.len()..];
        if after.starts_with('}') {
            return true;
        }
        rest = &rest[idx + needle_open.len()..];
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::query::Span;

    fn ctx_full() -> SqlContext {
        SqlContext::new()
            .with("date", "t.date")
            .with("amount_cents", "t.amount_cents")
            .with("description", "t.description")
            .with("bank_name", "b.name")
            .with("account_name", "a.name")
            .with("category_path", "c.path")
            .with("fts_match", "transactions_fts MATCH ?")
    }

    fn make_filter(sql: &str, params: Vec<Value>) -> QueryPart {
        QueryPart::Filter {
            name: "date",
            value: String::new(),
            result: FilterResult::Valid {
                sql: sql.to_string(),
                params,
            },
            span: Span::new(0, 0),
            value_span: Span::new(0, 0),
        }
    }

    #[test]
    fn render_template_substitutes_placeholders() {
        let ctx = ctx_full();
        assert_eq!(
            ctx.render_template("{date} >= ? AND {amount_cents} = ?")
                .unwrap(),
            "t.date >= ? AND t.amount_cents = ?"
        );
    }

    #[test]
    fn render_template_unknown_placeholder_returns_none() {
        let ctx = SqlContext::new().with("date", "t.date");
        assert!(
            ctx.render_template("{date} > ? AND {amount_cents} > ?")
                .is_none()
        );
    }

    #[test]
    fn render_template_escapes_double_open_brace() {
        // `{{` → literal `{` so callers can write "{x} = {{value}}" if they
        // really need a brace in the output. `}` is unambiguous on its own
        // and is passed through verbatim.
        let ctx = SqlContext::new().with("d", "t.date");
        assert_eq!(ctx.render_template("{{x}} {d}").unwrap(), "{x}} t.date");
    }

    #[test]
    fn render_template_unterminated_placeholder_is_literal() {
        let ctx = SqlContext::new().with("d", "t.date");
        // Unterminated `{date` — preserved as-is rather than panicking.
        assert_eq!(ctx.render_template("{d} {date").unwrap(), "t.date {date");
    }

    #[test]
    fn render_drops_filter_with_missing_placeholder() {
        let ctx = SqlContext::new().with("date", "t.date");
        let query = ParsedQuery {
            parts: vec![
                make_filter("{date} >= ?", vec![Value::Text("2024-01-01".into())]),
                make_filter("{category_path} LIKE ?", vec![Value::Text("Food".into())]),
            ],
        };
        let r = query.render(&ctx);
        assert_eq!(r.where_clause, "t.date >= ?");
        assert_eq!(r.params.len(), 1);
    }

    #[test]
    fn render_joins_clauses_with_and() {
        let ctx = ctx_full();
        let query = ParsedQuery {
            parts: vec![
                make_filter("{date} >= ?", vec![Value::Text("2024".into())]),
                QueryPart::Whitespace {
                    span: Span::new(0, 0),
                },
                make_filter("{amount_cents} > ?", vec![Value::Integer(100)]),
            ],
        };
        let r = query.render(&ctx);
        assert_eq!(r.where_clause, "t.date >= ? AND t.amount_cents > ?");
        assert_eq!(r.params.len(), 2);
    }

    #[test]
    fn render_empty_query_yields_empty() {
        let ctx = ctx_full();
        let r = ParsedQuery::empty().render(&ctx);
        assert!(r.is_empty());
        assert!(r.and_prefix().is_empty());
    }

    #[test]
    fn render_regex_uses_description_placeholder() {
        let ctx = SqlContext::new().with("description", "t.description");
        let query = ParsedQuery {
            parts: vec![QueryPart::Regex {
                original: "/foo/".into(),
                pattern: "foo".into(),
                valid: true,
                span: Span::new(0, 0),
            }],
        };
        let r = query.render(&ctx);
        assert_eq!(r.where_clause, "regexp(?, t.description)");
        assert_eq!(r.params, vec![Value::Text("foo".into())]);
    }

    #[test]
    fn render_regex_dropped_without_description_placeholder() {
        let ctx = SqlContext::new().with("date", "t.date");
        let query = ParsedQuery {
            parts: vec![QueryPart::Regex {
                original: "/foo/".into(),
                pattern: "foo".into(),
                valid: true,
                span: Span::new(0, 0),
            }],
        };
        let r = query.render(&ctx);
        assert!(r.is_empty());
    }

    #[test]
    fn render_fts_emits_match() {
        let ctx = SqlContext::new().with("fts_match", "transactions_fts MATCH ?");
        let query = ParsedQuery {
            parts: vec![QueryPart::Fts {
                original: "coffee".into(),
                query: "coffee*".into(),
                valid: true,
                span: Span::new(0, 0),
            }],
        };
        let r = query.render(&ctx);
        assert_eq!(r.where_clause, "transactions_fts MATCH ?");
        assert_eq!(r.params, vec![Value::Text("coffee*".into())]);
    }

    #[test]
    fn render_fts_dropped_without_fts_placeholder() {
        let ctx = SqlContext::new().with("date", "t.date");
        let query = ParsedQuery {
            parts: vec![QueryPart::Fts {
                original: "coffee".into(),
                query: "coffee*".into(),
                valid: true,
                span: Span::new(0, 0),
            }],
        };
        let r = query.render(&ctx);
        assert!(r.is_empty());
    }

    #[test]
    fn render_transfers_combines_with_or() {
        let lhs = SqlContext::new().with("date", "ft.date");
        let rhs = SqlContext::new().with("date", "tt.date");
        let query = ParsedQuery {
            parts: vec![make_filter(
                "{date} >= ?",
                vec![Value::Text("2024-01-01".into())],
            )],
        };
        let r = query.render_transfers(&lhs, &rhs);
        assert_eq!(r.where_clause, "((ft.date >= ?) OR (tt.date >= ?))");
        assert_eq!(r.params.len(), 2);
        assert_eq!(r.params[0], Value::Text("2024-01-01".into()));
        assert_eq!(r.params[1], Value::Text("2024-01-01".into()));
    }

    #[test]
    fn render_transfers_empty_yields_empty() {
        let lhs = SqlContext::new();
        let rhs = SqlContext::new();
        let r = ParsedQuery::empty().render_transfers(&lhs, &rhs);
        assert!(r.is_empty());
    }

    #[test]
    fn render_transfers_falls_back_when_one_side_empty() {
        // If only the lhs supports the placeholder, result is just the lhs clause
        // (no spurious OR with empty side).
        let lhs = SqlContext::new().with("date", "t.date");
        let rhs = SqlContext::new();
        let query = ParsedQuery {
            parts: vec![make_filter(
                "{date} >= ?",
                vec![Value::Text("2024-01-01".into())],
            )],
        };
        let r = query.render_transfers(&lhs, &rhs);
        assert_eq!(r.where_clause, "t.date >= ?");
        assert_eq!(r.params.len(), 1);
    }

    #[test]
    fn render_transfers_uses_side_scoped_fts() {
        // Each side's context provides its own FTS predicate (typically a
        // subquery scoped to that side's transactions). render_transfers should
        // OR those two side-scoped predicates without any special-casing here.
        let lhs = SqlContext::new().with(
            "fts_match",
            "ft.id IN (SELECT rowid FROM transactions_fts WHERE transactions_fts MATCH ?)",
        );
        let rhs = SqlContext::new().with(
            "fts_match",
            "tt.id IN (SELECT rowid FROM transactions_fts WHERE transactions_fts MATCH ?)",
        );
        let query = ParsedQuery {
            parts: vec![QueryPart::Fts {
                original: "coffee".into(),
                query: "coffee*".into(),
                valid: true,
                span: Span::new(0, 0),
            }],
        };
        let r = query.render_transfers(&lhs, &rhs);
        assert_eq!(
            r.where_clause,
            "((ft.id IN (SELECT rowid FROM transactions_fts WHERE transactions_fts MATCH ?)) \
             OR (tt.id IN (SELECT rowid FROM transactions_fts WHERE transactions_fts MATCH ?)))"
        );
        assert_eq!(
            r.params,
            vec![Value::Text("coffee*".into()), Value::Text("coffee*".into())]
        );
    }

    #[test]
    fn uses_placeholder_detects_filter_template_reference() {
        let query = ParsedQuery {
            parts: vec![make_filter("{category_path} LIKE ?", vec![])],
        };
        assert!(query.uses_placeholder("category_path"));
        assert!(!query.uses_placeholder("date"));
    }

    #[test]
    fn uses_placeholder_detects_fts_and_description() {
        let query = ParsedQuery {
            parts: vec![
                QueryPart::Fts {
                    original: "x".into(),
                    query: "x".into(),
                    valid: true,
                    span: Span::new(0, 0),
                },
                QueryPart::Regex {
                    original: "/x/".into(),
                    pattern: "x".into(),
                    valid: true,
                    span: Span::new(0, 0),
                },
            ],
        };
        assert!(query.uses_placeholder("fts_match"));
        assert!(query.uses_placeholder("description"));
    }

    #[test]
    fn uses_placeholder_does_not_match_prefixes() {
        // "{date}" should not match a search for "dat".
        let query = ParsedQuery {
            parts: vec![make_filter("{date} >= ?", vec![])],
        };
        assert!(query.uses_placeholder("date"));
        assert!(!query.uses_placeholder("dat"));
    }
}
