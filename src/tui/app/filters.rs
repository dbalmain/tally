//! Saved-search filter actions for the Filters tab.

use tui_input::InputRequest;

use crate::tui::search_bar::SearchBar;
use crate::{Filter, FilterOverride, Transaction};

use super::{
    App, ApplyFiltersPreview, ConfirmAction, FilterEditState, InputMode, Tab, TextPromptTarget,
    next_wrapping, prev_wrapping,
};

const FILTER_EDIT_PREVIEW_LIMIT: usize = 500;

impl App {
    pub fn selected_filter(&self) -> Option<&Filter> {
        if let Some(filter) = self.filter_edit_filter() {
            return Some(filter);
        }
        if self.current_tab == Tab::Filters {
            self.lists.filters.get(self.selected_index)
        } else {
            None
        }
    }

    pub fn start_filter_create(&mut self) {
        if self.current_tab == Tab::Filters {
            self.open_text_prompt("New filter", String::new(), TextPromptTarget::FilterCreate);
        }
    }

    pub fn start_filter_from_search(&mut self) {
        if self.current_tab != Tab::Transactions || !self.db_search_active() {
            return;
        }

        let query = self.db_search_value().to_string();
        if query.is_empty() {
            return;
        }

        self.open_text_prompt(
            "Save filter",
            String::new(),
            TextPromptTarget::FilterCreateFromQuery(query),
        );
    }

    pub fn start_filter_rename(&mut self) {
        let Some(filter) = self.selected_filter().cloned() else {
            return;
        };
        self.open_text_prompt(
            "Rename filter",
            filter.name,
            TextPromptTarget::FilterRename(filter.id),
        );
    }

    pub fn open_filter_edit(&mut self) {
        let Some(filter) = self.selected_filter().cloned() else {
            return;
        };
        let mut search_bar = SearchBar::new(self.build_search_config((Tab::Transactions, None)));
        search_bar.set_value(&filter.query);
        self.filter_edit = Some(FilterEditState {
            filter_id: filter.id,
            name: filter.name,
            search_bar,
            preview: Vec::new(),
            preview_scroll: 0,
        });
        self.input_mode = InputMode::FilterEdit;
        self.recompute_filter_preview();
    }

    pub fn filter_edit_input(&mut self, req: InputRequest) {
        let Some(state) = self.filter_edit.as_mut() else {
            return;
        };
        state.search_bar.handle_input_without_fuzzy_transition(req);
        state.preview_scroll = 0;
        self.recompute_filter_preview();
    }

    pub fn filter_edit_preview_next(&mut self) {
        let Some(state) = self.filter_edit.as_mut() else {
            return;
        };
        state.preview_scroll = next_wrapping(state.preview_scroll, state.preview.len());
    }

    pub fn filter_edit_preview_prev(&mut self) {
        let Some(state) = self.filter_edit.as_mut() else {
            return;
        };
        state.preview_scroll = prev_wrapping(state.preview_scroll, state.preview.len());
    }

    /// Save the in-edit query, reapply filters, and return to the Filters list.
    pub fn save_filter_edit(&mut self) {
        let Some((filter_id, query)) = self
            .filter_edit
            .as_ref()
            .map(|state| (state.filter_id, state.search_bar.value().to_string()))
        else {
            return;
        };

        if self.try_mutation("save filter query", |s| {
            s.set_filter_query(filter_id, &query)
        }) {
            self.reapply_filters();
            self.exit_filter_edit();
        }
    }

    /// Open the Ctrl-A confirmation modal listing the transactions the current
    /// filter set would (re)categorise. Available on the Filters tab and the
    /// filter edit screen; confirming runs the real apply.
    pub fn apply_filter_categories(&mut self) {
        if self.current_tab != Tab::Filters && self.filter_edit.is_none() {
            return;
        }
        let rows = match self.store.preview_filters() {
            Ok(rows) => rows,
            Err(e) => {
                self.error_message = Some(format!("Failed to preview filters: {e}"));
                return;
            }
        };
        self.apply_filters_preview = Some(ApplyFiltersPreview { rows, scroll: 0 });
        self.input_mode = InputMode::ConfirmApplyFilters;
    }

    pub fn apply_filters_preview_rows(&self) -> &[Transaction] {
        self.apply_filters_preview
            .as_ref()
            .map(|p| p.rows.as_slice())
            .unwrap_or(&[])
    }

