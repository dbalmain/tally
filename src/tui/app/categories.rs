//! Category actions: the category-assignment popup, AI-review confirmation,
//! and rename/merge on the Categories tab.

use std::path::Path;
use std::sync::mpsc::TryRecvError;

use crate::classify::{ClassifyReport, SIMILARITY_THRESHOLD, normalise};
use crate::{Category, CategorySource, Transaction, TransactionStore};

use super::{
    App, BulkApplyState, BulkRow, CategoryTarget, ConfirmAction, InputMode, Tab, TextPromptTarget,
    TodoSubTab, next_wrapping, prev_wrapping,
};

const BULK_APPLY_MATCH_LIMIT: usize = 200;

impl App {
    // ==================== Auto Classification ====================

    /// Kick off a local classification run on a background thread (with its
    /// own store connection, like the startup refresh), keeping the UI live;
    /// the event loop polls [`Self::poll_classification`] for the result,
    /// which lands as a toast. In-memory stores (tests) have no path to open
    /// a second connection on, so they run inline instead.
    pub fn request_classify(&mut self) {
        if self.classifying {
            return;
        }

        let Some(db_path) = self.store.db_path().map(Path::to_path_buf) else {
            let result = crate::classify::classify(&mut self.store);
            self.finish_classification(result);
            return;
        };

        let exports_dir = self.store.exports_dir().to_path_buf();
        let search_options = self.search_options;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = TransactionStore::open(&db_path, &exports_dir).and_then(|mut store| {
                store.set_search_options(search_options);
                crate::classify::classify(&mut store)
            });
            let _ = tx.send(result);
        });
        self.classify_rx = Some(rx);
        self.classifying = true;
    }

    /// Collect a finished background classification, if any. Called every
    /// event-loop iteration; cheap no-op while the run is still going.
    pub fn poll_classification(&mut self) {
        let Some(rx) = &self.classify_rx else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.classify_rx = None;
                self.classifying = false;
                self.error_message = Some("Classification stopped unexpectedly".to_string());
                return;
            }
        };
        self.classify_rx = None;
        self.finish_classification(result);
    }

    /// Refresh the lists, then surface the run's outcome: a summary toast on
    /// success, the error popup on failure. `refresh_data` runs before the
    /// outcome is stored because a successful tab reload clears
    /// `error_message`.
    fn finish_classification(&mut self, result: crate::Result<ClassifyReport>) {
        self.classifying = false;
        self.refresh_data();
        match result {
            Ok(report) => self.show_status(format!(
                "Classified — filters: {}, transfers: {}, suggested: {}, unclassified: {}",
                report.filtered,
                report.transfers,
                report.exact + report.recurring + report.model,
                report.unclassified,
            )),
            Err(e) => self.error_message = Some(format!("Failed to classify: {e}")),
        }
    }

    // ==================== Category Popup ====================

    pub fn start_category_edit(&mut self) {
        if let Some(filter) = self.filter_edit_filter_for_category().cloned() {
            let input = filter
                .category_id
                .and_then(|id| self.category_path(id))
                .unwrap_or("")
                .to_string();
            self.open_category_popup(CategoryTarget::Filter(filter.id), input);
            return;
        }

        if self.current_tab == Tab::Filters {
            let Some(filter) = self.selected_filter().cloned() else {
                return;
            };
            let input = filter
                .category_id
                .and_then(|id| self.category_path(id))
                .unwrap_or("")
                .to_string();
            self.open_category_popup(CategoryTarget::Filter(filter.id), input);
            return;
        }

        if self.selected_transaction().is_some() {
            self.open_category_popup(CategoryTarget::Transaction, String::new());
        }
    }

    fn open_category_popup(&mut self, target: CategoryTarget, input: String) {
        self.input_mode = InputMode::Category;
        self.category_target = target;
        self.category_input = input;
        self.category_suggestions = self.load_or_show("load categories", |s| s.list_categories());
        self.category_selected = 0;
        if !self.category_input.is_empty() {
            self.update_category_suggestions();
        }
    }

    pub(super) fn clear_category_popup(&mut self) {
        self.category_input.clear();
        self.category_suggestions.clear();
        self.category_selected = 0;
        self.category_target = CategoryTarget::Transaction;
    }

    pub fn update_category_input(&mut self, c: char) {
        self.category_input.push(c);
        self.update_category_suggestions();
    }

    pub fn backspace_category_input(&mut self) {
        self.category_input.pop();
        self.update_category_suggestions();
    }

    fn update_category_suggestions(&mut self) {
        if self.category_input.is_empty() {
            self.category_suggestions =
                self.load_or_show("load categories", |s| s.list_categories());
        } else {
            let input = self.category_input.clone();
            self.category_suggestions =
                self.load_or_show("search categories", |s| s.find_categories(&input));
            // Offer the literal typed text as a (possibly new) category so a value
            // that fuzzy-matches existing paths can still be committed verbatim.
            let typed = input.trim();
            if !typed.is_empty() && !self.category_suggestions.iter().any(|c| c.path == typed) {
                self.category_suggestions.push(Category {
                    id: 0,
                    path: typed.to_string(),
                    created_at: chrono::Utc::now(),
                });
            }
        }
        self.category_selected = 0;
    }

    pub fn category_next(&mut self) {
        self.category_selected =
            next_wrapping(self.category_selected, self.category_suggestions.len());
    }

    pub fn category_previous(&mut self) {
        self.category_selected =
            prev_wrapping(self.category_selected, self.category_suggestions.len());
    }

    /// Start bulk-categorising every transaction matching the current search.
    /// Scoped to Transactions and Todo → AI Review; a no-op elsewhere.
    pub fn start_bulk_categorise_matching(&mut self) {
        let supported = self.current_tab == Tab::Transactions
            || (self.current_tab == Tab::Todo && self.todo_subtab == TodoSubTab::AiReview);
        if !supported {
            return;
        }
        self.open_category_popup(CategoryTarget::MatchingTransactions, String::new());
    }

    pub fn confirm_category(&mut self) {
        if let CategoryTarget::Filter(filter_id) = self.category_target {
            self.confirm_filter_category(filter_id);
            return;
        }

        if self.category_target == CategoryTarget::MatchingTransactions {
            self.confirm_bulk_categorise_matching();
            return;
        }

        self.confirm_transaction_category();
    }

    /// Resolve the chosen category, count the matching (non-transfer)
    /// transactions, and open a confirmation before the bulk apply. Unlike the
    /// single-transaction path, this never offers the similar-transactions
    /// popup — it goes straight to the count confirmation.
    fn confirm_bulk_categorise_matching(&mut self) {
        let Some(category_path) = self.selected_category_path() else {
            self.cancel_input();
            return;
        };
        let ids = self.matching_transactions_for_bulk();
        self.clear_category_popup();
        if ids.is_empty() {
            self.cancel_input();
            return;
        }

        let count = ids.len();
        self.confirm(
            format!(
                "Apply category \"{}\" to {} transaction{}?",
                category_path,
                count,
                if count == 1 { "" } else { "s" }
            ),
            ConfirmAction::CategoriseMatching {
                category_path,
                tx_ids: ids,
            },
        );
    }

    /// Transaction ids matching the current DB search on the active tab,
    /// excluding transfer legs (which can't be categorised). Empty on tabs the
    /// feature doesn't cover.
    fn matching_transactions_for_bulk(&mut self) -> Vec<i64> {
        let parsed = self
            .current_search_state()
            .map(|s| s.search_bar.parsed().clone())
            .unwrap_or_default();
        match self.current_tab_key() {
            (Tab::Transactions, _) => {
                let txns = self.load_or_show("load matching transactions", |s| {
                    s.query_transactions(&parsed, None)
                });
                let ids: Vec<i64> = txns.iter().map(|tx| tx.id).collect();
                let transfers =
                    self.load_or_show("load transfers", |s| s.get_transfers_for_transactions(&ids));
                ids.into_iter()
                    .filter(|id| !transfers.contains_key(id))
                    .collect()
            }
            (Tab::Todo, Some(TodoSubTab::AiReview)) => self
                .load_or_show("load matching transactions", |s| {
                    s.get_pending_ai_reviews(&parsed, None)
                })
                .into_iter()
                .map(|r| r.transaction.id)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn confirm_transaction_category(&mut self) {
        let Some(tx) = self.selected_transaction().cloned() else {
            return;
        };

        let Some(category_path) = self.selected_category_path() else {
            self.cancel_input();
            return;
        };

        // A transaction is either a transfer or categorised, never both.
        // Categorising one that is part of a transfer breaks the link — confirm
        // first.
        if let Some(transfer_id) = self.get_cached_transfer(tx.id).map(|t| t.id) {
            self.clear_category_popup();
            self.confirm(
                "This transaction is part of a transfer. Categorising it will unlink the transfer. Continue?"
                    .to_string(),
                ConfirmAction::BreakTransferForCategory {
                    transfer_id,
                    tx,
                    category_path,
                },
            );
            return;
        }

        self.apply_category(tx, category_path);
    }

    fn confirm_filter_category(&mut self, filter_id: i64) {
        let clear_existing = self.category_input.trim().is_empty()
            && self
                .lists
                .filters
                .items()
                .iter()
                .find(|filter| filter.id == filter_id)
                .is_some_and(|filter| filter.category_id.is_some());

        if clear_existing {
            if self.try_mutation("clear filter category", |s| {
                s.set_filter_category(filter_id, None)
            }) {
                self.clear_category_popup();
                self.reapply_filters();
                self.restore_after_filter_modal(filter_id);
            }
            return;
        }

        let Some(category_path) = self.selected_category_path() else {
            self.cancel_input();
            return;
        };

        if self.try_mutation("set filter category", |s| {
            let category_id = s.get_or_create_category(&category_path)?;
            s.set_filter_category(filter_id, Some(category_id))
        }) {
            self.clear_category_popup();
            self.reapply_filters();
            self.restore_after_filter_modal(filter_id);
        }
    }

    fn selected_category_path(&self) -> Option<String> {
        if !self.category_suggestions.is_empty() {
            self.category_suggestions
                .get(self.category_selected)
                .map(|cat| cat.path.clone())
        } else if !self.category_input.is_empty() {
            Some(self.category_input.clone())
        } else {
            None
        }
    }

    /// Persist a manual category for `tx`, then offer to bulk-apply it to
    /// similar transactions. Assumes any transfer conflict has been resolved.
    pub(super) fn apply_category(&mut self, tx: Transaction, category_path: String) {
        let mut saved_category_id = None;
        let saved = self.try_mutation("set category", |s| {
            let category_id = s.get_or_create_category(&category_path)?;
            s.set_category(tx.id, category_id, CategorySource::Manual, true, None)?;
            saved_category_id = Some(category_id);
            Ok(())
        });
        if !saved {
            return;
        }

        let Some(category_id) = saved_category_id else {
            self.error_message = Some("Failed to set category: category was not resolved".into());
            return;
        };

        self.refresh_data();
        self.cancel_input();
        self.open_bulk_apply_for(tx, category_id, category_path);
    }

    pub fn confirm_ai_category(&mut self) {
        if self.current_tab != Tab::Todo || self.todo_subtab != TodoSubTab::AiReview {
            return;
        }
        let Some(tx_id) = self
            .lists
            .ai_reviews
            .get(self.selected_index)
            .map(|r| r.transaction.id)
        else {
            return;
        };
        if self.try_mutation("confirm AI category", |s| s.confirm_category(tx_id)) {
            self.refresh_data();
        }
    }

    /// Remove the category from the selected AI-review transaction, dropping
    /// it back to uncategorised.
    pub fn remove_ai_category(&mut self) {
        if self.current_tab != Tab::Todo || self.todo_subtab != TodoSubTab::AiReview {
            return;
        }
        let Some(tx_id) = self
            .lists
            .ai_reviews
            .get(self.selected_index)
            .map(|r| r.transaction.id)
        else {
            return;
        };
        if !self.try_mutation("remove category", |s| s.delete_enrichment(tx_id)) {
            return;
        }
        self.refresh_data();
    }

    fn open_bulk_apply_for(&mut self, tx: Transaction, category_id: i64, category_path: String) {
        if self.similarity_index.is_none() {
            self.rebuild_similarity_index();
        }

        let query_norm = normalise(&tx.description);
        let matches = self
            .similarity_index
            .as_ref()
            .map(|index| index.similar_to(&query_norm, tx.id, SIMILARITY_THRESHOLD))
            .unwrap_or_default();
        let mut rows: Vec<_> = matches
            .into_iter()
            .take(BULK_APPLY_MATCH_LIMIT)
            .filter_map(|(id, score)| {
                self.similarity_candidates
                    .get(&id)
                    .cloned()
                    .map(|tx| BulkRow {
                        tx,
                        score,
                        selected: true,
                    })
            })
            .collect();
        rows.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.tx.date.cmp(&a.tx.date))
        });

        if rows.is_empty() {
            return;
        }

        self.bulk_apply = Some(BulkApplyState {
            category_id,
            category_path,
            rows,
            cursor: 0,
        });
        self.input_mode = InputMode::BulkApply;
    }

    pub fn bulk_apply_toggle(&mut self) {
        let Some(state) = self.bulk_apply.as_mut() else {
            return;
        };
        if let Some(row) = state.rows.get_mut(state.cursor) {
            row.selected = !row.selected;
        }
    }

    pub fn bulk_apply_toggle_all(&mut self) {
        let Some(state) = self.bulk_apply.as_mut() else {
            return;
        };
        let selected = state.rows.iter().any(|row| !row.selected);
        for row in &mut state.rows {
            row.selected = selected;
        }
    }

    pub fn bulk_apply_next(&mut self) {
        let Some(state) = self.bulk_apply.as_mut() else {
            return;
        };
        state.cursor = next_wrapping(state.cursor, state.rows.len());
    }

    pub fn bulk_apply_prev(&mut self) {
        let Some(state) = self.bulk_apply.as_mut() else {
            return;
        };
        state.cursor = prev_wrapping(state.cursor, state.rows.len());
    }

    pub fn bulk_apply_confirm(&mut self) {
        let Some(state) = self.bulk_apply.as_ref() else {
            return;
        };
        let category_id = state.category_id;
        let selected_ids: Vec<_> = state
            .rows
            .iter()
            .filter(|row| row.selected)
            .map(|row| row.tx.id)
            .collect();

        if selected_ids.is_empty() {
            self.bulk_apply_cancel();
            return;
        }

        let applied = self.try_mutation("apply category", |s| {
            for tx_id in selected_ids {
                s.set_category(tx_id, category_id, CategorySource::Manual, true, None)?;
            }
            Ok(())
        });
        if applied {
            self.bulk_apply = None;
            self.input_mode = InputMode::Normal;
            self.refresh_data();
        }
    }

    pub fn bulk_apply_cancel(&mut self) {
        self.bulk_apply = None;
        self.input_mode = InputMode::Normal;
    }

    // ==================== Category Editing (Categories Tab) ====================

    pub fn selected_category(&self) -> Option<&Category> {
        if self.current_tab == Tab::Categories {
            self.lists.categories.get(self.selected_index)
        } else {
            None
        }
    }

    pub fn start_category_rename(&mut self) {
        if let Some(cat) = self.selected_category().cloned() {
            self.open_text_prompt(
                "Rename category",
                cat.path.clone(),
                TextPromptTarget::CategoryRename(cat),
            );
        }
    }

    /// Prompt to delete the selected category, noting how many transactions
    /// would be left uncategorised.
    pub fn start_category_delete(&mut self) {
        let Some(cat) = self.selected_category().cloned() else {
            return;
        };
        let count = self.load_or_show("count transactions in category", |s| {
            s.count_transactions_in_category(cat.id)
        });
        let filter_count = self
            .load_or_show("load filters", |s| s.filters_using_category(cat.id))
            .len();
        let suffix = if filter_count > 0 {
            format!(
                " and {} filter{} will lose their category.",
                filter_count,
                if filter_count == 1 { "" } else { "s" }
            )
        } else {
            ".".to_string()
        };
        self.confirm(
            format!(
                "Delete category \"{}\"? {} transaction{} will be left without a category{}",
                cat.path,
                count,
                if count == 1 { "" } else { "s" },
                suffix
            ),
            ConfirmAction::DeleteCategory(cat.id),
        );
    }

    /// Reload categories and keep the cursor in bounds after a deletion.
    pub(super) fn delete_category_after(&mut self) {
        self.reload_categories();
    }

    pub(super) fn confirm_category_rename(&mut self, cat: Category, new_path: String) {
        if new_path.is_empty() || new_path == cat.path {
            self.cancel_input();
            return;
        }

        match self.store.rename_category(cat.id, &new_path) {
            Ok(()) => {
                self.reload_categories();
                self.move_cursor_to_category(&new_path);
                self.input_mode = InputMode::Normal;
                self.clear_text_prompt();
            }
            Err(crate::Error::CategoryExists(existing_path)) => {
                if let Ok(Some(target)) = self.store.get_category_by_path(&existing_path) {
                    let source_count = self.load_or_show("count transactions in category", |s| {
                        s.count_transactions_in_category(cat.id)
                    });
                    let source_id = cat.id;
                    self.restore_text_prompt(
                        "Rename category",
                        new_path,
                        TextPromptTarget::CategoryRename(cat),
                    );
                    self.confirm(
                        format!(
                            "Merge {} transaction{} into \"{}\"?",
                            source_count,
                            if source_count == 1 { "" } else { "s" },
                            existing_path
                        ),
                        ConfirmAction::MergeCategory {
                            source_id,
                            target_id: target.id,
                        },
                    );
                } else {
                    self.error_message = Some(format!("Category \"{}\" already exists", new_path));
                    self.restore_text_prompt(
                        "Rename category",
                        new_path,
                        TextPromptTarget::CategoryRename(cat),
                    );
                }
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to rename: {}", e));
                self.restore_text_prompt(
                    "Rename category",
                    new_path,
                    TextPromptTarget::CategoryRename(cat),
                );
            }
        }
    }

    pub(super) fn reload_categories(&mut self) {
        let categories = self.load_or_show("load categories", |s| s.list_categories());
        self.lists.categories.set_items(categories);
        self.rebuild_search_configs();
        self.rebuild_category_counts();
        self.apply_fuzzy_filter();
        self.clamp_selection();
        self.reload_category_transactions();
    }

    /// Rebuild the per-category transaction count cache in one bulk query.
    /// Only needs to run when category assignments change.
    pub(super) fn rebuild_category_counts(&mut self) {
        self.category_tx_count = self.load_or_show("load category counts", |s| {
            s.get_category_transaction_counts()
        });
    }

    pub fn category_transaction_count(&self, category_id: i64) -> usize {
        self.category_tx_count
            .get(&category_id)
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn move_cursor_to_category(&mut self, path: &str) {
        if let Some(pos) = self.lists.categories.iter().position(|c| c.path == path) {
            self.selected_index = pos;
        }
        self.reload_category_transactions();
    }

    // ==================== Category Transactions Side Panel ====================

    pub fn toggle_category_transactions(&mut self) {
        self.show_category_transactions = !self.show_category_transactions;
        self.reload_category_transactions();
    }

    /// Reload the side-panel transactions for the selected category, or clear
    /// them when the panel is closed / not on the Categories tab / no category
    /// selected.
    pub(super) fn reload_category_transactions(&mut self) {
        if self.current_tab == Tab::Categories
            && self.show_category_transactions
            && let Some(cat_id) = self.selected_category().map(|c| c.id)
        {
            self.category_transactions = self.load_or_show("load category transactions", |s| {
                s.query_transactions_in_category(cat_id, Some(super::LIST_LIMIT))
            });
            return;
        }
        self.category_transactions.clear();
    }

    /// Jump to the Transactions tab with its DB search set to this category, so
    /// the user can act on its transactions. Focus lands on the first
    /// transaction.
    pub fn manage_category_transactions(&mut self) {
        let Some(path) = self.selected_category().map(|c| c.path.clone()) else {
            return;
        };
        self.save_tab_state();
        self.show_category_transactions = false;
        self.category_transactions.clear();
        self.current_tab = Tab::Transactions;

        let query = format!("category:{path}");
        let state = self.current_search_state_mut();
        state.search_bar.set_value(&query);
        state.db_search_active = true;
        state.editing_db_search = false;
        state.fuzzy_search_active = false;
        state.editing_fuzzy_search = false;
        state.selected_index = 0;

        self.input_mode = InputMode::Normal;
        self.selected_index = 0;
        self.reload_current_tab();
    }

    fn filter_edit_filter_for_category(&self) -> Option<&crate::Filter> {
        if self.input_mode == InputMode::FilterEdit {
            self.selected_filter()
        } else {
            None
        }
    }
}
