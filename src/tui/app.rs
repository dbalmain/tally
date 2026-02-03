use std::collections::HashMap;

use tui_input::Input;

use crate::{
    Account, Bank, Category, CategorySource, DbSearchQuery, FuzzyMatcher, Transaction,
    TransactionFilter, TransactionStore, TransactionWithEnrichment, Transfer, TransferSource,
    TransferWithTransactions,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoSubTab {
    Uncategorised,
    AiReview,
    TransferReview,
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
    pub db_search_input: Input,
    pub db_search_query: DbSearchQuery,
    pub fuzzy_search_input: Input,
    pub fuzzy_pattern: String,
    pub db_search_active: bool,
    pub fuzzy_search_active: bool,
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
            db_search_input: Input::default(),
            db_search_query: DbSearchQuery::default(),
            fuzzy_search_input: Input::default(),
            fuzzy_pattern: String::new(),
            db_search_active: false,
            fuzzy_search_active: false,
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
        };
        app.rebuild_caches();
        app
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
        let tabs = Tab::all();
        let current_idx = tabs
            .iter()
            .position(|&t| t == self.current_tab)
            .unwrap_or(0);
        self.current_tab = tabs[(current_idx + 1) % tabs.len()];
        self.selected_index = 0;
        self.clear_transfer_mode();
    }

    pub fn previous_tab(&mut self) {
        let tabs = Tab::all();
        let current_idx = tabs
            .iter()
            .position(|&t| t == self.current_tab)
            .unwrap_or(0);
        self.current_tab = tabs[(current_idx + tabs.len() - 1) % tabs.len()];
        self.selected_index = 0;
        self.clear_transfer_mode();
    }

    pub fn next_subtab(&mut self) {
        if self.current_tab != Tab::Todo {
            return;
        }
        let subtabs = TodoSubTab::all();
        let current_idx = subtabs
            .iter()
            .position(|&t| t == self.todo_subtab)
            .unwrap_or(0);
        self.todo_subtab = subtabs[(current_idx + 1) % subtabs.len()];
        self.selected_index = 0;
    }

    pub fn previous_subtab(&mut self) {
        if self.current_tab != Tab::Todo {
            return;
        }
        let subtabs = TodoSubTab::all();
        let current_idx = subtabs
            .iter()
            .position(|&t| t == self.todo_subtab)
            .unwrap_or(0);
        self.todo_subtab = subtabs[(current_idx + subtabs.len() - 1) % subtabs.len()];
        self.selected_index = 0;
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
        self.reload_from_db();
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

    pub fn start_db_search(&mut self) {
        self.input_mode = InputMode::DbSearch;
        self.db_search_active = true;
    }

    pub fn handle_db_search_input(&mut self, req: tui_input::InputRequest) {
        self.db_search_input.handle(req);
        let cursor = self.db_search_input.cursor();
        let (query, transition_to_fuzzy) =
            DbSearchQuery::parse_with_cursor(self.db_search_input.value(), Some(cursor));

        if transition_to_fuzzy {
            // Remove " ~" from input and transition to fuzzy mode
            let trimmed =
                self.db_search_input.value()[..self.db_search_input.value().len() - 2].to_string();
            self.db_search_input = Input::new(trimmed.clone());
            self.db_search_query = DbSearchQuery::parse(&trimmed).0;
            self.reload_from_db();
            self.start_fuzzy_search();
        } else {
            self.db_search_query = query;
            self.reload_from_db();
        }
        self.selected_index = 0;
    }

    pub fn clear_db_search(&mut self) {
        self.db_search_input.reset();
        self.db_search_query = DbSearchQuery::default();
        self.db_search_active = false;
        self.reload_from_db();
        self.selected_index = 0;
        self.input_mode = InputMode::Normal;
    }

    pub fn confirm_db_search(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn db_search_value(&self) -> &str {
        self.db_search_input.value()
    }

    pub fn db_search_cursor(&self) -> usize {
        self.db_search_input.visual_cursor()
    }

    // ==================== Fuzzy Search ====================

    pub fn start_fuzzy_search(&mut self) {
        self.input_mode = InputMode::FuzzySearch;
        self.fuzzy_search_active = true;
    }

    pub fn handle_fuzzy_search_input(&mut self, req: tui_input::InputRequest) {
        self.fuzzy_search_input.handle(req);
        self.fuzzy_pattern = self.fuzzy_search_input.value().to_string();
        self.apply_fuzzy_filter();
        self.selected_index = 0;
    }

    pub fn clear_fuzzy_search(&mut self) {
        self.fuzzy_search_input.reset();
        self.fuzzy_pattern.clear();
        self.fuzzy_search_active = false;
        self.apply_fuzzy_filter();
        self.selected_index = 0;
        // Return to DB search mode if it's still active, else normal
        if self.db_search_active {
            self.input_mode = InputMode::DbSearch;
        } else {
            self.input_mode = InputMode::Normal;
        }
    }

    pub fn confirm_fuzzy_search(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn fuzzy_search_value(&self) -> &str {
        self.fuzzy_search_input.value()
    }

    pub fn fuzzy_search_cursor(&self) -> usize {
        self.fuzzy_search_input.visual_cursor()
    }

    // ==================== Filtering Logic ====================

    /// Reload transactions from DB based on current db_search_query
    fn reload_from_db(&mut self) {
        let filter = self.db_search_query.to_filter(Some(500));
        self.transactions = self.store.query_transactions(&filter).unwrap_or_default();
        self.uncategorised = self
            .store
            .get_uncategorised_transactions(&filter)
            .unwrap_or_default();
        self.ai_reviews = self
            .store
            .get_pending_ai_reviews(&filter)
            .unwrap_or_default();
        self.linked_transfers = self
            .store
            .list_transfers_with_transactions(true, &filter)
            .unwrap_or_default();
        self.transfer_reviews = self
            .store
            .get_pending_transfer_reviews(&filter)
            .unwrap_or_default();
        self.rebuild_caches();
        self.apply_fuzzy_filter();
    }

    /// Apply fuzzy filter on top of loaded data
    fn apply_fuzzy_filter(&mut self) {
        if self.fuzzy_pattern.is_empty() {
            // No fuzzy filter - show all loaded data
            self.filtered_transaction_idx = (0..self.transactions.len()).collect();
            self.filtered_transfer_idx = (0..self.linked_transfers.len()).collect();
            self.filtered_uncategorised_idx = (0..self.uncategorised.len()).collect();
            self.filtered_ai_review_idx = (0..self.ai_reviews.len()).collect();
            self.filtered_transfer_review_idx = (0..self.transfer_reviews.len()).collect();
            return;
        }

        let pattern = &self.fuzzy_pattern;

        self.filtered_transaction_idx = self
            .transactions
            .iter()
            .enumerate()
            .filter(|(_, tx)| self.fuzzy_matcher.fuzzy_matches(pattern, &tx.description))
            .map(|(i, _)| i)
            .collect();

        self.filtered_transfer_idx = self
            .linked_transfers
            .iter()
            .enumerate()
            .filter(|(_, twt)| {
                self.fuzzy_matcher
                    .fuzzy_matches(pattern, &twt.from_transaction.description)
                    || self
                        .fuzzy_matcher
                        .fuzzy_matches(pattern, &twt.to_transaction.description)
            })
            .map(|(i, _)| i)
            .collect();

        self.filtered_uncategorised_idx = self
            .uncategorised
            .iter()
            .enumerate()
            .filter(|(_, tx)| self.fuzzy_matcher.fuzzy_matches(pattern, &tx.description))
            .map(|(i, _)| i)
            .collect();

        self.filtered_ai_review_idx = self
            .ai_reviews
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                self.fuzzy_matcher
                    .fuzzy_matches(pattern, &r.transaction.description)
            })
            .map(|(i, _)| i)
            .collect();

        self.filtered_transfer_review_idx = self
            .transfer_reviews
            .iter()
            .enumerate()
            .filter(|(_, tr)| {
                match (
                    self.tx_by_id.get(&tr.from_transaction_id),
                    self.tx_by_id.get(&tr.to_transaction_id),
                ) {
                    (Some(from), Some(to)) => {
                        self.fuzzy_matcher.fuzzy_matches(pattern, &from.description)
                            || self.fuzzy_matcher.fuzzy_matches(pattern, &to.description)
                    }
                    _ => true,
                }
            })
            .map(|(i, _)| i)
            .collect();
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
