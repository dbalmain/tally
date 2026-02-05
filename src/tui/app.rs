use std::collections::HashMap;

use tui_input::{Input, InputRequest};

use crate::{
    Account, Bank, Category, CategorySource, DbSearchQuery, FuzzyMatcher, Transaction,
    TransactionFilter, TransactionStore, TransactionWithEnrichment, Transfer, TransferSource,
    TransferWithTransactions,
};

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

/// Key for per-tab search state storage.
/// Todo subtabs each get their own state; other tabs use None.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabKey {
    Transactions,
    Transfers,
    Categories,
    TodoUncategorised,
    TodoAiReview,
    TodoTransferReview,
}

impl TabKey {
    pub fn from_tab(tab: Tab, subtab: TodoSubTab) -> Self {
        match tab {
            Tab::Transactions => TabKey::Transactions,
            Tab::Transfers => TabKey::Transfers,
            Tab::Categories => TabKey::Categories,
            Tab::Todo => match subtab {
                TodoSubTab::Uncategorised => TabKey::TodoUncategorised,
                TodoSubTab::AiReview => TabKey::TodoAiReview,
                TodoSubTab::TransferReview => TabKey::TodoTransferReview,
            },
        }
    }
}

/// Per-tab search state (DB search + fuzzy search + selection).
#[derive(Default)]
pub struct TabSearchState {
    pub db_search_input: Input,
    pub db_search_query: DbSearchQuery,
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

/// Token type for filter reordering logic
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReorderTokenType {
    Filter,
    Regex,
    Fts,
}

/// Token info for filter reordering
#[derive(Debug, Clone)]
struct ReorderToken {
    start: usize,
    end: usize,
    token_type: ReorderTokenType,
}

pub struct App {
    pub store: TransactionStore,
    pub current_tab: Tab,
    pub todo_subtab: TodoSubTab,
    pub transactions: Vec<Transaction>,
    pub selected_index: usize,
    pub input_mode: InputMode,
    pub category_input: String,
    pub category_suggestions: Vec<Category>,
    pub category_selected: usize,
    pub pending_transfer_tx: Option<Transaction>,
    pub transfer_candidates: Vec<Transaction>,
    pub linked_transfers: Vec<TransferWithTransactions>,
    pub uncategorised: Vec<Transaction>,
    pub ai_reviews: Vec<TransactionWithEnrichment>,
    pub transfer_reviews: Vec<Transfer>,
    pub error_message: Option<String>,
    pub banks: HashMap<i64, Bank>,
    pub accounts: HashMap<i64, Account>,
    pub fuzzy_matcher: FuzzyMatcher,
    filtered_transaction_idx: Vec<usize>,
    filtered_transfer_idx: Vec<usize>,
    filtered_uncategorised_idx: Vec<usize>,
    filtered_ai_review_idx: Vec<usize>,
    filtered_transfer_review_idx: Vec<usize>,
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
    pub fn filtered_transactions(&self) -> impl Iterator<Item = &Transaction> {
        self.filtered_transaction_idx
            .iter()
            .filter_map(|&i| self.transactions.get(i))
    }

    pub fn filtered_transfers(&self) -> impl Iterator<Item = &TransferWithTransactions> {
        self.filtered_transfer_idx
            .iter()
            .filter_map(|&i| self.linked_transfers.get(i))
    }

    pub fn filtered_uncategorised(&self) -> impl Iterator<Item = &Transaction> {
        self.filtered_uncategorised_idx
            .iter()
            .filter_map(|&i| self.uncategorised.get(i))
    }

    pub fn filtered_ai_reviews(&self) -> impl Iterator<Item = &TransactionWithEnrichment> {
        self.filtered_ai_review_idx
            .iter()
            .filter_map(|&i| self.ai_reviews.get(i))
    }

    pub fn filtered_transfer_reviews(&self) -> impl Iterator<Item = &Transfer> {
        self.filtered_transfer_review_idx
            .iter()
            .filter_map(|&i| self.transfer_reviews.get(i))
    }

    pub fn filtered_transactions_len(&self) -> usize {
        self.filtered_transaction_idx.len()
    }

    pub fn filtered_transfers_len(&self) -> usize {
        self.filtered_transfer_idx.len()
    }

    pub fn filtered_uncategorised_len(&self) -> usize {
        self.filtered_uncategorised_idx.len()
    }

    pub fn filtered_ai_reviews_len(&self) -> usize {
        self.filtered_ai_review_idx.len()
    }

    pub fn filtered_transfer_reviews_len(&self) -> usize {
        self.filtered_transfer_review_idx.len()
    }

    pub fn get_filtered_transaction(&self, filtered_idx: usize) -> Option<&Transaction> {
        self.filtered_transaction_idx
            .get(filtered_idx)
            .and_then(|&i| self.transactions.get(i))
    }

    pub fn get_filtered_transfer(&self, filtered_idx: usize) -> Option<&TransferWithTransactions> {
        self.filtered_transfer_idx
            .get(filtered_idx)
            .and_then(|&i| self.linked_transfers.get(i))
    }

    pub fn get_filtered_uncategorised(&self, filtered_idx: usize) -> Option<&Transaction> {
        self.filtered_uncategorised_idx
            .get(filtered_idx)
            .and_then(|&i| self.uncategorised.get(i))
    }

