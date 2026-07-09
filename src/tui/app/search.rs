//! Per-tab search state and the DB-search / fuzzy-search / autocomplete
//! actions. The parsing and SQL rendering live in `crate::search`; the
//! search bar widget lives in `crate::tui::search_bar`.

use tui_input::Input;

use crate::search::SearchConfig;
use crate::tui::search_bar::{KeyResult, SearchBar};

use super::{App, InputMode, Tab, TabKey, TodoSubTab};

/// Per-tab search state (DB search + fuzzy search + selection).
pub struct TabSearchState {
    /// SearchBar for DB search queries
    pub search_bar: SearchBar,
    pub db_search_active: bool,
    pub fuzzy_search_input: Input,
    pub fuzzy_pattern: String,
    pub fuzzy_search_active: bool,
    pub selected_index: usize,
    /// Was actively editing DB search when we left this tab
    pub editing_db_search: bool,
    /// Was actively editing fuzzy search when we left this tab
    pub editing_fuzzy_search: bool,
}

impl TabSearchState {
    /// Create a new TabSearchState with the given SearchConfig.
    pub fn new(config: SearchConfig) -> Self {
        Self {
            search_bar: SearchBar::new(config),
            db_search_active: false,
            fuzzy_search_input: Input::default(),
            fuzzy_pattern: String::new(),
            fuzzy_search_active: false,
            selected_index: 0,
            editing_db_search: false,
            editing_fuzzy_search: false,
        }
    }
}

impl App {
    pub fn current_search_state(&self) -> Option<&TabSearchState> {
        self.tab_search_state.get(&self.current_tab_key())
    }

    pub(super) fn current_search_state_mut(&mut self) -> &mut TabSearchState {
        let key = self.current_tab_key();
        // Build the config up-front: build_search_config borrows &self, which
        // can't co-exist with the &mut self.tab_search_state that entry() needs.
        // The config is cheap (a handful of small allocations), so paying for it
        // on each hit is fine.
        let config = self.build_search_config(key);
        self.tab_search_state
            .entry(key)
            .or_insert_with(|| TabSearchState::new(config))
    }

    /// Build SearchConfig for a given TabKey. The category filter is omitted
    /// on tabs whose rows have no category (Uncategorised) or where the
    /// filter is meaningless (raw transfer lists).
    pub(super) fn build_search_config(&self, key: TabKey) -> SearchConfig {
        if matches!(key, (Tab::Categories | Tab::Accounts | Tab::Filters, _)) {
            return SearchConfig::new(Vec::new());
        }

        let account_options: Vec<String> = self
            .accounts
            .values()
            .filter_map(|a| {
                self.banks
                    .get(&a.bank_id)
                    .map(|b| format!("{}/{}", b.name, a.name))
            })
            .collect();

        let with_category = matches!(
            key,
            (Tab::Transactions, _) | (Tab::Todo, Some(TodoSubTab::AiReview))
        );
        let category_options = with_category.then(|| {
            self.lists
                .categories
                .iter()
                .map(|c| c.path.clone())
                .collect()
        });

        SearchConfig::standard(account_options, category_options, self.search_options)
    }

    pub(super) fn rebuild_search_configs(&mut self) {
        let keys: Vec<TabKey> = self.tab_search_state.keys().copied().collect();
        for key in keys {
            let config = self.build_search_config(key);
            if let Some(state) = self.tab_search_state.get_mut(&key) {
                state.search_bar.set_config(config);
            }
        }
        if self.filter_edit.is_some() {
            let config = self.build_search_config((Tab::Transactions, None));
            if let Some(state) = self.filter_edit.as_mut() {
                state.search_bar.set_config(config);
            }
        }
    }

    // ==================== DB Search ====================

    pub fn db_search_active(&self) -> bool {
        self.current_search_state()
            .map(|s| s.db_search_active)
            .unwrap_or(false)
    }

    pub fn fuzzy_search_active(&self) -> bool {
        self.current_search_state()
            .map(|s| s.fuzzy_search_active)
            .unwrap_or(false)
    }

    pub fn start_db_search(&mut self) {
        self.input_mode = InputMode::DbSearch;
        self.current_search_state_mut().db_search_active = true;
    }

