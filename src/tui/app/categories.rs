//! Category actions: the category-assignment popup, AI-review confirmation,
//! and rename/merge on the Categories tab.

use tui_input::Input;

use crate::classify::{SIMILARITY_THRESHOLD, normalise};
use crate::{Category, CategorySource, Transaction};

use super::{App, BulkApplyState, BulkRow, ConfirmAction, InputMode, Tab, TodoSubTab};

const BULK_APPLY_MATCH_LIMIT: usize = 200;

impl App {
    // ==================== Category Popup (assign to transaction) ====================

    pub fn start_category_edit(&mut self) {
        if self.selected_transaction().is_some() {
            self.input_mode = InputMode::Category;
            self.category_input.clear();
            self.category_suggestions =
                self.load_or_show("load categories", |s| s.list_categories());
            self.category_selected = 0;
        }
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
        }
        self.category_selected = 0;
    }

    pub fn category_next(&mut self) {
        if !self.category_suggestions.is_empty() {
            self.category_selected = (self.category_selected + 1) % self.category_suggestions.len();
        }
    }

    pub fn category_previous(&mut self) {
        if !self.category_suggestions.is_empty() {
            self.category_selected = (self.category_selected + self.category_suggestions.len() - 1)
                % self.category_suggestions.len();
        }
    }

    pub fn confirm_category(&mut self) {
        let Some(tx) = self.selected_transaction().cloned() else {
            return;
        };

        let category_path = if !self.category_suggestions.is_empty() {
            self.category_suggestions[self.category_selected]
                .path
                .clone()
        } else if !self.category_input.is_empty() {
            self.category_input.clone()
        } else {
            self.cancel_input();
            return;
        };

        // A transaction is either a transfer or categorised, never both.
        // Categorising one that is part of a transfer breaks the link — confirm
        // first.
        if let Some(transfer_id) = self.get_cached_transfer(tx.id).map(|t| t.id) {
            self.category_input.clear();
            self.category_suggestions.clear();
            self.category_selected = 0;
            self.confirm_message = Some(
                "This transaction is part of a transfer. Categorising it will unlink the transfer. Continue?"
                    .to_string(),
            );
            self.confirm_action = Some(ConfirmAction::BreakTransferForCategory {
                transfer_id,
                tx,
                category_path,
            });
            self.input_mode = InputMode::Confirm;
            return;
        }

        self.apply_category(tx, category_path);
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
        if self.selected_index >= self.lists.ai_reviews.len() && self.selected_index > 0 {
            self.selected_index -= 1;
        }
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
        let rows: Vec<_> = matches
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
        if !state.rows.is_empty() {
            state.cursor = (state.cursor + 1) % state.rows.len();
        }
    }

    pub fn bulk_apply_prev(&mut self) {
        let Some(state) = self.bulk_apply.as_mut() else {
            return;
        };
        if !state.rows.is_empty() {
            state.cursor = (state.cursor + state.rows.len() - 1) % state.rows.len();
        }
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
            self.category_edit_input = Input::new(cat.path.clone());
            self.editing_category = Some(cat);
            self.input_mode = InputMode::CategoryEdit;
        }
    }

    pub fn handle_category_edit_input(&mut self, req: tui_input::InputRequest) {
        self.category_edit_input.handle(req);
    }

    pub fn category_edit_value(&self) -> &str {
        self.category_edit_input.value()
    }

    pub fn category_edit_cursor(&self) -> usize {
        self.category_edit_input.visual_cursor()
    }

    pub fn category_edit_scroll(&self, width: usize) -> usize {
        self.category_edit_input.visual_scroll(width)
    }

    pub fn confirm_category_rename(&mut self) {
        let Some(cat) = self.editing_category.take() else {
            self.cancel_input();
            return;
        };

        let new_path = self.category_edit_input.value().trim().to_string();
        if new_path.is_empty() || new_path == cat.path {
            self.cancel_input();
            return;
        }

        match self.store.rename_category(cat.id, &new_path) {
            Ok(()) => {
                self.reload_categories();
                self.move_cursor_to_category(&new_path);
                self.input_mode = InputMode::Normal;
                self.category_edit_input.reset();
            }
            Err(crate::Error::CategoryExists(existing_path)) => {
                if let Ok(Some(target)) = self.store.get_category_by_path(&existing_path) {
                    let source_count = self
                        .store
                        .count_transactions_in_category(cat.id)
                        .unwrap_or(0);
                    self.confirm_message = Some(format!(
                        "Merge {} transaction{} into \"{}\"?",
                        source_count,
                        if source_count == 1 { "" } else { "s" },
                        existing_path
                    ));
                    self.confirm_action = Some(ConfirmAction::MergeCategory {
                        source_id: cat.id,
                        target_id: target.id,
                    });
                    self.editing_category = Some(cat);
                    self.input_mode = InputMode::ConfirmMerge;
                } else {
                    self.error_message = Some(format!("Category \"{}\" already exists", new_path));
                    self.editing_category = Some(cat);
                }
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to rename: {}", e));
                self.editing_category = Some(cat);
            }
        }
    }

    pub fn confirm_merge(&mut self) {
        let Some(ConfirmAction::MergeCategory {
            source_id,
            target_id,
        }) = self.confirm_action.take()
        else {
            self.cancel_input();
            return;
        };

        match self.store.merge_categories(source_id, target_id) {
            Ok(()) => {
                self.reload_categories();
                if let Ok(Some(target)) = self.store.get_category(target_id) {
                    self.move_cursor_to_category(&target.path);
                }
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to merge: {}", e));
            }
        }

        self.clear_category_edit();
        self.clear_confirm();
        self.input_mode = InputMode::Normal;
    }

    pub fn cancel_merge(&mut self) {
        self.clear_confirm();
        self.input_mode = InputMode::CategoryEdit;
    }

    pub(super) fn clear_category_edit(&mut self) {
        self.editing_category = None;
        self.category_edit_input.reset();
    }

    fn reload_categories(&mut self) {
        let categories = self.load_or_show("load categories", |s| s.list_categories());
        self.lists.categories.set_items(categories);
        self.rebuild_search_configs();
        self.rebuild_category_counts();
        self.apply_fuzzy_filter();
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

    fn move_cursor_to_category(&mut self, path: &str) {
        if let Some(pos) = self.lists.categories.iter().position(|c| c.path == path) {
            self.selected_index = pos;
        }
    }
}
