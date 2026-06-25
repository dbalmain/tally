//! Application state for the TUI.
//!
//! Split by feature: this file owns the `App` struct, construction, caches,
//! navigation and the core load/filter data path. Feature-specific actions
//! live in submodules:
//!
//! - `tabs` — Tab/TodoSubTab enums + `TabLists` (all per-tab dispatch)
//! - `search` — per-tab search state, DB/fuzzy search, autocomplete
//! - `categories` — category popup, AI review, rename/merge
//! - `filters` — saved-search filter management
//! - `transfers` — transfer marking, confirmation, deletion

mod categories;
mod filters;
mod search;
mod tabs;
mod transfers;

pub use search::TabSearchState;
pub use tabs::{Tab, TabKey, TabLists, TodoSubTab};

use std::collections::HashMap;

use tui_input::Input;

use crate::classify::{SimilarityIndex, normalise};
use crate::search::ParsedQuery;
use crate::tui::search_bar::SearchBar;
use crate::{
    Account, Bank, Category, FuzzyMatcher, Result, Transaction, TransactionStore, Transfer,
};

use tabs::{tab_key, tab_title};

/// Row limit for every list load.
const LIST_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    DbSearch,
    FuzzySearch,
    FilterEdit,
    Category,
    TextPrompt,
    BulkApply,
    ConfirmMerge,
    /// Generic yes/no confirmation driven by `confirm_action` (the
    /// transfer/category-breaking flows). `ConfirmMerge` predates this and
    /// keeps its own handlers.
    Confirm,
    TransferPending,
    TransferNoMatch,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    MergeCategory {
        source_id: i64,
        target_id: i64,
    },
    /// Categorising a transaction that is part of a transfer: unlink the
    /// transfer first, then apply the category.
    BreakTransferForCategory {
        transfer_id: i64,
        tx: Transaction,
        category_path: String,
    },
    /// Marking a transfer whose chosen endpoints are already linked elsewhere:
    /// delete the existing transfer(s), then create the new one.
    BreakTransfersForTransfer {
        transfer_ids: Vec<i64>,
        from_id: i64,
        to_id: i64,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CategoryTarget {
    #[default]
    Transaction,
    Filter(i64),
}

#[derive(Debug, Clone)]
pub enum TextPromptTarget {
    CategoryRename(Category),
    FilterCreate,
    FilterCreateFromQuery(String),
    FilterRename(i64),
}

#[derive(Debug, Clone)]
pub struct TextPrompt {
    title: &'static str,
    input: Input,
    target: TextPromptTarget,
    return_mode: InputMode,
}

pub struct FilterEditState {
    pub(super) filter_id: i64,
    pub(super) name: String,
    pub(super) search_bar: SearchBar,
    pub(super) preview: Vec<Transaction>,
    pub(super) preview_scroll: usize,
}

pub struct App {
    pub store: TransactionStore,
    pub current_tab: Tab,
    pub todo_subtab: TodoSubTab,
    /// The data behind every tab (see `tabs::TabLists`).
    pub lists: TabLists,
    pub selected_index: usize,
    pub input_mode: InputMode,
    pub similarity_index: Option<SimilarityIndex>,
    pub bulk_apply: Option<BulkApplyState>,
    pub should_quit: bool,
    pub refreshing: bool,
    pub keybind_help_open: bool,
    pub hints_visible: bool,
    /// Whether transaction detail panels expand to show source + metadata.
    pub tx_details_expanded: bool,
    // Category popup state
    pub category_input: String,
    pub category_suggestions: Vec<Category>,
    pub category_selected: usize,
    pub category_target: CategoryTarget,
    // Transfer marking state
    pub pending_transfer_tx: Option<Transaction>,
    pub transfer_candidates: Vec<Transaction>,
    pub error_message: Option<String>,
    pub banks: HashMap<i64, Bank>,
    pub accounts: HashMap<i64, Account>,
    pub fuzzy_matcher: FuzzyMatcher,
    // Caches to avoid DB queries during render/filter
    tx_by_id: HashMap<i64, Transaction>,
    category_by_tx_id: HashMap<i64, String>,
    transfer_by_tx_id: HashMap<i64, Transfer>,
    category_tx_count: HashMap<i64, usize>,
    similarity_candidates: HashMap<i64, Transaction>,
    // Shared single-line text prompt state
    text_prompt: Option<TextPrompt>,
    // Dedicated saved-filter query editor state
    filter_edit: Option<FilterEditState>,
    // Confirmation popup state
    pub confirm_message: Option<String>,
    pub confirm_action: Option<ConfirmAction>,
    // Per-tab search state
    tab_search_state: HashMap<TabKey, TabSearchState>,
}

pub struct BulkApplyState {
    pub category_id: i64,
    pub category_path: String,
    pub rows: Vec<BulkRow>,
    pub cursor: usize,
}

pub struct BulkRow {
    pub tx: Transaction,
    pub score: f32,
    pub selected: bool,
}

impl App {
    /// Build the application state, doing initial loads of every tab's data
    /// plus banks/accounts/categories. Returns Err if any of the startup
    /// queries fails — the TUI hasn't drawn anything yet, so a hard failure
    /// here is the right behaviour (the alternative is a half-populated UI
    /// that silently lies about what's in the database).
    pub fn new(store: TransactionStore) -> Result<Self> {
        Self::new_with_refreshing(store, false)
    }

    pub fn new_with_refreshing(store: TransactionStore, refreshing: bool) -> Result<Self> {
        let lists = TabLists::load(&store, Some(LIST_LIMIT))?;

        let bank_list = store.list_banks()?;
        let banks: HashMap<i64, Bank> = bank_list.iter().cloned().map(|b| (b.id, b)).collect();

        let mut accounts = HashMap::new();
        for bank in &bank_list {
            for account in store.list_accounts(bank.id)? {
                accounts.insert(account.id, account);
            }
        }

        let mut app = Self {
            lists,
            store,
            current_tab: Tab::Todo,
            todo_subtab: TodoSubTab::Uncategorised,
            selected_index: 0,
            input_mode: InputMode::Normal,
            similarity_index: None,
            bulk_apply: None,
            should_quit: false,
            refreshing,
            keybind_help_open: false,
            hints_visible: true,
            tx_details_expanded: false,
            category_input: String::new(),
            category_suggestions: Vec::new(),
            category_selected: 0,
            category_target: CategoryTarget::Transaction,
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
            similarity_candidates: HashMap::new(),
            text_prompt: None,
            filter_edit: None,
            confirm_message: None,
            confirm_action: None,
            tab_search_state: HashMap::new(),
        };
        app.rebuild_tx_caches();
        app.rebuild_category_counts();
        Ok(app)
    }

    /// Run a store load whose failure shouldn't tear down the UI: on error,
    /// surface a message via `error_message` and return `T::default()` so
    /// callers keep the existing list state coherent. Used for mid-flight
    /// loads (cache rebuilds, popup data) where a stack trace would lose the
    /// user their typed input.
    fn load_or_show<T: Default>(
        &mut self,
        what: &str,
        f: impl FnOnce(&TransactionStore) -> Result<T>,
    ) -> T {
        match f(&self.store) {
            Ok(v) => v,
            Err(e) => {
                self.error_message = Some(format!("Failed to {}: {}", what, e));
                T::default()
            }
        }
    }

    /// Run a store mutation; surface failures via `error_message` and return
    /// `false`. Callers gate follow-up work (refresh_data, cursor adjustment)
    /// on the returned bool so we don't refresh after a no-op. Closure form
    /// lets a single call chain multiple store operations with `?` while
    /// sharing one `&mut TransactionStore` borrow.
    fn try_mutation(
        &mut self,
        what: &str,
        f: impl FnOnce(&mut TransactionStore) -> Result<()>,
    ) -> bool {
        match f(&mut self.store) {
            Ok(()) => true,
            Err(e) => {
                self.error_message = Some(format!("Failed to {}: {}", what, e));
                false
            }
        }
    }

    fn current_tab_key(&self) -> TabKey {
        tab_key(self.current_tab, self.todo_subtab)
    }

    /// Rebuild the per-transaction caches (`tx_by_id`, `category_by_tx_id`,
    /// `transfer_by_tx_id`) from currently-loaded list contents. Cheap: no
    /// per-row DB queries — at most one bulk lookup plus a few singletons for
    /// transfer-review sides that aren't already in the loaded data.
    fn rebuild_tx_caches(&mut self) {
        self.tx_by_id.clear();
        for tx in self.lists.transactions.items() {
            self.tx_by_id.insert(tx.id, tx.clone());
        }
        for tx in self.lists.uncategorised.items() {
            self.tx_by_id.entry(tx.id).or_insert_with(|| tx.clone());
        }
        for review in self.lists.ai_reviews.items() {
            self.tx_by_id
                .entry(review.transaction.id)
                .or_insert_with(|| review.transaction.clone());
        }
        for twt in self.lists.linked_transfers.items() {
            self.tx_by_id
                .entry(twt.from_transaction.id)
                .or_insert_with(|| twt.from_transaction.clone());
            self.tx_by_id
                .entry(twt.to_transaction.id)
                .or_insert_with(|| twt.to_transaction.clone());
        }
        // Load transactions for pending transfer reviews (they only have IDs)
        for tr in self.lists.transfer_reviews.items() {
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
        let tx_ids: Vec<i64> = self
            .lists
            .transactions
            .items()
            .iter()
            .map(|t| t.id)
            .collect();
        self.category_by_tx_id = self.load_or_show("load transaction categories", |s| {
            s.get_categories_for_transactions(&tx_ids)
        });

        self.transfer_by_tx_id.clear();
        for twt in self.lists.linked_transfers.items() {
            self.transfer_by_tx_id
                .insert(twt.from_transaction.id, twt.transfer.clone());
            self.transfer_by_tx_id
                .insert(twt.to_transaction.id, twt.transfer.clone());
        }
        for tr in self.lists.transfer_reviews.items() {
            self.transfer_by_tx_id
                .entry(tr.from_transaction_id)
                .or_insert_with(|| tr.clone());
            self.transfer_by_tx_id
                .entry(tr.to_transaction_id)
                .or_insert_with(|| tr.clone());
        }
    }

    pub fn get_cached_transaction(&self, id: i64) -> Option<&Transaction> {
        self.tx_by_id.get(&id)
    }

    pub fn get_cached_category(&self, tx_id: i64) -> Option<&str> {
        self.category_by_tx_id.get(&tx_id).map(|s| s.as_str())
    }

    pub fn category_path(&self, category_id: i64) -> Option<&str> {
        self.lists
            .categories
            .items()
            .iter()
            .find(|c| c.id == category_id)
            .map(|c| c.path.as_str())
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

    // ==================== Tab Navigation ====================

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

    // ==================== Selection ====================

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
        self.lists.len(self.current_tab_key())
    }

    fn get_current_transaction(&self, filtered_idx: usize) -> Option<&Transaction> {
        self.lists
            .transaction_at(self.current_tab_key(), filtered_idx)
    }

    fn find_filtered_position_by_tx_id(&self, tx_id: i64) -> Option<usize> {
        self.lists.position_of_tx(self.current_tab_key(), tx_id)
    }

    pub fn selected_transaction(&self) -> Option<&Transaction> {
        self.get_current_transaction(self.selected_index)
    }

    /// Toggle the expanded (source + metadata) transaction detail panel.
    pub fn toggle_tx_details(&mut self) {
        self.tx_details_expanded = !self.tx_details_expanded;
    }

    // ==================== Data Loading ====================

    /// Reload only the current tab's data from DB based on its search query.
    /// On failure the previous items stay visible alongside the error popup.
    fn reload_current_tab(&mut self) {
        let parsed = self
            .current_search_state()
            .map(|s| s.search_bar.parsed().clone())
            .unwrap_or_default();
        let key = self.current_tab_key();
        match self
            .lists
            .reload(key, &self.store, &parsed, Some(LIST_LIMIT))
        {
            // A successful load clears any stale error: fixing the query,
            // leaving search, or switching tabs all reload through here, so the
            // error popup dismisses itself once the underlying problem is gone.
            Ok(()) => self.error_message = None,
            Err(e) => {
                self.error_message = Some(format!("Failed to load {}: {}", tab_title(key), e))
            }
        }
        self.rebuild_tx_caches();
        self.apply_fuzzy_filter();
    }

    /// Apply fuzzy filter on top of loaded data for current tab only
    fn apply_fuzzy_filter(&mut self) {
        let (db_query, pattern) = self
            .current_search_state()
            .map(|s| (s.search_bar.value().to_string(), s.fuzzy_pattern.clone()))
            .unwrap_or_default();
        let key = self.current_tab_key();
        self.lists.apply_fuzzy(
            key,
            &db_query,
            &pattern,
            &mut self.fuzzy_matcher,
            &self.tx_by_id,
        );
    }

    /// Reload data after a mutation (categorisation, transfers) — both tx
    /// caches and category counts may have changed.
    pub fn refresh_data(&mut self) {
        self.similarity_index = None;
        self.similarity_candidates.clear();
        let categories = self.load_or_show("load categories", |s| s.list_categories());
        self.lists.categories.set_items(categories);
        self.rebuild_search_configs();
        self.reload_current_tab();
        self.rebuild_category_counts();
    }

    fn rebuild_similarity_index(&mut self) {
        let query = ParsedQuery::empty();
        let candidates = self.load_or_show("load unconfirmed transactions", |s| {
            s.get_unconfirmed_transactions(&query, None)
        });
        let examples = self.load_or_show("load confirmed category examples", |s| {
            s.get_confirmed_examples()
        });
        let extra_corpus: Vec<_> = examples
            .iter()
            .map(|example| normalise(&example.description))
            .collect();
        let candidate_norms: Vec<_> = candidates
            .iter()
            .map(|tx| (tx.id, normalise(&tx.description)))
            .collect();

        self.similarity_candidates = candidates.into_iter().map(|tx| (tx.id, tx)).collect();
        self.similarity_index = SimilarityIndex::build(&candidate_norms, &extra_corpus);
    }

    // ==================== Input ====================

    pub(super) fn open_text_prompt(
        &mut self,
        title: &'static str,
        value: String,
        target: TextPromptTarget,
    ) {
        let return_mode = self.input_mode;
        self.open_text_prompt_with_return(title, value, target, return_mode);
    }

    pub(super) fn open_text_prompt_with_return(
        &mut self,
        title: &'static str,
        value: String,
        target: TextPromptTarget,
        return_mode: InputMode,
    ) {
        self.text_prompt = Some(TextPrompt {
            title,
            input: Input::new(value),
            target,
            return_mode,
        });
        self.input_mode = InputMode::TextPrompt;
    }

    pub(super) fn restore_text_prompt(
        &mut self,
        title: &'static str,
        value: String,
        target: TextPromptTarget,
    ) {
        let return_mode = if self.filter_edit.is_some() {
            InputMode::FilterEdit
        } else {
            InputMode::Normal
        };
        self.restore_text_prompt_with_return(title, value, target, return_mode);
    }

    pub(super) fn restore_text_prompt_with_return(
        &mut self,
        title: &'static str,
        value: String,
        target: TextPromptTarget,
        return_mode: InputMode,
    ) {
        self.open_text_prompt_with_return(title, value, target, return_mode);
    }

    pub fn handle_text_prompt_input(&mut self, req: tui_input::InputRequest) {
        if let Some(prompt) = self.text_prompt.as_mut() {
            prompt.input.handle(req);
        }
    }

    pub fn text_prompt_title(&self) -> &'static str {
        self.text_prompt
            .as_ref()
            .map(|prompt| prompt.title)
            .unwrap_or("")
    }

    pub fn text_prompt_value(&self) -> &str {
        self.text_prompt
            .as_ref()
            .map(|prompt| prompt.input.value())
            .unwrap_or("")
    }

    pub fn text_prompt_cursor(&self) -> usize {
        self.text_prompt
            .as_ref()
            .map(|prompt| prompt.input.visual_cursor())
            .unwrap_or(0)
    }

    pub fn text_prompt_scroll(&self, width: usize) -> usize {
        self.text_prompt
            .as_ref()
            .map(|prompt| prompt.input.visual_scroll(width))
            .unwrap_or(0)
    }

    pub fn confirm_text_prompt(&mut self) {
        let Some(prompt) = self.text_prompt.take() else {
            self.cancel_input();
            return;
        };
        let value = prompt.input.value().trim().to_string();
        let return_mode = prompt.return_mode;
        match prompt.target {
            TextPromptTarget::CategoryRename(category) => {
                self.confirm_category_rename(category, value);
            }
            TextPromptTarget::FilterCreate => self.confirm_filter_create(value),
            TextPromptTarget::FilterCreateFromQuery(query) => {
                self.confirm_filter_from_query(value, query, return_mode);
            }
            TextPromptTarget::FilterRename(id) => self.confirm_filter_rename(id, value),
        }
    }

    pub(super) fn clear_text_prompt(&mut self) {
        self.text_prompt = None;
    }

    pub fn cancel_input(&mut self) {
        let text_prompt_return_mode = (self.input_mode == InputMode::TextPrompt).then(|| {
            self.text_prompt
                .as_ref()
                .map(|prompt| prompt.return_mode)
                .unwrap_or(InputMode::Normal)
        });
        let return_to_filter_edit = self.filter_edit.is_some()
            && matches!(
                self.input_mode,
                InputMode::Category
                    | InputMode::TextPrompt
                    | InputMode::BulkApply
                    | InputMode::Confirm
                    | InputMode::ConfirmMerge
                    | InputMode::TransferNoMatch
            );
        self.input_mode = text_prompt_return_mode.unwrap_or(if return_to_filter_edit {
            InputMode::FilterEdit
        } else {
            InputMode::Normal
        });
        self.clear_category_popup();
        self.error_message = None;
        self.clear_transfer_mode();
        self.clear_text_prompt();
        self.clear_confirm();
        self.bulk_apply = None;
    }

    fn clear_confirm(&mut self) {
        self.confirm_message = None;
        self.confirm_action = None;
    }

    /// Carry out the pending `confirm_action` (the generic `InputMode::Confirm`
    /// flows). `ConfirmMerge` predates this and keeps its own `confirm_merge`.
    pub fn confirm_proceed(&mut self) {
        let Some(action) = self.confirm_action.take() else {
            self.cancel_input();
            return;
        };
        self.confirm_message = None;
        match action {
            ConfirmAction::MergeCategory {
                source_id,
                target_id,
            } => {
                // Never routed through this mode, but keep merge working if it is.
                self.confirm_action = Some(ConfirmAction::MergeCategory {
                    source_id,
                    target_id,
                });
                self.confirm_merge();
            }
            ConfirmAction::BreakTransferForCategory {
                transfer_id,
                tx,
                category_path,
            } => {
                if !self.try_mutation("unlink transfer", |s| s.delete_transfer(transfer_id)) {
                    self.input_mode = InputMode::Normal;
                    return;
                }
                self.apply_category(tx, category_path);
            }
            ConfirmAction::BreakTransfersForTransfer {
                transfer_ids,
                from_id,
                to_id,
            } => {
                let applied = self.try_mutation("recreate transfer", |s| {
                    for id in &transfer_ids {
                        s.delete_transfer(*id)?;
                    }
                    s.create_transfer(from_id, to_id, crate::TransferSource::Manual, true, None)?;
                    Ok(())
                });
                self.input_mode = InputMode::Normal;
                if applied {
                    self.refresh_data();
                }
            }
        }
    }
}
