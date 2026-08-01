//! Import orchestration: `refresh()`, pull/CSV import, imported-file
//! tracking, and bank/account sync with soft deletes.

use chrono::{NaiveDate, Utc};
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::db::{TRANSACTIONS_FTS_DDL, build_searchable_text};
use crate::import::{
    compute_hash, find_csv_files, find_import_script, find_pull_script, hash_file,
    run_import_script, run_pull_script,
};
use crate::{Account, Bank, Error, RawTransaction, RefreshReport, Result};

use super::{TransactionStore, parse_datetime};

const PULL_CONCURRENCY: usize = 6;

type PullResults = HashMap<(String, String), Result<Vec<RawTransaction>>>;

type Metadata = HashMap<String, serde_json::Value>;

/// What one raw transaction did to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportOutcome {
    /// A new row.
    Added,
    /// An existing `(account_id, hash)` row whose description or metadata the
    /// source has since refined.
    Updated,
    /// An existing row the source re-sent unchanged.
    Unchanged,
}

struct PullJob {
    bank_name: String,
    account_name: String,
    script: PathBuf,
    account_dir: PathBuf,
}

impl TransactionStore {
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

    /// Resolve a bank by name, undeleting or inserting as needed. Returns
    /// `(bank_id, was_created)`; `was_created` is true only for the INSERT
    /// branch (an undelete is not a creation).
    pub(crate) fn get_or_create_bank(&self, name: &str) -> Result<(i64, bool)> {
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
                Ok((id, false))
            }
            Some((id, None)) => Ok((id, false)),
            None => {
                self.conn
                    .execute("INSERT INTO banks (name) VALUES (?)", [name])?;
                Ok((self.conn.last_insert_rowid(), true))
            }
        }
    }

    fn ensure_bank(&self, name: &str, report: &mut RefreshReport) -> Result<i64> {
        let (id, created) = self.get_or_create_bank(name)?;
        if created {
            report.banks_added += 1;
        }
        Ok(id)
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
    /// tallying added/updated/skipped counts in `report`.
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

            let outcome = self.insert_transaction(
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

            match outcome {
                ImportOutcome::Added => report.transactions_added += 1,
                ImportOutcome::Updated => report.transactions_updated += 1,
                ImportOutcome::Unchanged => report.transactions_skipped += 1,
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
        metadata: &Metadata,
        source_file: &str,
        batch_id: i64,
    ) -> Result<ImportOutcome> {
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
            let rowid = self.conn.last_insert_rowid();
            // A row that was just inserted has no note yet, so there is nothing
            // to fold in; `set_note` rewrites the posting when one is added.
            self.write_transaction_fts(rowid, description, metadata, None)?;
            return Ok(ImportOutcome::Added);
        }

        self.refine_existing_transaction(account_id, hash, description, metadata)
    }

    /// The row already exists under `(account_id, hash)`. Feeds refine text
    /// after first posting — a card BPAY lands as `BPAY SALES PARRAMATTA AUS`
    /// and only later becomes `BPAYN ACT REVENUE OFFICE BPAY, Camp Rates Aug26`
    /// — so a re-pull carrying better text should apply it rather than being
    /// discarded as a duplicate.
    ///
    /// Only description and metadata move. The hash asserts this is the same
    /// real-world transaction, so identity and economic fields (`date`,
    /// `amount_cents`, `balance_cents`, `account_id`) are never touched: a feed
    /// that changed those is reporting a different transaction, not a
    /// refinement of this one. The row id is preserved, so categories, notes,
    /// tags and transfer links all survive.
    fn refine_existing_transaction(
        &self,
        account_id: i64,
        hash: &str,
        description: &str,
        metadata: &Metadata,
    ) -> Result<ImportOutcome> {
        let existing: Option<(i64, String, String)> = self
            .conn
            .query_row(
                "SELECT id, description, metadata FROM transactions
                 WHERE account_id = ? AND hash = ?",
                params![account_id, hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        // Defensive: the INSERT was ignored, so a row must exist. If some other
        // constraint swallowed it, leave the store alone.
        let Some((id, stored_description, stored_metadata_json)) = existing else {
            return Ok(ImportOutcome::Unchanged);
        };

        let stored_metadata: Metadata =
            serde_json::from_str(&stored_metadata_json).unwrap_or_default();
        let merged_metadata = merge_metadata(&stored_metadata, metadata);

        // A feed that drops the payee entirely is a regression, not a
        // refinement, and the stored text is unrecoverable — so never blank it.
        let new_description = if description.trim().is_empty() {
            stored_description.as_str()
        } else {
            description
        };

        if new_description == stored_description && merged_metadata == stored_metadata {
            return Ok(ImportOutcome::Unchanged);
        }

        // Keep the stored JSON verbatim when the map is unchanged: re-encoding
        // a HashMap reorders its keys, which would churn the column on every
        // description-only refinement.
        let new_metadata_json = if merged_metadata == stored_metadata {
            stored_metadata_json
        } else {
            serde_json::to_string(&merged_metadata)?
        };

        self.conn.execute(
            "UPDATE transactions SET description = ?, metadata = ? WHERE id = ?",
            params![new_description, new_metadata_json, id],
        )?;
        // Note-aware: the row may carry a note that is part of its indexed text.
        self.refresh_transaction_fts(id)?;

        Ok(ImportOutcome::Updated)
    }

    /// Replace the contentless FTS posting for `rowid` with
    /// [`build_searchable_text`] of the given description, metadata, and note.
    ///
    /// Contentless FTS5 permits multiple postings per rowid and never
    /// cross-checks them against the real row, so every write must DELETE
    /// first — otherwise a reused or re-imported rowid leaves phantom tokens
    /// that produce false-positive search matches.
    pub(super) fn write_transaction_fts(
        &self,
        rowid: i64,
        description: &str,
        metadata: &std::collections::HashMap<String, serde_json::Value>,
        note: Option<&str>,
    ) -> Result<()> {
        let searchable_text = build_searchable_text(description, metadata, note);
        self.conn.execute(
            "DELETE FROM transactions_fts WHERE rowid = ?",
            params![rowid],
        )?;
        self.conn.execute(
            "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
            params![rowid, searchable_text],
        )?;
        Ok(())
    }

    /// Re-derive one transaction's FTS posting from its current stored row and
    /// note. Used after a note changes, where the caller has the id but not the
    /// description/metadata.
    pub(super) fn refresh_transaction_fts(&self, tx_id: i64) -> Result<()> {
        let row = self
            .conn
            .query_row(
                "SELECT t.description, t.metadata, n.note
                 FROM transactions t
                 LEFT JOIN transaction_notes n ON n.transaction_id = t.id
                 WHERE t.id = ?",
                params![tx_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;

        let Some((description, metadata_json, note)) = row else {
            return Ok(());
        };
        let metadata: HashMap<String, serde_json::Value> =
            serde_json::from_str(&metadata_json).unwrap_or_default();
        self.write_transaction_fts(tx_id, &description, &metadata, note.as_deref())
    }

    /// Drop and recreate `transactions_fts`, then repopulate one posting per
    /// transaction from [`build_searchable_text`]. Returns the number of rows
    /// reindexed. This is the only safe full-rebuild path.
    pub fn rebuild_fts(&self) -> Result<usize> {
        self.conn
            .execute_batch("DROP TABLE IF EXISTS transactions_fts;")?;
        self.conn.execute_batch(TRANSACTIONS_FTS_DDL)?;

        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.description, t.metadata, n.note
             FROM transactions t
             LEFT JOIN transaction_notes n ON n.transaction_id = t.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;

        let mut count = 0usize;
        for row in rows {
            let (id, description, metadata_json, note) = row?;
            let metadata: HashMap<String, serde_json::Value> =
                serde_json::from_str(&metadata_json).unwrap_or_default();
            let searchable_text = build_searchable_text(&description, &metadata, note.as_deref());
            self.conn.execute(
                "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
                params![id, searchable_text],
            )?;
            count += 1;
        }
        Ok(count)
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
}

/// Later pulls enrich a transaction's metadata (labels, memo, category hints,
/// `original_payee`), so incoming keys win. Keys the newer payload omits are
/// kept rather than dropped: a pull script that narrows what it emits should
/// not silently delete data already imported.
fn merge_metadata(stored: &Metadata, incoming: &Metadata) -> Metadata {
    let mut merged = stored.clone();
    for (key, value) in incoming {
        merged.insert(key.clone(), value.clone());
    }
    merged
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
    use std::collections::HashMap;
    use std::fs;

    use chrono::NaiveDate;
    use rusqlite::params;
    use tempfile::TempDir;

    use super::ImportOutcome;
    use crate::TransactionStore;
    use crate::db::build_searchable_text;
    use crate::search::ParsedQuery;
    use crate::store::test_support::{setup_test_exports, write_pull_script};

    /// Token guaranteed never present in fixture searchable text.
    const ABSENT_TOKEN: &str = "zzphantomxyz";

    /// Seed a bank/account/batch and return `(store, account_id)` ready for
    /// [`TransactionStore::insert_transaction`].
    fn store_ready_for_insert() -> (TempDir, TransactionStore, i64) {
        let temp = TempDir::new().unwrap();
        let store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store
            .conn
            .execute("INSERT INTO banks (name) VALUES ('TB')", [])
            .unwrap();
        let bank_id = store.conn.last_insert_rowid();
        store
            .conn
            .execute(
                "INSERT INTO accounts (bank_id, name) VALUES (?, 'A1')",
                [bank_id],
            )
            .unwrap();
        let account_id = store.conn.last_insert_rowid();
        store
            .conn
            .execute(
                "INSERT INTO import_batches (started_at) VALUES ('2024-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        (temp, store, account_id)
    }

    fn insert_tx_with_meta(
        store: &TransactionStore,
        account_id: i64,
        description: &str,
        metadata: &HashMap<String, serde_json::Value>,
        hash: &str,
    ) -> i64 {
        let outcome = reimport(store, account_id, description, metadata, hash);
        assert_eq!(outcome, ImportOutcome::Added);
        store.conn.last_insert_rowid()
    }

    /// Feed one raw transaction through the production insert path, as a
    /// re-pull of the same `(account_id, hash)` would.
    fn reimport(
        store: &TransactionStore,
        account_id: i64,
        description: &str,
        metadata: &HashMap<String, serde_json::Value>,
        hash: &str,
    ) -> ImportOutcome {
        let date = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        store
            .insert_transaction(
                account_id,
                &date,
                description,
                -100,
                0,
                hash,
                metadata,
                "test.csv",
                1,
            )
            .unwrap()
    }

    /// The stored `(description, amount_cents, date, metadata)` of one row.
    fn stored_row(
        store: &TransactionStore,
        id: i64,
    ) -> (String, i64, String, HashMap<String, serde_json::Value>) {
        store
            .conn
            .query_row(
                "SELECT description, amount_cents, date, metadata FROM transactions WHERE id = ?",
                [id],
                |row| {
                    let metadata_json: String = row.get(3)?;
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        serde_json::from_str(&metadata_json).unwrap(),
                    ))
                },
            )
            .unwrap()
    }

    fn meta(pairs: &[(&str, &str)]) -> HashMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect()
    }

    /// Rowids that match `token` in `transactions_fts`.
    fn fts_match_rowids(store: &TransactionStore, token: &str) -> Vec<i64> {
        let mut stmt = store
            .conn
            .prepare(
                "SELECT rowid FROM transactions_fts WHERE transactions_fts MATCH ? ORDER BY rowid",
            )
            .unwrap();
        stmt.query_map([token], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
    }

    /// Whitespace-separated FTS tokens of a transaction's searchable text.
    fn tokens_of(description: &str, metadata: &HashMap<String, serde_json::Value>) -> Vec<String> {
        build_searchable_text(description, metadata, None)
            .split_whitespace()
            .map(|t| t.to_string())
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// Assert every row's FTS posting matches exactly its own searchable-text
    /// tokens (present) and never the guaranteed-absent token.
    fn assert_fts_invariant(
        store: &TransactionStore,
        rows: &[(i64, &str, HashMap<String, serde_json::Value>)],
    ) {
        for &(rowid, description, ref metadata) in rows {
            let own_tokens = tokens_of(description, metadata);
            for token in &own_tokens {
                let hits = fts_match_rowids(store, token);
                assert!(
                    hits.contains(&rowid),
                    "rowid {rowid} should match own token {token:?}; hits={hits:?}"
                );
            }
            let absent_hits = fts_match_rowids(store, ABSENT_TOKEN);
            assert!(
                !absent_hits.contains(&rowid),
                "rowid {rowid} must not match absent token {ABSENT_TOKEN}"
            );

            // No token that is absent from this row's searchable text may match it.
            // Use tokens that appear on other rows (or ABSENT_TOKEN) as negatives.
            let own_lower: std::collections::HashSet<String> =
                own_tokens.iter().map(|t| t.to_ascii_lowercase()).collect();
            for &(other_id, other_desc, ref other_meta) in rows {
                if other_id == rowid {
                    continue;
                }
                for token in tokens_of(other_desc, other_meta) {
                    if own_lower.contains(&token.to_ascii_lowercase()) {
                        continue;
                    }
                    let hits = fts_match_rowids(store, &token);
                    assert!(
                        !hits.contains(&rowid),
                        "rowid {rowid} must not match foreign token {token:?} \
                         (from row {other_id}); hits={hits:?}"
                    );
                }
            }
        }
        // Globally, absent token matches nothing.
        assert!(
            fts_match_rowids(store, ABSENT_TOKEN).is_empty(),
            "absent token must match no rows"
        );
    }

    fn sample_rows() -> Vec<(&'static str, HashMap<String, serde_json::Value>)> {
        let mut shared_meta = HashMap::new();
        shared_meta.insert(
            "merchant".to_string(),
            serde_json::Value::String("SharedMerchant".to_string()),
        );

        let mut youtube_meta = HashMap::new();
        youtube_meta.insert(
            "service".to_string(),
            serde_json::Value::String("Premium".to_string()),
        );
        youtube_meta.insert("score".to_string(), serde_json::json!(42));

        let mut aami_meta = HashMap::new();
        aami_meta.insert(
            "policy".to_string(),
            serde_json::Value::String("CarCover".to_string()),
        );

        vec![
            ("Google YouTubePremium", youtube_meta),
            ("AAMI Insurance March", aami_meta),
            ("Coffee Shop", shared_meta.clone()),
            ("Grocery SharedMerchant Run", shared_meta),
            ("Salary Deposit", HashMap::new()),
        ]
    }

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

    #[test]
    fn fts_invariant_holds_after_insert() {
        let (_temp, store, account_id) = store_ready_for_insert();
        let samples = sample_rows();
        let mut rows = Vec::new();
        for (i, (desc, meta)) in samples.iter().enumerate() {
            let id = insert_tx_with_meta(&store, account_id, desc, meta, &format!("h-{i}"));
            rows.push((id, *desc, meta.clone()));
        }
        assert_fts_invariant(&store, &rows);
    }

    #[test]
    fn fts_drift_heals_on_idempotent_rewrite() {
        // Reproduce the live-vault bug: a contentless phantom posting at an
        // existing rowid makes an unrelated row match a leftover token (e.g.
        // "Google YouTubePremium" matching "aami").
        let (_temp, store, account_id) = store_ready_for_insert();
        let meta = HashMap::new();
        let rowid = insert_tx_with_meta(
            &store,
            account_id,
            "Google YouTubePremium",
            &meta,
            "yt-hash",
        );

        // Corrupt: extra posting at the same rowid (contentless permits this).
        store
            .conn
            .execute(
                "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
                params![rowid, "AAMI Insurance leftover"],
            )
            .unwrap();
        assert!(
            fts_match_rowids(&store, "aami").contains(&rowid),
            "precondition: phantom token must match before rewrite"
        );

        // Rewrite via the production path (DELETE-then-INSERT).
        store
            .write_transaction_fts(rowid, "Google YouTubePremium", &meta, None)
            .unwrap();

        assert!(
            !fts_match_rowids(&store, "aami").contains(&rowid),
            "phantom token must be gone after idempotent rewrite"
        );
        assert!(
            fts_match_rowids(&store, "YouTubePremium").contains(&rowid),
            "real tokens must still match after rewrite"
        );
        assert_fts_invariant(&store, &[(rowid, "Google YouTubePremium", meta)]);
    }

    #[test]
    fn rebuild_fts_repairs_corrupted_index() {
        let (_temp, store, account_id) = store_ready_for_insert();
        let samples = sample_rows();
        let mut rows = Vec::new();
        for (i, (desc, meta)) in samples.iter().enumerate() {
            let id = insert_tx_with_meta(&store, account_id, desc, meta, &format!("rb-{i}"));
            rows.push((id, *desc, meta.clone()));
        }

        // Corrupt: foreign posting on an existing rowid + orphan posting.
        let victim = rows[0].0;
        store
            .conn
            .execute(
                "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
                params![victim, "AAMI phantom drift"],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
                params![999_999i64, "orphan phantom row"],
            )
            .unwrap();
        assert!(fts_match_rowids(&store, "aami").contains(&victim));
        assert!(fts_match_rowids(&store, "orphan").contains(&999_999));

        let count = store.rebuild_fts().unwrap();
        assert_eq!(count, rows.len());

        // Victim is YouTube; "aami" must no longer hit it (AAMI row may still).
        assert!(
            !fts_match_rowids(&store, "aami").contains(&victim),
            "phantom aami posting on YouTube row must be gone after rebuild"
        );
        assert!(
            fts_match_rowids(&store, "orphan").is_empty(),
            "orphan phantom must be gone after rebuild"
        );
        assert_fts_invariant(&store, &rows);
    }

    #[test]
    fn reimport_applies_a_refined_description() {
        // The card-BPAY case: the feed first posts a generic merchant string
        // and only later resolves the biller and the user's own description.
        let (_temp, store, account_id) = store_ready_for_insert();
        let empty = HashMap::new();
        let id = insert_tx_with_meta(
            &store,
            account_id,
            "BPAY SALES PARRAMATTA AUS",
            &empty,
            "ps-1955648710",
        );

        let refined = "BPAYN ACT REVENUE OFFICE BPAY, Camp Rates Aug26";
        let outcome = reimport(&store, account_id, refined, &empty, "ps-1955648710");
        assert_eq!(outcome, ImportOutcome::Updated);

        // One row, same id — so categories, notes, tags and transfers survive.
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let (description, amount, date, _) = stored_row(&store, id);
        assert_eq!(description, refined);
        assert_eq!(amount, -100, "economic fields must not move");
        assert_eq!(date, "2024-01-15", "the date must not move");

        assert_eq!(fts_match_rowids(&store, "revenue"), vec![id]);
        assert!(
            fts_match_rowids(&store, "parramatta").is_empty(),
            "the superseded text must leave no phantom posting"
        );
    }

    #[test]
    fn reimport_of_an_unchanged_row_is_skipped() {
        let (_temp, store, account_id) = store_ready_for_insert();
        let metadata = meta(&[("pocketsmith_id", "42")]);
        insert_tx_with_meta(&store, account_id, "Coffee Shop", &metadata, "ps-42");

        let outcome = reimport(&store, account_id, "Coffee Shop", &metadata, "ps-42");
        assert_eq!(outcome, ImportOutcome::Unchanged);
    }

    #[test]
    fn reimport_merges_metadata_without_dropping_keys() {
        let (_temp, store, account_id) = store_ready_for_insert();
        let id = insert_tx_with_meta(
            &store,
            account_id,
            "Coffee Shop",
            &meta(&[("pocketsmith_id", "42"), ("original_payee", "COFFEE SHP")]),
            "ps-42",
        );

        // A later pull adds a label and revises the category hint, but no
        // longer emits original_payee.
        let outcome = reimport(
            &store,
            account_id,
            "Coffee Shop",
            &meta(&[("pocketsmith_id", "42"), ("pocketsmith_category", "Dining")]),
            "ps-42",
        );
        assert_eq!(outcome, ImportOutcome::Updated);

        let (_, _, _, stored) = stored_row(&store, id);
        assert_eq!(
            stored,
            meta(&[
                ("pocketsmith_id", "42"),
                ("original_payee", "COFFEE SHP"),
                ("pocketsmith_category", "Dining"),
            ]),
            "incoming keys win; omitted ones are kept"
        );
        assert!(
            fts_match_rowids(&store, "Dining").contains(&id),
            "new metadata must be searchable"
        );
    }

    #[test]
    fn a_description_only_refinement_leaves_metadata_byte_identical() {
        // Re-encoding a HashMap reorders its keys, so an unchanged map must be
        // written back verbatim rather than churned on every refinement.
        let (_temp, store, account_id) = store_ready_for_insert();
        let metadata = meta(&[
            ("pocketsmith_id", "42"),
            ("pocketsmith_category", "Professional Services"),
            ("original_payee", "BPAY SALES"),
        ]);
        let id = insert_tx_with_meta(&store, account_id, "BPAY SALES", &metadata, "ps-42");
        let raw_before: String = store
            .conn
            .query_row(
                "SELECT metadata FROM transactions WHERE id = ?",
                [id],
                |r| r.get(0),
            )
            .unwrap();

        let outcome = reimport(&store, account_id, "ACT REVENUE OFFICE", &metadata, "ps-42");
        assert_eq!(outcome, ImportOutcome::Updated);

        let raw_after: String = store
            .conn
            .query_row(
                "SELECT metadata FROM transactions WHERE id = ?",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw_before, raw_after);
    }

    #[test]
    fn reimport_never_blanks_a_stored_description() {
        let (_temp, store, account_id) = store_ready_for_insert();
        let empty = HashMap::new();
        let id = insert_tx_with_meta(&store, account_id, "ACT REVENUE OFFICE", &empty, "ps-7");

        let outcome = reimport(&store, account_id, "   ", &empty, "ps-7");
        assert_eq!(outcome, ImportOutcome::Unchanged);
        assert_eq!(stored_row(&store, id).0, "ACT REVENUE OFFICE");
    }

    #[test]
    fn reimport_keeps_the_note_in_the_searchable_text() {
        let (_temp, mut store, account_id) = store_ready_for_insert();
        let empty = HashMap::new();
        let id = insert_tx_with_meta(&store, account_id, "BPAY SALES", &empty, "ps-9");
        store.set_note(id, "reimbursable zzmarker").unwrap();

        let outcome = reimport(&store, account_id, "ACT REVENUE OFFICE", &empty, "ps-9");
        assert_eq!(outcome, ImportOutcome::Updated);

        assert_eq!(fts_match_rowids(&store, "revenue"), vec![id]);
        assert_eq!(
            fts_match_rowids(&store, "zzmarker"),
            vec![id],
            "the note must survive the description rewrite in the FTS text"
        );
    }

    #[test]
    fn refresh_applies_a_description_the_feed_refined() {
        let temp = TempDir::new().unwrap();
        let account_dir = temp.path().join("TestBank").join("Mastercard");
        fs::create_dir_all(&account_dir).unwrap();
        let script = account_dir.join("pull");
        write_pull_script(&script, "BPAY SALES PARRAMATTA AUS", "ps-1955648710");

        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        let report = store.refresh().unwrap();
        assert_eq!(report.transactions_added, 1);
        assert_eq!(report.transactions_updated, 0);

        write_pull_script(&script, "BPAYN ACT REVENUE OFFICE BPAY", "ps-1955648710");
        let report = store.refresh().unwrap();
        assert_eq!(report.transactions_added, 0);
        assert_eq!(report.transactions_updated, 1);
        assert_eq!(report.transactions_skipped, 0);

        let txs = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].description, "BPAYN ACT REVENUE OFFICE BPAY");

        // And a third, identical pull is a plain skip.
        let report = store.refresh().unwrap();
        assert_eq!(report.transactions_updated, 0);
        assert_eq!(report.transactions_skipped, 1);
    }
}
