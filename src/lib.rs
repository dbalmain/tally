//! Tally: Personal finance transaction aggregator with local SQLite storage.

pub mod db;
pub mod error;
pub mod import;
pub mod search;
pub mod store;
pub mod tui;
pub mod types;

pub use error::{Error, Result};
pub use search::{DbSearchQuery, DbTextMatch, FuzzyMatcher};
pub use store::TransactionStore;
pub use types::*;
