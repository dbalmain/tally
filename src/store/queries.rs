//! General transaction read methods: the main search-backed query, the
//! Todo-tab queries, and id-based lookups.

use rusqlite::OptionalExtension;

use crate::search::ParsedQuery;
use crate::{Result, Transaction, TransactionWithEnrichment};

use super::{
    ENRICHMENT_COL_COUNT, TODO_TAB_JOINS, TX_COL_COUNT, TransactionStore, category_cols,
    enrichment_cols, parse_category_at_offset, parse_enrichment_at_offset, parse_transaction,
    push_limit, transaction_category_join, transaction_ctx, transaction_fts_join,
    transaction_joins, tx_cols,
};

impl TransactionStore {
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

    /// Get transactions that need categorization, scoped by a parsed search query.
    pub fn get_uncategorised_transactions(
        &self,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<Vec<Transaction>> {
        self.query_transactions_where(
            query,
            TODO_TAB_JOINS,
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
            TODO_TAB_JOINS,
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
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use rusqlite::params;

    use crate::CategorySource;
    use crate::search::ParsedQuery;
    use crate::store::test_support::{
        annotate_transaction, assert_category_matches_db, assert_enrichment_matches_db,
        assert_transaction_matches_db, insert_tx, q, q_exact, setup_rich_fixture,
        store_with_two_accounts,
    };

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

    // ----- id-based lookups -----

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

    // ----- Round-trip guard: TX_COLS ↔ parse_transaction_at_offset -----

    #[test]
    fn transaction_round_trips_every_field() {
        let (_tmp, store, a1, _a2) = store_with_two_accounts();
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
                 VALUES (?, '2024-07-19', 'Distinct description', -12345, 67890,
                         'distinct-hash', '{\"key\":\"value\",\"n\":7}',
                         'Bank/Acct/file.csv', ?)",
                params![a1, batch_id],
            )
            .unwrap();
        let id = store.conn.last_insert_rowid();
        let bank_id: i64 = store
            .conn
            .query_row("SELECT bank_id FROM accounts WHERE id = ?", [a1], |r| {
                r.get(0)
            })
            .unwrap();

        // Read back through the general query path so a slip in the
        // TX_COLS ↔ parse_transaction_at_offset contract fails loudly.
        let all = store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap();
        let tx = all.iter().find(|tx| tx.id == id).expect("inserted row");
        assert_eq!(tx.id, id);
        assert_eq!(tx.bank_id, bank_id);
        assert_eq!(tx.account_id, a1);
        assert_eq!(tx.date, NaiveDate::from_ymd_opt(2024, 7, 19).unwrap());
        assert_eq!(tx.description, "Distinct description");
        assert_eq!(tx.amount_cents, -12345);
        assert_eq!(tx.balance_cents, 67890);
        assert_eq!(tx.hash, "distinct-hash");
        assert_eq!(tx.metadata.len(), 2);
        assert_eq!(tx.metadata.get("key"), Some(&serde_json::json!("value")));
        assert_eq!(tx.metadata.get("n"), Some(&serde_json::json!(7)));
        assert_eq!(tx.source_file, "Bank/Acct/file.csv");
        assert_eq!(tx.import_batch_id, batch_id);
    }
}
