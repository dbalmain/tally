use chrono::{NaiveDate, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

use crate::db::init_db;
use crate::import::{compute_hash, find_csv_files, find_import_script, hash_file, run_import_script};
use crate::{
    Account, Bank, Category, CategorySource, Error, RefreshReport, Result, Transaction,
    TransactionEnrichment, TransactionFilter, TransactionWithEnrichment, Transfer,
    TransferSource, TransferWithTransactions,
};

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

        let batch_id = self.create_import_batch()?;

        let discovered = self.discover_banks_and_accounts()?;

        for (bank_name, account_names) in &discovered {
            let bank_id = self.ensure_bank(bank_name, &mut report)?;

            for account_name in account_names {
                let account_id = self.ensure_account(bank_id, account_name, &mut report)?;

                self.import_account_transactions(
                    bank_id,
                    account_id,
                    bank_name,
                    account_name,
                    batch_id,
                    &mut report,
                )?;
            }
        }

        self.soft_delete_missing_banks(&discovered, &mut report)?;
        self.soft_delete_missing_accounts(&discovered, &mut report)?;

        self.complete_import_batch(batch_id)?;

        Ok(report)
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

    /// Query transactions with optional filters.
    pub fn query_transactions(&self, filter: &TransactionFilter) -> Result<Vec<Transaction>> {
        let mut sql = String::from(
            "SELECT t.id, a.bank_id, t.account_id, t.date, t.description, 
                    t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id
             FROM transactions t
             JOIN accounts a ON t.account_id = a.id
             WHERE a.deleted_at IS NULL",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(bank_id) = filter.bank_id {
            sql.push_str(" AND a.bank_id = ?");
            params_vec.push(Box::new(bank_id));
        }
        if let Some(account_id) = filter.account_id {
            sql.push_str(" AND t.account_id = ?");
            params_vec.push(Box::new(account_id));
        }
        if let Some(ref from_date) = filter.from_date {
            sql.push_str(" AND t.date >= ?");
            params_vec.push(Box::new(from_date.to_string()));
        }
        if let Some(ref to_date) = filter.to_date {
            sql.push_str(" AND t.date <= ?");
            params_vec.push(Box::new(to_date.to_string()));
        }
        if let Some(ref desc) = filter.description_contains {
            sql.push_str(" AND t.description LIKE ?");
            params_vec.push(Box::new(format!("%{}%", desc)));
        }

        sql.push_str(" ORDER BY t.date DESC, t.id DESC");

        if let Some(limit) = filter.limit {
            sql.push_str(" LIMIT ?");
            params_vec.push(Box::new(limit as i64));
        }
        if let Some(offset) = filter.offset {
            sql.push_str(" OFFSET ?");
            params_vec.push(Box::new(offset as i64));
        }

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let transactions = stmt
            .query_map(params_refs.as_slice(), |row| {
                let metadata_str: String = row.get(8)?;
                let metadata: std::collections::HashMap<String, serde_json::Value> =
                    serde_json::from_str(&metadata_str).unwrap_or_default();
                let date_str: String = row.get(3)?;
                let date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());

                Ok(Transaction {
                    id: row.get(0)?,
                    bank_id: row.get(1)?,
                    account_id: row.get(2)?,
                    date,
                    description: row.get(4)?,
                    amount_cents: row.get(5)?,
                    balance_cents: row.get(6)?,
                    hash: row.get(7)?,
                    metadata,
                    source_file: row.get(9)?,
                    import_batch_id: row.get(10)?,
                })
            })?
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
                self.conn.execute(
                    "UPDATE banks SET deleted_at = NULL WHERE id = ?",
                    [id],
                )?;
                Ok(id)
            }
            Some((id, None)) => Ok(id),
            None => {
                self.conn.execute(
                    "INSERT INTO banks (name) VALUES (?)",
                    [name],
                )?;
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
                self.conn.execute(
                    "UPDATE accounts SET deleted_at = NULL WHERE id = ?",
                    [id],
                )?;
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
                    compute_hash(&raw_tx.date, &raw_tx.description, raw_tx.amount_cents, raw_tx.balance_cents)
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

    fn mark_file_imported(&self, account_id: i64, path: &str, content_hash: &str, batch_id: i64) -> Result<()> {
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
        Ok(result > 0)
    }

    fn soft_delete_missing_banks(
        &self,
        discovered: &[(String, Vec<String>)],
        report: &mut RefreshReport,
    ) -> Result<()> {
        let discovered_names: Vec<&str> = discovered.iter().map(|(name, _)| name.as_str()).collect();

        let mut stmt = self.conn.prepare(
            "SELECT id, name FROM banks WHERE deleted_at IS NULL",
        )?;
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
        let query_lower = query.to_lowercase();
        let mut scored: Vec<(i32, Category)> = all
            .into_iter()
            .filter_map(|cat| {
                let path_lower = cat.path.to_lowercase();
                fuzzy_match(&path_lower, &query_lower).map(|score| (score, cat))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(scored.into_iter().map(|(_, cat)| cat).collect())
    }

    /// Get or create a category by path.
    pub fn get_or_create_category(&mut self, path: &str) -> Result<i64> {
        let normalized = path.trim().trim_matches('/');
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM categories WHERE path = ?",
                [normalized],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(id);
        }

        self.conn.execute(
            "INSERT INTO categories (path, created_at) VALUES (?, ?)",
            params![normalized, Utc::now().to_rfc3339()],
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

    // ==================== Enrichments ====================

    /// Get enrichment data for a transaction.
    pub fn get_enrichment(&self, transaction_id: i64) -> Result<Option<TransactionEnrichment>> {
        self.conn
            .query_row(
                "SELECT id, transaction_id, category_id, category_source, category_confirmed, 
                        ai_confidence, created_at, updated_at 
                 FROM transaction_enrichments WHERE transaction_id = ?",
                [transaction_id],
                parse_enrichment,
            )
            .optional()
            .map_err(Into::into)
    }

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

    /// Get transactions that need categorization.
    pub fn get_uncategorized_transactions(&self, limit: usize) -> Result<Vec<Transaction>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, a.bank_id, t.account_id, t.date, t.description,
                    t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id
             FROM transactions t
             JOIN accounts a ON t.account_id = a.id
             LEFT JOIN transaction_enrichments e ON t.id = e.transaction_id
             LEFT JOIN transfers tr ON t.id = tr.from_transaction_id OR t.id = tr.to_transaction_id
             WHERE a.deleted_at IS NULL
               AND (e.category_id IS NULL OR e.id IS NULL)
               AND tr.id IS NULL
             ORDER BY t.date DESC, t.id DESC
             LIMIT ?",
        )?;
        let transactions = stmt
            .query_map([limit as i64], parse_transaction)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(transactions)
    }

    /// Get transactions with AI-suggested categories pending review.
    pub fn get_pending_ai_reviews(&self, limit: usize) -> Result<Vec<TransactionWithEnrichment>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, a.bank_id, t.account_id, t.date, t.description,
                    t.amount_cents, t.balance_cents, t.hash, t.metadata, t.source_file, t.import_batch_id,
                    e.id, e.transaction_id, e.category_id, e.category_source, e.category_confirmed,
                    e.ai_confidence, e.created_at, e.updated_at,
                    c.id, c.path, c.created_at
             FROM transactions t
             JOIN accounts a ON t.account_id = a.id
             JOIN transaction_enrichments e ON t.id = e.transaction_id
             LEFT JOIN categories c ON e.category_id = c.id
             WHERE a.deleted_at IS NULL
               AND e.category_source = 'ai'
               AND e.category_confirmed = 0
             ORDER BY t.date DESC, t.id DESC
             LIMIT ?",
        )?;
        let results = stmt
            .query_map([limit as i64], |row| {
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
    pub fn get_pending_transfer_reviews(&self, limit: usize) -> Result<Vec<Transfer>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, from_transaction_id, to_transaction_id, source, confirmed, created_at
             FROM transfers
             WHERE confirmed = 0
             ORDER BY created_at DESC
             LIMIT ?",
        )?;
        let transfers = stmt
            .query_map([limit as i64], parse_transfer)?
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

    /// List transfers, optionally filtered to confirmed only.
    pub fn list_transfers(&self, confirmed_only: bool) -> Result<Vec<Transfer>> {
        let sql = if confirmed_only {
            "SELECT id, from_transaction_id, to_transaction_id, source, confirmed, created_at
             FROM transfers WHERE confirmed = 1 ORDER BY created_at DESC"
        } else {
            "SELECT id, from_transaction_id, to_transaction_id, source, confirmed, created_at
             FROM transfers ORDER BY created_at DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let transfers = stmt
            .query_map([], parse_transfer)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(transfers)
    }

    /// List transfers with all transaction data resolved (single query, no N+1).
    pub fn list_transfers_with_transactions(
        &self,
        confirmed_only: bool,
    ) -> Result<Vec<TransferWithTransactions>> {
        let sql = format!(
            "SELECT 
                tr.id, tr.from_transaction_id, tr.to_transaction_id, tr.source, tr.confirmed, tr.created_at,
                ft.id, fa.bank_id, ft.account_id, ft.date, ft.description, ft.amount_cents, ft.balance_cents, ft.hash, ft.metadata, ft.source_file, ft.import_batch_id,
                tt.id, ta.bank_id, tt.account_id, tt.date, tt.description, tt.amount_cents, tt.balance_cents, tt.hash, tt.metadata, tt.source_file, tt.import_batch_id
             FROM transfers tr
             JOIN transactions ft ON ft.id = tr.from_transaction_id
             JOIN accounts fa ON fa.id = ft.account_id
             JOIN transactions tt ON tt.id = tr.to_transaction_id
             JOIN accounts ta ON ta.id = tt.account_id
             {}
             ORDER BY tr.created_at DESC",
            if confirmed_only { "WHERE tr.confirmed = 1" } else { "" }
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let result = stmt
            .query_map([], |row| {
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
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(e),
        ))
}

fn parse_transaction(row: &rusqlite::Row) -> rusqlite::Result<Transaction> {
    parse_transaction_at_offset(row, 0)
}

fn parse_transaction_at_offset(row: &rusqlite::Row, offset: usize) -> rusqlite::Result<Transaction> {
    let metadata_str: String = row.get(offset + 8)?;
    let metadata: std::collections::HashMap<String, serde_json::Value> =
        serde_json::from_str(&metadata_str).unwrap_or_default();
    let date_str: String = row.get(offset + 3)?;
    let date = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(offset + 3, rusqlite::types::Type::Text, Box::new(e))
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

fn parse_enrichment(row: &rusqlite::Row) -> rusqlite::Result<TransactionEnrichment> {
    parse_enrichment_at_offset(row, 0)
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

fn fuzzy_match(haystack: &str, needle: &str) -> Option<i32> {
    let mut score = 0i32;
    let mut haystack_chars = haystack.chars().peekable();
    let mut prev_matched = false;
    let mut needle_pos = 0;

    for needle_char in needle.chars() {
        let mut found = false;
        while let Some(&h_char) = haystack_chars.peek() {
            haystack_chars.next();
            if h_char == needle_char {
                found = true;
                if prev_matched {
                    score += 2;
                } else {
                    score += 1;
                }
                prev_matched = true;
                break;
            } else {
                prev_matched = false;
            }
        }
        if !found {
            return None;
        }
        needle_pos += 1;
    }

    if haystack.starts_with(needle) {
        score += 10;
    }

    Some(score + needle_pos)
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
    use std::fs;
    use tempfile::TempDir;

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

    #[test]
    fn test_discover_banks_and_accounts() {
        let temp = setup_test_exports();
        let store = TransactionStore::open_in_memory(temp.path()).unwrap();

        let discovered = store.discover_banks_and_accounts().unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].0, "TestBank");
        assert_eq!(discovered[0].1, vec!["Checking"]);
    }

    #[test]
    fn test_refresh_creates_banks_and_accounts() {
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
    fn test_soft_delete_missing_bank() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();

        store.refresh().unwrap();

        fs::remove_dir_all(temp.path().join("TestBank")).unwrap();

        let report = store.refresh().unwrap();
        assert_eq!(report.banks_deleted, 1);

        let banks = store.list_banks().unwrap();
        assert!(banks.is_empty());
    }
}