    pub fn apply_filters_preview_scroll(&self) -> usize {
        self.apply_filters_preview
            .as_ref()
            .map(|p| p.scroll)
            .unwrap_or(0)
    }

    pub fn apply_filters_preview_next(&mut self) {
        if let Some(p) = self.apply_filters_preview.as_mut()
            && !p.rows.is_empty()
        {
            p.scroll = next_wrapping(p.scroll, p.rows.len());
        }
    }

    pub fn apply_filters_preview_prev(&mut self) {
        if let Some(p) = self.apply_filters_preview.as_mut()
            && !p.rows.is_empty()
        {
            p.scroll = prev_wrapping(p.scroll, p.rows.len());
        }
    }

    /// Confirm the Ctrl-A modal: run the real apply, then return to the screen
    /// the modal was opened from.
    pub fn apply_filters_confirm(&mut self) {
        self.apply_filters_preview = None;
        self.reapply_filters();
        self.input_mode = if self.filter_edit.is_some() {
            InputMode::FilterEdit
        } else {
            InputMode::Normal
        };
    }

    /// Whether the in-edit query differs from the saved filter's query.
    pub fn filter_edit_dirty(&self) -> bool {
        let Some(state) = self.filter_edit.as_ref() else {
            return false;
        };
        let saved = self.filter_edit_filter().map(|f| f.query.as_str());
        saved != Some(state.search_bar.value())
    }

    /// Esc on the edit screen: prompt before discarding unsaved query edits,
    /// otherwise exit straight away.
    pub fn request_exit_filter_edit(&mut self) {
        if self.filter_edit_dirty() {
            self.confirm(
                "Discard unsaved changes?".to_string(),
                ConfirmAction::DiscardFilterEdit,
            );
        } else {
            self.exit_filter_edit();
        }
    }

    pub fn exit_filter_edit(&mut self) {
        self.filter_edit = None;
        self.input_mode = InputMode::Normal;
    }

    pub(super) fn recompute_filter_preview(&mut self) {
        let Some(parsed) = self
            .filter_edit
            .as_ref()
            .map(|state| state.search_bar.parsed().clone())
        else {
            return;
        };
        let preview = self.load_or_show("load filter preview", |s| {
            s.query_transactions(&parsed, Some(FILTER_EDIT_PREVIEW_LIMIT))
        });
        if let Some(state) = self.filter_edit.as_mut() {
            state.preview = preview;
            clamp_preview_scroll(state);
        }
    }

    pub fn filter_edit_visible(&self) -> bool {
        self.filter_edit.is_some()
    }

    pub fn filter_edit_name(&self) -> &str {
        self.filter_edit
            .as_ref()
            .map(|state| state.name.as_str())
            .unwrap_or("")
    }

    pub fn filter_edit_category_path(&self) -> Option<&str> {
        let filter = self.filter_edit_filter()?;
        self.category_path(filter.category_id?)
    }

