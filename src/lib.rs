//! Tally: Personal finance transaction aggregator with local SQLite storage.

pub mod classify;
pub mod config;
pub mod db;
pub mod error;
pub mod import;
pub mod logging;
pub mod search;
pub mod store;
pub mod tui;
pub mod types;

pub use error::{Error, Result};
pub use search::FuzzyMatcher;
pub use store::TransactionStore;
pub use types::*;
