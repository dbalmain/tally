//! Search system with pluggable filters and context-aware parsing.
//!
//! This module provides a clean, extensible search system with:
//! - Simple tokenization (filters, regex, FTS)
//! - Trait-based pluggable filters that return SQL directly
//! - Context-aware key handling based on cursor position
//!
//! # Query Syntax
//!
//! - **Filters**: `name:value` where value has no whitespace
//!   - `date:2024`, `date:2024-01`, `date:2024-01..2024-06`
//!   - `amount:100`, `amount:>100`, `amount:50..200`
//!   - `account:ING/Orange`, `account:ING|NAB`
//!   - `category:Food`, `category:Food|Transport`
//!
//! - **Regex**: `/pattern/flags` (e.g., `/coffee.*/i`)
//!
//! - **FTS**: Everything else is full-text search
//!   - `groceries` - simple term
//!   - `coffee shop` - implicit AND
//!   - `coffee OR tea` - native FTS5 OR
//!   - `"exact phrase"` - phrase match
//!
//! - **Transition**: Type `~` at a word boundary to switch to fuzzy mode

mod context;
mod filter;
pub mod filters;
mod fuzzy;
mod parse;
mod query;
mod render;
mod tokenize;

pub use context::CursorContext;
pub use filter::{Filter, FilterResult};
pub use filters::{AccountFilter, AmountFilter, CategoryFilter, DateFilter};
pub use parse::{SearchConfig, parse};
pub use query::{ParsedQuery, QueryPart, Span};
pub use render::{Rendered, SqlContext};
pub use tokenize::{RawToken, tokenize};

pub use fuzzy::FuzzyMatcher;
