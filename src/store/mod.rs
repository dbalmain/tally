//! `TransactionStore`: all database operations.
//!
//! This module owns the column-array ↔ row-parser contract: each `*_COLS`
//! list below must stay in the exact order its `parse_*_at_offset` /
//! `parse_*` counterpart reads columns. The store methods themselves live in
//! focused submodules, each contributing an `impl TransactionStore` block.

mod categories;
mod enrichments;
mod filters;
mod import;
mod queries;
#[cfg(test)]
mod test_support;
mod transfers;

use chrono::{NaiveDate, Utc};
use rusqlite::{Connection, Row, types::Value};
use std::path::{Path, PathBuf};

use crate::db::init_db;
use crate::search::{ParsedQuery, SqlContext, placeholders as ph};
use crate::{Category, Filter, Result, Transaction, TransactionEnrichment, Transfer};

/// Column list for selecting a full `Transaction` from `transactions_view`
/// under `alias`. Order must match `parse_transaction_at_offset`.
const TX_COLS: [&str; 11] = [
    "id",
    "bank_id",
    "account_id",
    "date",
    "description",
    "amount_cents",
    "balance_cents",
    "hash",
    "metadata",
    "source_file",
    "import_batch_id",
];
const TX_COL_COUNT: usize = TX_COLS.len();

fn tx_cols(alias: &str) -> String {
    TX_COLS.map(|c| format!("{alias}.{c}")).join(", ")
}

/// Column list for selecting a full `TransactionEnrichment` under `alias`.
/// Order must match `parse_enrichment_at_offset`.
const ENRICHMENT_COLS: [&str; 8] = [
    "id",
    "transaction_id",
    "category_id",
    "category_source",
    "category_confirmed",
    "ai_confidence",
    "created_at",
    "updated_at",
];
const ENRICHMENT_COL_COUNT: usize = ENRICHMENT_COLS.len();

fn enrichment_cols(alias: &str) -> String {
    ENRICHMENT_COLS.map(|c| format!("{alias}.{c}")).join(", ")
}

/// Column list for selecting a full `Category` under `alias`.
/// Order must match `parse_category_at_offset`.
const CATEGORY_COLS: [&str; 3] = ["id", "path", "created_at"];

fn category_cols(alias: &str) -> String {
    CATEGORY_COLS.map(|c| format!("{alias}.{c}")).join(", ")
}

/// Column list for selecting a full `Transfer` under `alias`.
/// Order must match `parse_transfer`.
const TRANSFER_COLS: [&str; 7] = [
    "id",
    "from_transaction_id",
    "to_transaction_id",
    "source",
    "confirmed",
    "created_at",
    "ai_confidence",
];
const TRANSFER_COL_COUNT: usize = TRANSFER_COLS.len();

fn transfer_cols(alias: &str) -> String {
    TRANSFER_COLS.map(|c| format!("{alias}.{c}")).join(", ")
}

/// Column list for selecting a full `Filter`.
/// Order must match `parse_filter`.
const FILTER_COLS: &str =
    "id, name, query, category_id, override_mode, review_required, position, created_at";

/// SQL context for queries rooted at `transactions_view t` (with optional
/// `categories c` and `transactions_fts fts` joins).
const SEARCHABLE_TRANSACTION_COLUMNS: [(&str, &str); 5] = [
    (ph::DATE, "date"),
    (ph::AMOUNT_CENTS, "amount_cents"),
    (ph::DESCRIPTION, "description"),
    (ph::BANK_NAME, "bank_name"),
    (ph::ACCOUNT_NAME, "account_name"),
];

fn aliased_transaction_ctx(alias: &str) -> SqlContext {
    let mut ctx = SqlContext::new();
    for (placeholder, column) in SEARCHABLE_TRANSACTION_COLUMNS {
        ctx = ctx.with(placeholder, format!("{alias}.{column}"));
    }
    ctx
}

fn transaction_ctx() -> SqlContext {
    aliased_transaction_ctx("t")
        .with(ph::CATEGORY_PATH, "c.path")
        .with(ph::FTS_MATCH, "transactions_fts MATCH ?")
}

/// SQL context for one side of a transfer query, rooted at
/// `transactions_view <alias>`.
///
/// Filters render against the given alias (so we can render once for the
/// from-side, once for the to-side). The FTS clause uses a side-scoped
/// subquery rather than a top-level JOIN, because each transfer pair has two
/// transactions and we want a match on either side to qualify the row.
fn transfer_side_ctx(alias: &str) -> SqlContext {
    aliased_transaction_ctx(alias).with(
        ph::FTS_MATCH,
        format!(
            "{alias}.id IN (SELECT rowid FROM transactions_fts WHERE transactions_fts MATCH ?)"
        ),
    )
}

/// Enrichment/transfer joins backing the Todo-tab queries (uncategorised and
/// unconfirmed transactions).
const TODO_TAB_JOINS: &str = " LEFT JOIN transaction_enrichments e ON t.id = e.transaction_id
             LEFT JOIN transfers tr ON t.id = tr.from_transaction_id OR t.id = tr.to_transaction_id";

