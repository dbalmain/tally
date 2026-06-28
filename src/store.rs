use chrono::{NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params, types::Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::db::{build_searchable_text, init_db};
use crate::import::{
    compute_hash, find_csv_files, find_import_script, find_pull_script, hash_file,
    run_import_script, run_pull_script,
};
use crate::search::{ParsedQuery, SearchConfig, SqlContext, parse, placeholders as ph};
use crate::{
    Account, Bank, Category, CategorySource, ConfirmedCategoryExample, ConfirmedTransferExample,
    Error, Filter, FilterOverride, FuzzyMatcher, RawTransaction, RefreshReport, Result,
    Transaction, TransactionEnrichment, TransactionWithEnrichment, Transfer, TransferSource,
    TransferWithTransactions,
};

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

const PULL_CONCURRENCY: usize = 6;

type PullResults = HashMap<(String, String), Result<Vec<RawTransaction>>>;

struct PullJob {
    bank_name: String,
    account_name: String,
    script: PathBuf,
    account_dir: PathBuf,
}

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

const TODO_TRANSACTION_JOINS: &str = " LEFT JOIN transaction_enrichments e ON t.id = e.transaction_id
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

    /// Scan exports directory and import all new transactions.
    pub fn refresh(&mut self) -> Result<RefreshReport> {
        let mut report = RefreshReport::default();
        let discovered = self.discover_banks_and_accounts()?;
        let pull_jobs = self.collect_pull_jobs(&discovered);
        let mut pulled = Self::run_pull_jobs(&pull_jobs)?;

        // Wrap entire import in a transaction for performance
        self.conn.execute("BEGIN", [])?;

        let result = self.refresh_inner(&mut report, &discovered, &mut pulled);

        match result {
            Ok(()) => {
                self.conn.execute("COMMIT", [])?;
                Ok(report)
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    fn refresh_inner(
        &mut self,
        report: &mut RefreshReport,
        discovered: &[(String, Vec<String>)],
        pulled: &mut PullResults,
    ) -> Result<()> {
        let batch_id = self.create_import_batch()?;

        for (bank_name, account_names) in discovered {
            let bank_id = self.ensure_bank(bank_name, report)?;

            for account_name in account_names {
                let account_id = self.ensure_account(bank_id, account_name, report)?;
                let pulled = pulled.remove(&(bank_name.clone(), account_name.clone()));

                self.import_account_transactions(
                    account_id,
                    bank_name,
                    account_name,
                    batch_id,
                    report,
                    pulled,
                )?;
            }
        }

        self.soft_delete_missing_banks(discovered, report)?;
        self.soft_delete_missing_accounts(discovered, report)?;

        self.complete_import_batch(batch_id)?;

        Ok(())
    }

    fn collect_pull_jobs(&self, discovered: &[(String, Vec<String>)]) -> Vec<PullJob> {
        let mut jobs = Vec::new();
        for (bank_name, account_names) in discovered {
            for account_name in account_names {
                if let Some(script) = find_pull_script(&self.exports_dir, bank_name, account_name) {
                    jobs.push(PullJob {
                        bank_name: bank_name.clone(),
                        account_name: account_name.clone(),
                        script,
                        account_dir: self.exports_dir.join(bank_name).join(account_name),
                    });
                }
            }
        }
        jobs
    }

    fn run_pull_jobs(jobs: &[PullJob]) -> Result<PullResults> {
        let mut pulled = HashMap::new();

        for chunk in jobs.chunks(PULL_CONCURRENCY) {
            std::thread::scope(|scope| {
                let handles = chunk
                    .iter()
                    .map(|job| {
                        scope.spawn(move || {
                            let transactions = run_pull_script(&job.script, &job.account_dir);
                            (
                                (job.bank_name.clone(), job.account_name.clone()),
                                transactions,
                            )
                        })
                    })
                    .collect::<Vec<_>>();

                for handle in handles {
                    let (key, transactions) = handle.join().map_err(|_| {
                        Error::ImportFailed("pull script worker panicked".to_string())
                    })?;
                    pulled.insert(key, transactions);
                }

                Ok::<(), Error>(())
            })?;
        }

        Ok(pulled)
    }

    /// List all non-deleted banks.
    pub fn list_banks(&self) -> Result<Vec<Bank>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, deleted_at FROM banks WHERE deleted_at IS NULL ORDER BY name",
        )?;
        let banks = stmt
            .query_map([], |row| {
                Ok(Bank {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    deleted_at: row
                        .get::<_, Option<String>>(2)?
                        .map(|s| parse_datetime(&s))
                        .transpose()?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(banks)
    }

    /// List all non-deleted accounts for a bank.
    pub fn list_accounts(&self, bank_id: i64) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, bank_id, name, deleted_at FROM accounts 
             WHERE bank_id = ? AND deleted_at IS NULL ORDER BY name",
        )?;
        let accounts = stmt
            .query_map([bank_id], |row| {
                Ok(Account {
                    id: row.get(0)?,
                    bank_id: row.get(1)?,
                    name: row.get(2)?,
                    deleted_at: row
                        .get::<_, Option<String>>(3)?
                        .map(|s| parse_datetime(&s))
                        .transpose()?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(accounts)
    }

    /// Query transactions matching a parsed search query.
    ///
    /// `limit` is `None` for unbounded queries.
    pub fn query_transactions(
        &self,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<Transaction>> {
        self.query_transactions_where(query, "", "", false, "query_transactions", limit)
    }

    fn query_transactions_where(
        &self,
        query: &ParsedQuery,
        extra_joins: &str,
        extra_where: &str,
        extra_joins_include_enrichment: bool,
        debug_label: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Transaction>> {
        let mut sql = format!("SELECT {} FROM transactions_view t", tx_cols("t"));
        if extra_joins.is_empty() {
            sql.push_str(&transaction_joins(query));
        } else {
            sql.push_str(transaction_fts_join(query));
            sql.push_str(extra_joins);
            sql.push_str(&transaction_category_join(
                query,
                extra_joins_include_enrichment,
            ));
        }
        sql.push_str(" WHERE t.account_deleted_at IS NULL");
        sql.push_str(extra_where);

        let rendered = query.render(&transaction_ctx());
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY t.date DESC, t.id DESC");
        push_limit(&mut sql, &mut params, limit);

        log::debug!("{debug_label} SQL: {} params: {:?}", sql, params);

        let mut stmt = self.conn.prepare(&sql)?;
        let transactions = stmt
            .query_map(rusqlite::params_from_iter(params), parse_transaction)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(transactions)
    }

    fn create_import_batch(&self) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO import_batches (started_at) VALUES (?)",
            [Utc::now().to_rfc3339()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn complete_import_batch(&self, batch_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE import_batches SET completed_at = ? WHERE id = ?",
            params![Utc::now().to_rfc3339(), batch_id],
        )?;
        Ok(())
    }

    fn discover_banks_and_accounts(&self) -> Result<Vec<(String, Vec<String>)>> {
        let mut result = Vec::new();

        for entry in std::fs::read_dir(&self.exports_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let bank_name = entry.file_name().to_string_lossy().to_string();
            let bank_path = entry.path();

            let mut accounts = Vec::new();
            for account_entry in std::fs::read_dir(&bank_path)? {
                let account_entry = account_entry?;
                if !account_entry.file_type()?.is_dir() {
                    continue;
                }
                let account_name = account_entry.file_name().to_string_lossy().to_string();
                accounts.push(account_name);
            }

            if !accounts.is_empty() {
                result.push((bank_name, accounts));
            }
        }

        Ok(result)
    }

    fn ensure_bank(&self, name: &str, report: &mut RefreshReport) -> Result<i64> {
        let existing: Option<(i64, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, deleted_at FROM banks WHERE name = ?",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match existing {
            Some((id, Some(_))) => {
                self.conn
                    .execute("UPDATE banks SET deleted_at = NULL WHERE id = ?", [id])?;
                Ok(id)
            }
            Some((id, None)) => Ok(id),
            None => {
                self.conn
                    .execute("INSERT INTO banks (name) VALUES (?)", [name])?;
                report.banks_added += 1;
                Ok(self.conn.last_insert_rowid())
            }
        }
    }

    fn ensure_account(&self, bank_id: i64, name: &str, report: &mut RefreshReport) -> Result<i64> {
        let existing: Option<(i64, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, deleted_at FROM accounts WHERE bank_id = ? AND name = ?",
                params![bank_id, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        match existing {
            Some((id, Some(_))) => {
                self.conn
                    .execute("UPDATE accounts SET deleted_at = NULL WHERE id = ?", [id])?;
                Ok(id)
            }
            Some((id, None)) => Ok(id),
            None => {
                self.conn.execute(
                    "INSERT INTO accounts (bank_id, name) VALUES (?, ?)",
                    params![bank_id, name],
                )?;
                report.accounts_added += 1;
                Ok(self.conn.last_insert_rowid())
            }
        }
    }

    fn import_account_transactions(
        &mut self,
        account_id: i64,
        bank_name: &str,
        account_name: &str,
        batch_id: i64,
        report: &mut RefreshReport,
        pulled: Option<Result<Vec<RawTransaction>>>,
    ) -> Result<()> {
        let account_dir = self.exports_dir.join(bank_name).join(account_name);

        // CSV drop import: parse each unseen CSV with the account's import script.
        if let Some(script) = find_import_script(&self.exports_dir, bank_name, account_name) {
            let csv_files = find_csv_files(&account_dir)?;

            for csv_file in csv_files {
                let relative_path = csv_file
                    .strip_prefix(&self.exports_dir)
                    .unwrap_or(&csv_file)
                    .to_string_lossy()
                    .to_string();

                let content_hash = hash_file(&csv_file)?;

                if self.is_file_imported(account_id, &content_hash)? {
                    continue;
                }

                let transactions = run_import_script(&script, &csv_file)?;
                report.files_processed += 1;
                self.insert_raw_transactions(
                    account_id,
                    transactions,
                    &relative_path,
                    batch_id,
                    report,
                )?;

                self.mark_file_imported(account_id, &relative_path, &content_hash, batch_id)?;
            }
        }

        // Pull import: fetch transactions directly from an external source. The
        // pull script owns incremental windowing; we rely on the
        // (account_id, hash) uniqueness constraint to dedupe re-pulled overlap.
        if let Some(script) = find_pull_script(&self.exports_dir, bank_name, account_name) {
            let relative_path = script
                .strip_prefix(&self.exports_dir)
                .unwrap_or(&script)
                .to_string_lossy()
                .to_string();

            if let Some(transactions) = pulled {
                let transactions = transactions?;
                report.files_processed += 1;
                self.insert_raw_transactions(
                    account_id,
                    transactions,
                    &relative_path,
                    batch_id,
                    report,
                )?;
            }
        }

        Ok(())
    }

    /// Insert a batch of raw transactions, computing a fallback hash and
    /// tallying added/skipped counts in `report`.
    fn insert_raw_transactions(
        &self,
        account_id: i64,
        transactions: Vec<RawTransaction>,
        source_file: &str,
        batch_id: i64,
        report: &mut RefreshReport,
    ) -> Result<()> {
        for raw_tx in transactions {
            let date = parse_date(&raw_tx.date)?;
            let hash = raw_tx.hash.clone().unwrap_or_else(|| {
                compute_hash(
                    &raw_tx.date,
                    &raw_tx.description,
                    raw_tx.amount_cents,
                    raw_tx.balance_cents,
                )
            });

            let inserted = self.insert_transaction(
                account_id,
                &date,
                &raw_tx.description,
                raw_tx.amount_cents,
                raw_tx.balance_cents,
                &hash,
                &raw_tx.metadata,
                source_file,
                batch_id,
            )?;

            if inserted {
                report.transactions_added += 1;
            } else {
                report.transactions_skipped += 1;
            }
        }

        Ok(())
    }

    fn is_file_imported(&self, account_id: i64, content_hash: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM imported_files WHERE account_id = ? AND content_hash = ?",
            params![account_id, content_hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    fn mark_file_imported(
        &self,
        account_id: i64,
        path: &str,
        content_hash: &str,
        batch_id: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO imported_files (account_id, path, content_hash, imported_at, import_batch_id) 
             VALUES (?, ?, ?, ?, ?)",
            params![account_id, path, content_hash, Utc::now().to_rfc3339(), batch_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_transaction(
        &self,
        account_id: i64,
        date: &NaiveDate,
        description: &str,
        amount_cents: i64,
        balance_cents: i64,
        hash: &str,
        metadata: &std::collections::HashMap<String, serde_json::Value>,
        source_file: &str,
        batch_id: i64,
    ) -> Result<bool> {
        let metadata_json = serde_json::to_string(metadata)?;
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO transactions 
             (account_id, date, description, amount_cents, balance_cents, hash, metadata, source_file, import_batch_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                account_id,
                date.to_string(),
                description,
                amount_cents,
                balance_cents,
                hash,
                metadata_json,
                source_file,
                batch_id
            ],
        )?;

        if result > 0 {
            // Insert into FTS index
            let rowid = self.conn.last_insert_rowid();
            let searchable_text = build_searchable_text(description, metadata);
            self.conn.execute(
                "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
                params![rowid, searchable_text],
            )?;
        }

        Ok(result > 0)
    }

    fn soft_delete_missing_banks(
        &self,
        discovered: &[(String, Vec<String>)],
        report: &mut RefreshReport,
    ) -> Result<()> {
        let discovered_names: Vec<&str> =
            discovered.iter().map(|(name, _)| name.as_str()).collect();

        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM banks WHERE deleted_at IS NULL")?;
        let existing: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        for (id, name) in existing {
            if !discovered_names.contains(&name.as_str()) {
                self.conn.execute(
                    "UPDATE banks SET deleted_at = ? WHERE id = ?",
                    params![Utc::now().to_rfc3339(), id],
                )?;
                report.banks_deleted += 1;
            }
        }

        Ok(())
    }

    fn soft_delete_missing_accounts(
        &self,
        discovered: &[(String, Vec<String>)],
        report: &mut RefreshReport,
    ) -> Result<()> {
        for (bank_name, account_names) in discovered {
            let bank_id: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM banks WHERE name = ? AND deleted_at IS NULL",
                    [bank_name],
                    |row| row.get(0),
                )
                .optional()?;

            let bank_id = match bank_id {
                Some(id) => id,
                None => continue,
            };

            let mut stmt = self.conn.prepare(
                "SELECT id, name FROM accounts WHERE bank_id = ? AND deleted_at IS NULL",
            )?;
            let existing: Vec<(i64, String)> = stmt
                .query_map([bank_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;

            for (id, name) in existing {
                if !account_names.contains(&name) {
                    self.conn.execute(
                        "UPDATE accounts SET deleted_at = ? WHERE id = ?",
                        params![Utc::now().to_rfc3339(), id],
                    )?;
                    report.accounts_deleted += 1;
                }
            }
        }

        Ok(())
    }

    // ==================== Categories ====================

    /// List all categories in path order.
    pub fn list_categories(&self) -> Result<Vec<Category>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, created_at FROM categories ORDER BY path")?;
        let categories = stmt
            .query_map([], parse_category)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(categories)
    }

    /// Find categories matching a fuzzy query.
    pub fn find_categories(&self, query: &str) -> Result<Vec<Category>> {
        let all = self.list_categories()?;
        let mut matcher = FuzzyMatcher::new();
        let mut scored: Vec<(u32, Category)> = all
            .into_iter()
            .filter_map(|cat| matcher.score(query, &cat.path).map(|score| (score, cat)))
            .collect();
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        Ok(scored.into_iter().map(|(_, cat)| cat).collect())
    }

    /// Get or create a category by path.
    pub fn get_or_create_category(&mut self, path: &str) -> Result<i64> {
        let normalised = path.trim().trim_matches('/');
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM categories WHERE path = ?",
                [normalised],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }

        self.conn.execute(
            "INSERT INTO categories (path, created_at) VALUES (?, ?)",
            params![normalised, Utc::now().to_rfc3339()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get a category by ID.
    pub fn get_category(&self, id: i64) -> Result<Option<Category>> {
        self.conn
            .query_row(
                "SELECT id, path, created_at FROM categories WHERE id = ?",
                [id],
                parse_category,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Get the category assigned to a transaction.
    pub fn get_transaction_category(&self, transaction_id: i64) -> Result<Option<Category>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {}
                     FROM categories c
                 JOIN transaction_enrichments e ON c.id = e.category_id
                     WHERE e.transaction_id = ?",
                    category_cols("c")
                ),
                [transaction_id],
                parse_category,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Get categories for multiple transactions in bulk.
    /// Returns a map of transaction_id -> category_path.
    pub fn get_categories_for_transactions(
        &self,
        transaction_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, String>> {
        use std::collections::HashMap;

        if transaction_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let placeholders = transaction_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT e.transaction_id, c.path 
             FROM transaction_enrichments e
             JOIN categories c ON c.id = e.category_id
             WHERE e.transaction_id IN ({})",
            placeholders
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> = transaction_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut result = HashMap::new();
        for row in rows {
            let (tx_id, path) = row?;
            result.insert(tx_id, path);
        }
        Ok(result)
    }

    // ==================== Enrichments ====================

    /// Set or update the category for a transaction.
    pub fn set_category(
        &mut self,
        transaction_id: i64,
        category_id: i64,
        source: CategorySource,
        confirmed: bool,
        ai_confidence: Option<f64>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO transaction_enrichments 
             (transaction_id, category_id, category_source, category_confirmed, ai_confidence, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(transaction_id) DO UPDATE SET
                category_id = excluded.category_id,
                category_source = excluded.category_source,
                category_confirmed = excluded.category_confirmed,
                ai_confidence = excluded.ai_confidence,
                updated_at = excluded.updated_at",
            params![
                transaction_id,
                category_id,
                source.as_str(),
                confirmed,
                ai_confidence,
                now,
                now
            ],
        )?;
        Ok(())
    }

    /// Mark a category as user-confirmed.
    pub fn confirm_category(&mut self, transaction_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE transaction_enrichments SET category_confirmed = 1, updated_at = ? WHERE transaction_id = ?",
            params![Utc::now().to_rfc3339(), transaction_id],
        )?;
        Ok(())
    }

    /// Remove a transaction's enrichment entirely (category + AI metadata).
    pub fn delete_enrichment(&mut self, transaction_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM transaction_enrichments WHERE transaction_id = ?",
            [transaction_id],
        )?;
        Ok(())
    }

    // ==================== Filters ====================

    /// List all saved filters, ordered for display and apply precedence.
    pub fn list_filters(&self) -> Result<Vec<Filter>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, query, category_id, override_mode, review_required, position, created_at
             FROM filters
             ORDER BY position ASC, id ASC",
        )?;
        let filters = stmt
            .query_map([], parse_filter)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(filters)
    }

    /// Create a filter (no category, no override) appended after the last one.
    pub fn create_filter(&mut self, name: &str, query: &str) -> Result<i64> {
        let position: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM filters",
            [],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO filters (name, query, override_mode, review_required, position, created_at)
             VALUES (?, ?, 'uncategorised', 0, ?, ?)",
            params![name, query, position, Utc::now().to_rfc3339()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Rename a filter.
    pub fn rename_filter(&mut self, id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE filters SET name = ? WHERE id = ?",
            params![name, id],
        )?;
        Ok(())
    }

    /// Replace a filter's search query.
    pub fn set_filter_query(&mut self, id: i64, query: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE filters SET query = ? WHERE id = ?",
            params![query, id],
        )?;
        Ok(())
    }

    /// Set (or clear) the category a filter auto-applies.
    pub fn set_filter_category(&mut self, id: i64, category_id: Option<i64>) -> Result<()> {
        self.conn.execute(
            "UPDATE filters SET category_id = ? WHERE id = ?",
            params![category_id, id],
        )?;
        Ok(())
    }

    /// Set a filter's override mode.
    pub fn set_filter_override(&mut self, id: i64, mode: FilterOverride) -> Result<()> {
        self.conn.execute(
            "UPDATE filters SET override_mode = ? WHERE id = ?",
            params![mode.as_str(), id],
        )?;
        Ok(())
    }

    /// Set whether a filter's applied categories require review (unconfirmed).
    pub fn set_filter_review(&mut self, id: i64, review: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE filters SET review_required = ? WHERE id = ?",
            params![review, id],
        )?;
        Ok(())
    }

    /// Delete a filter.
    pub fn delete_filter(&mut self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM filters WHERE id = ?", [id])?;
        Ok(())
    }

    /// Auto-categorise transactions matching category-bearing filters.
    ///
    /// Shared entry point for `tally classify` and (later) the TUI. Rule-sourced
    /// unconfirmed enrichments are wiped first so the result is a pure function
    /// of the current filter set; confirmed enrichments are never deleted.
    /// Filters apply in order with first-match-wins per transaction, and never
    /// touch a transfer leg. Returns the number of transactions categorised.
    pub fn apply_filters(&mut self) -> Result<usize> {
        Ok(self.apply_filters_inner()?.len())
    }

    /// Dry-run of [`apply_filters`]: return the transactions it would
    /// (re)categorise, in apply order, leaving the database unchanged. Runs the
    /// real apply inside a savepoint and rolls back, so the leading delete of
    /// unconfirmed rule rows and every override check match production exactly.
    pub fn preview_filters(&mut self) -> Result<Vec<Transaction>> {
        self.conn.execute_batch("SAVEPOINT preview_filters")?;
        let result = self.apply_filters_inner();
        self.conn
            .execute_batch("ROLLBACK TO preview_filters; RELEASE preview_filters")?;
        result
    }

    /// Shared body of [`apply_filters`] / [`preview_filters`]: re-derive rule
    /// categories and return the affected transactions in apply order.
    fn apply_filters_inner(&mut self) -> Result<Vec<Transaction>> {
        // Snapshot the existing rule categories before clearing the unconfirmed
        // ones, so re-running a review-required filter that lands the same
        // category on the same transaction is reported as a no-op rather than a
        // fresh change. Without this, the delete-then-reinsert below would make
        // every re-apply re-report rows it had already categorised.
        let prior_rule = self.rule_categories()?;

        self.conn.execute(
            "DELETE FROM transaction_enrichments
             WHERE category_source = 'rule' AND category_confirmed = 0",
            [],
        )?;

        let config = SearchConfig::standard(Vec::new(), None);
        let mut claimed: HashSet<i64> = HashSet::new();
        let mut applied = Vec::new();

        for filter in self.list_filters()? {
            let Some(category_id) = filter.category_id else {
                continue;
            };
            let parsed = parse(&config, &filter.query, 0).0;
            for tx in self.query_transactions(&parsed, None)? {
                if !claimed.insert(tx.id) {
                    continue;
                }
                if self.get_transfer_for_transaction(tx.id)?.is_some() {
                    continue;
                }
                if self.filter_applies(category_id, filter.override_mode, tx.id)? {
                    self.set_category(
                        tx.id,
                        category_id,
                        CategorySource::Rule,
                        !filter.review_required,
                        None,
                    )?;
                    // Only a genuine change counts as "applied": skip rows that
                    // already carried this exact rule category.
                    if prior_rule.get(&tx.id) != Some(&category_id) {
                        applied.push(tx);
                    }
                }
            }
        }
        Ok(applied)
    }

    /// Map of transaction id → category id for every rule-sourced enrichment,
    /// used to tell genuine (re)categorisations apart from no-op re-applies.
    fn rule_categories(&self) -> Result<HashMap<i64, i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT transaction_id, category_id
             FROM transaction_enrichments
             WHERE category_source = 'rule' AND category_id IS NOT NULL",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        Ok(rows)
    }

    /// Decide whether a filter may categorise `tx_id` given its override mode
    /// and the transaction's existing enrichment.
    fn filter_applies(&self, category_id: i64, mode: FilterOverride, tx_id: i64) -> Result<bool> {
        let Some((source, confirmed, existing_category)) = self.get_enrichment_meta(tx_id)? else {
            return Ok(true);
        };
        // Preserve a user's confirmation of this very category across runs.
        if confirmed && existing_category == Some(category_id) {
            return Ok(false);
        }
        Ok(match mode {
            FilterOverride::Uncategorised => false,
            FilterOverride::Ai => source == Some(CategorySource::Ai),
            FilterOverride::All => true,
        })
    }

    /// Enrichment source, confirmed flag, and category id for a transaction.
    fn get_enrichment_meta(&self, tx_id: i64) -> Result<Option<EnrichmentMeta>> {
        self.conn
            .query_row(
                "SELECT category_source, category_confirmed, category_id
                 FROM transaction_enrichments WHERE transaction_id = ?",
                [tx_id],
                |row| {
                    let source = row
                        .get::<_, Option<String>>(0)?
                        .and_then(|s| s.parse().ok());
                    Ok((
                        source,
                        row.get::<_, bool>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Return all user-confirmed category assignments as classifier examples.
    pub fn get_confirmed_examples(&self) -> Result<Vec<ConfirmedCategoryExample>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.description, t.amount_cents, t.date, c.id, c.path
             FROM transactions_view t
             JOIN transaction_enrichments e ON t.id = e.transaction_id
             JOIN categories c ON c.id = e.category_id
             WHERE e.category_confirmed = 1
             ORDER BY t.id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let examples = rows
            .into_iter()
            .map(
                |(description, amount_cents, date, category_id, category_path)| {
                    Ok(ConfirmedCategoryExample {
                        description,
                        amount_cents,
                        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d")
                            .map_err(|_| Error::InvalidDate(date))?,
                        category_id,
                        category_path,
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?;
        Ok(examples)
    }

    /// Return all user-confirmed transfers as transfer-detection examples.
    pub fn get_confirmed_transfer_examples(&self) -> Result<Vec<ConfirmedTransferExample>> {
        let mut stmt = self.conn.prepare(
            "SELECT ft.account_id, tt.account_id, ft.description, tt.description
             FROM transfers tr
             JOIN transactions_view ft ON ft.id = tr.from_transaction_id
             JOIN transactions_view tt ON tt.id = tr.to_transaction_id
             WHERE tr.confirmed = 1
               AND ft.account_deleted_at IS NULL
               AND tt.account_deleted_at IS NULL
             ORDER BY tr.id",
        )?;
        let examples = stmt
            .query_map([], |row| {
                Ok(ConfirmedTransferExample {
                    from_account_id: row.get(0)?,
                    to_account_id: row.get(1)?,
                    from_description: row.get(2)?,
                    to_description: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(examples)
    }

    /// Rename a category. Returns error if new name already exists.
    pub fn rename_category(&mut self, category_id: i64, new_path: &str) -> Result<()> {
        let normalised = new_path.trim().trim_matches('/');

        // Check if target name already exists
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM categories WHERE path = ? AND id != ?",
                params![normalised, category_id],
                |row| row.get(0),
            )
            .optional()?;

        if existing.is_some() {
            return Err(Error::CategoryExists(normalised.to_string()));
        }

        self.conn.execute(
            "UPDATE categories SET path = ? WHERE id = ?",
            params![normalised, category_id],
        )?;
        Ok(())
    }

    /// Merge source category into target category.
    /// Moves all transactions from source to target, then deletes source.
    pub fn merge_categories(&mut self, source_id: i64, target_id: i64) -> Result<()> {
        // Move all enrichments from source category to target category
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE transaction_enrichments SET category_id = ?, updated_at = ? WHERE category_id = ?",
            params![target_id, now, source_id],
        )?;

        // Delete the source category
        self.conn
            .execute("DELETE FROM categories WHERE id = ?", [source_id])?;

        Ok(())
    }

    /// Delete a category, dropping its enrichments so those transactions fall
    /// back to uncategorised. Returns the number of transactions that lost
    /// their category.
    pub fn delete_category(&mut self, category_id: i64) -> Result<usize> {
        let affected = self.conn.execute(
            "DELETE FROM transaction_enrichments WHERE category_id = ?",
            [category_id],
        )?;
        self.conn
            .execute("DELETE FROM categories WHERE id = ?", [category_id])?;
        Ok(affected)
    }

    /// Get category by path.
    pub fn get_category_by_path(&self, path: &str) -> Result<Option<Category>> {
        let normalised = path.trim().trim_matches('/');
        self.conn
            .query_row(
                "SELECT id, path, created_at FROM categories WHERE path = ?",
                [normalised],
                parse_category,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Count transactions in a category.
    pub fn count_transactions_in_category(&self, category_id: i64) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM transaction_enrichments WHERE category_id = ?",
            [category_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Get transaction counts for every category that has at least one
    /// enrichment, in a single query. Categories with zero transactions
    /// are absent from the returned map.
    pub fn get_category_transaction_counts(&self) -> Result<std::collections::HashMap<i64, usize>> {
        use std::collections::HashMap;
        let mut stmt = self.conn.prepare(
            "SELECT category_id, COUNT(*) FROM transaction_enrichments GROUP BY category_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        let mut counts = HashMap::new();
        for row in rows {
            let (id, count) = row?;
            counts.insert(id, count);
        }
        Ok(counts)
    }

    // ==================== Transfers ====================

    /// Create a transfer linking two transactions.
    pub fn create_transfer(
        &mut self,
        from_transaction_id: i64,
        to_transaction_id: i64,
        source: TransferSource,
        confirmed: bool,
        confidence: Option<f64>,
    ) -> Result<i64> {
        // Invariant: a transaction is either part of a transfer or categorised,
        // never both. Marking a transfer clears any category enrichment on both
        // endpoints (a no-op for the uncategorised transactions AI detection
        // picks, and the deliberate behaviour for a manual mark).
        self.conn.execute(
            "DELETE FROM transaction_enrichments WHERE transaction_id IN (?, ?)",
            params![from_transaction_id, to_transaction_id],
        )?;
        self.conn.execute(
            "INSERT INTO transfers
             (from_transaction_id, to_transaction_id, source, confirmed, ai_confidence, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                from_transaction_id,
                to_transaction_id,
                source.as_str(),
                confirmed,
                confidence,
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Mark a transfer as user-confirmed.
    pub fn confirm_transfer(&mut self, transfer_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE transfers SET confirmed = 1 WHERE id = ?",
            [transfer_id],
        )?;
        Ok(())
    }

    /// Delete a transfer.
    pub fn delete_transfer(&mut self, transfer_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM transfers WHERE id = ?", [transfer_id])?;
        Ok(())
    }

    /// Get the transfer (if any) involving a transaction.
    pub fn get_transfer_for_transaction(&self, transaction_id: i64) -> Result<Option<Transfer>> {
        let sql = format!(
            "SELECT {}
             FROM transfers tr
             WHERE tr.from_transaction_id = ? OR tr.to_transaction_id = ?",
            transfer_cols("tr")
        );
        self.conn
            .query_row(
                &sql,
                params![transaction_id, transaction_id],
                parse_transfer,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Map each of `transaction_ids` that is part of a transfer to that
    /// transfer. Both endpoints of a matching transfer are inserted, so a row
    /// can resolve its own link whether it is the "from" or "to" side. Mirrors
    /// [`Self::get_categories_for_transactions`] so per-transaction caches can
    /// be rebuilt from the DB rather than from a separately-loaded list.
    pub fn get_transfers_for_transactions(
        &self,
        transaction_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Transfer>> {
        use std::collections::HashMap;

        if transaction_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let placeholders = transaction_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {}
             FROM transfers tr
             WHERE tr.from_transaction_id IN ({placeholders})
                OR tr.to_transaction_id IN ({placeholders})",
            transfer_cols("tr")
        );

        let mut stmt = self.conn.prepare(&sql)?;
        // The placeholder list appears twice (from_… and to_…), so bind the ids
        // twice in order.
        let params: Vec<&dyn rusqlite::ToSql> = transaction_ids
            .iter()
            .chain(transaction_ids.iter())
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt.query_map(params.as_slice(), parse_transfer)?;

        let mut result = HashMap::new();
        for row in rows {
            let transfer = row?;
            result.insert(transfer.from_transaction_id, transfer.clone());
            result.insert(transfer.to_transaction_id, transfer);
        }
        Ok(result)
    }

    /// Find potential matching transactions for a transfer.
    ///
    /// Prefers candidates in *other* accounts (the common transfer case). Only
    /// if there are none does it fall back to the same account (rebates,
    /// refunds, internal corrections).
    pub fn find_matching_transfer_candidates(&self, tx: &Transaction) -> Result<Vec<Transaction>> {
        let candidates = self.transfer_candidates(tx, true)?;
        if candidates.is_empty() {
            self.transfer_candidates(tx, false)
        } else {
            Ok(candidates)
        }
    }

    fn transfer_candidates(
        &self,
        tx: &Transaction,
        exclude_same_account: bool,
    ) -> Result<Vec<Transaction>> {
        let opposite_amount = -tx.amount_cents;
        let date_str = tx.date.to_string();
        let same_account_clause = if exclude_same_account {
            " AND t.account_id != ?"
        } else {
            ""
        };
        // Transactions already involved in a transfer are intentionally NOT
        // excluded: the caller offers to break the existing link (with
        // confirmation) when the chosen candidate is already linked.
        let sql = format!(
            "SELECT {}
             FROM transactions_view t
             WHERE t.amount_cents = ?{}
               AND t.id != ?
               AND t.account_deleted_at IS NULL
             ORDER BY ABS(julianday(t.date) - julianday(?)), t.id",
            tx_cols("t"),
            same_account_clause
        );

        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&opposite_amount];
        if exclude_same_account {
            params.push(&tx.account_id);
        }
        params.push(&tx.id);
        params.push(&date_str);

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params.as_slice(), parse_transaction)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ==================== Todo Queries ====================

    /// Get transactions that need categorization, scoped by a parsed search query.
    pub fn get_uncategorised_transactions(
        &self,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<Transaction>> {
        self.query_transactions_where(
            query,
            TODO_TRANSACTION_JOINS,
            " AND (e.category_id IS NULL OR e.id IS NULL)
               AND tr.id IS NULL",
            true,
            "get_uncategorised_transactions",
            limit,
        )
    }

    /// Transactions eligible for (re)categorisation: no enrichment, or an
    /// enrichment that is not user-confirmed. Excludes transfer legs.
    pub fn get_unconfirmed_transactions(
        &self,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<Transaction>> {
        self.query_transactions_where(
            query,
            TODO_TRANSACTION_JOINS,
            " AND (e.id IS NULL OR e.category_id IS NULL OR e.category_confirmed = 0)
               AND tr.id IS NULL",
            true,
            "get_unconfirmed_transactions",
            limit,
        )
    }

    /// Get transactions with AI-suggested categories pending review.
    pub fn get_pending_ai_reviews(
        &self,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<TransactionWithEnrichment>> {
        let mut sql = format!(
            "SELECT {}, {}, {} FROM transactions_view t",
            tx_cols("t"),
            enrichment_cols("e"),
            category_cols("c"),
        );
        sql.push_str(transaction_fts_join(query));
        // Enrichment is required (we filter on it) and category is needed for SELECT.
        sql.push_str(
            " JOIN transaction_enrichments e ON t.id = e.transaction_id
             LEFT JOIN categories c ON e.category_id = c.id",
        );
        sql.push_str(
            " WHERE t.account_deleted_at IS NULL
               AND e.category_source = 'ai'
               AND e.category_confirmed = 0",
        );

        let rendered = query.render(&transaction_ctx());
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY t.date DESC, t.id DESC");
        push_limit(&mut sql, &mut params, limit);

        log::debug!("get_pending_ai_reviews SQL: {} params: {:?}", sql, params);

        let mut stmt = self.conn.prepare(&sql)?;
        let results = stmt
            .query_map(rusqlite::params_from_iter(params), |row| {
                let transaction = parse_transaction(row)?;
                let enrichment = Some(parse_enrichment_at_offset(row, TX_COL_COUNT)?);
                let category_offset = TX_COL_COUNT + ENRICHMENT_COL_COUNT;
                let category = if row.get::<_, Option<i64>>(category_offset)?.is_some() {
                    Some(parse_category_at_offset(row, category_offset)?)
                } else {
                    None
                };
                Ok(TransactionWithEnrichment {
                    transaction,
                    enrichment,
                    category,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    }

    /// Get transfers pending user confirmation.
    /// Filters match if EITHER the from or to transaction matches.
    pub fn get_pending_transfer_reviews(
        &self,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<Transfer>> {
        let mut sql = format!(
            "SELECT {}
             FROM transfers tr
             JOIN transactions_view ft ON ft.id = tr.from_transaction_id
             JOIN transactions_view tt ON tt.id = tr.to_transaction_id",
            transfer_cols("tr")
        );

        sql.push_str(
            " WHERE tr.confirmed = 0
               AND ft.account_deleted_at IS NULL AND tt.account_deleted_at IS NULL",
        );

        let rendered = query.render_transfers(&transfer_side_ctx("ft"), &transfer_side_ctx("tt"));
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY tr.created_at DESC");
        push_limit(&mut sql, &mut params, limit);

        log::debug!(
            "get_pending_transfer_reviews SQL: {} params: {:?}",
            sql,
            params
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let transfers = stmt
            .query_map(rusqlite::params_from_iter(params), parse_transfer)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(transfers)
    }

    /// Get a transaction by ID.
    pub fn get_transaction_by_id(&self, id: i64) -> Result<Option<Transaction>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {} FROM transactions_view t WHERE t.id = ?",
                    tx_cols("t")
                ),
                [id],
                parse_transaction,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Fetch transactions for a set of ids in one query, keyed by id. Mirrors
    /// [`Self::get_categories_for_transactions`] so per-transaction caches can
    /// be filled without a query per row. Ids with no matching (live)
    /// transaction are simply absent from the map.
    pub fn get_transactions_by_ids(
        &self,
        ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Transaction>> {
        use std::collections::HashMap;

        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {} FROM transactions_view t WHERE t.id IN ({})",
            tx_cols("t"),
            placeholders
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(params.as_slice(), parse_transaction)?;

        let mut result = HashMap::new();
        for row in rows {
            let tx = row?;
            result.insert(tx.id, tx);
        }
        Ok(result)
    }

    /// List transfers with all transaction data resolved.
    /// Filters match if EITHER the from or to transaction matches.
    pub fn list_transfers_with_transactions(
        &self,
        confirmed_only: bool,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<TransferWithTransactions>> {
        let mut sql = format!(
            "SELECT {}, {}, {}
             FROM transfers tr
             JOIN transactions_view ft ON ft.id = tr.from_transaction_id
             JOIN transactions_view tt ON tt.id = tr.to_transaction_id",
            transfer_cols("tr"),
            tx_cols("ft"),
            tx_cols("tt"),
        );

        sql.push_str(" WHERE ft.account_deleted_at IS NULL AND tt.account_deleted_at IS NULL");

        if confirmed_only {
            sql.push_str(" AND tr.confirmed = 1");
        }

        let rendered = query.render_transfers(&transfer_side_ctx("ft"), &transfer_side_ctx("tt"));
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY tr.created_at DESC");
        push_limit(&mut sql, &mut params, limit);

        log::debug!(
            "list_transfers_with_transactions SQL: {} params: {:?}",
            sql,
            params
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let result = stmt
            .query_map(rusqlite::params_from_iter(params), |row| {
                let transfer = parse_transfer(row)?;
                let from_transaction = parse_transaction_at_offset(row, TRANSFER_COL_COUNT)?;
                let to_transaction =
                    parse_transaction_at_offset(row, TRANSFER_COL_COUNT + TX_COL_COUNT)?;
                Ok(TransferWithTransactions {
                    transfer,
                    from_transaction,
                    to_transaction,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(result)
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

/// An enrichment's category source, confirmed flag, and category id.
type EnrichmentMeta = (Option<CategorySource>, bool, Option<i64>);

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

fn parse_date(date_str: &str) -> Result<NaiveDate> {
    if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        return Ok(date);
    }
    if let Ok(date) = NaiveDate::parse_from_str(date_str, "%d/%m/%Y") {
        return Ok(date);
    }
    Err(Error::InvalidDate(date_str.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransferSource;
    use std::fs;
    use tempfile::TempDir;

    /// Build a SearchConfig with all standard filters and no completion options.
    fn search_config() -> SearchConfig {
        SearchConfig::standard(vec![], Some(vec![]))
    }

    /// Convenience: parse a query string with the standard search config.
    fn q(input: &str) -> ParsedQuery {
        let (parsed, _) = parse(&search_config(), input, input.chars().count());
        parsed
    }

    /// Strip the cursor's implicit `*` so FTS tests get exact matching.
    /// `parse()` adds a `*` at the end if the cursor is at the end of the FTS
    /// text — handy in the TUI but a foot-gun in unit tests.
    fn q_exact(input: &str) -> ParsedQuery {
        // Pass cursor=0 so no implicit prefix is added.
        let (parsed, _) = parse(&search_config(), input, 0);
        parsed
    }

    fn annotate_transaction(
        store: &TransactionStore,
        description: &str,
        metadata: &str,
        source_file: &str,
        hash: &str,
    ) {
        store
            .conn
            .execute(
                "UPDATE transactions
                 SET metadata = ?, source_file = ?, hash = ?
                 WHERE description = ?",
                params![metadata, source_file, hash, description],
            )
            .unwrap();
    }

    fn assert_transaction_matches_db(store: &TransactionStore, tx: &Transaction) {
        let (
            bank_id,
            account_id,
            date_str,
            description,
            amount_cents,
            balance_cents,
            hash,
            metadata_json,
            source_file,
            import_batch_id,
        ): (
            i64,
            i64,
            String,
            String,
            i64,
            i64,
            String,
            String,
            String,
            i64,
        ) = store
            .conn
            .query_row(
                "SELECT bank_id, account_id, date, description, amount_cents,
                        balance_cents, hash, metadata, source_file, import_batch_id
                 FROM transactions_view
                 WHERE id = ?",
                [tx.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                    ))
                },
            )
            .unwrap();

        let metadata: HashMap<String, serde_json::Value> =
            serde_json::from_str(&metadata_json).unwrap();

        assert_eq!(tx.bank_id, bank_id);
        assert_eq!(tx.account_id, account_id);
        assert_eq!(
            tx.date,
            NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").unwrap()
        );
        assert_eq!(tx.description, description);
        assert_eq!(tx.amount_cents, amount_cents);
        assert_eq!(tx.balance_cents, balance_cents);
        assert_eq!(tx.hash, hash);
        assert_eq!(tx.metadata, metadata);
        assert_eq!(tx.source_file, source_file);
        assert_eq!(tx.import_batch_id, import_batch_id);
    }

    fn assert_enrichment_matches_db(store: &TransactionStore, enrichment: &TransactionEnrichment) {
        let (
            transaction_id,
            category_id,
            category_source,
            category_confirmed,
            ai_confidence,
            created_at,
            updated_at,
        ): (
            i64,
            Option<i64>,
            Option<String>,
            i32,
            Option<f64>,
            String,
            String,
        ) = store
            .conn
            .query_row(
                "SELECT transaction_id, category_id, category_source,
                        category_confirmed, ai_confidence, created_at, updated_at
                 FROM transaction_enrichments
                 WHERE id = ?",
                [enrichment.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(enrichment.transaction_id, transaction_id);
        assert_eq!(enrichment.category_id, category_id);
        assert_eq!(
            enrichment.category_source,
            category_source.and_then(|source| source.parse().ok())
        );
        assert_eq!(enrichment.category_confirmed, category_confirmed != 0);
        assert_eq!(enrichment.ai_confidence, ai_confidence);
        assert_eq!(enrichment.created_at, parse_datetime(&created_at).unwrap());
        assert_eq!(enrichment.updated_at, parse_datetime(&updated_at).unwrap());
    }

    fn assert_category_matches_db(store: &TransactionStore, category: &Category) {
        let (path, created_at): (String, String) = store
            .conn
            .query_row(
                "SELECT path, created_at FROM categories WHERE id = ?",
                [category.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(category.path, path);
        assert_eq!(category.created_at, parse_datetime(&created_at).unwrap());
    }

    fn assert_transfer_matches_db(store: &TransactionStore, transfer: &Transfer) {
        let (
            from_transaction_id,
            to_transaction_id,
            source,
            confirmed,
            ai_confidence,
            created_at,
        ): (i64, i64, String, i32, Option<f64>, String) = store
            .conn
            .query_row(
                "SELECT from_transaction_id, to_transaction_id, source, confirmed,
                        ai_confidence, created_at
                 FROM transfers
                 WHERE id = ?",
                [transfer.id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(transfer.from_transaction_id, from_transaction_id);
        assert_eq!(transfer.to_transaction_id, to_transaction_id);
        assert_eq!(transfer.source, source.parse().unwrap());
        assert_eq!(transfer.confirmed, confirmed != 0);
        assert_eq!(transfer.ai_confidence, ai_confidence);
        assert_eq!(transfer.created_at, parse_datetime(&created_at).unwrap());
    }

    fn setup_test_exports() -> TempDir {
        let temp = TempDir::new().unwrap();
        let bank_dir = temp.path().join("TestBank");
        let account_dir = bank_dir.join("Checking");
        fs::create_dir_all(&account_dir).unwrap();

        fs::write(
            account_dir.join("transactions.csv"),
            "Date,Description,Amount,Balance\n2025-01-01,Test,-100,500\n",
        )
        .unwrap();

        let import_script = account_dir.join("import");
        fs::write(
            &import_script,
            r#"#!/usr/bin/env bash
echo '[{"date":"2025-01-01","description":"Test transaction","amount_cents":-10000,"balance_cents":50000}]'
"#,
        )
        .unwrap();

        make_executable(&import_script);

        temp
    }

    fn make_executable(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        #[cfg(not(unix))]
        let _ = path;
    }

    fn write_pull_script(path: &std::path::Path, description: &str, hash: &str) {
        fs::write(
            path,
            format!(
                r#"#!/usr/bin/env bash
echo '[{{"date":"2025-01-01","description":"{description}","amount_cents":-10000,"balance_cents":50000,"hash":"{hash}"}}]'
"#
            ),
        )
        .unwrap();
        make_executable(path);
    }

    /// Test fixture with two banks, three accounts, several transactions,
    /// some categorised, and one transfer pair. Used to exercise rendering
    /// across multiple filter types and store query methods.
    fn setup_rich_fixture() -> (TempDir, TransactionStore) {
        let temp = TempDir::new().unwrap();
        let store = TransactionStore::open_in_memory(temp.path()).unwrap();

        // Insert banks/accounts directly to avoid relying on import scripts.
        store
            .conn
            .execute("INSERT INTO banks (name) VALUES ('ING')", [])
            .unwrap();
        store
            .conn
            .execute("INSERT INTO banks (name) VALUES ('NAB')", [])
            .unwrap();
        let ing_id: i64 = store
            .conn
            .query_row("SELECT id FROM banks WHERE name='ING'", [], |r| r.get(0))
            .unwrap();
        let nab_id: i64 = store
            .conn
            .query_row("SELECT id FROM banks WHERE name='NAB'", [], |r| r.get(0))
            .unwrap();

        store
            .conn
            .execute(
                "INSERT INTO accounts (bank_id, name) VALUES (?, 'Orange Everyday')",
                [ing_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO accounts (bank_id, name) VALUES (?, 'Savings Maximiser')",
                [ing_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO accounts (bank_id, name) VALUES (?, 'Classic')",
                [nab_id],
            )
            .unwrap();
        let ing_orange: i64 = store
            .conn
            .query_row(
                "SELECT id FROM accounts WHERE name='Orange Everyday'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let ing_savings: i64 = store
            .conn
            .query_row(
                "SELECT id FROM accounts WHERE name='Savings Maximiser'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let nab_classic: i64 = store
            .conn
            .query_row("SELECT id FROM accounts WHERE name='Classic'", [], |r| {
                r.get(0)
            })
            .unwrap();

        // Make a batch.
        store
            .conn
            .execute(
                "INSERT INTO import_batches (started_at) VALUES ('2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        let batch_id: i64 = store.conn.last_insert_rowid();

        // Insert a handful of transactions.
        let txs = [
            (ing_orange, "2024-01-15", "Coffee Shop", -500i64, 100000i64),
            (ing_orange, "2024-02-20", "Grocery Store", -8500, 91500),
            (ing_orange, "2024-03-10", "Salary", 250000, 341500),
            (ing_savings, "2024-03-15", "Interest", 1234, 50000),
            (nab_classic, "2024-04-05", "Coffee Bean", -750, 75000),
            // Transfer pair (equal & opposite)
            (ing_orange, "2024-05-01", "Transfer Out", -10000, 331500),
            (nab_classic, "2024-05-01", "Transfer In", 10000, 85000),
        ];
        for (account_id, date, desc, amount, balance) in txs {
            store
                .conn
                .execute(
                    "INSERT INTO transactions
                     (account_id, date, description, amount_cents, balance_cents,
                      hash, metadata, source_file, import_batch_id)
                     VALUES (?, ?, ?, ?, ?, ?, '{}', '', ?)",
                    params![
                        account_id,
                        date,
                        desc,
                        amount,
                        balance,
                        format!("{}-{}-{}", account_id, date, amount),
                        batch_id,
                    ],
                )
                .unwrap();
            let tx_id = store.conn.last_insert_rowid();
            store
                .conn
                .execute(
                    "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
                    params![tx_id, desc],
                )
                .unwrap();
        }

        // Categorise some.
        let mut store = store; // need mut for set_category
        let food_id = store.get_or_create_category("Food/Groceries").unwrap();
        let coffee_id = store.get_or_create_category("Food/Coffee").unwrap();
        let income_id = store.get_or_create_category("Income/Salary").unwrap();

        let id_of = |store: &TransactionStore, desc: &str| -> i64 {
            store
                .conn
                .query_row(
                    "SELECT id FROM transactions WHERE description = ? LIMIT 1",
                    [desc],
                    |r| r.get(0),
                )
                .unwrap()
        };

        let coffee_tx = id_of(&store, "Coffee Shop");
        let groc_tx = id_of(&store, "Grocery Store");
        let salary_tx = id_of(&store, "Salary");
        let coffee_bean_tx = id_of(&store, "Coffee Bean");

        store
            .set_category(coffee_tx, coffee_id, CategorySource::Manual, true, None)
            .unwrap();
        store
            .set_category(groc_tx, food_id, CategorySource::Manual, true, None)
            .unwrap();
        store
            .set_category(salary_tx, income_id, CategorySource::Manual, true, None)
            .unwrap();
        // AI-suggested, awaiting review:
        store
            .set_category(
                coffee_bean_tx,
                coffee_id,
                CategorySource::Ai,
                false,
                Some(0.85),
            )
            .unwrap();

        // Create the transfer link.
        let xfer_out = id_of(&store, "Transfer Out");
        let xfer_in = id_of(&store, "Transfer In");
        store
            .create_transfer(xfer_out, xfer_in, TransferSource::Manual, true, None)
            .unwrap();

        (temp, store)
    }

    // ----- Schema/refresh tests (unchanged) -----

    #[test]
    fn discover_banks_and_accounts() {
        let temp = setup_test_exports();
        let store = TransactionStore::open_in_memory(temp.path()).unwrap();

        let discovered = store.discover_banks_and_accounts().unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].0, "TestBank");
        assert_eq!(discovered[0].1, vec!["Checking"]);
    }

    #[test]
    fn refresh_creates_banks_and_accounts() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();

        let report = store.refresh().unwrap();
        assert_eq!(report.banks_added, 1);
        assert_eq!(report.accounts_added, 1);

        let banks = store.list_banks().unwrap();
        assert_eq!(banks.len(), 1);
        assert_eq!(banks[0].name, "TestBank");
    }

    #[test]
    fn refresh_imports_pull_results_for_multiple_accounts() {
        let temp = TempDir::new().unwrap();
        let bank_dir = temp.path().join("TestBank");
        let checking_dir = bank_dir.join("Checking");
        let savings_dir = bank_dir.join("Savings");
        fs::create_dir_all(&checking_dir).unwrap();
        fs::create_dir_all(&savings_dir).unwrap();
        write_pull_script(&checking_dir.join("pull"), "Checking pull", "checking-pull");
        write_pull_script(&savings_dir.join("pull"), "Savings pull", "savings-pull");

        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();

        let report = store.refresh().unwrap();
        assert_eq!(report.banks_added, 1);
        assert_eq!(report.accounts_added, 2);
        assert_eq!(report.files_processed, 2);
        assert_eq!(report.transactions_added, 2);

        let mut txs = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        txs.sort_by(|a, b| a.description.cmp(&b.description));

        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0].description, "Checking pull");
        assert_eq!(txs[0].source_file, "TestBank/Checking/pull");
        assert_eq!(txs[1].description, "Savings pull");
        assert_eq!(txs[1].source_file, "TestBank/Savings/pull");
    }

    #[test]
    fn soft_delete_missing_bank() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();

        store.refresh().unwrap();

        fs::remove_dir_all(temp.path().join("TestBank")).unwrap();

        let report = store.refresh().unwrap();
        assert_eq!(report.banks_deleted, 1);

        let banks = store.list_banks().unwrap();
        assert!(banks.is_empty());
    }

    // ----- query_transactions: filter coverage -----

    #[test]
    fn query_with_empty_query_returns_all() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(txs.len(), 7);
    }

    #[test]
    fn query_respects_limit() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .query_transactions(&ParsedQuery::empty(), Some(3))
            .unwrap();
        assert_eq!(txs.len(), 3);
    }

    #[test]
    fn query_amount_exact_matches_either_sign() {
        // ABS(amount) = X — matches both -X and +X
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("amount:100"), None).unwrap();
        // Both Transfer Out (-10000) and Transfer In (10000) hit
        assert_eq!(txs.len(), 2);
    }

    #[test]
    fn query_amount_greater_than() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("amount:>50"), None).unwrap();
        // Anything with |amount| > $50: Grocery Store (-85), Salary (250),
        // Transfer Out (-100), Transfer In (100) — 4 rows
        assert_eq!(txs.len(), 4);
    }

    #[test]
    fn query_amount_range() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("amount:1..50"), None).unwrap();
        // |amount| between $1 and $50: Coffee Shop (-5), Coffee Bean (-7.50),
        // Interest (12.34) — 3 rows
        assert_eq!(txs.len(), 3);
    }

    #[test]
    fn query_date_year() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("date:2024"), None).unwrap();
        assert_eq!(txs.len(), 7);
    }

    #[test]
    fn query_date_month() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("date:2024-03"), None).unwrap();
        // March: Salary, Interest
        assert_eq!(txs.len(), 2);
    }

    #[test]
    fn query_date_range() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .query_transactions(&q("date:2024-02..2024-04"), None)
            .unwrap();
        // Feb 20, Mar 10, Mar 15, Apr 5 — 4 rows
        assert_eq!(txs.len(), 4);
    }

    #[test]
    fn query_account_bank_only() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("account:ING"), None).unwrap();
        // ING has 5 transactions
        assert_eq!(txs.len(), 5);
    }

    #[test]
    fn query_account_bank_account() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .query_transactions(&q("account:ING/Orange"), None)
            .unwrap();
        // ING/Orange Everyday: Coffee Shop, Grocery Store, Salary, Transfer Out
        assert_eq!(txs.len(), 4);
    }

    #[test]
    fn query_account_or() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .query_transactions(&q("account:NAB|ING/Savings"), None)
            .unwrap();
        // NAB (2) + ING Savings (1) = 3
        assert_eq!(txs.len(), 3);
    }

    #[test]
    fn query_account_is_case_insensitive() {
        let (_t, store) = setup_rich_fixture();
        let upper = store.query_transactions(&q("account:ING"), None).unwrap();
        let lower = store.query_transactions(&q("account:ing"), None).unwrap();
        assert_eq!(upper.len(), lower.len());
        assert!(!upper.is_empty());
    }

    #[test]
    fn query_category_filter() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("category:Food"), None).unwrap();
        // Food/Groceries + Food/Coffee assigned to: Coffee Shop, Grocery Store, Coffee Bean
        assert_eq!(txs.len(), 3);
    }

    #[test]
    fn query_category_or() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .query_transactions(&q("category:Income|Coffee"), None)
            .unwrap();
        // Salary (Income), Coffee Shop (Food/Coffee), Coffee Bean (Food/Coffee)
        assert_eq!(txs.len(), 3);
    }

    #[test]
    fn query_category_is_case_insensitive() {
        let (_t, store) = setup_rich_fixture();
        let upper = store.query_transactions(&q("category:Food"), None).unwrap();
        let lower = store.query_transactions(&q("category:food"), None).unwrap();
        assert_eq!(upper.len(), lower.len());
    }

    #[test]
    fn query_regex_description() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("/Coffee.*/"), None).unwrap();
        // "Coffee Shop", "Coffee Bean"
        assert_eq!(txs.len(), 2);
    }

    #[test]
    fn query_regex_case_insensitive_flag() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("/coffee/i"), None).unwrap();
        assert_eq!(txs.len(), 2);
    }

    #[test]
    fn query_fts() {
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q_exact("Salary"), None).unwrap();
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].description, "Salary");
    }

    #[test]
    fn query_fts_prefix_at_cursor() {
        // Cursor at end → implicit prefix → "trans" matches "Transfer Out"/"Transfer In"
        let (_t, store) = setup_rich_fixture();
        let txs = store.query_transactions(&q("trans"), None).unwrap();
        assert_eq!(txs.len(), 2);
    }

    #[test]
    fn query_fts_no_match() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .query_transactions(&q_exact("notpresent"), None)
            .unwrap();
        assert!(txs.is_empty());
    }

    #[test]
    fn query_combines_filters_with_and() {
        // amount range AND date AND account — narrows to a single tx
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .query_transactions(&q("date:2024-01..2024-02 amount:>50 account:ING/"), None)
            .unwrap();
        // Jan 15 Coffee Shop ($5, too small), Feb 20 Grocery ($85, matches)
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].description, "Grocery Store");
    }

    #[test]
    fn transaction_query_methods_parse_full_transaction_payloads() {
        let (_t, store) = setup_rich_fixture();
        annotate_transaction(
            &store,
            "Interest",
            r#"{"note":"savings","tags":["fy24","interest"],"cleared":true}"#,
            "fixtures/interest.json",
            "interest-fixture-hash",
        );
        annotate_transaction(
            &store,
            "Coffee Bean",
            r#"{"merchant":{"name":"Coffee Bean"},"score":0.85}"#,
            "fixtures/coffee-bean.json",
            "coffee-bean-fixture-hash",
        );

        let all = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        let interest = all
            .iter()
            .find(|tx| tx.description == "Interest")
            .expect("fixture has Interest");
        assert_transaction_matches_db(&store, interest);
        assert_eq!(
            interest.metadata.get("note"),
            Some(&serde_json::json!("savings"))
        );

        let uncategorised = store
            .get_uncategorised_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(uncategorised.len(), 1);
        assert_transaction_matches_db(&store, &uncategorised[0]);
        assert_eq!(uncategorised[0].description, "Interest");

        let unconfirmed = store
            .get_unconfirmed_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        let coffee_bean = unconfirmed
            .iter()
            .find(|tx| tx.description == "Coffee Bean")
            .expect("fixture has Coffee Bean");
        assert_transaction_matches_db(&store, coffee_bean);
        assert_eq!(
            coffee_bean.metadata.get("merchant"),
            Some(&serde_json::json!({"name": "Coffee Bean"}))
        );
    }

    // ----- get_uncategorised_transactions -----

    #[test]
    fn uncategorised_excludes_categorised_and_transfers() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .get_uncategorised_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        // Categorised (3 confirmed + 1 ai) → 4 categorised, 7 total, 2 in transfers, 1 leftover (Interest)
        // Coffee Bean has an AI enrichment → counts as categorised here
        // So uncategorised excluding transfer = Interest (1)
        let descs: Vec<_> = txs.iter().map(|t| t.description.clone()).collect();
        assert_eq!(descs, vec!["Interest"]);
    }

    #[test]
    fn uncategorised_respects_filters() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .get_uncategorised_transactions(&q("date:2025"), None)
            .unwrap();
        assert!(txs.is_empty());
    }

    #[test]
    fn confirmed_examples_include_only_confirmed_assignments() {
        let (_t, store) = setup_rich_fixture();
        let examples = store.get_confirmed_examples().unwrap();

        assert_eq!(examples.len(), 3);
        assert!(examples.iter().any(|example| {
            example.description == "Grocery Store"
                && example.amount_cents == -8500
                && example.date == NaiveDate::from_ymd_opt(2024, 2, 20).unwrap()
                && example.category_path == "Food/Groceries"
        }));
        assert!(
            !examples
                .iter()
                .any(|example| example.description == "Coffee Bean")
        );
    }

    // ----- get_unconfirmed_transactions -----

    #[test]
    fn unconfirmed_includes_uncategorised_and_unconfirmed_ai() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .get_unconfirmed_transactions(&ParsedQuery::empty(), None)
            .unwrap();

        let descs: Vec<_> = txs.iter().map(|t| t.description.as_str()).collect();
        assert_eq!(descs, vec!["Coffee Bean", "Interest"]);
    }

    #[test]
    fn unconfirmed_respects_filters() {
        let (_t, store) = setup_rich_fixture();
        let txs = store
            .get_unconfirmed_transactions(&q("account:NAB"), None)
            .unwrap();

        let descs: Vec<_> = txs.iter().map(|t| t.description.as_str()).collect();
        assert_eq!(descs, vec!["Coffee Bean"]);
    }

    // ----- get_pending_ai_reviews -----

    #[test]
    fn pending_ai_reviews_only_unconfirmed_ai() {
        let (_t, store) = setup_rich_fixture();
        let pending = store
            .get_pending_ai_reviews(&ParsedQuery::empty(), None)
            .unwrap();
        // Only Coffee Bean is AI + unconfirmed
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].transaction.description, "Coffee Bean");
    }

    #[test]
    fn pending_ai_reviews_filtered_by_account() {
        let (_t, store) = setup_rich_fixture();
        let pending = store
            .get_pending_ai_reviews(&q("account:ING"), None)
            .unwrap();
        // Coffee Bean is on NAB → filtered out
        assert!(pending.is_empty());
    }

    #[test]
    fn pending_ai_reviews_parse_transaction_enrichment_and_category() {
        let (_t, store) = setup_rich_fixture();
        annotate_transaction(
            &store,
            "Coffee Bean",
            r#"{"merchant":{"name":"Coffee Bean"},"review":true}"#,
            "fixtures/ai-review.json",
            "ai-review-fixture-hash",
        );

        let pending = store
            .get_pending_ai_reviews(&ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(pending.len(), 1);

        let row = &pending[0];
        assert_transaction_matches_db(&store, &row.transaction);

        let enrichment = row
            .enrichment
            .as_ref()
            .expect("pending review has enrichment");
        assert_enrichment_matches_db(&store, enrichment);
        assert_eq!(enrichment.transaction_id, row.transaction.id);
        assert_eq!(enrichment.category_source, Some(CategorySource::Ai));
        assert!(!enrichment.category_confirmed);
        assert_eq!(enrichment.ai_confidence, Some(0.85));

        let category = row.category.as_ref().expect("pending review has category");
        assert_category_matches_db(&store, category);
        assert_eq!(enrichment.category_id, Some(category.id));
        assert_eq!(category.path, "Food/Coffee");
    }

    // ----- transfer queries -----

    #[test]
    fn transfers_listed_for_either_side_match() {
        let (_t, store) = setup_rich_fixture();
        // Filter on NAB — should still return the transfer because tt-side is on NAB
        let xfers = store
            .list_transfers_with_transactions(true, &q("account:NAB"), None)
            .unwrap();
        assert_eq!(xfers.len(), 1);
        assert_eq!(xfers[0].to_transaction.description, "Transfer In");
    }

    #[test]
    fn transfers_listed_for_from_side_match() {
        let (_t, store) = setup_rich_fixture();
        let xfers = store
            .list_transfers_with_transactions(true, &q("account:ING"), None)
            .unwrap();
        assert_eq!(xfers.len(), 1);
        assert_eq!(xfers[0].from_transaction.description, "Transfer Out");
    }

    #[test]
    fn transfers_dropped_when_no_side_matches() {
        let (_t, store) = setup_rich_fixture();
        let xfers = store
            .list_transfers_with_transactions(true, &q("account:Nonexistent"), None)
            .unwrap();
        assert!(xfers.is_empty());
    }

    #[test]
    fn transfers_filter_by_fts_on_either_side() {
        let (_t, store) = setup_rich_fixture();
        let xfers = store
            .list_transfers_with_transactions(true, &q_exact("Transfer"), None)
            .unwrap();
        assert_eq!(xfers.len(), 1);

        let xfers = store
            .list_transfers_with_transactions(true, &q_exact("notpresent"), None)
            .unwrap();
        assert!(xfers.is_empty());
    }

    #[test]
    fn transfers_keep_filters_and_fts_on_same_side() {
        let (_t, store) = setup_rich_fixture();

        let xfers = store
            .list_transfers_with_transactions(true, &q_exact("account:ING In"), None)
            .unwrap();
        assert!(xfers.is_empty());

        let xfers = store
            .list_transfers_with_transactions(true, &q_exact("account:NAB In"), None)
            .unwrap();
        assert_eq!(xfers.len(), 1);
        assert_eq!(xfers[0].to_transaction.description, "Transfer In");
    }

    #[test]
    fn transfers_with_transactions_parse_transfer_and_endpoint_payloads() {
        let (_t, store) = setup_rich_fixture();
        annotate_transaction(
            &store,
            "Transfer Out",
            r#"{"side":"from","reference":"xfer-001"}"#,
            "fixtures/transfer-out.json",
            "transfer-out-fixture-hash",
        );
        annotate_transaction(
            &store,
            "Transfer In",
            r#"{"side":"to","reference":"xfer-001"}"#,
            "fixtures/transfer-in.json",
            "transfer-in-fixture-hash",
        );

        let xfers = store
            .list_transfers_with_transactions(true, &ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(xfers.len(), 1);

        let transfer = &xfers[0].transfer;
        assert_transfer_matches_db(&store, transfer);
        assert_eq!(transfer.source, TransferSource::Manual);
        assert!(transfer.confirmed);
        assert_eq!(transfer.ai_confidence, None);

        let from = &xfers[0].from_transaction;
        assert_transaction_matches_db(&store, from);
        assert_eq!(from.id, transfer.from_transaction_id);
        assert_eq!(from.description, "Transfer Out");
        assert_eq!(from.amount_cents, -10000);
        assert_eq!(from.metadata.get("side"), Some(&serde_json::json!("from")));

        let to = &xfers[0].to_transaction;
        assert_transaction_matches_db(&store, to);
        assert_eq!(to.id, transfer.to_transaction_id);
        assert_eq!(to.description, "Transfer In");
        assert_eq!(to.amount_cents, 10000);
        assert_eq!(to.metadata.get("side"), Some(&serde_json::json!("to")));
    }

    #[test]
    fn pending_transfer_reviews_empty_when_all_confirmed() {
        let (_t, store) = setup_rich_fixture();
        let pending = store
            .get_pending_transfer_reviews(&ParsedQuery::empty(), None)
            .unwrap();
        assert!(pending.is_empty());
    }

    // ----- Category management (unchanged behaviour) -----

    #[test]
    fn rename_category_works() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();

        let cat_id = store.get_or_create_category("Food/Groceries").unwrap();
        store.rename_category(cat_id, "Food/Supermarket").unwrap();

        let cat = store.get_category(cat_id).unwrap().unwrap();
        assert_eq!(cat.path, "Food/Supermarket");
    }

    #[test]
    fn rename_category_conflict_errors() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();

        store.get_or_create_category("Food/Groceries").unwrap();
        let cat2_id = store.get_or_create_category("Food/Supermarket").unwrap();

        let result = store.rename_category(cat2_id, "Food/Groceries");
        assert!(matches!(result, Err(Error::CategoryExists(_))));
    }

    #[test]
    fn merge_categories_moves_transactions_and_drops_source() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();

        let source_id = store.get_or_create_category("OldCategory").unwrap();
        let target_id = store.get_or_create_category("NewCategory").unwrap();

        let txs = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        let tx_id = txs[0].id;
        store
            .set_category(tx_id, source_id, CategorySource::Manual, true, None)
            .unwrap();

        store.merge_categories(source_id, target_id).unwrap();

        assert!(store.get_category(source_id).unwrap().is_none());
        let cat = store.get_transaction_category(tx_id).unwrap().unwrap();
        assert_eq!(cat.id, target_id);
    }

    #[test]
    fn delete_category_drops_enrichments_and_reports_count() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();

        let cat_id = store.get_or_create_category("Doomed").unwrap();
        let txs = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        let tx_id = txs[0].id;
        store
            .set_category(tx_id, cat_id, CategorySource::Manual, true, None)
            .unwrap();

        assert_eq!(store.delete_category(cat_id).unwrap(), 1);
        assert!(store.get_category(cat_id).unwrap().is_none());
        assert!(store.get_transaction_category(tx_id).unwrap().is_none());
    }

    #[test]
    fn count_transactions_in_category_tracks_assignments() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();

        let cat_id = store.get_or_create_category("TestCategory").unwrap();
        assert_eq!(store.count_transactions_in_category(cat_id).unwrap(), 0);

        let txs = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        store
            .set_category(txs[0].id, cat_id, CategorySource::Manual, true, None)
            .unwrap();

        assert_eq!(store.count_transactions_in_category(cat_id).unwrap(), 1);
    }

    #[test]
    fn get_category_transaction_counts_groups_in_one_query() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();

        // No assignments yet — empty map (categories with zero transactions
        // are omitted, not zeroed).
        assert!(store.get_category_transaction_counts().unwrap().is_empty());

        let food = store.get_or_create_category("Food").unwrap();
        let transport = store.get_or_create_category("Transport").unwrap();
        let unused = store.get_or_create_category("UnusedCategory").unwrap();

        let tx = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap()
            .into_iter()
            .next()
            .expect("fixture has one transaction");

        // Categories with zero transactions stay out of the result.
        store
            .set_category(tx.id, food, CategorySource::Manual, true, None)
            .unwrap();
        let counts = store.get_category_transaction_counts().unwrap();
        assert_eq!(counts.get(&food), Some(&1));
        assert_eq!(counts.get(&transport), None);
        assert_eq!(counts.get(&unused), None);
        assert_eq!(counts.len(), 1);

        // Reassigning the transaction moves the count, doesn't duplicate it.
        store
            .set_category(tx.id, transport, CategorySource::Manual, true, None)
            .unwrap();
        let counts = store.get_category_transaction_counts().unwrap();
        assert_eq!(counts.get(&food), None);
        assert_eq!(counts.get(&transport), Some(&1));
        assert_eq!(counts.len(), 1);
    }

    /// Build a small store with two accounts and a controllable set of
    /// transactions, so transfer-candidate behavior can be exercised directly.
    fn store_with_two_accounts() -> (TempDir, TransactionStore, i64, i64) {
        let temp = TempDir::new().unwrap();
        let store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store
            .conn
            .execute("INSERT INTO banks (name) VALUES ('TB')", [])
            .unwrap();
        let bank_id: i64 = store.conn.last_insert_rowid();
        store
            .conn
            .execute(
                "INSERT INTO accounts (bank_id, name) VALUES (?, 'A1')",
                [bank_id],
            )
            .unwrap();
        let a1: i64 = store.conn.last_insert_rowid();
        store
            .conn
            .execute(
                "INSERT INTO accounts (bank_id, name) VALUES (?, 'A2')",
                [bank_id],
            )
            .unwrap();
        let a2: i64 = store.conn.last_insert_rowid();
        store
            .conn
            .execute(
                "INSERT INTO import_batches (started_at) VALUES ('2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        (temp, store, a1, a2)
    }

    fn insert_tx(store: &TransactionStore, account_id: i64, date: &str, amount: i64) -> i64 {
        let batch_id: i64 = store
            .conn
            .query_row("SELECT id FROM import_batches LIMIT 1", [], |r| r.get(0))
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO transactions
                 (account_id, date, description, amount_cents, balance_cents,
                  hash, metadata, source_file, import_batch_id)
                 VALUES (?, ?, 'tx', ?, 0, ?, '{}', '', ?)",
                params![
                    account_id,
                    date,
                    amount,
                    format!("{}-{}-{}", account_id, date, amount),
                    batch_id
                ],
            )
            .unwrap();
        store.conn.last_insert_rowid()
    }

    fn get_tx(store: &TransactionStore, id: i64) -> Transaction {
        store.get_transaction_by_id(id).unwrap().unwrap()
    }

    #[test]
    fn transfer_candidates_prefer_other_accounts() {
        let (_tmp, store, a1, a2) = store_with_two_accounts();
        let source = insert_tx(&store, a1, "2024-03-10", -5000);
        let same_account = insert_tx(&store, a1, "2024-03-09", 5000);
        let other_account = insert_tx(&store, a2, "2024-03-12", 5000);

        let candidates = store
            .find_matching_transfer_candidates(&get_tx(&store, source))
            .unwrap();

        // Other-account match wins; same-account match is suppressed.
        let ids: Vec<i64> = candidates.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![other_account]);
        assert!(!ids.contains(&same_account));
    }

    #[test]
    fn transfer_candidates_fall_back_to_same_account() {
        let (_tmp, store, a1, _a2) = store_with_two_accounts();
        let source = insert_tx(&store, a1, "2024-03-10", -5000);
        let same_account = insert_tx(&store, a1, "2024-03-09", 5000);

        let candidates = store
            .find_matching_transfer_candidates(&get_tx(&store, source))
            .unwrap();

        // No other-account candidate exists, so fall through to same-account.
        let ids: Vec<i64> = candidates.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![same_account]);
    }

    #[test]
    fn transfer_candidates_include_already_linked() {
        let (_tmp, mut store, a1, a2) = store_with_two_accounts();
        let source = insert_tx(&store, a1, "2024-03-10", -5000);
        let linked = insert_tx(&store, a2, "2024-03-11", 5000);
        let unlinked = insert_tx(&store, a2, "2024-03-12", 5000);

        // Link `linked` to a third transaction. It is still offered as a
        // candidate: the caller confirms before breaking the existing link.
        let other = insert_tx(&store, a1, "2024-03-11", 9999);
        store
            .create_transfer(other, linked, TransferSource::Manual, true, None)
            .unwrap();

        let candidates = store
            .find_matching_transfer_candidates(&get_tx(&store, source))
            .unwrap();

        let ids: Vec<i64> = candidates.iter().map(|t| t.id).collect();
        assert!(ids.contains(&linked));
        assert!(ids.contains(&unlinked));
    }

    #[test]
    fn create_transfer_clears_category_on_both_endpoints() {
        let (_tmp, mut store, a1, a2) = store_with_two_accounts();
        let from = insert_tx(&store, a1, "2024-03-10", -5000);
        let to = insert_tx(&store, a2, "2024-03-10", 5000);

        let cat = store.get_or_create_category("Food").unwrap();
        store
            .set_category(from, cat, CategorySource::Manual, true, None)
            .unwrap();
        store
            .set_category(to, cat, CategorySource::Manual, true, None)
            .unwrap();

        store
            .create_transfer(from, to, TransferSource::Manual, true, None)
            .unwrap();

        // Invariant: a transfer is never categorised.
        assert!(store.get_transaction_category(from).unwrap().is_none());
        assert!(store.get_transaction_category(to).unwrap().is_none());
    }

    #[test]
    fn get_transfers_for_transactions_maps_both_endpoints_and_reflects_deletion() {
        let (_tmp, mut store, a1, a2) = store_with_two_accounts();
        let from = insert_tx(&store, a1, "2024-03-10", -5000);
        let to = insert_tx(&store, a2, "2024-03-10", 5000);
        store
            .create_transfer(from, to, TransferSource::Manual, true, None)
            .unwrap();

        // Both endpoints resolve to the same transfer, even when only one id is
        // queried.
        let map = store.get_transfers_for_transactions(&[from]).unwrap();
        assert_eq!(map.len(), 2);
        let transfer = &map[&from];
        assert_eq!(transfer.from_transaction_id, from);
        assert_eq!(transfer.to_transaction_id, to);
        assert_eq!(map[&to].id, transfer.id);

        // After deleting the transfer the lookup is empty — the source of the
        // bug where an unlink wasn't reflected until restart.
        store.delete_transfer(transfer.id).unwrap();
        assert!(
            store
                .get_transfers_for_transactions(&[from, to])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn get_transactions_by_ids_returns_requested_rows() {
        let (_tmp, store, a1, a2) = store_with_two_accounts();
        let t1 = insert_tx(&store, a1, "2024-03-10", -5000);
        let t2 = insert_tx(&store, a2, "2024-03-11", 5000);

        // Empty input → empty map, no query.
        assert!(store.get_transactions_by_ids(&[]).unwrap().is_empty());

        // Known ids → the matching transactions; an unknown id is simply absent.
        let unknown = t2 + 1000;
        let map = store.get_transactions_by_ids(&[t1, t2, unknown]).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&t1].id, t1);
        assert_eq!(map[&t1].amount_cents, -5000);
        assert_eq!(map[&t2].id, t2);
        assert_eq!(map[&t2].amount_cents, 5000);
        assert!(!map.contains_key(&unknown));
    }

    // ----- Filter tests -----

    fn tx_id_of(store: &TransactionStore, desc: &str) -> i64 {
        store
            .conn
            .query_row(
                "SELECT id FROM transactions WHERE description = ? LIMIT 1",
                [desc],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// Source, confirmed, category path for a transaction's enrichment.
    fn enrichment(store: &TransactionStore, desc: &str) -> Option<(CategorySource, bool, String)> {
        let id = tx_id_of(store, desc);
        let (source, confirmed, cat_id) = store.get_enrichment_meta(id).unwrap()?;
        let path = store.get_category(cat_id.unwrap()).unwrap().unwrap().path;
        Some((source.unwrap(), confirmed, path))
    }

    /// Add a category-bearing filter and return its id.
    fn add_filter(
        store: &mut TransactionStore,
        name: &str,
        query: &str,
        category: &str,
        mode: FilterOverride,
        review: bool,
    ) -> i64 {
        let cat = store.get_or_create_category(category).unwrap();
        let id = store.create_filter(name, query).unwrap();
        store.set_filter_category(id, Some(cat)).unwrap();
        store.set_filter_override(id, mode).unwrap();
        store.set_filter_review(id, review).unwrap();
        id
    }

    #[test]
    fn filter_applies_rule_to_uncategorised_match_only() {
        let (_t, mut store) = setup_rich_fixture();
        // "Interest" is the only uncategorised, non-transfer transaction.
        add_filter(
            &mut store,
            "interest",
            "Interest",
            "Income/Interest",
            FilterOverride::Uncategorised,
            false,
        );

        assert_eq!(store.apply_filters().unwrap(), 1);
        assert_eq!(
            enrichment(&store, "Interest"),
            Some((CategorySource::Rule, true, "Income/Interest".into()))
        );
        // A non-matching uncategorised-elsewhere row is untouched; the manual
        // Grocery enrichment keeps its source.
        assert_eq!(
            enrichment(&store, "Grocery Store").map(|e| e.0),
            Some(CategorySource::Manual)
        );
    }

    #[test]
    fn filter_review_required_leaves_unconfirmed() {
        let (_t, mut store) = setup_rich_fixture();
        add_filter(
            &mut store,
            "interest",
            "Interest",
            "Income/Interest",
            FilterOverride::Uncategorised,
            true,
        );
        store.apply_filters().unwrap();
        assert_eq!(
            enrichment(&store, "Interest"),
            Some((CategorySource::Rule, false, "Income/Interest".into()))
        );
    }

    #[test]
    fn override_modes_respect_existing_enrichment() {
        let (_t, mut store) = setup_rich_fixture();
        // Grocery Store is Manual+confirmed; Coffee Bean is AI+unconfirmed.
        add_filter(
            &mut store,
            "uncat",
            "Grocery",
            "Override/Uncat",
            FilterOverride::Uncategorised,
            false,
        );
        add_filter(
            &mut store,
            "ai-on-ai",
            "Coffee Bean",
            "Override/Ai",
            FilterOverride::Ai,
            false,
        );
        store.apply_filters().unwrap();
        // Uncategorised never overrides a manual category.
        assert_eq!(
            enrichment(&store, "Grocery Store").map(|e| e.0),
            Some(CategorySource::Manual)
        );
        // Ai overrides an existing AI enrichment.
        assert_eq!(
            enrichment(&store, "Coffee Bean"),
            Some((CategorySource::Rule, true, "Override/Ai".into()))
        );

        // A fresh All-mode filter overrides a manual category (Salary, which no
        // earlier filter claims).
        add_filter(
            &mut store,
            "all",
            "Salary",
            "Override/All",
            FilterOverride::All,
            false,
        );
        store.apply_filters().unwrap();
        assert_eq!(
            enrichment(&store, "Salary"),
            Some((CategorySource::Rule, true, "Override/All".into()))
        );

        // Ai mode must NOT override the manual Grocery category. The earlier
        // Uncategorised "uncat" filter claims Grocery first, but even alone an
        // Ai filter would skip a manual enrichment.
        add_filter(
            &mut store,
            "ai-on-manual",
            "Coffee Shop",
            "Override/AiManual",
            FilterOverride::Ai,
            false,
        );
        store.apply_filters().unwrap();
        // Coffee Shop is Manual+confirmed; Ai mode leaves it.
        assert_eq!(
            enrichment(&store, "Coffee Shop").map(|e| e.0),
            Some(CategorySource::Manual)
        );
    }

    #[test]
    fn first_matching_filter_wins() {
        let (_t, mut store) = setup_rich_fixture();
        // Both match "Interest"; the earlier (lower position) filter wins.
        add_filter(
            &mut store,
            "first",
            "Interest",
            "Win/First",
            FilterOverride::All,
            false,
        );
        add_filter(
            &mut store,
            "second",
            "Interest",
            "Win/Second",
            FilterOverride::All,
            false,
        );
        store.apply_filters().unwrap();
        assert_eq!(
            enrichment(&store, "Interest").map(|e| e.2),
            Some("Win/First".into())
        );
    }

    #[test]
    fn apply_filters_is_idempotent_and_preserves_confirmed() {
        let (_t, mut store) = setup_rich_fixture();
        let filter_id = add_filter(
            &mut store,
            "interest",
            "Interest",
            "Income/Interest",
            FilterOverride::Uncategorised,
            false,
        );

        assert_eq!(store.apply_filters().unwrap(), 1);
        // User confirms the rule assignment.
        store
            .confirm_category(tx_id_of(&store, "Interest"))
            .unwrap();
        // Switching the filter to require review must not re-touch the now
        // user-confirmed same-category row.
        store.set_filter_review(filter_id, true).unwrap();

        assert_eq!(store.apply_filters().unwrap(), 0);
        assert_eq!(
            enrichment(&store, "Interest"),
            Some((CategorySource::Rule, true, "Income/Interest".into()))
        );
        // Exactly one enrichment row for the transaction.
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM transaction_enrichments WHERE transaction_id = ?",
                [tx_id_of(&store, "Interest")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn review_required_filter_reapply_reports_no_change() {
        let (_t, mut store) = setup_rich_fixture();
        add_filter(
            &mut store,
            "interest",
            "Interest",
            "Income/Interest",
            FilterOverride::Uncategorised,
            true,
        );

        // First apply categorises the row (unconfirmed, pending review).
        assert_eq!(store.apply_filters().unwrap(), 1);
        assert_eq!(
            enrichment(&store, "Interest"),
            Some((CategorySource::Rule, false, "Income/Interest".into()))
        );

        // Re-applying must not re-report the already-categorised row, even
        // though its rule enrichment stays unconfirmed.
        assert!(store.preview_filters().unwrap().is_empty());
        assert_eq!(store.apply_filters().unwrap(), 0);
        assert_eq!(
            enrichment(&store, "Interest"),
            Some((CategorySource::Rule, false, "Income/Interest".into()))
        );
    }

    #[test]
    fn preview_filters_matches_apply_and_leaves_db_unchanged() {
        let (_t, mut store) = setup_rich_fixture();
        add_filter(
            &mut store,
            "interest",
            "Interest",
            "Income/Interest",
            FilterOverride::Uncategorised,
            false,
        );

        let before = store
            .get_uncategorised_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        let preview: Vec<i64> = store
            .preview_filters()
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();
        // The dry-run touched nothing.
        let after = store
            .get_uncategorised_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(
            before.iter().map(|t| t.id).collect::<Vec<_>>(),
            after.iter().map(|t| t.id).collect::<Vec<_>>()
        );
        assert!(enrichment(&store, "Interest").is_none());

        // The real apply categorises exactly the previewed transactions.
        let applied = store.preview_filters().unwrap();
        assert_eq!(applied.iter().map(|t| t.id).collect::<Vec<_>>(), preview);
        assert_eq!(store.apply_filters().unwrap(), preview.len());
        assert_eq!(
            enrichment(&store, "Interest"),
            Some((CategorySource::Rule, true, "Income/Interest".into()))
        );
    }

    #[test]
    fn filter_never_categorises_transfer_leg() {
        let (_t, mut store) = setup_rich_fixture();
        add_filter(
            &mut store,
            "xfer",
            "Transfer",
            "Should/NotApply",
            FilterOverride::All,
            false,
        );
        assert_eq!(store.apply_filters().unwrap(), 0);
        assert!(enrichment(&store, "Transfer Out").is_none());
        assert!(enrichment(&store, "Transfer In").is_none());
    }
}
