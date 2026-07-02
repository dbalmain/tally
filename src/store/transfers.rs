//! Transfer lifecycle: create/confirm/delete links, candidate search, and
//! transfer read queries.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};

use crate::search::ParsedQuery;
use crate::{
    ConfirmedTransferExample, Result, Transaction, Transfer, TransferSource,
    TransferWithTransactions,
};

use super::{
    TRANSFER_COL_COUNT, TX_COL_COUNT, TransactionStore, parse_transaction,
    parse_transaction_at_offset, parse_transfer, push_limit, transfer_cols, transfer_side_ctx,
    tx_cols,
};

impl TransactionStore {
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
}

#[cfg(test)]
mod tests {
    use crate::search::ParsedQuery;
    use crate::store::test_support::{
        annotate_transaction, assert_transaction_matches_db, assert_transfer_matches_db, get_tx,
        insert_tx, q, q_exact, setup_rich_fixture, store_with_two_accounts,
    };
    use crate::{CategorySource, TransferSource};

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

    // ----- Round-trip guard: TRANSFER_COLS ↔ parse_transfer -----

    #[test]
    fn transfer_round_trips_every_field() {
        let (_tmp, mut store, a1, a2) = store_with_two_accounts();
        let from = insert_tx(&store, a1, "2024-06-02", -9876);
        let to = insert_tx(&store, a2, "2024-06-02", 9876);
        let transfer_id = store
            .create_transfer(from, to, TransferSource::Auto, false, Some(0.73))
            .unwrap();

        let pending = store
            .get_pending_transfer_reviews(&ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(pending.len(), 1);
        let transfer = &pending[0];
        assert_eq!(transfer.id, transfer_id);
        assert_eq!(transfer.from_transaction_id, from);
        assert_eq!(transfer.to_transaction_id, to);
        assert_eq!(transfer.source, TransferSource::Auto);
        assert!(!transfer.confirmed);
        assert_eq!(transfer.ai_confidence, Some(0.73));
        // created_at round-trips against the stored string.
        assert_transfer_matches_db(&store, transfer);
    }
}
