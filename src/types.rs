use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Transaction data from import script output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawTransaction {
    pub date: String,
    pub description: String,
    pub amount_cents: i64,
    pub balance_cents: i64,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Imported transaction from a bank account.
#[derive(Debug, Clone)]
pub struct Transaction {
    pub id: i64,
    pub bank_id: i64,
    pub account_id: i64,
    pub date: NaiveDate,
    pub description: String,
    pub amount_cents: i64,
    pub balance_cents: i64,
    pub hash: String,
    pub metadata: HashMap<String, serde_json::Value>,
    pub source_file: String,
    pub import_batch_id: i64,
}

/// Bank institution.
#[derive(Debug, Clone)]
pub struct Bank {
    pub id: i64,
    pub name: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Bank account.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: i64,
    pub bank_id: i64,
    pub name: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Tracked CSV import file metadata.
#[derive(Debug, Clone)]
pub struct ImportedFile {
    pub id: i64,
    pub account_id: i64,
    pub path: String,
    pub content_hash: String,
    pub imported_at: DateTime<Utc>,
    pub import_batch_id: i64,
}

/// Summary of changes from a refresh operation.
#[derive(Debug, Default)]
pub struct RefreshReport {
    pub banks_added: usize,
    pub banks_deleted: usize,
    pub accounts_added: usize,
    pub accounts_deleted: usize,
    pub files_processed: usize,
    pub transactions_added: usize,
    pub transactions_skipped: usize,
}

/// Confirmed category training example used by the local classifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedCategoryExample {
    pub description: String,
    pub amount_cents: i64,
    pub date: NaiveDate,
    pub category_id: i64,
    pub category_path: String,
}

/// Confirmed transfer training example used by local transfer detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedTransferExample {
    pub from_account_id: i64,
    pub to_account_id: i64,
    pub from_description: String,
    pub to_description: String,
}

/// Hierarchical category (e.g., "Food/Groceries").
#[derive(Debug, Clone)]
pub struct Category {
    pub id: i64,
    pub path: String,
    pub created_at: DateTime<Utc>,
}

/// How a category was assigned to a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategorySource {
    Manual,
    Ai,
    Rule,
}

impl CategorySource {
    /// Convert to string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            CategorySource::Manual => "manual",
            CategorySource::Ai => "ai",
            CategorySource::Rule => "rule",
        }
    }
}

impl std::str::FromStr for CategorySource {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "manual" => Ok(CategorySource::Manual),
            "ai" => Ok(CategorySource::Ai),
            "rule" => Ok(CategorySource::Rule),
            _ => Err(()),
        }
    }
}

/// Category and metadata enrichment for a transaction.
#[derive(Debug, Clone)]
pub struct TransactionEnrichment {
    pub id: i64,
    pub transaction_id: i64,
    pub category_id: Option<i64>,
    pub category_source: Option<CategorySource>,
    pub category_confirmed: bool,
    pub ai_confidence: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Whether a category-bearing filter may overwrite an existing enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOverride {
    /// Only categorise transactions that have no enrichment at all.
    Uncategorised,
    /// Categorise uncategorised transactions and overwrite AI suggestions.
    Ai,
    /// Overwrite any enrichment (manual, AI, or rule).
    All,
}

impl FilterOverride {
    /// Convert to string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            FilterOverride::Uncategorised => "uncategorised",
            FilterOverride::Ai => "ai",
            FilterOverride::All => "all",
        }
    }
}

impl std::str::FromStr for FilterOverride {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "uncategorised" => Ok(FilterOverride::Uncategorised),
            "ai" => Ok(FilterOverride::Ai),
            "all" => Ok(FilterOverride::All),
            _ => Err(()),
        }
    }
}

/// A saved search. When it carries a `category_id`, matching transactions are
/// auto-categorised by `TransactionStore::apply_filters`.
#[derive(Debug, Clone)]
pub struct Filter {
    pub id: i64,
    pub name: String,
    pub query: String,
    pub category_id: Option<i64>,
    pub override_mode: FilterOverride,
    pub review_required: bool,
    pub position: i64,
    pub created_at: DateTime<Utc>,
}

/// How a transfer between transactions was detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferSource {
    Manual,
    Auto,
}

impl TransferSource {
    /// Convert to string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            TransferSource::Manual => "manual",
            TransferSource::Auto => "auto",
        }
    }
}

impl std::str::FromStr for TransferSource {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "manual" => Ok(TransferSource::Manual),
            "auto" => Ok(TransferSource::Auto),
            _ => Err(()),
        }
    }
}

/// Link between two transactions identified as a transfer.
#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: i64,
    pub from_transaction_id: i64,
    pub to_transaction_id: i64,
    pub source: TransferSource,
    pub confirmed: bool,
    pub ai_confidence: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// Transaction with its category enrichment.
#[derive(Debug, Clone)]
pub struct TransactionWithEnrichment {
    pub transaction: Transaction,
    pub enrichment: Option<TransactionEnrichment>,
    pub category: Option<Category>,
}

/// Transfer with both resolved transactions.
#[derive(Debug, Clone)]
pub struct TransferWithTransactions {
    pub transfer: Transfer,
    pub from_transaction: Transaction,
    pub to_transaction: Transaction,
}