    pub fn get_filtered_ai_review(
        &self,
        filtered_idx: usize,
    ) -> Option<&TransactionWithEnrichment> {
        self.filtered_ai_review_idx
            .get(filtered_idx)
            .and_then(|&i| self.ai_reviews.get(i))
    }

    pub fn get_filtered_transfer_review(&self, filtered_idx: usize) -> Option<&Transfer> {
        self.filtered_transfer_review_idx
            .get(filtered_idx)
            .and_then(|&i| self.transfer_reviews.get(i))
    }
}

impl App {
    pub fn new(store: TransactionStore) -> Self {
        let transactions = store
            .query_transactions(&TransactionFilter {
                limit: Some(500),
                ..Default::default()
            })
            .unwrap_or_default();

        let default_filter = TransactionFilter {
            limit: Some(500),
            ..Default::default()
        };
        let uncategorised = store
            .get_uncategorised_transactions(&default_filter)
            .unwrap_or_default();
        let ai_reviews = store
            .get_pending_ai_reviews(&default_filter)
            .unwrap_or_default();
        let transfer_reviews = store
            .get_pending_transfer_reviews(&default_filter)
            .unwrap_or_default();
        let linked_transfers = store
            .list_transfers_with_transactions(true, &default_filter)
            .unwrap_or_default();

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
            filtered_transaction_idx: (0..transactions.len()).collect(),
            filtered_transfer_idx: (0..linked_transfers.len()).collect(),
            filtered_uncategorised_idx: (0..uncategorised.len()).collect(),
            filtered_ai_review_idx: (0..ai_reviews.len()).collect(),
            filtered_transfer_review_idx: (0..transfer_reviews.len()).collect(),
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
        app.rebuild_caches();
        app
    }

    fn current_tab_key(&self) -> TabKey {
        TabKey::from_tab(self.current_tab, self.todo_subtab)
    }

    fn current_search_state(&self) -> Option<&TabSearchState> {
        self.tab_search_state.get(&self.current_tab_key())
    }

    fn current_search_state_mut(&mut self) -> &mut TabSearchState {
        let key = self.current_tab_key();
        self.tab_search_state.entry(key).or_default()
    }

    fn rebuild_caches(&mut self) {
        // Build transaction lookup cache
        self.tx_by_id.clear();
        for tx in &self.transactions {
            self.tx_by_id.insert(tx.id, tx.clone());
        }
        for tx in &self.uncategorised {
            self.tx_by_id.entry(tx.id).or_insert_with(|| tx.clone());
        }
        for review in &self.ai_reviews {
            self.tx_by_id
                .entry(review.transaction.id)
                .or_insert_with(|| review.transaction.clone());
        }
        for twt in &self.linked_transfers {
            self.tx_by_id
                .entry(twt.from_transaction.id)
                .or_insert_with(|| twt.from_transaction.clone());
            self.tx_by_id
                .entry(twt.to_transaction.id)
                .or_insert_with(|| twt.to_transaction.clone());
        }
        // Load transactions for pending transfer reviews (they only have IDs)
        for tr in &self.transfer_reviews {
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

        // Build category cache for all transactions
        self.category_by_tx_id.clear();
        let tx_ids: Vec<i64> = self.transactions.iter().map(|t| t.id).collect();
        if let Ok(categories) = self.store.get_categories_for_transactions(&tx_ids) {
            self.category_by_tx_id = categories;
        }

        // Build transfer lookup cache
        self.transfer_by_tx_id.clear();
        for twt in &self.linked_transfers {
            self.transfer_by_tx_id
                .insert(twt.from_transaction.id, twt.transfer.clone());
            self.transfer_by_tx_id
                .insert(twt.to_transaction.id, twt.transfer.clone());
        }
        for tr in &self.transfer_reviews {
            self.transfer_by_tx_id
                .entry(tr.from_transaction_id)
                .or_insert_with(|| tr.clone());
            self.transfer_by_tx_id
                .entry(tr.to_transaction_id)
                .or_insert_with(|| tr.clone());
        }

        // Build category transaction count cache
        self.category_tx_count.clear();
        for cat in &self.categories {
            if let Ok(count) = self.store.count_transactions_in_category(cat.id) {
                self.category_tx_count.insert(cat.id, count);
            }
        }
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
        let key = self.current_tab_key();
        let state = self.tab_search_state.entry(key).or_default();
        state.selected_index = self.selected_index;
        state.editing_db_search = self.input_mode == InputMode::DbSearch;
        state.editing_fuzzy_search = self.input_mode == InputMode::FuzzySearch;
    }

    /// Restore state from the new tab's search state
    fn restore_tab_state(&mut self) {
        // Extract values before mutating self
        let (selected_index, editing_fuzzy, editing_db) = self
            .current_search_state()
            .map(|s| (s.selected_index, s.editing_fuzzy_search, s.editing_db_search))
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
            Tab::Transactions => self.filtered_transactions_len(),
            Tab::Transfers => self.filtered_transfers_len(),
            Tab::Categories => self.categories.len(),
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => self.filtered_uncategorised_len(),
                TodoSubTab::AiReview => self.filtered_ai_reviews_len(),
                TodoSubTab::TransferReview => self.filtered_transfer_reviews_len(),
            },
        }
    }

