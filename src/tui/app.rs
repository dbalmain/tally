use std::collections::HashMap;

use tui_input::Input;

use crate::{
    Account, Bank, Category, CategorySource, FuzzyMatcher, Transaction, TransactionStore,
    TransactionWithEnrichment, Transfer, TransferSource, TransferWithTransactions,
    search::{
        AccountFilter, AmountFilter, CategoryFilter, DateFilter, Filter, ParsedQuery, SearchConfig,
    },
};

use super::filtered_list::FilteredList;
use super::search_bar::{KeyResult, SearchBar};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Transactions,
    Transfers,
    Categories,
    Todo,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[
            Tab::Transactions,
            Tab::Transfers,
            Tab::Categories,
            Tab::Todo,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Transactions => "Transactions",
            Tab::Transfers => "Transfers",
            Tab::Categories => "Categories",
            Tab::Todo => "Todo",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TodoSubTab {
    Uncategorised,
    AiReview,
    TransferReview,
}

/// Key for per-tab search state storage. The subtab is only meaningful on the
/// Todo tab (where each subtab gets its own state); all other tabs use `None`
/// so that switching the Todo subtab while on, say, Transactions doesn't
/// silently fork the saved search state.
pub type TabKey = (Tab, Option<TodoSubTab>);

fn tab_key(tab: Tab, subtab: TodoSubTab) -> TabKey {
    match tab {
        Tab::Todo => (Tab::Todo, Some(subtab)),
        other => (other, None),
    }
}

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

impl TodoSubTab {
    pub fn all() -> &'static [TodoSubTab] {
        &[
            TodoSubTab::Uncategorised,
            TodoSubTab::AiReview,
            TodoSubTab::TransferReview,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            TodoSubTab::Uncategorised => "Uncategorised",
            TodoSubTab::AiReview => "AI Review",
            TodoSubTab::TransferReview => "Transfer Review",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    DbSearch,
    FuzzySearch,
    Category,
    CategoryEdit,
    ConfirmMerge,
    TransferPending,
    TransferNoMatch,
}

pub struct App {
    pub store: TransactionStore,
    pub current_tab: Tab,
    pub todo_subtab: TodoSubTab,
    pub transactions: FilteredList<Transaction>,
    pub selected_index: usize,
    pub input_mode: InputMode,
    pub category_input: String,
    pub category_suggestions: Vec<Category>,
    pub category_selected: usize,
    pub pending_transfer_tx: Option<Transaction>,
    pub transfer_candidates: Vec<Transaction>,
    pub linked_transfers: FilteredList<TransferWithTransactions>,
    pub uncategorised: FilteredList<Transaction>,
    pub ai_reviews: FilteredList<TransactionWithEnrichment>,
    pub transfer_reviews: FilteredList<Transfer>,
    pub error_message: Option<String>,
    pub banks: HashMap<i64, Bank>,
    pub accounts: HashMap<i64, Account>,
    pub fuzzy_matcher: FuzzyMatcher,
    // Caches to avoid DB queries during render/filter
    tx_by_id: HashMap<i64, Transaction>,
    category_by_tx_id: HashMap<i64, String>,
    transfer_by_tx_id: HashMap<i64, Transfer>,
    category_tx_count: HashMap<i64, usize>,
    // Categories tab
    pub categories: Vec<Category>,
    // Category editing state
    pub editing_category: Option<Category>,
    pub category_edit_input: Input,
    // Confirmation popup state
    pub confirm_message: Option<String>,
    pub confirm_action: Option<ConfirmAction>,
    // Per-tab search state
    tab_search_state: HashMap<TabKey, TabSearchState>,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    MergeCategory { source_id: i64, target_id: i64 },
}

impl App {
    pub fn new(store: TransactionStore) -> Self {
        let empty_query = ParsedQuery::empty();
        let transactions = FilteredList::new(
            store
                .query_transactions(&empty_query, Some(500))
                .unwrap_or_default(),
        );
        let uncategorised = FilteredList::new(
            store
                .get_uncategorised_transactions(&empty_query, Some(500))
                .unwrap_or_default(),
        );
        let ai_reviews = FilteredList::new(
            store
                .get_pending_ai_reviews(&empty_query, Some(500))
                .unwrap_or_default(),
        );
        let transfer_reviews = FilteredList::new(
            store
                .get_pending_transfer_reviews(&empty_query, Some(500))
                .unwrap_or_default(),
        );
        let linked_transfers = FilteredList::new(
            store
                .list_transfers_with_transactions(true, &empty_query, Some(500))
                .unwrap_or_default(),
        );

        let bank_list = store.list_banks().unwrap_or_default();
        let banks: HashMap<i64, Bank> = bank_list.iter().cloned().map(|b| (b.id, b)).collect();

        let mut accounts = HashMap::new();
        for bank in &bank_list {
            for account in store.list_accounts(bank.id).unwrap_or_default() {
                accounts.insert(account.id, account);
            }
        }

        let categories = store.list_categories().unwrap_or_default();

        let mut app = Self {
            transactions,
            linked_transfers,
            uncategorised,
            ai_reviews,
            transfer_reviews,
            store,
            current_tab: Tab::Todo,
            todo_subtab: TodoSubTab::Uncategorised,
            selected_index: 0,
            input_mode: InputMode::Normal,
            category_input: String::new(),
            category_suggestions: Vec::new(),
            category_selected: 0,
            pending_transfer_tx: None,
            transfer_candidates: Vec::new(),
            error_message: None,
            banks,
            accounts,
            fuzzy_matcher: FuzzyMatcher::new(),
            tx_by_id: HashMap::new(),
            category_by_tx_id: HashMap::new(),
            transfer_by_tx_id: HashMap::new(),
            category_tx_count: HashMap::new(),
            categories,
            editing_category: None,
            category_edit_input: Input::default(),
            confirm_message: None,
            confirm_action: None,
            tab_search_state: HashMap::new(),
        };
        app.rebuild_tx_caches();
        app.rebuild_category_counts();
        app
    }

    fn current_tab_key(&self) -> TabKey {
        tab_key(self.current_tab, self.todo_subtab)
    }

    pub fn current_search_state(&self) -> Option<&TabSearchState> {
        self.tab_search_state.get(&self.current_tab_key())
    }

    fn current_search_state_mut(&mut self) -> &mut TabSearchState {
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

    /// The standard filter set used by every transaction-oriented tab. The
    /// category filter is omitted on tabs whose rows have no category
    /// (Uncategorised) or where the filter is meaningless (raw transfer lists).
    fn standard_filters(&self, with_category: bool) -> Vec<Box<dyn Filter>> {
        let account_options: Vec<String> = self
            .accounts
            .values()
            .filter_map(|a| {
                self.banks
                    .get(&a.bank_id)
                    .map(|b| format!("{}/{}", b.name, a.name))
            })
            .collect();

        let mut filters: Vec<Box<dyn Filter>> = vec![
            Box::new(DateFilter),
            Box::new(AmountFilter),
            Box::new(AccountFilter::with_options(account_options)),
        ];
        if with_category {
            let category_options: Vec<String> =
                self.categories.iter().map(|c| c.path.clone()).collect();
            filters.push(Box::new(CategoryFilter::with_options(category_options)));
        }
        filters
    }

    /// Build SearchConfig for a given TabKey.
    fn build_search_config(&self, key: TabKey) -> SearchConfig {
        if matches!(key, (Tab::Categories, _)) {
            return SearchConfig::new(Vec::new());
        }
        let with_category = matches!(
            key,
            (Tab::Transactions, _) | (Tab::Todo, Some(TodoSubTab::AiReview))
        );
        SearchConfig::new(self.standard_filters(with_category))
    }

    fn rebuild_search_configs(&mut self) {
        let keys: Vec<TabKey> = self.tab_search_state.keys().copied().collect();
        for key in keys {
            let config = self.build_search_config(key);
            if let Some(state) = self.tab_search_state.get_mut(&key) {
                state.search_bar.set_config(config);
            }
        }
    }

    /// Rebuild the per-transaction caches (`tx_by_id`, `category_by_tx_id`,
    /// `transfer_by_tx_id`) from currently-loaded list contents. Cheap: no
    /// per-row DB queries — at most one bulk lookup plus a few singletons for
    /// transfer-review sides that aren't already in the loaded data.
    fn rebuild_tx_caches(&mut self) {
        self.tx_by_id.clear();
        for tx in self.transactions.items() {
            self.tx_by_id.insert(tx.id, tx.clone());
        }
        for tx in self.uncategorised.items() {
            self.tx_by_id.entry(tx.id).or_insert_with(|| tx.clone());
        }
        for review in self.ai_reviews.items() {
            self.tx_by_id
                .entry(review.transaction.id)
                .or_insert_with(|| review.transaction.clone());
        }
        for twt in self.linked_transfers.items() {
            self.tx_by_id
                .entry(twt.from_transaction.id)
                .or_insert_with(|| twt.from_transaction.clone());
            self.tx_by_id
                .entry(twt.to_transaction.id)
                .or_insert_with(|| twt.to_transaction.clone());
        }
        // Load transactions for pending transfer reviews (they only have IDs)
        for tr in self.transfer_reviews.items() {
            if !self.tx_by_id.contains_key(&tr.from_transaction_id)
                && let Ok(Some(tx)) = self.store.get_transaction_by_id(tr.from_transaction_id)
            {
                self.tx_by_id.insert(tr.from_transaction_id, tx);
            }
            if !self.tx_by_id.contains_key(&tr.to_transaction_id)
                && let Ok(Some(tx)) = self.store.get_transaction_by_id(tr.to_transaction_id)
            {
                self.tx_by_id.insert(tr.to_transaction_id, tx);
            }
        }

        self.category_by_tx_id.clear();
        let tx_ids: Vec<i64> = self.transactions.items().iter().map(|t| t.id).collect();
        if let Ok(categories) = self.store.get_categories_for_transactions(&tx_ids) {
            self.category_by_tx_id = categories;
        }

        self.transfer_by_tx_id.clear();
        for twt in self.linked_transfers.items() {
            self.transfer_by_tx_id
                .insert(twt.from_transaction.id, twt.transfer.clone());
            self.transfer_by_tx_id
                .insert(twt.to_transaction.id, twt.transfer.clone());
        }
        for tr in self.transfer_reviews.items() {
            self.transfer_by_tx_id
                .entry(tr.from_transaction_id)
                .or_insert_with(|| tr.clone());
            self.transfer_by_tx_id
                .entry(tr.to_transaction_id)
                .or_insert_with(|| tr.clone());
        }
    }

    /// Rebuild the per-category transaction count cache in one bulk query.
    /// Only needs to run when category assignments change.
    fn rebuild_category_counts(&mut self) {
        self.category_tx_count = self
            .store
            .get_category_transaction_counts()
            .unwrap_or_default();
    }

    pub fn get_cached_transaction(&self, id: i64) -> Option<&Transaction> {
        self.tx_by_id.get(&id)
    }

    pub fn get_cached_category(&self, tx_id: i64) -> Option<&str> {
        self.category_by_tx_id.get(&tx_id).map(|s| s.as_str())
    }

    pub fn get_cached_transfer(&self, tx_id: i64) -> Option<&Transfer> {
        self.transfer_by_tx_id.get(&tx_id)
    }

    pub fn bank_name(&self, bank_id: i64) -> &str {
        self.banks
            .get(&bank_id)
            .map(|b| b.name.as_str())
            .unwrap_or("Unknown")
    }

    pub fn account_name(&self, account_id: i64) -> &str {
        self.accounts
            .get(&account_id)
            .map(|a| a.name.as_str())
            .unwrap_or("Unknown")
    }

    pub fn next_tab(&mut self) {
        self.save_tab_state();
        let tabs = Tab::all();
        let current_idx = tabs
            .iter()
            .position(|&t| t == self.current_tab)
            .unwrap_or(0);
        self.current_tab = tabs[(current_idx + 1) % tabs.len()];
        self.restore_tab_state();
        self.clear_transfer_mode();
    }

    pub fn previous_tab(&mut self) {
        self.save_tab_state();
        let tabs = Tab::all();
        let current_idx = tabs
            .iter()
            .position(|&t| t == self.current_tab)
            .unwrap_or(0);
        self.current_tab = tabs[(current_idx + tabs.len() - 1) % tabs.len()];
        self.restore_tab_state();
        self.clear_transfer_mode();
    }

    pub fn next_subtab(&mut self) {
        if self.current_tab != Tab::Todo {
            return;
        }
        self.save_tab_state();
        let subtabs = TodoSubTab::all();
        let current_idx = subtabs
            .iter()
            .position(|&t| t == self.todo_subtab)
            .unwrap_or(0);
        self.todo_subtab = subtabs[(current_idx + 1) % subtabs.len()];
        self.restore_tab_state();
    }

    pub fn previous_subtab(&mut self) {
        if self.current_tab != Tab::Todo {
            return;
        }
        self.save_tab_state();
        let subtabs = TodoSubTab::all();
        let current_idx = subtabs
            .iter()
            .position(|&t| t == self.todo_subtab)
            .unwrap_or(0);
        self.todo_subtab = subtabs[(current_idx + subtabs.len() - 1) % subtabs.len()];
        self.restore_tab_state();
    }

    /// Save current state to the tab's search state before switching away
    fn save_tab_state(&mut self) {
        let selected_index = self.selected_index;
        let editing_db = self.input_mode == InputMode::DbSearch;
        let editing_fuzzy = self.input_mode == InputMode::FuzzySearch;
        let state = self.current_search_state_mut();
        state.selected_index = selected_index;
        state.editing_db_search = editing_db;
        state.editing_fuzzy_search = editing_fuzzy;
    }

    /// Restore state from the new tab's search state
    fn restore_tab_state(&mut self) {
        // Extract values before mutating self
        let (selected_index, editing_fuzzy, editing_db) = self
            .current_search_state()
            .map(|s| {
                (
                    s.selected_index,
                    s.editing_fuzzy_search,
                    s.editing_db_search,
                )
            })
            .unwrap_or((0, false, false));

        self.selected_index = selected_index;

        // Restore input mode based on what we were doing when we left this tab
        if editing_fuzzy {
            self.input_mode = InputMode::FuzzySearch;
        } else if editing_db {
            self.input_mode = InputMode::DbSearch;
        } else {
            self.input_mode = InputMode::Normal;
        }

        // Reload data for this tab based on its search state
        self.reload_current_tab();
    }

    pub fn next(&mut self) {
        let len = self.list_len();
        if len > 0 {
            if self.input_mode == InputMode::TransferPending && !self.transfer_candidates.is_empty()
            {
                let current_tx_id = self
                    .get_current_transaction(self.selected_index)
                    .map(|t| t.id);
                let current_pos = self
                    .transfer_candidates
                    .iter()
                    .position(|c| current_tx_id == Some(c.id))
                    .unwrap_or(0);
                if current_pos + 1 < self.transfer_candidates.len() {
                    let next_candidate_id = self.transfer_candidates[current_pos + 1].id;
                    if let Some(pos) = self.find_filtered_position_by_tx_id(next_candidate_id) {
                        self.selected_index = pos;
                    }
                }
            } else {
                self.selected_index = (self.selected_index + 1) % len;
            }
        }
    }

    pub fn previous(&mut self) {
        let len = self.list_len();
        if len > 0 {
            if self.input_mode == InputMode::TransferPending && !self.transfer_candidates.is_empty()
            {
                let current_tx_id = self
                    .get_current_transaction(self.selected_index)
                    .map(|t| t.id);
                let current_pos = self
                    .transfer_candidates
                    .iter()
                    .position(|c| current_tx_id == Some(c.id))
                    .unwrap_or(0);
                if current_pos > 0 {
                    let prev_candidate_id = self.transfer_candidates[current_pos - 1].id;
                    if let Some(pos) = self.find_filtered_position_by_tx_id(prev_candidate_id) {
                        self.selected_index = pos;
                    }
                }
            } else {
                self.selected_index = (self.selected_index + len - 1) % len;
            }
        }
    }

    fn list_len(&self) -> usize {
        match self.current_tab {
            Tab::Transactions => self.transactions.len(),
            Tab::Transfers => self.linked_transfers.len(),
            Tab::Categories => self.categories.len(),
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => self.uncategorised.len(),
                TodoSubTab::AiReview => self.ai_reviews.len(),
                TodoSubTab::TransferReview => self.transfer_reviews.len(),
            },
        }
    }

    fn get_current_transaction(&self, filtered_idx: usize) -> Option<&Transaction> {
        match self.current_tab {
            Tab::Transactions => self.transactions.get(filtered_idx),
            Tab::Transfers => None,
            Tab::Categories => None,
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => self.uncategorised.get(filtered_idx),
                TodoSubTab::AiReview => None,
                TodoSubTab::TransferReview => None,
            },
        }
    }

    fn find_filtered_position_by_tx_id(&self, tx_id: i64) -> Option<usize> {
        match self.current_tab {
            Tab::Transactions => self.transactions.position(|tx| tx.id == tx_id),
            Tab::Transfers => None,
            Tab::Categories => None,
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => self.uncategorised.position(|tx| tx.id == tx_id),
                TodoSubTab::AiReview => None,
                TodoSubTab::TransferReview => None,
            },
        }
    }

    pub fn selected_transaction(&self) -> Option<&Transaction> {
        self.get_current_transaction(self.selected_index)
    }

    pub fn start_category_edit(&mut self) {
        if self.selected_transaction().is_some() {
            self.input_mode = InputMode::Category;
            self.category_input.clear();
            self.category_suggestions = self.store.list_categories().unwrap_or_default();
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
            self.category_suggestions = self.store.list_categories().unwrap_or_default();
        } else {
            self.category_suggestions = self
                .store
                .find_categories(&self.category_input)
                .unwrap_or_default();
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

        if let Ok(category_id) = self.store.get_or_create_category(&category_path) {
            let _ = self
                .store
                .set_category(tx.id, category_id, CategorySource::Manual, true, None);
            self.refresh_data();
        }

        self.cancel_input();
    }

    pub fn start_transfer_mark(&mut self) {
        let Some(tx) = self.selected_transaction().cloned() else {
            return;
        };

        let candidates = self
            .store
            .find_matching_transfer_candidates(&tx)
            .unwrap_or_default();

        if candidates.is_empty() {
            self.input_mode = InputMode::TransferNoMatch;
            self.pending_transfer_tx = Some(tx);
            self.transfer_candidates = Vec::new();
        } else {
            self.pending_transfer_tx = Some(tx);
            let first_id = candidates.first().map(|c| c.id);
            self.transfer_candidates = candidates;
            self.input_mode = InputMode::TransferPending;
            if let Some(first_id) = first_id
                && let Some(pos) = self.find_filtered_position_by_tx_id(first_id)
            {
                self.selected_index = pos;
            }
        }
    }

    pub fn complete_transfer(&mut self) {
        let Some(from_tx) = self.pending_transfer_tx.take() else {
            return;
        };
        let Some(to_tx) = self.selected_transaction().cloned() else {
            return;
        };

        if from_tx.amount_cents != -to_tx.amount_cents {
            self.error_message = Some("Amounts don't match".to_string());
            self.clear_transfer_mode();
            return;
        }

        let (from_id, to_id) = if from_tx.amount_cents < 0 {
            (from_tx.id, to_tx.id)
        } else {
            (to_tx.id, from_tx.id)
        };

        if self
            .store
            .create_transfer(from_id, to_id, TransferSource::Manual, true)
            .is_ok()
        {
            self.refresh_data();
        }

        self.clear_transfer_mode();
    }

    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.category_input.clear();
        self.category_suggestions.clear();
        self.category_selected = 0;
        self.error_message = None;
        self.clear_transfer_mode();
        self.clear_category_edit();
        self.clear_confirm();
    }

    fn clear_transfer_mode(&mut self) {
        self.pending_transfer_tx = None;
        self.transfer_candidates.clear();
        if self.input_mode == InputMode::TransferPending
            || self.input_mode == InputMode::TransferNoMatch
        {
            self.input_mode = InputMode::Normal;
        }
    }

    fn clear_category_edit(&mut self) {
        self.editing_category = None;
        self.category_edit_input.reset();
    }

    fn clear_confirm(&mut self) {
        self.confirm_message = None;
        self.confirm_action = None;
    }

    pub fn refresh_data(&mut self) {
        // Reload the current tab's data using its search state. Called after
        // mutations (categorisation, transfers) — both tx caches and category
        // counts may have changed.
        self.categories = self.store.list_categories().unwrap_or_default();
        self.rebuild_search_configs();
        self.reload_current_tab();
        self.rebuild_category_counts();
    }

    // ==================== Category Editing (Categories Tab) ====================

    pub fn selected_category(&self) -> Option<&Category> {
        if self.current_tab == Tab::Categories {
            self.categories.get(self.selected_index)
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

    fn reload_categories(&mut self) {
        self.categories = self.store.list_categories().unwrap_or_default();
        self.rebuild_search_configs();
        self.rebuild_category_counts();
    }

    pub fn category_transaction_count(&self, category_id: i64) -> usize {
        self.category_tx_count
            .get(&category_id)
            .copied()
            .unwrap_or(0)
    }

    fn move_cursor_to_category(&mut self, path: &str) {
        if let Some(pos) = self.categories.iter().position(|c| c.path == path) {
            self.selected_index = pos;
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

    pub fn clear_db_search(&mut self) {
        let state = self.current_search_state_mut();
        state.search_bar.reset();
        state.db_search_active = false;
        state.selected_index = 0;
        self.selected_index = 0;
        self.reload_current_tab();
        self.input_mode = InputMode::Normal;
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

    pub fn clear_fuzzy_search(&mut self) {
        let db_search_active = self.db_search_active();
        let state = self.current_search_state_mut();
        state.fuzzy_search_input.reset();
        state.fuzzy_pattern.clear();
        state.fuzzy_search_active = false;
        state.selected_index = 0;
        self.selected_index = 0;
        self.apply_fuzzy_filter();
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

    // ==================== Filtering Logic ====================

    /// Reload only the current tab's data from DB based on its search query
    fn reload_current_tab(&mut self) {
        let parsed = self
            .current_search_state()
            .map(|s| s.search_bar.parsed().clone())
            .unwrap_or_default();
        let limit = Some(500);

        match self.current_tab {
            Tab::Transactions => {
                self.transactions.set_items(
                    self.store
                        .query_transactions(&parsed, limit)
                        .unwrap_or_default(),
                );
            }
            Tab::Transfers => {
                self.linked_transfers.set_items(
                    self.store
                        .list_transfers_with_transactions(true, &parsed, limit)
                        .unwrap_or_default(),
                );
            }
            Tab::Categories => {
                // Categories don't use transaction filters
            }
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => {
                    self.uncategorised.set_items(
                        self.store
                            .get_uncategorised_transactions(&parsed, limit)
                            .unwrap_or_default(),
                    );
                }
                TodoSubTab::AiReview => {
                    self.ai_reviews.set_items(
                        self.store
                            .get_pending_ai_reviews(&parsed, limit)
                            .unwrap_or_default(),
                    );
                }
                TodoSubTab::TransferReview => {
                    self.transfer_reviews.set_items(
                        self.store
                            .get_pending_transfer_reviews(&parsed, limit)
                            .unwrap_or_default(),
                    );
                }
            },
        }
        self.rebuild_tx_caches();
        self.apply_fuzzy_filter();
    }

    /// Apply fuzzy filter on top of loaded data for current tab only
    fn apply_fuzzy_filter(&mut self) {
        let pattern = self
            .current_search_state()
            .map(|s| s.fuzzy_pattern.clone())
            .unwrap_or_default();

        let matcher = &mut self.fuzzy_matcher;
        let mut any_match =
            |fields: &[&str]| -> bool { fields.iter().any(|f| matcher.fuzzy_matches(&pattern, f)) };

        match self.current_tab {
            Tab::Transactions => {
                if pattern.is_empty() {
                    self.transactions.show_all();
                } else {
                    self.transactions
                        .refilter(|tx| any_match(&[&tx.description]));
                }
            }
            Tab::Transfers => {
                if pattern.is_empty() {
                    self.linked_transfers.show_all();
                } else {
                    self.linked_transfers.refilter(|twt| {
                        any_match(&[
                            &twt.from_transaction.description,
                            &twt.to_transaction.description,
                        ])
                    });
                }
            }
            Tab::Categories => {
                // Categories don't use fuzzy filtering (for now)
            }
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => {
                    if pattern.is_empty() {
                        self.uncategorised.show_all();
                    } else {
                        self.uncategorised
                            .refilter(|tx| any_match(&[&tx.description]));
                    }
                }
                TodoSubTab::AiReview => {
                    if pattern.is_empty() {
                        self.ai_reviews.show_all();
                    } else {
                        self.ai_reviews
                            .refilter(|r| any_match(&[&r.transaction.description]));
                    }
                }
                TodoSubTab::TransferReview => {
                    if pattern.is_empty() {
                        self.transfer_reviews.show_all();
                    } else {
                        let tx_by_id = &self.tx_by_id;
                        self.transfer_reviews.refilter(|tr| {
                            match (
                                tx_by_id.get(&tr.from_transaction_id),
                                tx_by_id.get(&tr.to_transaction_id),
                            ) {
                                (Some(from), Some(to)) => {
                                    any_match(&[&from.description, &to.description])
                                }
                                // If we couldn't load the referenced transactions, keep the
                                // entry visible — hiding it would silently drop work.
                                _ => true,
                            }
                        });
                    }
                }
            },
        }
    }

    pub fn is_transfer_candidate(&self, tx_id: i64) -> bool {
        self.transfer_candidates.iter().any(|c| c.id == tx_id)
    }

    pub fn is_pending_transfer_tx(&self, tx_id: i64) -> bool {
        self.pending_transfer_tx
            .as_ref()
            .is_some_and(|t| t.id == tx_id)
    }

    pub fn confirm_ai_category(&mut self) {
        if self.current_tab != Tab::Todo || self.todo_subtab != TodoSubTab::AiReview {
            return;
        }
        if let Some(review) = self.ai_reviews.get(self.selected_index) {
            let tx_id = review.transaction.id;
            let _ = self.store.confirm_category(tx_id);
            self.refresh_data();
        }
    }

    pub fn confirm_transfer_review(&mut self) {
        if self.current_tab != Tab::Todo || self.todo_subtab != TodoSubTab::TransferReview {
            return;
        }
        if let Some(transfer) = self.transfer_reviews.get(self.selected_index) {
            let _ = self.store.confirm_transfer(transfer.id);
            self.refresh_data();
        }
    }

    pub fn delete_transfer(&mut self) {
        if self.current_tab != Tab::Transfers {
            return;
        }
        let Some(transfer_id) = self
            .linked_transfers
            .get(self.selected_index)
            .map(|twt| twt.transfer.id)
        else {
            return;
        };
        let _ = self.store.delete_transfer(transfer_id);
        self.refresh_data();
        if self.selected_index >= self.linked_transfers.len() && self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }
}
