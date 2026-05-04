use chrono::{NaiveDate, Utc};
use rusqlite::{Connection, OptionalExtension, params, types::Value};
use std::path::{Path, PathBuf};

use crate::db::{build_searchable_text, init_db};
use crate::import::{
    compute_hash, find_csv_files, find_import_script, hash_file, run_import_script,
};
use crate::search::{ParsedQuery, SqlContext};
use crate::{
    Account, Bank, Category, CategorySource, Error, FuzzyMatcher, RefreshReport, Result,
    Transaction, TransactionEnrichment, TransactionWithEnrichment, Transfer, TransferSource,
    TransferWithTransactions,
};

/// SQL context for queries rooted at the `transactions t` / `accounts a` /
/// `banks b` aliases (with optional `categories c` and `transactions_fts fts`).
fn transaction_ctx() -> SqlContext {
    SqlContext::new()
        .with("date", "t.date")
        .with("amount_cents", "t.amount_cents")
        .with("description", "t.description")
        .with("bank_name", "b.name")
        .with("account_name", "a.name")
        .with("category_path", "c.path")
        .with("fts", "transactions_fts")
}

/// SQL context for transfer queries — same as transaction_ctx but with custom
/// table aliases (so we can render once for from-side, once for to-side).
///
fn transfer_side_ctx(tx_alias: &str, account_alias: &str, bank_alias: &str) -> SqlContext {
    SqlContext::new()
        .with("date", format!("{}.date", tx_alias))
        .with("amount_cents", format!("{}.amount_cents", tx_alias))
        .with("description", format!("{}.description", tx_alias))
        .with("bank_name", format!("{}.name", bank_alias))
        .with("account_name", format!("{}.name", account_alias))
}

/// Joins to splice into a transaction query based on what the parsed query needs.
fn transaction_joins(parsed: &ParsedQuery) -> String {
    let mut joins = String::new();
    if parsed.fts_query().is_some() {
        joins.push_str(" JOIN transactions_fts fts ON t.id = fts.rowid");
    }
    if parsed.uses_placeholder("category_path") {
        joins.push_str(
            " LEFT JOIN transaction_enrichments e ON t.id = e.transaction_id\
             \n LEFT JOIN categories c ON e.category_id = c.id",
        );
    }
    joins
}

fn render_transfer_query(query: &ParsedQuery) -> crate::search::Rendered {
    let mut lhs = query.render(&transfer_side_ctx("ft", "fa", "fb"));
    add_transfer_side_fts(&mut lhs, "ft", query);

    let mut rhs = query.render(&transfer_side_ctx("tt", "ta", "tb"));
    add_transfer_side_fts(&mut rhs, "tt", query);

    match (lhs.is_empty(), rhs.is_empty()) {
        (true, true) => crate::search::Rendered::default(),
        (false, true) => lhs,
        (true, false) => rhs,
        (false, false) => {
            let mut params = lhs.params;
            params.extend(rhs.params);
            crate::search::Rendered {
                where_clause: format!("(({}) OR ({}))", lhs.where_clause, rhs.where_clause),
                params,
            }
        }
    }
}