fn transaction_fts_join(parsed: &ParsedQuery) -> &'static str {
    if parsed.fts_query().is_some() {
        " JOIN transactions_fts fts ON t.id = fts.rowid"
    } else {
        ""
    }
}

fn transaction_category_join(parsed: &ParsedQuery, enrichment_joined: bool) -> String {
    if !parsed.uses_placeholder(ph::CATEGORY_PATH) {
        return String::new();
    }

    if enrichment_joined {
        " LEFT JOIN categories c ON e.category_id = c.id".to_string()
    } else {
        " LEFT JOIN transaction_enrichments e ON t.id = e.transaction_id\
         \n LEFT JOIN categories c ON e.category_id = c.id"
            .to_string()
    }
}

/// Joins to splice into a transaction query based on what the parsed query needs.
fn transaction_joins(parsed: &ParsedQuery) -> String {
    let mut joins = String::new();
    joins.push_str(transaction_fts_join(parsed));
    joins.push_str(&transaction_category_join(parsed, false));
    joins
}

/// Append `LIMIT ?` and its parameter when a limit is requested.
fn push_limit(sql: &mut String, params: &mut Vec<Value>, limit: Option<usize>) {
    if let Some(limit) = limit {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(limit as i64));
    }
}

pub struct TransactionStore {
    conn: Connection,
    exports_dir: PathBuf,
}

impl TransactionStore {
    /// Open or create the database at the given path.
    pub fn open(db_path: &Path, exports_dir: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        init_db(&conn)?;
        Ok(Self {
            conn,
            exports_dir: exports_dir.to_path_buf(),
        })
    }

    /// Open an in-memory database for testing.
    pub fn open_in_memory(exports_dir: &Path) -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        init_db(&conn)?;
        Ok(Self {
            conn,
            exports_dir: exports_dir.to_path_buf(),
        })
    }
}

fn parse_datetime(s: &str) -> rusqlite::Result<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

fn parse_category(row: &Row) -> rusqlite::Result<Category> {
    parse_category_at_offset(row, 0)
}

fn parse_category_at_offset(row: &Row, offset: usize) -> rusqlite::Result<Category> {
    Ok(Category {
        id: row.get(offset)?,
        path: row.get(offset + 1)?,
        created_at: parse_datetime(&row.get::<_, String>(offset + 2)?)?,
    })
}

fn parse_transaction(row: &Row) -> rusqlite::Result<Transaction> {
    parse_transaction_at_offset(row, 0)
}

fn parse_transaction_at_offset(row: &Row, offset: usize) -> rusqlite::Result<Transaction> {
    let metadata_str: String = row.get(offset + 8)?;
    let metadata: std::collections::HashMap<String, serde_json::Value> =
        serde_json::from_str(&metadata_str).unwrap_or_default();
    let date_str: String = row.get(offset + 3)?;
    let date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            offset + 3,
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    })?;

    Ok(Transaction {
        id: row.get(offset)?,
        bank_id: row.get(offset + 1)?,
        account_id: row.get(offset + 2)?,
        date,
        description: row.get(offset + 4)?,
        amount_cents: row.get(offset + 5)?,
        balance_cents: row.get(offset + 6)?,
        hash: row.get(offset + 7)?,
        metadata,
        source_file: row.get(offset + 9)?,
        import_batch_id: row.get(offset + 10)?,
    })
}

fn parse_enrichment_at_offset(row: &Row, offset: usize) -> rusqlite::Result<TransactionEnrichment> {
    Ok(TransactionEnrichment {
        id: row.get(offset)?,
        transaction_id: row.get(offset + 1)?,
        category_id: row.get(offset + 2)?,
        category_source: row
            .get::<_, Option<String>>(offset + 3)?
            .and_then(|s| s.parse().ok()),
        category_confirmed: row.get::<_, i32>(offset + 4)? != 0,
        ai_confidence: row.get(offset + 5)?,
        created_at: parse_datetime(&row.get::<_, String>(offset + 6)?)?,
        updated_at: parse_datetime(&row.get::<_, String>(offset + 7)?)?,
    })
}

fn parse_transfer(row: &rusqlite::Row) -> rusqlite::Result<Transfer> {
    let source_str: String = row.get(3)?;
    let source = source_str.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("invalid transfer source: {}", source_str).into(),
        )
    })?;
    Ok(Transfer {
        id: row.get(0)?,
        from_transaction_id: row.get(1)?,
        to_transaction_id: row.get(2)?,
        source,
        confirmed: row.get::<_, i32>(4)? != 0,
        created_at: parse_datetime(&row.get::<_, String>(5)?)?,
        ai_confidence: row.get(6)?,
    })
}

fn parse_filter(row: &rusqlite::Row) -> rusqlite::Result<Filter> {
    let override_str: String = row.get(4)?;
    let override_mode = override_str.parse().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            format!("invalid filter override: {}", override_str).into(),
        )
    })?;
    Ok(Filter {
        id: row.get(0)?,
        name: row.get(1)?,
        query: row.get(2)?,
        category_id: row.get(3)?,
        override_mode,
        review_required: row.get::<_, i32>(5)? != 0,
        position: row.get(6)?,
        created_at: parse_datetime(&row.get::<_, String>(7)?)?,
    })
}