    fn current_transaction_indices(&self) -> &[usize] {
        match self.current_tab {
            Tab::Transactions => &self.filtered_transaction_idx,
            Tab::Transfers => &[],
            Tab::Categories => &[],
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => &self.filtered_uncategorised_idx,
                TodoSubTab::AiReview => &[],
                TodoSubTab::TransferReview => &[],
            },
        }
    }

    fn get_current_transaction(&self, filtered_idx: usize) -> Option<&Transaction> {
        match self.current_tab {
            Tab::Transactions => self.get_filtered_transaction(filtered_idx),
            Tab::Transfers => None,
            Tab::Categories => None,
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => self.get_filtered_uncategorised(filtered_idx),
                TodoSubTab::AiReview => None,
                TodoSubTab::TransferReview => None,
            },
        }
    }

    fn find_filtered_position_by_tx_id(&self, tx_id: i64) -> Option<usize> {
        let indices = self.current_transaction_indices();
        let base_list: &[Transaction] = match self.current_tab {
            Tab::Transactions => &self.transactions,
            Tab::Transfers => return None,
            Tab::Categories => return None,
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => &self.uncategorised,
                TodoSubTab::AiReview => return None,
                TodoSubTab::TransferReview => return None,
            },
        };
        indices
            .iter()
            .position(|&base_idx| base_list.get(base_idx).is_some_and(|tx| tx.id == tx_id))
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
        // Reload the current tab's data using its search state
        self.reload_current_tab();
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
        self.category_tx_count.clear();
        for cat in &self.categories {
            if let Ok(count) = self.store.count_transactions_in_category(cat.id) {
                self.category_tx_count.insert(cat.id, count);
            }
        }
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
        // Handle special regex behavior before standard input processing
        if self.handle_db_search_regex_input(&req) {
            // Regex handling consumed the input - also apply reordering
            self.reorder_filters_before_fts();

            // Re-parse and reload
            let state = self.current_search_state_mut();
            let cursor = state.db_search_input.cursor();
            let input_value = state.db_search_input.value().to_string();
            let (query, transition_to_fuzzy) =
                DbSearchQuery::parse_with_cursor(&input_value, Some(cursor));

            if transition_to_fuzzy {
                let trimmed = input_value[..input_value.len() - 2].to_string();
                let state = self.current_search_state_mut();
                state.db_search_input = Input::new(trimmed.clone());
                state.db_search_query = DbSearchQuery::parse(&trimmed).0;
                self.reload_current_tab();
                self.start_fuzzy_search();
            } else {
                let state = self.current_search_state_mut();
                state.db_search_query = query;
                self.reload_current_tab();
            }
            self.current_search_state_mut().selected_index = 0;
            self.selected_index = 0;
            return;
        }

        let state = self.current_search_state_mut();
        state.db_search_input.handle(req);

        // Expand shortcuts like d: -> date:, a: -> account:, etc.
        self.expand_db_search_shortcuts();

        // Jump to existing filter if typing a duplicate filter keyword
        self.deduplicate_db_search_filter();

        // Jump to existing regex if typing a second /
        self.deduplicate_db_search_regex();

        // Move filters before FTS text (filters must come before free text)
        self.reorder_filters_before_fts();

        let state = self.current_search_state_mut();
        let cursor = state.db_search_input.cursor();
        let input_value = state.db_search_input.value().to_string();
        let (query, transition_to_fuzzy) =
            DbSearchQuery::parse_with_cursor(&input_value, Some(cursor));

        if transition_to_fuzzy {
            // Remove " ~" from input and transition to fuzzy mode
            let trimmed = input_value[..input_value.len() - 2].to_string();
            let state = self.current_search_state_mut();
            state.db_search_input = Input::new(trimmed.clone());
            state.db_search_query = DbSearchQuery::parse(&trimmed).0;
            self.reload_current_tab();
            self.start_fuzzy_search();
        } else {
            let state = self.current_search_state_mut();
            state.db_search_query = query;
            self.reload_current_tab();
        }
        self.current_search_state_mut().selected_index = 0;
        self.selected_index = 0;
    }

    /// Expand filter shortcuts at cursor position:
    /// - `d:` → `date:`
    /// - `a:` → `account:`
    /// - `am:` → `amount:`
    /// - `c:` → `category:`
    fn expand_db_search_shortcuts(&mut self) {
        const SHORTCUTS: &[(&str, &str)] = &[
            ("am:", "amount:"),
            ("d:", "date:"),
            ("a:", "account:"),
            ("c:", "category:"),
        ];

        let state = self.current_search_state_mut();
        let cursor = state.db_search_input.cursor();
        let value = state.db_search_input.value();

        // Check if any shortcut ends at cursor position
        for (shortcut, expansion) in SHORTCUTS {
            if cursor >= shortcut.len() {
                let start = cursor - shortcut.len();
                // Only expand at word boundary (start of input or preceded by space)
                let at_word_start = start == 0
                    || value
                        .chars()
                        .nth(start - 1)
                        .map(|c| c.is_whitespace())
                        .unwrap_or(false);

                if at_word_start && value[start..cursor] == **shortcut {
                    // Build new value with expansion
                    let mut new_value = String::with_capacity(value.len() + expansion.len());
                    new_value.push_str(&value[..start]);
                    new_value.push_str(expansion);
                    new_value.push_str(&value[cursor..]);

                    // Replace input and position cursor at end of expansion
                    // Input::new puts cursor at end, so move back by the tail length
                    let tail_len = value.len() - cursor;
                    state.db_search_input = Input::new(new_value);
                    for _ in 0..tail_len {
                        state.db_search_input.handle(InputRequest::GoToPrevChar);
                    }
                    return;
                }
            }
        }
    }

    /// Deduplicate filter keywords: if a filter like `date:` is typed when one
    /// already exists, delete the new one and jump cursor to end of existing filter.
    /// Any value typed after the duplicate keyword is appended to the existing filter.
    ///
    /// Example: `date:2024 date:2025` → `date:20242025` with cursor at end
    fn deduplicate_db_search_filter(&mut self) {
        const FILTER_KEYWORDS: &[&str] = &["date:", "account:", "amount:", "category:"];

        let state = self.current_search_state_mut();
        let cursor = state.db_search_input.cursor();
        let value = state.db_search_input.value().to_string();

        // For each filter keyword, check if it appears twice and cursor is in the second one
        for keyword in FILTER_KEYWORDS {
            // Find all occurrences of this keyword
            let occurrences: Vec<usize> = value
                .match_indices(keyword)
                .filter(|(pos, _)| {
                    // Must be at word boundary (start of input or preceded by space)
                    *pos == 0
                        || value
                            .chars()
                            .nth(*pos - 1)
                            .map(|c| c.is_whitespace())
                            .unwrap_or(false)
                })
                .map(|(pos, _)| pos)
                .collect();

            if occurrences.len() < 2 {
                continue;
            }

            // Check if cursor is within or just after the second (or later) occurrence
            let first_pos = occurrences[0];
            for &dup_pos in &occurrences[1..] {
                let dup_end = dup_pos + keyword.len();
                // Cursor must be at or after the duplicate keyword
                if cursor >= dup_pos {
                    // Find the end of the first filter's value (next space or end of input)
                    let first_value_start = first_pos + keyword.len();
                    let first_value_end = value[first_value_start..]
                        .find(|c: char| c.is_whitespace())
                        .map(|i| first_value_start + i)
                        .unwrap_or(value.len());

                    // Find the end of the duplicate filter's value
                    let dup_value_start = dup_end;
                    let dup_value_end = value[dup_value_start..]
                        .find(|c: char| c.is_whitespace())
                        .map(|i| dup_value_start + i)
                        .unwrap_or(value.len());

                    // Only proceed if cursor is within the duplicate filter token
                    if cursor > dup_value_end {
                        continue;
                    }

                    // Extract value typed after duplicate keyword (portion before cursor)
                    let extra_value = if cursor > dup_value_start {
                        &value[dup_value_start..cursor]
                    } else {
                        ""
                    };

                    // Build new value:
                    // 1. Everything up to end of first filter's value
                    // 2. Append extra value from duplicate
                    // 3. Add middle content (between first filter and duplicate)
                    // 4. Add everything after the duplicate token
                    let after_dup = if dup_value_end < value.len() {
                        &value[dup_value_end..]
                    } else {
                        ""
                    };

                    // Insert extra value at end of first filter's value
                    let mut new_value = String::with_capacity(value.len());
                    new_value.push_str(&value[..first_value_end]);
                    new_value.push_str(extra_value);
                    // Add back the rest (between first filter end and duplicate start)
                    if first_value_end < dup_pos {
                        let middle = value[first_value_end..dup_pos].trim();
                        if !middle.is_empty() {
                            new_value.push(' ');
                            new_value.push_str(middle);
                        }
                    }
                    new_value.push_str(after_dup);

                    // Position cursor at end of first filter's value + extra
                    let new_cursor = first_value_end + extra_value.len();

                    // Replace input
                    let tail_len = new_value.len() - new_cursor;
                    let state = self.current_search_state_mut();
                    state.db_search_input = Input::new(new_value);
                    for _ in 0..tail_len {
                        state.db_search_input.handle(InputRequest::GoToPrevChar);
                    }
                    return;
                }
            }
        }
    }

    /// Handle regex deduplication: only one regex allowed in query.
    /// When typing `/` and a regex already exists, delete the new `/` and jump cursor
    /// to end of existing regex content (before closing `/` or flags).
    fn deduplicate_db_search_regex(&mut self) {
        let state = self.current_search_state_mut();
        let cursor = state.db_search_input.cursor();
        let value = state.db_search_input.value().to_string();

        // Find existing regex using proper parsing (handles escaped slashes, etc.)
        let Some((_regex_start, closing_slash, regex_end)) = self.find_regex_in_value(&value)
        else {
            return;
        };

        // Find `/` at word boundary OUTSIDE the existing regex
        let dup_slash = value
            .char_indices()
            .find(|(pos, c)| {
                *c == '/'
                    && *pos >= regex_end // Must be after the existing regex
                    && (*pos == 0
                        || value
                            .chars()
                            .nth(*pos - 1)
                            .map(|prev| prev.is_whitespace())
                            .unwrap_or(false))
            })
            .map(|(pos, _)| pos);

        let Some(dup_slash) = dup_slash else {
            return;
        };

        // Cursor must be at or just after this duplicate slash
        if cursor < dup_slash || cursor > dup_slash + 1 {
            return;
        }

        // Build new value: remove the duplicate slash
        let mut new_value = String::with_capacity(value.len() - 1);
        new_value.push_str(&value[..dup_slash]);
        if dup_slash + 1 < value.len() {
            new_value.push_str(&value[dup_slash + 1..]);
        }

        // Position cursor at end of first regex content (before closing /)
        let new_cursor = closing_slash.min(new_value.len());
        let tail_len = new_value.len() - new_cursor;

        let state = self.current_search_state_mut();
        state.db_search_input = Input::new(new_value);
        for _ in 0..tail_len {
            state.db_search_input.handle(InputRequest::GoToPrevChar);
        }
    }

    /// Reorder filters and regex to appear before FTS text.
    /// When a filter or regex is typed after free text, move it before the FTS portion.
    ///
    /// Examples:
    /// - `groceries date:2024` → `date:2024 groceries`
    /// - `coffee /pattern/` → `/pattern/ coffee`
    fn reorder_filters_before_fts(&mut self) {
        let state = self.current_search_state_mut();
        let cursor = state.db_search_input.cursor();
        let value = state.db_search_input.value().to_string();

        // Parse tokens to identify structure
        let tokens = Self::tokenize_for_reorder(&value);
        if tokens.is_empty() {
            return;
        }

        // Find the first FTS token (not a filter, not a regex)
        let first_fts_idx = tokens.iter().position(|t| t.token_type == ReorderTokenType::Fts);

        // Find if there's a filter or regex AFTER the first FTS token
        let token_after_fts = first_fts_idx.and_then(|fts_idx| {
            tokens[fts_idx + 1..]
                .iter()
                .enumerate()
                .find(|(_, t)| {
                    t.token_type == ReorderTokenType::Filter
                        || t.token_type == ReorderTokenType::Regex
                })
                .map(|(i, t)| (fts_idx + 1 + i, t.clone()))
        });

        let Some((_token_idx, move_token)) = token_after_fts else {
            return;
        };

        // Check if cursor is within or just after the token we want to move
        // Only reorder if cursor is in the token being typed
        if cursor < move_token.start || cursor > move_token.end {
            return;
        }

        // Build new value with token moved before FTS
        let first_fts_idx = first_fts_idx.unwrap();

        // Calculate where to insert the token (just before first FTS token)
        let insert_pos = tokens[first_fts_idx].start;

        // Build new value:
        // 1. Everything before insert position
        // 2. The token + space
        // 3. Everything from insert position to token start (minus trailing space before token)
        // 4. Everything after token end
        let before_insert = &value[..insert_pos];
        let token_text = &value[move_token.start..move_token.end];

        // Handle spacing: remove space before the token if present
        let between_start = insert_pos;
        let between_end = move_token.start;
        let between = value[between_start..between_end].trim_end();

        let after_token = &value[move_token.end..];
        let after_token = after_token.trim_start();

        let mut new_value = String::with_capacity(value.len());
        new_value.push_str(before_insert);
        new_value.push_str(token_text);
        if !between.is_empty() || !after_token.is_empty() {
            new_value.push(' ');
        }
        new_value.push_str(between);
        if !between.is_empty() && !after_token.is_empty() {
            new_value.push(' ');
        }
        new_value.push_str(after_token);

        // Calculate new cursor position:
        // Cursor was at `cursor` within the token. The token moved from move_token.start
        // to insert_pos. Adjust cursor by the delta.
        let cursor_offset_in_token = cursor - move_token.start;
        let new_cursor = insert_pos + cursor_offset_in_token;

        let tail_len = new_value.len().saturating_sub(new_cursor);
        let state = self.current_search_state_mut();
        state.db_search_input = Input::new(new_value);
        for _ in 0..tail_len {
            state.db_search_input.handle(InputRequest::GoToPrevChar);
        }
    }

    /// Tokenize input for reorder logic, identifying token types and positions.
    fn tokenize_for_reorder(input: &str) -> Vec<ReorderToken> {
        let mut tokens = Vec::new();
        let mut current_start = None;
        let mut in_quotes = false;
        let mut in_regex = false;
        let mut regex_closed = false;
        let mut pos = 0;
        let mut chars = input.chars().peekable();

        while let Some(c) = chars.next() {
            match c {
                '/' if !in_quotes && !in_regex && current_start.is_none() => {
                    // Start of regex
                    current_start = Some(pos);
                    pos += 1;
                    in_regex = true;
                    regex_closed = false;
                }
                '\\' if in_regex && !regex_closed => {
                    // Escaped char in regex
                    pos += 1;
                    if chars.next().is_some() {
                        pos += 1;
                    }
                }
                '/' if in_regex && !regex_closed => {
                    // Closing slash
                    pos += 1;
                    regex_closed = true;
                }
                c if in_regex && regex_closed => {
                    // Consume flags
                    if c.is_ascii_alphabetic() {
                        pos += 1;
                    } else {
                        // End of regex token
                        if let Some(start) = current_start.take() {
                            tokens.push(ReorderToken {
                                start,
                                end: pos,
                                token_type: ReorderTokenType::Regex,
                            });
                        }
                        in_regex = false;
                        regex_closed = false;
                        // Handle current char
                        if c == ' ' || c == '\t' {
                            pos += 1;
                        } else {
                            current_start = Some(pos);
                            pos += 1;
                        }
                    }
                }
                _ if in_regex => {
                    // Inside regex
                    pos += 1;
                }
                '\\' if !in_quotes => {
                    // Escaped char outside regex
                    if current_start.is_none() {
                        current_start = Some(pos);
                    }
                    pos += 1;
                    if chars.next().is_some() {
                        pos += 1;
                    }
                }
                '"' => {
                    in_quotes = !in_quotes;
                    if current_start.is_none() && in_quotes {
                        current_start = Some(pos);
                    }
                    pos += 1;
                }
                ' ' | '\t' if !in_quotes => {
                    // End of token
                    if let Some(start) = current_start.take() {
                        let token_text = &input[start..pos];
                        let token_type = Self::classify_token(token_text);
                        tokens.push(ReorderToken {
                            start,
                            end: pos,
                            token_type,
                        });
                    }
                    pos += 1;
                }
                _ => {
                    if current_start.is_none() {
                        current_start = Some(pos);
                    }
                    pos += 1;
                }
            }
        }

        // Handle final token
        if in_regex {
            if let Some(start) = current_start {
                tokens.push(ReorderToken {
                    start,
                    end: pos,
                    token_type: ReorderTokenType::Regex,
                });
            }
        } else if let Some(start) = current_start {
            let token_text = &input[start..pos];
            let token_type = Self::classify_token(token_text);
            tokens.push(ReorderToken {
                start,
                end: pos,
                token_type,
            });
        }

        tokens
    }

    /// Classify a token as Filter, Regex, or FTS based on its content
    fn classify_token(token: &str) -> ReorderTokenType {
        const FILTER_PREFIXES: &[&str] = &[
            "date:",
            "d:",
            "account:",
            "a:",
            "amount:",
            "am:",
            "category:",
            "c:",
        ];

        if token.starts_with('/') {
            ReorderTokenType::Regex
        } else if FILTER_PREFIXES.iter().any(|p| token.starts_with(p)) {
            ReorderTokenType::Filter
        } else {
            ReorderTokenType::Fts
        }
    }

    /// Handle special regex input behavior:
    /// - Typing `/` when no regex exists: insert `//` and place cursor between
    /// - Typing `/` when cursor is inside regex (before closing `/`): move cursor past closing `/`
    /// - Deleting either `/` delimiter: delete entire regex including flags
    ///
    /// Returns true if the input was handled (caller should skip normal processing).
    fn handle_db_search_regex_input(&mut self, req: &InputRequest) -> bool {
        let state = self.current_search_state_mut();
        let cursor = state.db_search_input.cursor();
        let value = state.db_search_input.value().to_string();

        // Find existing regex: /.../ or /.../flags
        let regex_info = self.find_regex_in_value(&value);

        // Check if previous char is backslash (for escaping / inside regex)
        let prev_is_backslash = cursor > 0
            && value
                .chars()
                .nth(cursor - 1)
                .map(|c| c == '\\')
                .unwrap_or(false);

        match req {
            InputRequest::InsertChar('/') => {
                if let Some((regex_start, closing_slash, _regex_end)) = regex_info {
                    // Regex exists - check if cursor is inside (between opening and closing /)
                    if cursor > regex_start && cursor <= closing_slash {
                        // If previous char is \, insert literal / (escaped slash in regex)
                        if prev_is_backslash {
                            return false; // Let normal input handling insert the /
                        }
                        // Move cursor past closing slash (but before flags)
                        let new_cursor = closing_slash + 1;
                        let tail_len = value.len() - new_cursor;
                        let state = self.current_search_state_mut();
                        state.db_search_input = Input::new(value);
                        for _ in 0..tail_len {
                            state.db_search_input.handle(InputRequest::GoToPrevChar);
                        }
                        return true;
                    }
                    // Cursor is outside regex - let deduplication handle it
                    false
                } else {
                    // No regex exists - insert // and place cursor between
                    let mut new_value = String::with_capacity(value.len() + 2);
                    new_value.push_str(&value[..cursor]);
                    new_value.push_str("//");
                    new_value.push_str(&value[cursor..]);

                    // Cursor goes between the slashes (one from end of inserted //)
                    let new_cursor = cursor + 1;
                    let tail_len = new_value.len() - new_cursor;
                    let state = self.current_search_state_mut();
                    state.db_search_input = Input::new(new_value);
                    for _ in 0..tail_len {
                        state.db_search_input.handle(InputRequest::GoToPrevChar);
                    }
                    true
                }
            }
            InputRequest::DeletePrevChar => {
                if let Some((regex_start, closing_slash, regex_end)) = regex_info {
                    // Check if we're about to delete a / delimiter
                    if cursor == regex_start + 1 || cursor == closing_slash + 1 {
                        // Deleting opening or closing / - remove entire regex
                        let mut new_value = String::with_capacity(value.len());
                        new_value.push_str(&value[..regex_start]);
                        // Skip space before regex if present
                        let after = &value[regex_end..];
                        let after = after.strip_prefix(' ').unwrap_or(after);
                        if !new_value.is_empty() && !after.is_empty() {
                            new_value.push(' ');
                        }
                        new_value.push_str(after);

                        let new_cursor = regex_start.min(new_value.len());
                        let tail_len = new_value.len() - new_cursor;
                        let state = self.current_search_state_mut();
                        state.db_search_input = Input::new(new_value);
                        for _ in 0..tail_len {
                            state.db_search_input.handle(InputRequest::GoToPrevChar);
                        }
                        return true;
                    }
                }
                false
            }
            InputRequest::DeleteNextChar => {
                if let Some((regex_start, closing_slash, regex_end)) = regex_info {
                    // Check if we're about to delete a / delimiter
                    if cursor == regex_start || cursor == closing_slash {
                        // Deleting opening or closing / - remove entire regex
                        let mut new_value = String::with_capacity(value.len());
                        new_value.push_str(&value[..regex_start]);
                        let after = &value[regex_end..];
                        let after = after.strip_prefix(' ').unwrap_or(after);
                        if !new_value.is_empty() && !after.is_empty() {
                            new_value.push(' ');
                        }
                        new_value.push_str(after);

                        let new_cursor = regex_start.min(new_value.len());
                        let tail_len = new_value.len() - new_cursor;
                        let state = self.current_search_state_mut();
                        state.db_search_input = Input::new(new_value);
                        for _ in 0..tail_len {
                            state.db_search_input.handle(InputRequest::GoToPrevChar);
                        }
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Find regex in value, returning (start, closing_slash_pos, end) if found.
    /// End includes any flags after the closing slash.
    fn find_regex_in_value(&self, value: &str) -> Option<(usize, usize, usize)> {
        // Find opening / at word boundary
        let mut chars = value.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '/' {
                let at_word_boundary =
                    i == 0 || value.chars().nth(i - 1).map(|c| c.is_whitespace()).unwrap_or(false);
                if at_word_boundary {
                    // Look for closing /
                    let start = i;
                    while let Some((j, c2)) = chars.next() {
                        if c2 == '\\' {
                            // Skip escaped character
                            chars.next();
                        } else if c2 == '/' {
                            // Found closing /, now consume flags
                            let mut end = j + 1;
                            for (k, c3) in value[end..].char_indices() {
                                if c3.is_ascii_alphabetic() {
                                    end = j + 1 + k + 1;
                                } else {
                                    break;
                                }
                            }
                            return Some((start, j, end));
                        }
                    }
                    // No closing / found - unclosed regex
                    return Some((start, value.len(), value.len()));
                }
            }
        }
        None
    }

    pub fn clear_db_search(&mut self) {
        let state = self.current_search_state_mut();
        state.db_search_input.reset();
        state.db_search_query = DbSearchQuery::default();
        state.db_search_active = false;
        state.selected_index = 0;
        self.selected_index = 0;
        self.reload_current_tab();
        self.input_mode = InputMode::Normal;
    }

    pub fn confirm_db_search(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn db_search_value(&self) -> &str {
        self.current_search_state()
            .map(|s| s.db_search_input.value())
            .unwrap_or("")
    }

    pub fn db_search_cursor(&self) -> usize {
        self.current_search_state()
            .map(|s| s.db_search_input.visual_cursor())
            .unwrap_or(0)
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
        let filter = self
            .current_search_state()
            .map(|s| s.db_search_query.to_filter(Some(500)))
            .unwrap_or_else(|| TransactionFilter {
                limit: Some(500),
                ..Default::default()
            });

        match self.current_tab {
            Tab::Transactions => {
                self.transactions = self.store.query_transactions(&filter).unwrap_or_default();
            }
            Tab::Transfers => {
                self.linked_transfers = self
                    .store
                    .list_transfers_with_transactions(true, &filter)
                    .unwrap_or_default();
            }
            Tab::Categories => {
                // Categories don't use transaction filters
            }
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => {
                    self.uncategorised = self
                        .store
                        .get_uncategorised_transactions(&filter)
                        .unwrap_or_default();
                }
                TodoSubTab::AiReview => {
                    self.ai_reviews = self
                        .store
                        .get_pending_ai_reviews(&filter)
                        .unwrap_or_default();
                }
                TodoSubTab::TransferReview => {
                    self.transfer_reviews = self
                        .store
                        .get_pending_transfer_reviews(&filter)
                        .unwrap_or_default();
                }
            },
        }
        self.rebuild_caches();
        self.apply_fuzzy_filter();
    }

    /// Apply fuzzy filter on top of loaded data for current tab only
    fn apply_fuzzy_filter(&mut self) {
        let pattern = self
            .current_search_state()
            .map(|s| s.fuzzy_pattern.clone())
            .unwrap_or_default();

        match self.current_tab {
            Tab::Transactions => {
                self.filtered_transaction_idx = if pattern.is_empty() {
                    (0..self.transactions.len()).collect()
                } else {
                    self.transactions
                        .iter()
                        .enumerate()
                        .filter(|(_, tx)| self.fuzzy_matcher.fuzzy_matches(&pattern, &tx.description))
                        .map(|(i, _)| i)
                        .collect()
                };
            }
            Tab::Transfers => {
                self.filtered_transfer_idx = if pattern.is_empty() {
                    (0..self.linked_transfers.len()).collect()
                } else {
                    self.linked_transfers
                        .iter()
                        .enumerate()
                        .filter(|(_, twt)| {
                            self.fuzzy_matcher
                                .fuzzy_matches(&pattern, &twt.from_transaction.description)
                                || self
                                    .fuzzy_matcher
                                    .fuzzy_matches(&pattern, &twt.to_transaction.description)
                        })
                        .map(|(i, _)| i)
                        .collect()
                };
            }
            Tab::Categories => {
                // Categories don't use fuzzy filtering (for now)
            }
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorised => {
                    self.filtered_uncategorised_idx = if pattern.is_empty() {
                        (0..self.uncategorised.len()).collect()
                    } else {
                        self.uncategorised
                            .iter()
                            .enumerate()
                            .filter(|(_, tx)| {
                                self.fuzzy_matcher.fuzzy_matches(&pattern, &tx.description)
                            })
                            .map(|(i, _)| i)
                            .collect()
                    };
                }
                TodoSubTab::AiReview => {
                    self.filtered_ai_review_idx = if pattern.is_empty() {
                        (0..self.ai_reviews.len()).collect()
                    } else {
                        self.ai_reviews
                            .iter()
                            .enumerate()
                            .filter(|(_, r)| {
                                self.fuzzy_matcher
                                    .fuzzy_matches(&pattern, &r.transaction.description)
                            })
                            .map(|(i, _)| i)
                            .collect()
                    };
                }
                TodoSubTab::TransferReview => {
                    self.filtered_transfer_review_idx = if pattern.is_empty() {
                        (0..self.transfer_reviews.len()).collect()
                    } else {
                        self.transfer_reviews
                            .iter()
                            .enumerate()
                            .filter(|(_, tr)| {
                                match (
                                    self.tx_by_id.get(&tr.from_transaction_id),
                                    self.tx_by_id.get(&tr.to_transaction_id),
                                ) {
                                    (Some(from), Some(to)) => {
                                        self.fuzzy_matcher
                                            .fuzzy_matches(&pattern, &from.description)
                                            || self
                                                .fuzzy_matcher
                                                .fuzzy_matches(&pattern, &to.description)
                                    }
                                    _ => true,
                                }
                            })
                            .map(|(i, _)| i)
                            .collect()
                    };
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
        if let Some(twt) = self.linked_transfers.get(self.selected_index) {
            let _ = self.store.delete_transfer(twt.transfer.id);
            self.refresh_data();
            if self.selected_index >= self.linked_transfers.len() && self.selected_index > 0 {
                self.selected_index -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_for_reorder_simple() {
        let tokens = App::tokenize_for_reorder("groceries date:2024");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Fts);
        assert_eq!(tokens[0].start, 0);
        assert_eq!(tokens[0].end, 9);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Filter);
        assert_eq!(tokens[1].start, 10);
        assert_eq!(tokens[1].end, 19);
    }

    #[test]
    fn test_tokenize_for_reorder_filter_first() {
        let tokens = App::tokenize_for_reorder("date:2024 groceries");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Filter);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Fts);
    }

    #[test]
    fn test_tokenize_for_reorder_with_regex() {
        let tokens = App::tokenize_for_reorder("/pattern/i date:2024 coffee");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Regex);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Filter);
        assert_eq!(tokens[2].token_type, ReorderTokenType::Fts);
    }

    #[test]
    fn test_tokenize_for_reorder_filter_shortcuts() {
        let tokens = App::tokenize_for_reorder("coffee d:2024");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Fts);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Filter);

        let tokens = App::tokenize_for_reorder("a:ING food");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Filter);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Fts);
    }

    #[test]
    fn test_tokenize_for_reorder_quoted_filter() {
        let tokens = App::tokenize_for_reorder(r#"coffee account:"ING/Orange Everyday""#);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Fts);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Filter);
    }

    #[test]
    fn test_tokenize_for_reorder_multiple_fts() {
        let tokens = App::tokenize_for_reorder("coffee shop date:2024");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Fts);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Fts);
        assert_eq!(tokens[2].token_type, ReorderTokenType::Filter);
    }

    #[test]
    fn test_classify_token() {
        assert_eq!(App::classify_token("groceries"), ReorderTokenType::Fts);
        assert_eq!(App::classify_token("date:2024"), ReorderTokenType::Filter);
        assert_eq!(App::classify_token("d:2024"), ReorderTokenType::Filter);
        assert_eq!(App::classify_token("account:ING"), ReorderTokenType::Filter);
        assert_eq!(App::classify_token("a:ING"), ReorderTokenType::Filter);
        assert_eq!(App::classify_token("amount:>100"), ReorderTokenType::Filter);
        assert_eq!(App::classify_token("am:>100"), ReorderTokenType::Filter);
        assert_eq!(App::classify_token("category:Food"), ReorderTokenType::Filter);
        assert_eq!(App::classify_token("c:Food"), ReorderTokenType::Filter);
        assert_eq!(App::classify_token("/pattern/i"), ReorderTokenType::Regex);
    }

    #[test]
    fn test_tokenize_for_reorder_regex_after_fts() {
        // Regex after FTS text should be detected for reordering
        let tokens = App::tokenize_for_reorder("coffee /pattern/i");
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Fts);
        assert_eq!(tokens[0].start, 0);
        assert_eq!(tokens[0].end, 6);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Regex);
        assert_eq!(tokens[1].start, 7);
        assert_eq!(tokens[1].end, 17);
    }

    #[test]
    fn test_tokenize_for_reorder_mixed() {
        // Filter, FTS, then regex - regex should be detected after FTS
        let tokens = App::tokenize_for_reorder("date:2024 coffee /pat/");
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens[0].token_type, ReorderTokenType::Filter);
        assert_eq!(tokens[1].token_type, ReorderTokenType::Fts);
        assert_eq!(tokens[2].token_type, ReorderTokenType::Regex);
    }
}