    /// Override label for the in-edit filter, only when a category is set.
    pub fn filter_edit_override_label(&self) -> Option<&'static str> {
        let filter = self.filter_edit_filter()?;
        filter.category_id?;
        Some(match filter.override_mode {
            FilterOverride::Uncategorised => "new",
            FilterOverride::Ai => "+ai",
            FilterOverride::All => "all",
        })
    }

    /// Review-required flag for the in-edit filter, only when a category is set.
    pub fn filter_edit_review_required(&self) -> Option<bool> {
        let filter = self.filter_edit_filter()?;
        filter.category_id?;
        Some(filter.review_required)
    }

    pub fn filter_edit_search_bar(&self) -> Option<&SearchBar> {
        self.filter_edit.as_ref().map(|state| &state.search_bar)
    }

    pub fn filter_edit_preview(&self) -> &[Transaction] {
        self.filter_edit
            .as_ref()
            .map(|state| state.preview.as_slice())
            .unwrap_or(&[])
    }

    pub fn filter_edit_preview_scroll(&self) -> usize {
        self.filter_edit
            .as_ref()
            .map(|state| state.preview_scroll)
            .unwrap_or(0)
    }

    pub fn filter_edit_autocomplete_active(&self) -> bool {
        self.filter_edit
            .as_ref()
            .is_some_and(|state| state.search_bar.autocomplete_active())
    }

    pub fn filter_edit_autocomplete_next(&mut self) {
        if let Some(state) = self.filter_edit.as_mut() {
            state.search_bar.autocomplete_next();
        }
    }

    pub fn filter_edit_autocomplete_prev(&mut self) {
        if let Some(state) = self.filter_edit.as_mut() {
            state.search_bar.autocomplete_prev();
        }
    }

    pub fn filter_edit_autocomplete_select(&mut self) -> bool {
        let Some(state) = self.filter_edit.as_mut() else {
            return false;
        };
        let selected = state.search_bar.autocomplete_select();
        if selected {
            state.preview_scroll = 0;
            self.recompute_filter_preview();
        }
        selected
    }

    pub fn filter_edit_autocomplete_close(&mut self) {
        if let Some(state) = self.filter_edit.as_mut() {
            state.search_bar.autocomplete_close();
        }
    }

    pub(super) fn confirm_filter_create(&mut self, name: String) {
        if self.current_tab != Tab::Filters || name.is_empty() {
            self.cancel_input();
            return;
        }

        let mut created_id = None;
        if self.try_mutation("create filter", |s| {
            created_id = Some(s.create_filter(&name, "")?);
            Ok(())
        }) {
            self.clear_text_prompt();
            self.input_mode = InputMode::Normal;
            self.reload_filters();
            if let Some(id) = created_id {
                self.move_cursor_to_filter(id);
            }
        } else {
            self.restore_text_prompt("New filter", name, TextPromptTarget::FilterCreate);
        }
    }

    pub(super) fn confirm_filter_from_query(
        &mut self,
        name: String,
        query: String,
        return_mode: InputMode,
    ) {
        if self.current_tab != Tab::Transactions || name.is_empty() {
            self.input_mode = return_mode;
            return;
        }

        let mut created_id = None;
        if self.try_mutation("create filter", |s| {
            created_id = Some(s.create_filter(&name, &query)?);
            Ok(())
        }) {
            self.clear_text_prompt();
            self.save_current_tab_state_as(return_mode);
            self.reload_filters();
            self.current_tab = Tab::Filters;
            if let Some(id) = created_id {
                self.move_cursor_to_filter(id);
                self.open_filter_edit();
            }
        } else {
            self.restore_text_prompt_with_return(
                "Save filter",
                name,
                TextPromptTarget::FilterCreateFromQuery(query),
                return_mode,
            );
        }
    }

    pub(super) fn confirm_filter_rename(&mut self, id: i64, name: String) {
        if self.current_tab != Tab::Filters || name.is_empty() {
            self.cancel_input();
            return;
        }

        let unchanged = self
            .lists
            .filters
            .items()
            .iter()
            .any(|filter| filter.id == id && filter.name == name);
        if unchanged {
            self.cancel_input();
            return;
        }

        if self.try_mutation("rename filter", |s| s.rename_filter(id, &name)) {
            self.clear_text_prompt();
            self.reload_filters();
            self.move_cursor_to_filter(id);
            self.restore_after_filter_modal(id);
        } else {
            self.restore_text_prompt("Rename filter", name, TextPromptTarget::FilterRename(id));
        }
    }

    pub fn cycle_filter_override(&mut self) {
        let Some(filter) = self.selected_filter().cloned() else {
            return;
        };
        if filter.category_id.is_none() {
            return;
        }

        let mode = match filter.override_mode {
            FilterOverride::Uncategorised => FilterOverride::Ai,
            FilterOverride::Ai => FilterOverride::All,
            FilterOverride::All => FilterOverride::Uncategorised,
        };

        // A setting change must not apply categories on its own — that would
        // bypass the `a` / Ctrl-A confirmation summary (and, for `all`, silently
        // overwrite existing categories). Just persist the mode and refresh the
        // display; the user applies explicitly.
        if self.try_mutation("set filter override", |s| {
            s.set_filter_override(filter.id, mode)
        }) {
            self.reload_filters();
        }
    }

    pub fn toggle_filter_review(&mut self) {
        let Some(filter) = self.selected_filter().cloned() else {
            return;
        };
        if filter.category_id.is_none() {
            return;
        }

        // As with the override mode: persist the setting and refresh the
        // display, but leave applying to the explicit `a` / Ctrl-A flow.
        if self.try_mutation("set filter review", |s| {
            s.set_filter_review(filter.id, !filter.review_required)
        }) {
            self.reload_filters();
        }
    }

    /// Prompt before deleting the selected filter; the delete itself runs in
    /// `confirm_proceed` for `ConfirmAction::DeleteFilter`.
    pub fn delete_filter(&mut self) {
        let Some((id, name)) = self
            .selected_filter()
            .map(|filter| (filter.id, filter.name.clone()))
        else {
            return;
        };
        self.confirm(
            format!("Delete filter '{name}'?"),
            ConfirmAction::DeleteFilter(id),
        );
    }

    /// Persist nothing itself; re-derive rule categories and reload everything.
    pub(super) fn reapply_filters(&mut self) {
        if self.try_mutation("apply filters", |s| s.apply_filters().map(|_| ())) {
            self.refresh_data();
        }
    }

    fn reload_filters(&mut self) {
        let filters = self.load_or_show("load filters", |s| s.list_filters());
        self.lists.filters.set_items(filters);
        self.apply_fuzzy_filter();
        self.clamp_selection();
    }

    fn move_cursor_to_filter(&mut self, id: i64) {
        if let Some(pos) = self.lists.filters.position(|filter| filter.id == id) {
            self.selected_index = pos;
        }
    }

    fn save_current_tab_state_as(&mut self, return_mode: InputMode) {
        let selected_index = self.selected_index;
        let state = self.current_search_state_mut();
        state.selected_index = selected_index;
        state.editing_db_search = return_mode == InputMode::DbSearch;
        state.editing_fuzzy_search = return_mode == InputMode::FuzzySearch;
    }

    pub(super) fn restore_after_filter_modal(&mut self, id: i64) {
        if self
            .filter_edit
            .as_ref()
            .is_some_and(|state| state.filter_id == id)
        {
            self.refresh_filter_edit();
            self.recompute_filter_preview();
            self.input_mode = InputMode::FilterEdit;
        } else {
            self.input_mode = InputMode::Normal;
        }
    }

    fn refresh_filter_edit(&mut self) {
        let Some(filter) = self.filter_edit_filter().cloned() else {
            return;
        };
        if let Some(state) = self.filter_edit.as_mut() {
            state.name = filter.name;
            clamp_preview_scroll(state);
        }
    }

    fn filter_edit_filter(&self) -> Option<&Filter> {
        let id = self.filter_edit.as_ref()?.filter_id;
        self.lists
            .filters
            .items()
            .iter()
            .find(|filter| filter.id == id)
    }
}

