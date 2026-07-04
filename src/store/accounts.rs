//! Account management: list/lookup by id or path, per-account transaction
//! counts and listings, and rename/move/delete that keep the exports folder in
//! sync with the DB.

use chrono::Utc;
use rusqlite::{OptionalExtension, params, types::Value};

use crate::{AccountWithBank, Error, Result, Transaction};

use super::{TransactionStore, parse_transaction, push_limit, tx_cols};

/// The single message used for every [`Error::InvalidAccountPath`] case, so a
/// caller can surface one consistent hint.
const INVALID_ACCOUNT_PATH: &str =
    "expected Bank/Account with a single '/'; account names cannot contain '/'";

fn parse_account_with_bank(row: &rusqlite::Row) -> rusqlite::Result<AccountWithBank> {
    let bank_name: String = row.get(2)?;
    let name: String = row.get(3)?;
    let path = format!("{bank_name}/{name}");
    Ok(AccountWithBank {
        id: row.get(0)?,
        bank_id: row.get(1)?,
        bank_name,
        name,
        path,
    })
}

impl TransactionStore {
    /// List live accounts (bank and account both un-deleted), ordered by
    /// bank then account name.
    pub fn list_accounts_with_bank(&self) -> Result<Vec<AccountWithBank>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.bank_id, b.name, a.name
             FROM accounts a JOIN banks b ON b.id = a.bank_id
             WHERE a.deleted_at IS NULL AND b.deleted_at IS NULL
             ORDER BY b.name, a.name",
        )?;
        let accounts = stmt
            .query_map([], parse_account_with_bank)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(accounts)
    }

    /// Get a live account by id.
    pub fn get_account_with_bank(&self, id: i64) -> Result<Option<AccountWithBank>> {
        self.conn
            .query_row(
                "SELECT a.id, a.bank_id, b.name, a.name
                 FROM accounts a JOIN banks b ON b.id = a.bank_id
                 WHERE a.id = ? AND a.deleted_at IS NULL AND b.deleted_at IS NULL",
                [id],
                parse_account_with_bank,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Get a live account by its "Bank/Account" path. A path without exactly
    /// one usable `/`-separated bank and account segment resolves to `None`
    /// rather than an error.
    pub fn get_account_by_path(&self, path: &str) -> Result<Option<AccountWithBank>> {
        let Some((bank, account)) = path.trim().split_once('/') else {
            return Ok(None);
        };
        let (bank, account) = (bank.trim(), account.trim());
        if bank.is_empty() || account.is_empty() {
            return Ok(None);
        }
        self.conn
            .query_row(
                "SELECT a.id, a.bank_id, b.name, a.name
                 FROM accounts a JOIN banks b ON b.id = a.bank_id
                 WHERE b.name = ? AND a.name = ?
                   AND a.deleted_at IS NULL AND b.deleted_at IS NULL",
                params![bank, account],
                parse_account_with_bank,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Transaction counts keyed by account id. Accounts with zero transactions
    /// are absent from the map.
    pub fn get_account_transaction_counts(&self) -> Result<std::collections::HashMap<i64, usize>> {
        use std::collections::HashMap;
        let mut stmt = self
            .conn
            .prepare("SELECT account_id, COUNT(*) FROM transactions GROUP BY account_id")?;
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

    /// Count transactions in an account.
    pub fn count_transactions_in_account(&self, id: i64) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM transactions WHERE account_id = ?",
            [id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Transactions belonging to an account, newest first.
    pub fn query_transactions_in_account(
        &self,
        id: i64,
        limit: Option<usize>,
    ) -> Result<Vec<Transaction>> {
        let mut sql = format!(
            "SELECT {} FROM transactions_view t \
             WHERE t.account_deleted_at IS NULL AND t.account_id = ? \
             ORDER BY t.date DESC, t.id DESC",
            tx_cols("t")
        );
        let mut params: Vec<Value> = vec![Value::Integer(id)];
        push_limit(&mut sql, &mut params, limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let transactions = stmt
            .query_map(rusqlite::params_from_iter(params), parse_transaction)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(transactions)
    }

    /// Rename/move an account. `new_path` is "Bank/Account"; the bank is
    /// created if it does not exist. Moves the exports folder before touching
    /// the DB so a filesystem failure leaves the DB untouched. Never merges:
    /// a collision with a different existing account (in the DB or on disk) is
    /// an error.
    pub fn rename_account(&mut self, id: i64, new_path: &str) -> Result<()> {
        let current = self
            .get_account_with_bank(id)?
            .ok_or_else(|| Error::AccountNotFound(id.to_string()))?;

        let (new_bank, new_account) = new_path
            .split_once('/')
            .ok_or_else(|| Error::InvalidAccountPath(INVALID_ACCOUNT_PATH.into()))?;
        if new_account.contains('/') {
            return Err(Error::InvalidAccountPath(INVALID_ACCOUNT_PATH.into()));
        }
        let (new_bank, new_account) = (new_bank.trim(), new_account.trim());
        if new_bank.is_empty() || new_account.is_empty() {
            return Err(Error::InvalidAccountPath(INVALID_ACCOUNT_PATH.into()));
        }

        if new_bank == current.bank_name && new_account == current.name {
            return Ok(());
        }

        let (target_bank_id, _created) = self.get_or_create_bank(new_bank)?;

        let collision: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM accounts WHERE bank_id = ? AND name = ? AND id != ?",
                params![target_bank_id, new_account, id],
                |row| row.get(0),
            )
            .optional()?;
        if collision.is_some() || self.exports_dir.join(new_bank).join(new_account).exists() {
            return Err(Error::AccountExists(format!("{new_bank}/{new_account}")));
        }

        let old = self
            .exports_dir
            .join(&current.bank_name)
            .join(&current.name);
        let new_dir = self.exports_dir.join(new_bank).join(new_account);
        std::fs::create_dir_all(self.exports_dir.join(new_bank))?;
        if old.exists() {
            std::fs::rename(&old, &new_dir)?;
        }

        self.conn.execute(
            "UPDATE accounts SET bank_id = ?, name = ? WHERE id = ?",
            params![target_bank_id, new_account, id],
        )?;
        Ok(())
    }

    /// Delete an account: remove its exports folder and soft-delete the row.
    /// Transactions/enrichments/transfers are retained. Returns the number of
    /// transactions the account held.
    pub fn delete_account(&mut self, id: i64) -> Result<usize> {
        let current = self
            .get_account_with_bank(id)?
            .ok_or_else(|| Error::AccountNotFound(id.to_string()))?;
        let count = self.count_transactions_in_account(id)?;

        let dir = self
            .exports_dir
            .join(&current.bank_name)
            .join(&current.name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }

        self.conn.execute(
            "UPDATE accounts SET deleted_at = ? WHERE id = ?",
            params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::store::test_support::{insert_tx_desc, store_with_two_accounts};
    use crate::{Error, TransactionStore};

    fn make_dir_with_marker(dir: &std::path::Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("marker"), "x").unwrap();
    }

    fn account_name(store: &TransactionStore, id: i64) -> String {
        store.get_account_with_bank(id).unwrap().unwrap().name
    }

    #[test]
    fn list_orders_by_path_with_counts() {
        let (_temp, store, a1, a2) = store_with_two_accounts();
        insert_tx_desc(&store, a1, "2024-01-01", "one", -100);
        insert_tx_desc(&store, a1, "2024-01-02", "two", -200);

        let accounts = store.list_accounts_with_bank().unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].path, "TB/A1");
        assert_eq!(accounts[1].path, "TB/A2");

        let counts = store.get_account_transaction_counts().unwrap();
        assert_eq!(counts.get(&a1), Some(&2));
        assert!(!counts.contains_key(&a2));
    }

    #[test]
    fn rename_within_bank_moves_folder() {
        let (temp, mut store, a1, _a2) = store_with_two_accounts();
        make_dir_with_marker(&temp.path().join("TB").join("A1"));

        store.rename_account(a1, "TB/Renamed").unwrap();

        assert_eq!(account_name(&store, a1), "Renamed");
        assert!(temp.path().join("TB").join("Renamed").exists());
        assert!(!temp.path().join("TB").join("A1").exists());
        assert!(
            temp.path()
                .join("TB")
                .join("Renamed")
                .join("marker")
                .exists()
        );
    }

    #[test]
    fn rename_to_different_bank() {
        let (temp, mut store, a1, _a2) = store_with_two_accounts();
        let original_bank_id = store.get_account_with_bank(a1).unwrap().unwrap().bank_id;
        make_dir_with_marker(&temp.path().join("TB").join("A1"));

        store.rename_account(a1, "NewBank/Moved").unwrap();

        let account = store.get_account_with_bank(a1).unwrap().unwrap();
        assert_eq!(account.bank_name, "NewBank");
        assert_eq!(account.name, "Moved");
        assert_ne!(account.bank_id, original_bank_id);
        assert!(temp.path().join("NewBank").join("Moved").exists());
        assert!(!temp.path().join("TB").join("A1").exists());
    }

    #[test]
    fn rename_collision_errors_and_leaves_state() {
        let (temp, mut store, a1, _a2) = store_with_two_accounts();
        make_dir_with_marker(&temp.path().join("TB").join("A1"));
        make_dir_with_marker(&temp.path().join("TB").join("A2"));

        assert!(matches!(
            store.rename_account(a1, "TB/A2"),
            Err(Error::AccountExists(_))
        ));
        assert_eq!(account_name(&store, a1), "A1");
        assert!(temp.path().join("TB").join("A1").exists());
        assert!(temp.path().join("TB").join("A2").exists());
    }

    #[test]
    fn rename_invalid_path_errors() {
        let (_temp, mut store, a1, _a2) = store_with_two_accounts();
        assert!(matches!(
            store.rename_account(a1, "NoSlash"),
            Err(Error::InvalidAccountPath(_))
        ));
        assert!(matches!(
            store.rename_account(a1, "TB/"),
            Err(Error::InvalidAccountPath(_))
        ));
        assert!(matches!(
            store.rename_account(a1, "TB/Sub/Deep"),
            Err(Error::InvalidAccountPath(_))
        ));
        assert_eq!(account_name(&store, a1), "A1");
    }

    #[test]
    fn rename_with_missing_source_folder_still_updates_db() {
        let (_temp, mut store, a1, _a2) = store_with_two_accounts();
        store.rename_account(a1, "TB/Renamed2").unwrap();
        assert_eq!(account_name(&store, a1), "Renamed2");
    }

    #[test]
    fn rename_no_op_same_path() {
        let (_temp, mut store, a1, _a2) = store_with_two_accounts();
        store.rename_account(a1, "TB/A1").unwrap();
        assert_eq!(account_name(&store, a1), "A1");
    }

    #[test]
    fn delete_removes_folder_soft_deletes_and_retains_txns() {
        let (temp, mut store, a1, _a2) = store_with_two_accounts();
        insert_tx_desc(&store, a1, "2024-01-01", "one", -100);
        insert_tx_desc(&store, a1, "2024-01-02", "two", -200);
        make_dir_with_marker(&temp.path().join("TB").join("A1"));

        let n = store.delete_account(a1).unwrap();
        assert_eq!(n, 2);
        assert!(!temp.path().join("TB").join("A1").exists());
        assert!(
            !store
                .list_accounts_with_bank()
                .unwrap()
                .iter()
                .any(|a| a.id == a1)
        );
        assert!(store.get_account_with_bank(a1).unwrap().is_none());
        assert_eq!(store.count_transactions_in_account(a1).unwrap(), 2);
    }
}