fn add_transfer_side_fts(
    rendered: &mut crate::search::Rendered,
    tx_alias: &str,
    query: &ParsedQuery,
) {
    for fts_query in query.fts_queries() {
        if !rendered.where_clause.is_empty() {
            rendered.where_clause.push_str(" AND ");
        }
        rendered.where_clause.push_str(&format!(
            "{}.id IN (SELECT rowid FROM transactions_fts WHERE transactions_fts MATCH ?)",
            tx_alias
        ));
        rendered.params.push(Value::Text(fts_query.to_string()));
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

        // Wrap entire import in a transaction for performance
        self.conn.execute("BEGIN", [])?;

        let result = self.refresh_inner(&mut report);

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

    fn refresh_inner(&mut self, report: &mut RefreshReport) -> Result<()> {
        let batch_id = self.create_import_batch()?;

        let discovered = self.discover_banks_and_accounts()?;

        for (bank_name, account_names) in &discovered {
            let bank_id = self.ensure_bank(bank_name, report)?;

            for account_name in account_names {
                let account_id = self.ensure_account(bank_id, account_name, report)?;

                self.import_account_transactions(
                    bank_id,
                    account_id,
                    bank_name,
                    account_name,
                    batch_id,
                    report,
                )?;
            }
        }

        self.soft_delete_missing_banks(&discovered, report)?;
        self.soft_delete_missing_accounts(&discovered, report)?;

        self.complete_import_batch(batch_id)?;

        Ok(())
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
        let mut sql = String::from(
            "SELECT t.id, a.bank_id, t.account_id, t.date, t.description,
                    t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id
             FROM transactions t
             JOIN accounts a ON t.account_id = a.id
             JOIN banks b ON a.bank_id = b.id",
        );
        sql.push_str(&transaction_joins(query));
        sql.push_str(" WHERE a.deleted_at IS NULL");

        let rendered = query.render(&transaction_ctx());
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY t.date DESC, t.id DESC");

        if let Some(limit) = limit {
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer(limit as i64));
        }

        log::debug!("query_transactions SQL: {} params: {:?}", sql, params);

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
        _bank_id: i64,
        account_id: i64,
        bank_name: &str,
        account_name: &str,
        batch_id: i64,
        report: &mut RefreshReport,
    ) -> Result<()> {
        let script = find_import_script(&self.exports_dir, bank_name, account_name);
        let script = match script {
            Some(s) => s,
            None => {
                return Ok(());
            }
        };

        let account_dir = self.exports_dir.join(bank_name).join(account_name);
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
                    &relative_path,
                    batch_id,
                )?;

                if inserted {
                    report.transactions_added += 1;
                } else {
                    report.transactions_skipped += 1;
                }
            }

            self.mark_file_imported(account_id, &relative_path, &content_hash, batch_id)?;
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
            .query_map([], |row| {
                Ok(Category {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    created_at: parse_datetime(&row.get::<_, String>(2)?)?,
                })
            })?
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
        scored.sort_by(|a, b| b.0.cmp(&a.0));
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
                |row| {
                    Ok(Category {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        created_at: parse_datetime(&row.get::<_, String>(2)?)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Get the category assigned to a transaction.
    pub fn get_transaction_category(&self, transaction_id: i64) -> Result<Option<Category>> {
        self.conn
            .query_row(
                "SELECT c.id, c.path, c.created_at 
                 FROM categories c
                 JOIN transaction_enrichments e ON c.id = e.category_id
                 WHERE e.transaction_id = ?",
                [transaction_id],
                |row| {
                    Ok(Category {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        created_at: parse_datetime(&row.get::<_, String>(2)?)?,
                    })
                },
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

    /// Get category by path.
    pub fn get_category_by_path(&self, path: &str) -> Result<Option<Category>> {
        let normalised = path.trim().trim_matches('/');
        self.conn
            .query_row(
                "SELECT id, path, created_at FROM categories WHERE path = ?",
                [normalised],
                |row| {
                    Ok(Category {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        created_at: parse_datetime(&row.get::<_, String>(2)?)?,
                    })
                },
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

    // ==================== Transfers ====================

    /// Create a transfer linking two transactions.
    pub fn create_transfer(
        &mut self,
        from_transaction_id: i64,
        to_transaction_id: i64,
        source: TransferSource,
        confirmed: bool,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO transfers (from_transaction_id, to_transaction_id, source, confirmed, created_at)
             VALUES (?, ?, ?, ?, ?)",
            params![
                from_transaction_id,
                to_transaction_id,
                source.as_str(),
                confirmed,
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
        self.conn
            .query_row(
                "SELECT id, from_transaction_id, to_transaction_id, source, confirmed, created_at
                 FROM transfers 
                 WHERE from_transaction_id = ? OR to_transaction_id = ?",
                params![transaction_id, transaction_id],
                parse_transfer,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Find potential matching transactions for a transfer.
    pub fn find_matching_transfer_candidates(&self, tx: &Transaction) -> Result<Vec<Transaction>> {
        let opposite_amount = -tx.amount_cents;

        // First try other accounts
        let mut stmt = self.conn.prepare(
            "SELECT t.id, a.bank_id, t.account_id, t.date, t.description,
                    t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id
             FROM transactions t
             JOIN accounts a ON t.account_id = a.id
             LEFT JOIN transfers tr ON t.id = tr.from_transaction_id OR t.id = tr.to_transaction_id
             WHERE t.amount_cents = ? 
               AND t.account_id != ?
               AND t.id != ?
               AND tr.id IS NULL
               AND a.deleted_at IS NULL
             ORDER BY ABS(julianday(t.date) - julianday(?)), t.id",
        )?;
        let mut transactions: Vec<Transaction> = stmt
            .query_map(
                params![opposite_amount, tx.account_id, tx.id, tx.date.to_string()],
                parse_transaction,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // If no matches in other accounts, try same account (for rebates, etc.)
        if transactions.is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT t.id, a.bank_id, t.account_id, t.date, t.description,
                        t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id
                 FROM transactions t
                 JOIN accounts a ON t.account_id = a.id
                 LEFT JOIN transfers tr ON t.id = tr.from_transaction_id OR t.id = tr.to_transaction_id
                 WHERE t.amount_cents = ? 
                   AND t.id != ?
                   AND tr.id IS NULL
                   AND a.deleted_at IS NULL
                 ORDER BY ABS(julianday(t.date) - julianday(?)), t.id",
            )?;
            transactions = stmt
                .query_map(
                    params![opposite_amount, tx.id, tx.date.to_string()],
                    parse_transaction,
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
        }

        Ok(transactions)
    }

    // ==================== Todo Queries ====================

    /// Get transactions that need categorization, scoped by a parsed search query.
    pub fn get_uncategorised_transactions(
        &self,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<Transaction>> {
        let mut sql = String::from(
            "SELECT t.id, a.bank_id, t.account_id, t.date, t.description,
                    t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id
             FROM transactions t
             JOIN accounts a ON t.account_id = a.id
             JOIN banks b ON a.bank_id = b.id",
        );
        // FTS join (conditional on the parsed query)
        if query.fts_query().is_some() {
            sql.push_str(" JOIN transactions_fts fts ON t.id = fts.rowid");
        }
        // Always join enrichments (we filter on missing category) and transfers
        // (we exclude transactions involved in a transfer).
        sql.push_str(
            " LEFT JOIN transaction_enrichments e ON t.id = e.transaction_id
             LEFT JOIN transfers tr ON t.id = tr.from_transaction_id OR t.id = tr.to_transaction_id",
        );
        // Optional category join is a no-op here because `e` is already joined
        // — if the user filters by category we just need `c` too.
        if query.uses_placeholder("category_path") {
            sql.push_str(" LEFT JOIN categories c ON e.category_id = c.id");
        }
        sql.push_str(
            " WHERE a.deleted_at IS NULL
               AND (e.category_id IS NULL OR e.id IS NULL)
               AND tr.id IS NULL",
        );

        let rendered = query.render(&transaction_ctx());
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY t.date DESC, t.id DESC");

        if let Some(limit) = limit {
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer(limit as i64));
        }

        log::debug!(
            "get_uncategorised_transactions SQL: {} params: {:?}",
            sql,
            params
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let transactions = stmt
            .query_map(rusqlite::params_from_iter(params), parse_transaction)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(transactions)
    }

    /// Get transactions with AI-suggested categories pending review.
    pub fn get_pending_ai_reviews(
        &self,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<TransactionWithEnrichment>> {
        let mut sql = String::from(
            "SELECT t.id, a.bank_id, t.account_id, t.date, t.description,
                    t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id,
                    e.id, e.transaction_id, e.category_id, e.category_source, e.category_confirmed,
                    e.ai_confidence, e.created_at, e.updated_at,
                    c.id, c.path, c.created_at
             FROM transactions t
             JOIN accounts a ON t.account_id = a.id
             JOIN banks b ON a.bank_id = b.id",
        );
        if query.fts_query().is_some() {
            sql.push_str(" JOIN transactions_fts fts ON t.id = fts.rowid");
        }
        // Enrichment is required (we filter on it) and category is needed for SELECT.
        sql.push_str(
            " JOIN transaction_enrichments e ON t.id = e.transaction_id
             LEFT JOIN categories c ON e.category_id = c.id",
        );
        sql.push_str(
            " WHERE a.deleted_at IS NULL
               AND e.category_source = 'ai'
               AND e.category_confirmed = 0",
        );

        let rendered = query.render(&transaction_ctx());
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY t.date DESC, t.id DESC");

        if let Some(limit) = limit {
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer(limit as i64));
        }

        log::debug!("get_pending_ai_reviews SQL: {} params: {:?}", sql, params);

        let mut stmt = self.conn.prepare(&sql)?;
        let results = stmt
            .query_map(rusqlite::params_from_iter(params), |row| {
                let transaction = parse_transaction(row)?;
                let enrichment = Some(parse_enrichment_at_offset(row, 11)?);
                let category = if row.get::<_, Option<i64>>(19)?.is_some() {
                    Some(Category {
                        id: row.get(19)?,
                        path: row.get(20)?,
                        created_at: parse_datetime(&row.get::<_, String>(21)?)?,
                    })
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
        let mut sql = String::from(
            "SELECT tr.id, tr.from_transaction_id, tr.to_transaction_id, tr.source, tr.confirmed, tr.created_at
             FROM transfers tr
             JOIN transactions ft ON ft.id = tr.from_transaction_id
             JOIN accounts fa ON fa.id = ft.account_id
             JOIN banks fb ON fb.id = fa.bank_id
             JOIN transactions tt ON tt.id = tr.to_transaction_id
             JOIN accounts ta ON ta.id = tt.account_id
             JOIN banks tb ON tb.id = ta.bank_id
            ",
        );

        sql.push_str(
            " WHERE tr.confirmed = 0
               AND fa.deleted_at IS NULL AND ta.deleted_at IS NULL",
        );

        let rendered = render_transfer_query(query);
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY tr.created_at DESC");

        if let Some(limit) = limit {
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer(limit as i64));
        }

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
                "SELECT t.id, a.bank_id, t.account_id, t.date, t.description,
                        t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id
                 FROM transactions t
                 JOIN accounts a ON t.account_id = a.id
                 WHERE t.id = ?",
                [id],
                parse_transaction,
            )
            .optional()
            .map_err(Into::into)
    }

    /// List transfers with all transaction data resolved.
    /// Filters match if EITHER the from or to transaction matches.
    pub fn list_transfers_with_transactions(
        &self,
        confirmed_only: bool,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<TransferWithTransactions>> {
        let mut sql = String::from(
            "SELECT
                tr.id, tr.from_transaction_id, tr.to_transaction_id, tr.source, tr.confirmed, tr.created_at,
                ft.id, fb.id, ft.account_id, ft.date, ft.description, ft.amount_cents, ft.balance_cents, ft.hash, ft.metadata, ft.source_file, ft.import_batch_id,
                tt.id, tb.id, tt.account_id, tt.date, tt.description, tt.amount_cents, tt.balance_cents, tt.hash, tt.metadata, tt.source_file, tt.import_batch_id
             FROM transfers tr
             JOIN transactions ft ON ft.id = tr.from_transaction_id
             JOIN accounts fa ON fa.id = ft.account_id
             JOIN banks fb ON fb.id = fa.bank_id
             JOIN transactions tt ON tt.id = tr.to_transaction_id
             JOIN accounts ta ON ta.id = tt.account_id
             JOIN banks tb ON tb.id = ta.bank_id
            ",
        );

        sql.push_str(" WHERE fa.deleted_at IS NULL AND ta.deleted_at IS NULL");

        if confirmed_only {
            sql.push_str(" AND tr.confirmed = 1");
        }

        let rendered = render_transfer_query(query);
        sql.push_str(&rendered.and_prefix());
        let mut params = rendered.params;

        sql.push_str(" ORDER BY tr.created_at DESC");

        if let Some(limit) = limit {
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer(limit as i64));
        }

        log::debug!(
            "list_transfers_with_transactions SQL: {} params: {:?}",
            sql,
            params
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let result = stmt
            .query_map(rusqlite::params_from_iter(params), |row| {
                let transfer = parse_transfer(row)?;
                let from_transaction = parse_transaction_at_offset(row, 6)?;
                let to_transaction = parse_transaction_at_offset(row, 17)?;
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

fn parse_transaction(row: &rusqlite::Row) -> rusqlite::Result<Transaction> {
    parse_transaction_at_offset(row, 0)
}

fn parse_transaction_at_offset(
    row: &rusqlite::Row,
    offset: usize,
) -> rusqlite::Result<Transaction> {
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

fn parse_enrichment_at_offset(
    row: &rusqlite::Row,
    offset: usize,
) -> rusqlite::Result<TransactionEnrichment> {
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
    use crate::search::{
        AccountFilter, AmountFilter, CategoryFilter, DateFilter, SearchConfig, parse,
    };
    use std::fs;
    use tempfile::TempDir;

    /// Build a SearchConfig with all standard filters and no completion options.
    fn search_config() -> SearchConfig {
        SearchConfig::new(vec![
            Box::new(DateFilter),
            Box::new(AmountFilter),
            Box::new(AccountFilter::with_options(vec![])),
            Box::new(CategoryFilter::with_options(vec![])),
        ])
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

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&import_script, fs::Permissions::from_mode(0o755)).unwrap();
        }

        temp
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
            .create_transfer(xfer_out, xfer_in, TransferSource::Manual, true)
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
}
