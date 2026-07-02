//! Saved filters: CRUD, ordering, and the apply/preview pipeline that derives
//! rule-sourced categories from the filter set.

use chrono::Utc;
use rusqlite::params;
use std::collections::{HashMap, HashSet};

use crate::search::{SearchConfig, parse};
use crate::{CategorySource, Filter, FilterOverride, Result, Transaction};

use super::{FILTER_COLS, TransactionStore, parse_filter};

impl TransactionStore {
    /// List all saved filters, ordered for display and apply precedence.
    pub fn list_filters(&self) -> Result<Vec<Filter>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FILTER_COLS} FROM filters ORDER BY position ASC, id ASC"
        ))?;
        let filters = stmt
            .query_map([], parse_filter)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(filters)
    }

    /// List the saved filters that auto-apply the given category, in display
    /// order.
    pub fn filters_using_category(&self, category_id: i64) -> Result<Vec<Filter>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {FILTER_COLS} FROM filters WHERE category_id = ? ORDER BY position ASC, id ASC"
        ))?;
        let filters = stmt
            .query_map([category_id], parse_filter)?
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
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::search::ParsedQuery;
    use crate::store::test_support::setup_rich_fixture;
    use crate::store::{TransactionStore, parse_datetime};
    use crate::{CategorySource, FilterOverride};

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

    #[test]
    fn rename_category_keeps_filter_reference() {
        let (_t, mut store) = setup_rich_fixture();
        let cat = store.get_or_create_category("Income/Interest").unwrap();
        let filter_id = store.create_filter("interest", "Interest").unwrap();
        store.set_filter_category(filter_id, Some(cat)).unwrap();

        store.rename_category(cat, "Income/Earnings").unwrap();

        let filter = store
            .list_filters()
            .unwrap()
            .into_iter()
            .find(|f| f.id == filter_id)
            .unwrap();
        // Same category row id; only its path changed.
        assert_eq!(filter.category_id, Some(cat));
        assert_eq!(
            store.get_category(cat).unwrap().map(|c| c.path),
            Some("Income/Earnings".into())
        );
    }

    #[test]
    fn merge_category_repoints_filter() {
        let (_t, mut store) = setup_rich_fixture();
        let source = store.get_or_create_category("Income/Interest").unwrap();
        let target = store.get_or_create_category("Income/Earnings").unwrap();
        let filter_id = store.create_filter("interest", "Interest").unwrap();
        store.set_filter_category(filter_id, Some(source)).unwrap();

        store.merge_categories(source, target).unwrap();

        let filter = store
            .list_filters()
            .unwrap()
            .into_iter()
            .find(|f| f.id == filter_id)
            .unwrap();
        assert_eq!(filter.category_id, Some(target));
    }

    #[test]
    fn delete_category_clears_filter_reference() {
        let (_t, mut store) = setup_rich_fixture();
        let cat = store.get_or_create_category("Income/Interest").unwrap();
        let filter_id = store.create_filter("interest", "Interest").unwrap();
        store.set_filter_category(filter_id, Some(cat)).unwrap();
        store.apply_filters().unwrap();
        assert!(enrichment(&store, "Interest").is_some());

        store.delete_category(cat).unwrap();

        let filter = store
            .list_filters()
            .unwrap()
            .into_iter()
            .find(|f| f.id == filter_id)
            .unwrap();
        assert_eq!(filter.category_id, None);
        assert!(enrichment(&store, "Interest").is_none());
    }

    #[test]
    fn filters_using_category_returns_only_matches() {
        let (_t, mut store) = setup_rich_fixture();
        let interest = store.get_or_create_category("Income/Interest").unwrap();
        let groceries = store.get_or_create_category("Food/Groceries").unwrap();
        let matching = store.create_filter("interest", "Interest").unwrap();
        let other = store.create_filter("groceries", "Groceries").unwrap();
        store.create_filter("misc", "Misc").unwrap(); // no category
        store.set_filter_category(matching, Some(interest)).unwrap();
        store.set_filter_category(other, Some(groceries)).unwrap();

        let using = store.filters_using_category(interest).unwrap();
        assert_eq!(
            using.iter().map(|f| f.id).collect::<Vec<_>>(),
            vec![matching]
        );

        // A category no filter references yields nothing.
        let unused = store.get_or_create_category("Unused").unwrap();
        assert!(store.filters_using_category(unused).unwrap().is_empty());
    }

    // ----- Round-trip guard: FILTER_COLS ↔ parse_filter -----

    #[test]
    fn filter_round_trips_every_field() {
        let temp = TempDir::new().unwrap();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        let cat = store.get_or_create_category("Round/Filter").unwrap();
        // A first filter takes position 0 so the round-tripped one gets a
        // non-default position.
        store.create_filter("earlier", "placeholder").unwrap();
        let id = store
            .create_filter("initial name", "initial query")
            .unwrap();
        store.rename_filter(id, "round-trip name").unwrap();
        store.set_filter_query(id, "amount:>10 coffee").unwrap();
        store.set_filter_category(id, Some(cat)).unwrap();
        store.set_filter_override(id, FilterOverride::Ai).unwrap();
        store.set_filter_review(id, true).unwrap();

        let filter = store
            .list_filters()
            .unwrap()
            .into_iter()
            .find(|f| f.id == id)
            .expect("created filter");
        assert_eq!(filter.id, id);
        assert_eq!(filter.name, "round-trip name");
        assert_eq!(filter.query, "amount:>10 coffee");
        assert_eq!(filter.category_id, Some(cat));
        assert_eq!(filter.override_mode, FilterOverride::Ai);
        assert!(filter.review_required);
        assert_eq!(filter.position, 1);
        let created_at: String = store
            .conn
            .query_row("SELECT created_at FROM filters WHERE id = ?", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(filter.created_at, parse_datetime(&created_at).unwrap());
    }
}