fn clamp_preview_scroll(state: &mut FilterEditState) {
    if state.preview.is_empty() {
        state.preview_scroll = 0;
    } else {
        state.preview_scroll = state.preview_scroll.min(state.preview.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;
    use tui_input::InputRequest;

    use crate::TransactionStore;

    use super::*;

    #[test]
    fn filter_edit_opens_preview_and_saves_query() {
        let (_temp, mut store) = store_with_imported_transaction();
        let filter_id = store.create_filter("Test filter", "Test").unwrap();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Filters;

        app.open_filter_edit();

        let state = app.filter_edit.as_ref().unwrap();
        assert_eq!(app.input_mode, InputMode::FilterEdit);
        assert_eq!(state.filter_id, filter_id);
        assert_eq!(state.name, "Test filter");
        assert_eq!(state.search_bar.value(), "Test");
        assert_eq!(state.preview.len(), 1);
        assert_eq!(state.preview[0].description, "Test transaction");

        app.filter_edit
            .as_mut()
            .unwrap()
            .search_bar
            .set_value("NoMatch");
        app.save_filter_edit();

        let filters = app.store.list_filters().unwrap();
        assert_eq!(filters[0].query, "NoMatch");
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.filter_edit.is_none());
    }

    #[test]
    fn save_filter_edit_returns_to_filters_list() {
        let (_temp, mut store) = store_with_imported_transaction();
        store.create_filter("Test filter", "Test").unwrap();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Filters;
        app.open_filter_edit();

        app.filter_edit
            .as_mut()
            .unwrap()
            .search_bar
            .set_value("NoMatch");
        // Enter with no autocomplete open saves and exits to the Filters list.
        app.save_filter_edit();

        assert_eq!(app.store.list_filters().unwrap()[0].query, "NoMatch");
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.current_tab, Tab::Filters);
        assert!(app.filter_edit.is_none());
    }

    #[test]
    fn saving_transaction_search_as_filter_opens_filter_edit() {
        let (_temp, store) = store_with_imported_transaction();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Transactions;
        app.start_db_search();
        for c in "Test".chars() {
            app.handle_db_search_input(InputRequest::InsertChar(c));
        }

        app.start_filter_from_search();
        assert_eq!(app.input_mode, InputMode::TextPrompt);
        assert_eq!(app.text_prompt_title(), "Save filter");

        for c in "Saved test".chars() {
            app.handle_text_prompt_input(InputRequest::InsertChar(c));
        }
        app.confirm_text_prompt();

        let filters = app.store.list_filters().unwrap();
        let filter = filters
            .iter()
            .find(|filter| filter.name == "Saved test")
            .unwrap();
        assert_eq!(filter.query, "Test");
        assert_eq!(app.current_tab, Tab::Filters);
        assert_eq!(app.input_mode, InputMode::FilterEdit);
        assert_eq!(
            app.lists.filters.get(app.selected_index).unwrap().id,
            filter.id
        );

        let state = app.filter_edit.as_ref().unwrap();
        assert_eq!(state.filter_id, filter.id);
        assert_eq!(state.search_bar.value(), "Test");
        assert_eq!(state.preview.len(), 1);
        assert_eq!(state.preview[0].description, "Test transaction");
    }

    #[test]
    fn esc_prompts_only_when_filter_edit_query_is_dirty() {
        let (_temp, mut store) = store_with_imported_transaction();
        store.create_filter("Test filter", "Test").unwrap();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Filters;

        // Unchanged query exits straight to Normal.
        app.open_filter_edit();
        assert!(!app.filter_edit_dirty());
        app.request_exit_filter_edit();
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.filter_edit.is_none());

        // A dirty query routes to the discard confirmation instead of exiting.
        app.open_filter_edit();
        app.filter_edit
            .as_mut()
            .unwrap()
            .search_bar
            .set_value("Changed");
        assert!(app.filter_edit_dirty());
        app.request_exit_filter_edit();
        assert_eq!(app.input_mode, InputMode::Confirm);
        assert!(app.filter_edit.is_some());

        // Confirming the discard leaves the edit screen.
        app.confirm_proceed();
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.filter_edit.is_none());
    }

    #[test]
    fn cycling_override_persists_setting_without_applying() {
        let (_temp, mut store) = store_with_imported_transaction();
        let tx = store
            .query_transactions(&crate::search::ParsedQuery::empty(), None)
            .unwrap()
            .into_iter()
            .find(|t| t.description == "Test transaction")
            .unwrap();
        // A filter matches the transaction and carries a rule category.
        let rule_cat = store.get_or_create_category("Rule/Cat").unwrap();
        let filter_id = store.create_filter("f", "Test").unwrap();
        store
            .set_filter_category(filter_id, Some(rule_cat))
            .unwrap();
        // The transaction already has a confirmed manual category.
        let manual_cat = store.get_or_create_category("Manual/Cat").unwrap();
        store
            .set_category(tx.id, manual_cat, crate::CategorySource::Manual, true, None)
            .unwrap();

        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Filters;
        app.selected_index = 0;

        // Cycle override uncategorised -> ai -> all.
        app.cycle_filter_override();
        app.cycle_filter_override();

        // The setting persisted...
        assert_eq!(
            app.store.list_filters().unwrap()[0].override_mode,
            FilterOverride::All
        );
        // ...but nothing was applied: the manual category is untouched, even
        // though override=all would overwrite it on an explicit apply.
        assert_eq!(
            app.store
                .get_transaction_category(tx.id)
                .unwrap()
                .unwrap()
                .path,
            "Manual/Cat"
        );
        // The pending change only shows up when the user applies (a / Ctrl-A).
        let preview = app.store.preview_filters().unwrap();
        assert!(preview.iter().any(|t| t.id == tx.id));
    }

    fn store_with_imported_transaction() -> (TempDir, TransactionStore) {
        let temp = TempDir::new().unwrap();
        let account_dir = temp.path().join("TestBank").join("Checking");
        fs::create_dir_all(&account_dir).unwrap();
        fs::write(
            account_dir.join("transactions.csv"),
            "Date,Description,Amount,Balance\n2025-01-01,Test,-100,500\n",
        )
        .unwrap();

        let import_script = account_dir.join("import");
        fs::write(
            &import_script,
            r#"#!/usr/bin/env bash
echo '[{"date":"2025-01-01","description":"Test transaction","amount_cents":-10000,"balance_cents":50000}]'
"#,
        )
        .unwrap();
        make_executable(&import_script);

        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();
        (temp, store)
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        #[cfg(not(unix))]
        let _ = path;
    }
}
