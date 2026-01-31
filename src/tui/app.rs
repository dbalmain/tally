use std::collections::HashMap;

use tui_input::Input;

use crate::{
    Account, Bank, Category, CategorySource, FuzzyMatcher, SearchQuery, Transaction,
    TransactionFilter, TransactionStore, TransactionWithEnrichment, Transfer, TransferSource,
    TransferWithTransactions,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Transactions,
    Transfers,
    Todo,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[Tab::Transactions, Tab::Transfers, Tab::Todo]
    }

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Transactions => "Transactions",
            Tab::Transfers => "Transfers",
            Tab::Todo => "Todo",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoSubTab {
    Uncategorized,
    AiReview,
    TransferReview,
}

impl TodoSubTab {
    pub fn all() -> &'static [TodoSubTab] {
        &[
            TodoSubTab::Uncategorized,
            TodoSubTab::AiReview,
            TodoSubTab::TransferReview,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            TodoSubTab::Uncategorized => "Uncategorized",
            TodoSubTab::AiReview => "AI Review",
            TodoSubTab::TransferReview => "Transfer Review",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
    Category,
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
    pub uncategorized: Vec<Transaction>,
    pub ai_reviews: Vec<TransactionWithEnrichment>,
    pub transfer_reviews: Vec<Transfer>,
    pub error_message: Option<String>,
    pub banks: HashMap<i64, Bank>,
    pub accounts: HashMap<i64, Account>,
    pub search_input: Input,
    pub search_query: SearchQuery,
    pub fuzzy_matcher: FuzzyMatcher,
    pub filtered_transactions: Vec<Transaction>,
    pub filtered_transfers: Vec<TransferWithTransactions>,
    pub filtered_uncategorized: Vec<Transaction>,
    pub filtered_ai_reviews: Vec<TransactionWithEnrichment>,
    pub filtered_transfer_reviews: Vec<Transfer>,
}

impl App {
    pub fn new(store: TransactionStore) -> Self {
        let transactions = store
            .query_transactions(&TransactionFilter {
                limit: Some(500),
                ..Default::default()
            })
            .unwrap_or_default();

        let uncategorized = store.get_uncategorized_transactions(500).unwrap_or_default();
        let ai_reviews = store.get_pending_ai_reviews(500).unwrap_or_default();
        let transfer_reviews = store.get_pending_transfer_reviews(500).unwrap_or_default();
        let linked_transfers = store
            .list_transfers_with_transactions(true)
            .unwrap_or_default();

        let banks = store
            .list_banks()
            .unwrap_or_default()
            .into_iter()
            .map(|b| (b.id, b))
            .collect();

        let mut accounts = HashMap::new();
        for bank in store.list_banks().unwrap_or_default() {
            for account in store.list_accounts(bank.id).unwrap_or_default() {
                accounts.insert(account.id, account);
            }
        }

        Self {
            store,
            current_tab: Tab::Todo,
            todo_subtab: TodoSubTab::Uncategorized,
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
            search_input: Input::default(),
            search_query: SearchQuery::default(),
            fuzzy_matcher: FuzzyMatcher::new(),
            filtered_transactions: transactions.clone(),
            filtered_transfers: linked_transfers.clone(),
            filtered_uncategorized: uncategorized.clone(),
            filtered_ai_reviews: ai_reviews.clone(),
            filtered_transfer_reviews: transfer_reviews.clone(),
            transactions,
            linked_transfers,
            uncategorized,
            ai_reviews,
            transfer_reviews,
        }
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
        let current_idx = tabs.iter().position(|&t| t == self.current_tab).unwrap_or(0);
        self.current_tab = tabs[(current_idx + 1) % tabs.len()];
        self.selected_index = 0;
        self.clear_transfer_mode();
    }

