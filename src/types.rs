use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

#[derive(Debug, Clone)]
pub struct Bank {
    pub id: i64,
    pub name: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct Account {
    pub id: i64,
    pub bank_id: i64,
    pub name: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ImportedFile {
    pub id: i64,
    pub account_id: i64,
    pub path: String,
    pub content_hash: String,
    pub imported_at: DateTime<Utc>,
    pub import_batch_id: i64,
}

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

#[derive(Debug, Default, Clone)]
pub struct TransactionFilter {
    pub bank_id: Option<i64>,
    pub account_id: Option<i64>,
    pub from_date: Option<NaiveDate>,
    pub to_date: Option<NaiveDate>,
    pub description_contains: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Category {
    pub id: i64,
    pub path: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CategorySource {
    Manual,
    Ai,
    Rule,
}

impl CategorySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            CategorySource::Manual => "manual",
            CategorySource::Ai => "ai",
            CategorySource::Rule => "rule",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(CategorySource::Manual),
            "ai" => Some(CategorySource::Ai),
            "rule" => Some(CategorySource::Rule),
            _ => None,
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferSource {
    Manual,
    Auto,
}

impl TransferSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            TransferSource::Manual => "manual",
            TransferSource::Auto => "auto",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "manual" => Some(TransferSource::Manual),
            "auto" => Some(TransferSource::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Transfer {
    pub id: i64,
    pub from_transaction_id: i64,
    pub to_transaction_id: i64,
    pub source: TransferSource,
    pub confirmed: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct TransactionWithEnrichment {
    pub transaction: Transaction,
    pub enrichment: Option<TransactionEnrichment>,
    pub category: Option<Category>,
}

#[derive(Debug, Clone)]
pub struct TransferWithTransactions {
    pub transfer: Transfer,
    pub from_transaction: Transaction,
    pub to_transaction: Transaction,
}
