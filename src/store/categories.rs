//! Category CRUD: create/lookup, rename/merge/delete, and per-category
//! transaction counts and listings.

use chrono::Utc;
use rusqlite::{OptionalExtension, params, types::Value};

use crate::{Category, Error, FuzzyMatcher, Result, Transaction};

use super::{
    TransactionStore, category_cols, parse_category, parse_transaction, push_limit, tx_cols,
};

impl TransactionStore {
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

        // Repoint filters that auto-applied the source so they now apply the
        // target, before the source category row disappears.
        self.conn.execute(
            "UPDATE filters SET category_id = ? WHERE category_id = ?",
            params![target_id, source_id],
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
        // Clear filters that auto-applied this category so none is left pointing
        // at a row that no longer exists; such a filter simply stops applying.
        self.conn.execute(
            "UPDATE filters SET category_id = NULL WHERE category_id = ?",
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

    /// Transactions whose enrichment assigns them exactly `category_id`, newest
    /// first. Backs the Categories tab "view transactions" side panel.
    pub fn query_transactions_in_category(
        &self,
        category_id: i64,
        limit: Option<usize>,
    ) -> Result<Vec<Transaction>> {
        let mut sql = format!(
            "SELECT {} FROM transactions_view t \
             JOIN transaction_enrichments e ON e.transaction_id = t.id \
             WHERE t.account_deleted_at IS NULL AND e.category_id = ? \
             ORDER BY t.date DESC, t.id DESC",
            tx_cols("t")
        );
        let mut params: Vec<Value> = vec![Value::Integer(category_id)];
        push_limit(&mut sql, &mut params, limit);
        let mut stmt = self.conn.prepare(&sql)?;
        let transactions = stmt
            .query_map(rusqlite::params_from_iter(params), parse_transaction)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(transactions)
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
}

#[cfg(test)]
mod tests {
    use crate::search::ParsedQuery;
    use crate::store::test_support::setup_test_exports;
    use crate::{CategorySource, Error, TransactionStore};

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
    fn query_transactions_in_category_returns_assigned_only() {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();

        let cat_id = store.get_or_create_category("TestCategory").unwrap();
        let unused_id = store.get_or_create_category("Unused").unwrap();
        assert!(
            store
                .query_transactions_in_category(cat_id, None)
                .unwrap()
                .is_empty()
        );

        let txs = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        store
            .set_category(txs[0].id, cat_id, CategorySource::Manual, true, None)
            .unwrap();

        let assigned = store.query_transactions_in_category(cat_id, None).unwrap();
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].id, txs[0].id);
        assert!(
            store
                .query_transactions_in_category(unused_id, None)
                .unwrap()
                .is_empty()
        );
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
}