    pub fn previous_tab(&mut self) {
        let tabs = Tab::all();
        let current_idx = tabs.iter().position(|&t| t == self.current_tab).unwrap_or(0);
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
                let current_pos = self
                    .transfer_candidates
                    .iter()
                    .position(|c| {
                        self.current_transactions()
                            .get(self.selected_index)
                            .is_some_and(|t| t.id == c.id)
                    })
                    .unwrap_or(0);
                if current_pos + 1 < self.transfer_candidates.len() {
                    let next_candidate = &self.transfer_candidates[current_pos + 1];
                    if let Some(pos) = self
                        .current_transactions()
                        .iter()
                        .position(|t| t.id == next_candidate.id)
                    {
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
                let current_pos = self
                    .transfer_candidates
                    .iter()
                    .position(|c| {
                        self.current_transactions()
                            .get(self.selected_index)
                            .is_some_and(|t| t.id == c.id)
                    })
                    .unwrap_or(0);
                if current_pos > 0 {
                    let prev_candidate = &self.transfer_candidates[current_pos - 1];
                    if let Some(pos) = self
                        .current_transactions()
                        .iter()
                        .position(|t| t.id == prev_candidate.id)
                    {
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
            Tab::Transactions => self.filtered_transactions.len(),
            Tab::Transfers => self.filtered_transfers.len(),
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorized => self.filtered_uncategorized.len(),
                TodoSubTab::AiReview => self.filtered_ai_reviews.len(),
                TodoSubTab::TransferReview => self.filtered_transfer_reviews.len(),
            },
        }
    }

    pub fn current_transactions(&self) -> &[Transaction] {
        match self.current_tab {
            Tab::Transactions => &self.filtered_transactions,
            Tab::Transfers => &[],
            Tab::Todo => match self.todo_subtab {
                TodoSubTab::Uncategorized => &self.filtered_uncategorized,
                TodoSubTab::AiReview => &[],
                TodoSubTab::TransferReview => &[],
            },
        }
    }

    pub fn current_transfers(&self) -> &[TransferWithTransactions] {
        &self.filtered_transfers
    }

    pub fn current_ai_reviews(&self) -> &[TransactionWithEnrichment] {
        &self.filtered_ai_reviews
    }

    pub fn current_transfer_reviews(&self) -> &[Transfer] {
        &self.filtered_transfer_reviews
    }

    pub fn selected_transaction(&self) -> Option<&Transaction> {
        self.current_transactions().get(self.selected_index)
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
            self.category_suggestions[self.category_selected].path.clone()
        } else if !self.category_input.is_empty() {
            self.category_input.clone()
        } else {
            self.cancel_input();
            return;
        };

        if let Ok(category_id) = self.store.get_or_create_category(&category_path) {
            let _ = self.store.set_category(
                tx.id,
                category_id,
                CategorySource::Manual,
                true,
                None,
            );
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
            self.transfer_candidates = candidates;
            self.input_mode = InputMode::TransferPending;
            if let Some(pos) = self.transfer_candidates.first().and_then(|first| {
                self.current_transactions()
                    .iter()
                    .position(|t| t.id == first.id)
            }) {
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

    pub fn refresh_data(&mut self) {
        self.transactions = self
            .store
            .query_transactions(&TransactionFilter {
                limit: Some(500),
                ..Default::default()
            })
            .unwrap_or_default();
        self.uncategorized = self
            .store
            .get_uncategorized_transactions(500)
            .unwrap_or_default();
        self.ai_reviews = self.store.get_pending_ai_reviews(500).unwrap_or_default();
        self.transfer_reviews = self
            .store
            .get_pending_transfer_reviews(500)
            .unwrap_or_default();
        self.linked_transfers = self
            .store
            .list_transfers_with_transactions(true)
            .unwrap_or_default();
        self.apply_search_filter();
    }

    pub fn start_search(&mut self) {
        self.input_mode = InputMode::Search;
    }

    pub fn handle_search_input(&mut self, req: tui_input::InputRequest) {
        self.search_input.handle(req);
        self.search_query = SearchQuery::parse(self.search_input.value());
        self.apply_search_filter();
        self.selected_index = 0;
    }

    pub fn clear_search(&mut self) {
        self.search_input.reset();
        self.search_query = SearchQuery::default();
        self.apply_search_filter();
        self.selected_index = 0;
        self.input_mode = InputMode::Normal;
    }

    pub fn confirm_search(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn search_value(&self) -> &str {
        self.search_input.value()
    }

    pub fn search_cursor(&self) -> usize {
        self.search_input.visual_cursor()
    }

    fn apply_search_filter(&mut self) {
        if self.search_query.is_empty() {
            self.filtered_transactions = self.transactions.clone();
            self.filtered_transfers = self.linked_transfers.clone();
            self.filtered_uncategorized = self.uncategorized.clone();
            self.filtered_ai_reviews = self.ai_reviews.clone();
            self.filtered_transfer_reviews = self.transfer_reviews.clone();
            return;
        }

        let mut matcher = FuzzyMatcher::new();

        // If we have date filters, query the database directly
        let has_date_filter =
            self.search_query.date_from.is_some() || self.search_query.date_to.is_some();

        let source_transactions = if has_date_filter {
            let filter = TransactionFilter {
                from_date: self.search_query.date_from,
                to_date: self.search_query.date_to,
                limit: Some(500),
                ..Default::default()
            };
            self.store.query_transactions(&filter).unwrap_or_default()
        } else {
            self.transactions.clone()
        };

        self.filtered_transactions = source_transactions
            .into_iter()
            .filter(|tx| self.matches_transaction(tx, &mut matcher))
            .collect();

        self.filtered_transfers = self
            .linked_transfers
            .iter()
            .filter(|twt| {
                self.matches_transaction(&twt.from_transaction, &mut matcher)
                    || self.matches_transaction(&twt.to_transaction, &mut matcher)
            })
            .cloned()
            .collect();

        self.filtered_uncategorized = self
            .uncategorized
            .iter()
            .filter(|tx| self.matches_transaction(tx, &mut matcher))
            .cloned()
            .collect();

        self.filtered_ai_reviews = self
            .ai_reviews
            .iter()
            .filter(|r| self.matches_transaction(&r.transaction, &mut matcher))
            .cloned()
            .collect();

        self.filtered_transfer_reviews = self
            .transfer_reviews
            .iter()
            .filter(|tr| {
                match (
                    self.store.get_transaction_by_id(tr.from_transaction_id),
                    self.store.get_transaction_by_id(tr.to_transaction_id),
                ) {
                    (Ok(Some(ref from)), Ok(Some(ref to))) => {
                        self.matches_transaction(from, &mut matcher)
                            || self.matches_transaction(to, &mut matcher)
                    }
                    _ => true,
                }
            })
            .cloned()
            .collect();
    }

    fn matches_transaction(&self, tx: &Transaction, matcher: &mut FuzzyMatcher) -> bool {
        let q = &self.search_query;

        if q.date_from.is_some_and(|from| tx.date < from) {
            return false;
        }
        if q.date_to.is_some_and(|to| tx.date > to) {
            return false;
        }
        if q.amount_min.is_some_and(|min| tx.amount_cents < min) {
            return false;
        }
        if q.amount_max.is_some_and(|max| tx.amount_cents > max) {
            return false;
        }
        if let Some(ref bank_filter) = q.bank {
            let bank_name = self.bank_name(tx.bank_id).to_lowercase();
            if !bank_name.contains(bank_filter) {
                return false;
            }
        }
        if let Some(ref account_filter) = q.account {
            let account_name = self.account_name(tx.account_id).to_lowercase();
            if !account_name.contains(account_filter) {
                return false;
            }
        }
        if let Some(ref category_filter) = q.category {
            let cat = self
                .store
                .get_transaction_category(tx.id)
                .ok()
                .flatten()
                .map(|c| c.path.to_lowercase())
                .unwrap_or_default();
            if !cat.contains(category_filter) {
                return false;
            }
        }
        if !q.text_match.is_empty() && !matcher.matches(&q.text_match, &tx.description) {
            return false;
        }
        true
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
