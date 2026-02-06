//! Cursor context for context-aware key handling.

/// The context of the cursor position within a search query.
///
/// Used to determine how keys should behave based on where the cursor is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorContext {
    /// Cursor is within a filter value (e.g., `date:2024|` where `|` is cursor).
    Filter {
        /// The canonical filter name (e.g., "date", not "d").
        name: &'static str,
        /// Offset within the value portion (characters after the colon).
        offset: usize,
    },
    /// Cursor is within a regex pattern.
    Regex {
        /// Offset within the regex token.
        offset: usize,
    },
    /// Cursor is within FTS text.
    Fts {
        /// Offset within the FTS text.
        offset: usize,
    },
    /// Cursor is in whitespace between tokens.
    Whitespace,
}

impl CursorContext {
    /// Check if cursor is in a filter context.
    pub fn is_filter(&self) -> bool {
        matches!(self, CursorContext::Filter { .. })
    }

    /// Check if cursor is in FTS context.
    pub fn is_fts(&self) -> bool {
        matches!(self, CursorContext::Fts { .. })
    }

    /// Get the filter name if in a filter context.
    pub fn filter_name(&self) -> Option<&'static str> {
        match self {
            CursorContext::Filter { name, .. } => Some(name),
            _ => None,
        }
    }
}
