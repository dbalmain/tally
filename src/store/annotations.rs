//! Notes and tags: free-form per-transaction text and `#tag` labels.
//!
//! Both live in their own tables rather than on `transaction_enrichments`,
//! because that row means "category assignment" and `create_transfer` deletes
//! it on both endpoints — a note or tag must survive marking a transfer.
//!
//! Note text is folded into the FTS index (so bare-word search finds it), which
//! makes [`TransactionStore::set_note`] an FTS write: it re-derives the row's
//! posting through the shared DELETE-then-INSERT path.

use std::collections::HashMap;

use chrono::Utc;
use rusqlite::{OptionalExtension, params};

use crate::{Result, Tag, validate_tag};

use super::{TransactionStore, parse_datetime};

/// Placeholder list (`?, ?, ?`) for an `IN` clause of `n` ids.
fn id_placeholders(n: usize) -> String {
    std::iter::repeat_n("?", n).collect::<Vec<_>>().join(", ")
}

impl TransactionStore {
    // ==================== Notes ====================

    /// The transaction's note, if it has one. A stored note is never empty —
    /// clearing one deletes the row.
    pub fn get_note(&self, transaction_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT note FROM transaction_notes WHERE transaction_id = ?",
                params![transaction_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Set (or clear) a transaction's note and reindex it for full-text search.
    ///
    /// A blank note deletes the row rather than storing whitespace, so
    /// `note:none` and the "has a note" indicator agree with what the user sees
    /// in the editor. Returns whether the transaction now has a note.
    pub fn set_note(&mut self, transaction_id: i64, note: &str) -> Result<bool> {
        let trimmed = note.trim_end();
        if trimmed.trim().is_empty() {
            self.conn.execute(
                "DELETE FROM transaction_notes WHERE transaction_id = ?",
                params![transaction_id],
            )?;
            self.refresh_transaction_fts(transaction_id)?;
            return Ok(false);
        }

        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO transaction_notes (transaction_id, note, created_at, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(transaction_id) DO UPDATE SET
                note = excluded.note,
                updated_at = excluded.updated_at",
            params![transaction_id, trimmed, now, now],
        )?;
        self.refresh_transaction_fts(transaction_id)?;
        Ok(true)
    }

    /// Notes for a batch of transactions, keyed by transaction id. Transactions
    /// without a note are absent from the map.
    pub fn notes_for_transactions(&self, transaction_ids: &[i64]) -> Result<HashMap<i64, String>> {
        if transaction_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let sql = format!(
            "SELECT transaction_id, note FROM transaction_notes WHERE transaction_id IN ({})",
            id_placeholders(transaction_ids.len())
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(transaction_ids), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut notes = HashMap::new();
        for row in rows {
            let (id, note) = row?;
            notes.insert(id, note);
        }
        Ok(notes)
    }

    // ==================== Tags ====================

    /// Every tag in use, most-used first then alphabetical — the order the tag
    /// editor shows when nothing has been typed yet, so common tags are the
    /// ones in reach.
    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.name, t.created_at, COUNT(tt.transaction_id) AS uses
             FROM tags t
             LEFT JOIN transaction_tags tt ON tt.tag_id = t.id
             GROUP BY t.id
             ORDER BY uses DESC, t.name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Tag {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: parse_datetime(&row.get::<_, String>(2)?)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// How many transactions carry each tag, keyed by tag name.
    pub fn tag_transaction_counts(&self) -> Result<HashMap<String, usize>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name, COUNT(tt.transaction_id)
             FROM tags t
             LEFT JOIN transaction_tags tt ON tt.tag_id = t.id
             GROUP BY t.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
        })?;

        let mut counts = HashMap::new();
        for row in rows {
            let (name, count) = row?;
            counts.insert(name, count);
        }
        Ok(counts)
    }

    /// A transaction's tags, alphabetical.
    pub fn tags_for_transaction(&self, transaction_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name FROM tags t
             JOIN transaction_tags tt ON tt.tag_id = t.id
             WHERE tt.transaction_id = ?
             ORDER BY t.name",
        )?;
        let rows = stmt.query_map(params![transaction_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Tags for a batch of transactions, keyed by transaction id. Untagged
    /// transactions are absent from the map; each list is alphabetical.
    pub fn tags_for_transactions(
        &self,
        transaction_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<String>>> {
        if transaction_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let sql = format!(
            "SELECT tt.transaction_id, t.name FROM tags t
             JOIN transaction_tags tt ON tt.tag_id = t.id
             WHERE tt.transaction_id IN ({})
             ORDER BY tt.transaction_id, t.name",
            id_placeholders(transaction_ids.len())
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(transaction_ids), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut tags: HashMap<i64, Vec<String>> = HashMap::new();
        for row in rows {
            let (id, name) = row?;
            tags.entry(id).or_default().push(name);
        }
        Ok(tags)
    }

    /// Replace a transaction's tag set with `names`.
    ///
    /// Names are canonicalised by [`validate_tag`] and deduplicated; anything
    /// that isn't a usable tag is dropped rather than failing the save, since
    /// the editor already rejects invalid input as you type. Tags left with no
    /// transactions are deleted, so the tag list is exactly the set in use and
    /// autocomplete never offers a dead tag. Returns the stored tag names.
    pub fn set_transaction_tags(
        &mut self,
        transaction_id: i64,
        names: &[String],
    ) -> Result<Vec<String>> {
        let mut wanted: Vec<String> = Vec::new();
        for name in names {
            if let Some(name) = validate_tag(name)
                && !wanted.contains(&name)
            {
                wanted.push(name);
            }
        }
        wanted.sort();

        let now = Utc::now().to_rfc3339();
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM transaction_tags WHERE transaction_id = ?",
            params![transaction_id],
        )?;
        {
            let mut insert_tag =
                tx.prepare("INSERT OR IGNORE INTO tags (name, created_at) VALUES (?, ?)")?;
            let mut find_tag = tx.prepare("SELECT id FROM tags WHERE name = ?")?;
            let mut link = tx.prepare(
                "INSERT OR IGNORE INTO transaction_tags (transaction_id, tag_id) VALUES (?, ?)",
            )?;
            for name in &wanted {
                insert_tag.execute(params![name, now])?;
                let tag_id: i64 = find_tag.query_row(params![name], |row| row.get(0))?;
                link.execute(params![transaction_id, tag_id])?;
            }
        }
        // A tag exists because something is tagged with it; drop the orphans so
        // removing a tag's last use also removes it from autocomplete.
        tx.execute(
            "DELETE FROM tags WHERE id NOT IN (SELECT tag_id FROM transaction_tags)",
            [],
        )?;
        tx.commit()?;

        Ok(wanted)
    }
}

#[cfg(test)]
mod tests {
    use crate::TransactionStore;
    use crate::store::test_support::{insert_tx_desc, q, store_with_two_accounts};
    use tempfile::TempDir;

    /// Three transactions across two accounts, with distinct descriptions so
    /// FTS assertions can tell note tokens from description tokens.
    fn fixture() -> (TempDir, TransactionStore, Vec<i64>) {
        let (temp, store, a1, a2) = store_with_two_accounts();
        let ids = vec![
            insert_tx_desc(&store, a1, "2024-03-01", "Coffee Roasters", -850),
            insert_tx_desc(&store, a1, "2024-03-02", "Strata Corp", -45000),
            insert_tx_desc(&store, a2, "2024-03-03", "Salary", 500000),
        ];
        (temp, store, ids)
    }

    fn search_ids(store: &TransactionStore, term: &str) -> Vec<i64> {
        store
            .query_transactions(&q(term), None)
            .unwrap()
            .into_iter()
            .map(|tx| tx.id)
            .collect()
    }

    fn tag_names(store: &TransactionStore) -> Vec<String> {
        store
            .list_tags()
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect()
    }

    #[test]
    fn set_note_round_trips_and_clears() {
        let (_temp, mut store, ids) = fixture();

        assert!(store.set_note(ids[0], "Reimbursable\n\nby Acme").unwrap());
        assert_eq!(
            store.get_note(ids[0]).unwrap().as_deref(),
            Some("Reimbursable\n\nby Acme")
        );

        // Blank clears rather than storing whitespace.
        assert!(!store.set_note(ids[0], "   \n  ").unwrap());
        assert_eq!(store.get_note(ids[0]).unwrap(), None);
    }

    #[test]
    fn notes_are_full_text_searchable_and_reindexed_on_change() {
        let (_temp, mut store, ids) = fixture();

        store.set_note(ids[0], "reimbursable by Acme").unwrap();
        assert_eq!(search_ids(&store, "reimbursable"), vec![ids[0]]);

        // Rewriting the note must not leave the old tokens behind.
        store.set_note(ids[0], "personal spending").unwrap();
        assert!(search_ids(&store, "reimbursable").is_empty());
        assert_eq!(search_ids(&store, "personal"), vec![ids[0]]);

        // Clearing it drops the note tokens but keeps the description indexed.
        store.set_note(ids[0], "").unwrap();
        assert!(search_ids(&store, "personal").is_empty());
        assert_eq!(search_ids(&store, "roasters"), vec![ids[0]]);
    }

    #[test]
    fn rebuild_fts_reindexes_notes() {
        let (_temp, mut store, ids) = fixture();

        store.set_note(ids[1], "quarterly levy").unwrap();
        store.rebuild_fts().unwrap();
        assert_eq!(search_ids(&store, "levy"), vec![ids[1]]);
        assert_eq!(search_ids(&store, "strata"), vec![ids[1]]);
    }

    #[test]
    fn tags_are_canonicalised_deduplicated_and_sorted() {
        let (_temp, mut store, ids) = fixture();

        let stored = store
            .set_transaction_tags(
                ids[0],
                &[
                    "#Work".to_string(),
                    "work".to_string(),
                    " #travel ".to_string(),
                    "not a tag".to_string(),
                    String::new(),
                ],
            )
            .unwrap();

        assert_eq!(stored, vec!["travel".to_string(), "work".to_string()]);
        assert_eq!(store.tags_for_transaction(ids[0]).unwrap(), stored);
    }

    #[test]
    fn setting_tags_replaces_the_previous_set_and_gcs_orphans() {
        let (_temp, mut store, ids) = fixture();

        store
            .set_transaction_tags(ids[0], &["work".into(), "travel".into()])
            .unwrap();
        store
            .set_transaction_tags(ids[1], &["travel".into()])
            .unwrap();
        assert_eq!(tag_names(&store), vec!["travel", "work"]);

        // Dropping "work" from its only transaction removes the tag entirely,
        // while "travel" survives because another transaction still uses it.
        store
            .set_transaction_tags(ids[0], &["travel".into()])
            .unwrap();
        assert_eq!(tag_names(&store), vec!["travel"]);
        assert_eq!(store.tags_for_transaction(ids[0]).unwrap(), vec!["travel"]);
    }

    #[test]
    fn list_tags_orders_by_usage_then_name() {
        let (_temp, mut store, ids) = fixture();

        store
            .set_transaction_tags(ids[0], &["common".into(), "alpha".into()])
            .unwrap();
        store
            .set_transaction_tags(ids[1], &["common".into()])
            .unwrap();
        store
            .set_transaction_tags(ids[2], &["common".into(), "zeta".into()])
            .unwrap();

        assert_eq!(tag_names(&store), vec!["common", "alpha", "zeta"]);
        assert_eq!(store.tag_transaction_counts().unwrap()["common"], 3);
    }

    #[test]
    fn batch_lookups_key_by_transaction() {
        let (_temp, mut store, ids) = fixture();

        store.set_note(ids[0], "a note").unwrap();
        store
            .set_transaction_tags(ids[0], &["work".into(), "travel".into()])
            .unwrap();

        let notes = store.notes_for_transactions(&ids).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[&ids[0]], "a note");

        let tags = store.tags_for_transactions(&ids).unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[&ids[0]], vec!["travel", "work"]);

        assert!(store.notes_for_transactions(&[]).unwrap().is_empty());
        assert!(store.tags_for_transactions(&[]).unwrap().is_empty());
    }

    #[test]
    fn a_transfer_leg_keeps_its_note_and_tags() {
        let (_temp, mut store, ids) = fixture();

        store.set_note(ids[0], "paired with savings").unwrap();
        store
            .set_transaction_tags(ids[0], &["shuffle".into()])
            .unwrap();

        // create_transfer clears enrichments on both endpoints; annotations live
        // outside that table precisely so they survive.
        store
            .create_transfer(ids[0], ids[2], crate::TransferSource::Manual, true, None)
            .unwrap();

        assert_eq!(
            store.get_note(ids[0]).unwrap().as_deref(),
            Some("paired with savings")
        );
        assert_eq!(store.tags_for_transaction(ids[0]).unwrap(), vec!["shuffle"]);
    }
}
