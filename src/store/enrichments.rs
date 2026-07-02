//! Transaction enrichments: set/confirm/clear categories on transactions and
//! the confirmed-example reads that feed the classifier.

use chrono::{NaiveDate, Utc};
use rusqlite::{OptionalExtension, params};

use crate::{CategorySource, ConfirmedCategoryExample, Error, Result};

use super::TransactionStore;

/// An enrichment's category source, confirmed flag, and category id.
pub(super) type EnrichmentMeta = (Option<CategorySource>, bool, Option<i64>);

impl TransactionStore {
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

    /// Enrichment source, confirmed flag, and category id for a transaction.
    pub(super) fn get_enrichment_meta(&self, tx_id: i64) -> Result<Option<EnrichmentMeta>> {
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
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use crate::CategorySource;
    use crate::search::ParsedQuery;
    use crate::store::test_support::{
        assert_category_matches_db, assert_enrichment_matches_db, insert_tx, setup_rich_fixture,
        store_with_two_accounts,
    };

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

    // ----- Round-trip guard: ENRICHMENT_COLS ↔ parse_enrichment_at_offset -----

    #[test]
    fn enrichment_round_trips_every_field() {
        let (_tmp, mut store, a1, _a2) = store_with_two_accounts();
        let tx = insert_tx(&store, a1, "2024-06-01", -4321);
        let cat = store.get_or_create_category("Round/Enrich").unwrap();
        store
            .set_category(tx, cat, CategorySource::Ai, false, Some(0.42))
            .unwrap();

        let reviews = store
            .get_pending_ai_reviews(&ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(reviews.len(), 1);
        let row = &reviews[0];
        assert_eq!(row.transaction.id, tx);

        let enrichment = row.enrichment.as_ref().expect("row has enrichment");
        assert_eq!(enrichment.transaction_id, tx);
        assert_eq!(enrichment.category_id, Some(cat));
        assert_eq!(enrichment.category_source, Some(CategorySource::Ai));
        assert!(!enrichment.category_confirmed);
        assert_eq!(enrichment.ai_confidence, Some(0.42));
        // Timestamps round-trip against the stored strings.
        assert_enrichment_matches_db(&store, enrichment);

        let category = row.category.as_ref().expect("row has category");
        assert_eq!(category.id, cat);
        assert_eq!(category.path, "Round/Enrich");
        assert_category_matches_db(&store, category);
    }
}
