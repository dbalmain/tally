//! Filter trait and result types for pluggable search filters.

use rusqlite::types::Value;

/// Result of parsing a filter value.
#[derive(Debug, Clone)]
pub enum FilterResult {
    /// Valid filter, here's the SQL WHERE clause and parameters.
    Valid { sql: String, params: Vec<Value> },
    /// Invalid value, here's why.
    Invalid(String),
    /// Empty/incomplete value, ignore for now.
    Empty,
}

/// A pluggable filter that parses values and returns SQL.
///
/// Filters are stateful (may hold completion options) but `parse()` is pure.
/// Filters that need caching maintain internal state and expose `invalidate()` if needed.
pub trait Filter: Send + Sync {
    /// Canonical name used in search syntax (e.g., "date" for `date:2024`).
    fn name(&self) -> &'static str;

    /// Optional shortcut alias (e.g., "d" for `d:2024`).
    fn alias(&self) -> Option<&'static str> {
        None
    }

    /// Parse the value and return SQL if valid.
    ///
    /// The returned SQL should be a WHERE clause fragment using named
    /// placeholders that map to columns in a [`SqlContext`](super::SqlContext)
    /// — e.g., `{date} >= ? AND {date} <= ?`. The renderer substitutes
    /// `{date}` with the right column reference (`t.date`, `ft.date`, etc.)
    /// depending on the calling context, and silently drops clauses whose
    /// placeholders the context doesn't supply.
    fn parse(&self, value: &str) -> FilterResult;

    /// Provide completions for dropdown-style filters.
    ///
    /// Called when cursor is in this filter's value.
    ///
    /// # Arguments
    /// - `value`: full filter value (e.g., "income/sal|income/sales")
    /// - `cursor`: cursor position within the value (character offset)
    ///
    /// # Returns
    /// `Some((suggestions, anchor_offset))` where:
    /// - `suggestions`: list of completion options
    /// - `anchor_offset`: offset within value where popup should anchor (start of current segment)
    ///
    /// Returns `None` if no completions available (e.g., range filters like date/amount).
    fn completions(&self, _value: &str, _cursor: usize) -> Option<(Vec<String>, usize)> {
        None
    }
}
