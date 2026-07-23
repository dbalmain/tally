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
            let rowid = self.conn.last_insert_rowid();
            self.write_transaction_fts(rowid, description, metadata)?;
        }

        Ok(result > 0)
    }

    /// Replace the contentless FTS posting for `rowid` with
    /// [`build_searchable_text`] of the given description and metadata.
    ///
    /// Contentless FTS5 permits multiple postings per rowid and never
    /// cross-checks them against the real row, so every write must DELETE
    /// first — otherwise a reused or re-imported rowid leaves phantom tokens
    /// that produce false-positive search matches.
    fn write_transaction_fts(
        &self,
        rowid: i64,
        description: &str,
        metadata: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        let searchable_text = build_searchable_text(description, metadata);
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

    /// Drop and recreate `transactions_fts`, then repopulate one posting per
    /// transaction from [`build_searchable_text`]. Returns the number of rows
    /// reindexed. This is the only safe full-rebuild path.
    pub fn rebuild_fts(&self) -> Result<usize> {
        self.conn
            .execute_batch("DROP TABLE IF EXISTS transactions_fts;")?;
        self.conn.execute_batch(TRANSACTIONS_FTS_DDL)?;

        let mut stmt = self
            .conn
            .prepare("SELECT id, description, metadata FROM transactions")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut count = 0usize;
        for row in rows {
            let (id, description, metadata_json) = row?;
            let metadata: HashMap<String, serde_json::Value> =
                serde_json::from_str(&metadata_json).unwrap_or_default();
            let searchable_text = build_searchable_text(&description, &metadata);
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
        let date = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let inserted = store
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
            .unwrap();
        assert!(inserted);
        store.conn.last_insert_rowid()
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
        build_searchable_text(description, metadata)
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
            .write_transaction_fts(rowid, "Google YouTubePremium", &meta)
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
}