    pub fn handle_db_search_input(&mut self, req: tui_input::InputRequest) {
        let res = self.current_search_state_mut().search_bar.handle_input(req);

        // Check for transition to fuzzy search
        if res == KeyResult::TransitionToFuzzy {
            self.reload_current_tab();
            self.start_fuzzy_search();
        } else {
            self.reload_current_tab();
        }
        self.current_search_state_mut().selected_index = 0;
        self.selected_index = 0;
    }

    /// Clear the DB-search state without touching the input mode.
    fn clear_db_state(&mut self) {
        let state = self.current_search_state_mut();
        state.search_bar.reset();
        state.db_search_active = false;
        state.selected_index = 0;
        self.selected_index = 0;
        self.reload_current_tab();
    }

    pub fn clear_db_search(&mut self) {
        self.clear_db_state();
        self.input_mode = InputMode::Normal;
    }

    /// Clear whichever search is active from Normal mode, staying in Normal.
    /// Fuzzy clears first so a second Esc clears the DB search beneath it.
    pub fn clear_search(&mut self) {
        if self.fuzzy_search_active() {
            self.clear_fuzzy_state();
        } else if self.db_search_active() {
            self.clear_db_state();
        }
    }

    pub fn confirm_db_search(&mut self) {
        // If autocomplete is active, select from it instead of confirming
        let state = self.current_search_state_mut();
        if state.search_bar.autocomplete_active() {
            state.search_bar.autocomplete_select();
            self.reload_current_tab();
            self.selected_index = 0;
            return;
        }
        self.input_mode = InputMode::Normal;
    }

    pub fn db_search_value(&self) -> &str {
        self.current_search_state()
            .map(|s| s.search_bar.value())
            .unwrap_or("")
    }

    pub fn db_search_cursor(&self) -> usize {
        self.current_search_state()
            .map(|s| s.search_bar.cursor())
            .unwrap_or(0)
    }

    // ==================== Filter Autocomplete ====================

    pub fn filter_autocomplete_next(&mut self) {
        self.current_search_state_mut()
            .search_bar
            .autocomplete_next();
    }

    pub fn filter_autocomplete_prev(&mut self) {
        self.current_search_state_mut()
            .search_bar
            .autocomplete_prev();
    }

    pub fn filter_autocomplete_select(&mut self) -> bool {
        let state = self.current_search_state_mut();
        let selected = state.search_bar.autocomplete_select();
        if selected {
            self.reload_current_tab();
            self.selected_index = 0;
        }
        selected
    }

    pub fn filter_autocomplete_close(&mut self) {
        self.current_search_state_mut()
            .search_bar
            .autocomplete_close();
    }

    pub fn filter_autocomplete_active(&self) -> bool {
        self.current_search_state()
            .map(|s| s.search_bar.autocomplete_active())
            .unwrap_or(false)
    }

    // ==================== Fuzzy Search ====================

    pub fn start_fuzzy_search(&mut self) {
        self.input_mode = InputMode::FuzzySearch;
        self.current_search_state_mut().fuzzy_search_active = true;
    }

    pub fn handle_fuzzy_search_input(&mut self, req: tui_input::InputRequest) {
        let state = self.current_search_state_mut();
        state.fuzzy_search_input.handle(req);
        state.fuzzy_pattern = state.fuzzy_search_input.value().to_string();
        state.selected_index = 0;
        self.selected_index = 0;
        self.apply_fuzzy_filter();
    }

    /// Clear the fuzzy-search state without touching the input mode.
    fn clear_fuzzy_state(&mut self) {
        let state = self.current_search_state_mut();
        state.fuzzy_search_input.reset();
        state.fuzzy_pattern.clear();
        state.fuzzy_search_active = false;
        state.selected_index = 0;
        self.selected_index = 0;
        self.apply_fuzzy_filter();
    }

    pub fn clear_fuzzy_search(&mut self) {
        let db_search_active = self.db_search_active();
        self.clear_fuzzy_state();
        // Return to DB search mode if it's still active, else normal
        if db_search_active {
            self.input_mode = InputMode::DbSearch;
        } else {
            self.input_mode = InputMode::Normal;
        }
    }

    pub fn confirm_fuzzy_search(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn fuzzy_search_value(&self) -> &str {
        self.current_search_state()
            .map(|s| s.fuzzy_search_input.value())
            .unwrap_or("")
    }

    pub fn fuzzy_search_cursor(&self) -> usize {
        self.current_search_state()
            .map(|s| s.fuzzy_search_input.visual_cursor())
            .unwrap_or(0)
    }
}
