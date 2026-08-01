//! Application state for the TUI.
//!
//! Split by feature: this file owns the `App` struct, construction, caches,
//! navigation and the core load/filter data path. Feature-specific actions
//! live in submodules:
//!
//! - `tabs` — Tab/TodoSubTab enums + `TabLists` (all per-tab dispatch)
//! - `search` — per-tab search state, DB/fuzzy search, autocomplete
//! - `categories` — category popup, AI review, rename/merge
//! - `annotations` — note and tag editors
//! - `filters` — saved-search filter management
//! - `transfers` — transfer marking, confirmation, deletion

mod accounts;
mod annotations;
mod categories;
mod filters;
mod search;
mod tabs;
mod transfers;

pub use search::TabSearchState;
pub use tabs::{Tab, TabKey, TabLists, TodoSubTab};

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use tui_input::Input;

use crate::classify::{SimilarityIndex, normalise};
use crate::config::Config;
use crate::search::{ParsedQuery, SearchOptions};
use crate::tui::note_editor::NoteEditor;
use crate::tui::search_bar::SearchBar;
use crate::tui::tag_editor::TagEditor;
use crate::{
    Account, Bank, Category, FuzzyMatcher, Result, Transaction, TransactionStore, Transfer,
};

use tabs::{tab_key, tab_title};

/// Row limit for every list load.
const LIST_LIMIT: usize = 500;

fn next_wrapping(i: usize, len: usize) -> usize {
    if len == 0 { 0 } else { (i + 1) % len }
}

fn prev_wrapping(i: usize, len: usize) -> usize {
    if len == 0 { 0 } else { (i + len - 1) % len }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    DbSearch,
    FuzzySearch,
    FilterEdit,
    Category,
    /// Multi-line free-form note editor (`n`), backed by `note_editor`.
    Note,
    /// Space-separated `#tag` line editor (`#`), backed by `tag_editor`.
    Tags,
    TextPrompt,
    BulkApply,
    /// Generic yes/no confirmation driven by `confirm_action`.
    Confirm,
    /// Scrollable confirmation listing the transactions `apply_filters` would
    /// (re)categorise (Ctrl-A). Confirm applies; cancel does nothing.
    ConfirmApplyFilters,
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
    /// Leaving the filter edit screen with unsaved query changes.
    DiscardFilterEdit,
    /// Leaving the note editor with unsaved text.
    DiscardNoteEdit,
    /// Deleting a saved filter from the Filters tab.
    DeleteFilter(i64),
    /// Unlinking the selected transaction's transfer (`u` on Transactions).
    UnlinkTransfer {
        transfer_id: i64,
    },
    /// Removing the selected transaction's category (`u` on Transactions).
    Uncategorise {
        tx_id: i64,
    },
    /// Deleting a category from the Categories tab.
    DeleteCategory(i64),
    /// Deleting an account from the Accounts tab (also removes its exports
    /// folder; transactions are retained in history).
    DeleteAccount(i64),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CategoryTarget {
    #[default]
    Transaction,
    /// The popup applies to every transaction matching the current search
    /// rather than the single selected one (`C` on Transactions / AI Review).
    MatchingTransactions,
    Filter(i64),
}

#[derive(Debug, Clone)]
pub enum TextPromptTarget {
    CategoryRename(Category),
    AccountRename(crate::AccountWithBank),
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

/// The single background writer job the TUI can run at a time (WAL permits
/// only one writer). Tracked by [`App::active_job`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackgroundJob {
    Refresh,
    Classify,
    Reindex,
}

impl BackgroundJob {
    /// Present-progressive label for the tab-bar indicator and the
    /// "<X> in progress" conflict message.
    pub fn gerund(self) -> &'static str {
        match self {
            BackgroundJob::Refresh => "Refreshing",
            BackgroundJob::Classify => "Classifying",
            BackgroundJob::Reindex => "Reindexing",
        }
    }
}

/// Outcome of a finished [`BackgroundJob`], sent over [`App`]'s shared
/// `job_rx` channel.
enum JobOutcome {
    Refresh(Result<crate::RefreshReport>),
    Classify(Result<crate::classify::ClassifyReport>),
    Reindex(Result<usize>),
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
    pub apply_filters_preview: Option<ApplyFiltersPreview>,
    pub should_quit: bool,
    /// Which background writer (if any) is currently running. WAL allows only
    /// one writer; all of refresh / classify / reindex share this slot.
    pub active_job: Option<BackgroundJob>,
    pub keybind_help_open: bool,
    pub hints_visible: bool,
    /// Whether the transaction view shows the inline detail panel (full
    /// description, source file, and metadata) for the selected row.
    pub view_details: bool,
    /// Whether the Transactions tab shows a row summing the amounts of the
    /// currently visible transactions.
    pub show_sum: bool,
    /// Whether the Categories tab shows the side panel listing the selected
    /// category's transactions.
    pub show_category_transactions: bool,
    /// Transactions backing that side panel (the selected category's rows).
    pub category_transactions: Vec<Transaction>,
    /// Whether the Accounts tab shows the side panel listing the selected
    /// account's transactions.
    pub show_account_transactions: bool,
    /// Transactions backing that side panel (the selected account's rows).
    pub account_transactions: Vec<Transaction>,
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
    tags_by_tx_id: HashMap<i64, Vec<String>>,
    note_by_tx_id: HashMap<i64, String>,
    /// Every known tag, most-used first — feeds `tag:` autocomplete and the
    /// tag editor's suggestion list.
    tag_options: Vec<String>,
    category_tx_count: HashMap<i64, usize>,
    account_tx_count: HashMap<i64, usize>,
    similarity_candidates: HashMap<i64, Transaction>,
    // Note / tag editors (self-contained widgets; see `app::annotations`)
    pub note_editor: Option<NoteEditor>,
    pub tag_editor: Option<TagEditor>,
    // Shared single-line text prompt state
    text_prompt: Option<TextPrompt>,
    // Dedicated saved-filter query editor state
    filter_edit: Option<FilterEditState>,
    // Confirmation popup state
    pub confirm_message: Option<String>,
    pub confirm_action: Option<ConfirmAction>,
    // Transient tab-bar status message + its expiry instant.
    status: Option<(String, Instant)>,
    // Shared receiver for the single background writer (refresh / classify /
    // reindex). The worker opens its own store connection so the UI stays
    // responsive; `poll_job` dispatches the outcome to the matching finish_*.
    job_rx: Option<Receiver<JobOutcome>>,
    // Per-tab search state
    tab_search_state: HashMap<TabKey, TabSearchState>,
    search_options: SearchOptions,
}

/// Preview backing the Ctrl-A apply-filters confirmation modal: the
/// transactions `apply_filters` would (re)categorise, plus the list scroll
/// position.
pub struct ApplyFiltersPreview {
    pub rows: Vec<Transaction>,
    pub scroll: usize,
}

/// What `Enter` does with the selected rows in the bulk-apply popup.
#[derive(Debug, Clone)]
pub enum BulkAction {
    /// Apply a category to the selected transactions (similar-transactions and
    /// `C`). The category is resolved via get_or_create at apply time.
    ApplyCategory { category_path: String },
    /// Confirm the selected transactions' existing AI category suggestions
    /// (`A` on AI Review).
    ConfirmCategories,
    /// Confirm the selected pending transfers (`A` on Transfer Review).
    ConfirmTransfers,
}

pub struct BulkApplyState {
    pub action: BulkAction,
    pub rows: Vec<BulkRow>,
    pub cursor: usize,
}

pub struct BulkRow {
    pub selected: bool,
    pub item: BulkItem,
}

/// A row in the bulk-apply popup: either a transaction or a transfer.
#[derive(Debug, Clone)]
pub enum BulkItem {
    /// `score` is Some only for the similar-transactions flow (drives the score
    /// column); None for `C` and `A`-on-AI-Review.
    Transaction {
        tx: Transaction,
        score: Option<f32>,
    },
    Transfer(Transfer),
}

impl BulkRow {
    /// Id used when applying: the transaction id, or the transfer id.
    pub fn target_id(&self) -> i64 {
        match &self.item {
            BulkItem::Transaction { tx, .. } => tx.id,
            BulkItem::Transfer(t) => t.id,
        }
    }
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

    /// `refreshing` seeds [`Self::active_job`] with
    /// [`BackgroundJob::Refresh`] when a startup refresh is already claimed
    /// (tests); production startup passes `false` and then calls
    /// [`Self::request_refresh`].
    pub fn new_with_refreshing(store: TransactionStore, refreshing: bool) -> Result<Self> {
        Self::new_with_refreshing_and_search_options(
            store,
            refreshing,
            Config::default().search_options(),
        )
    }

    pub fn new_with_refreshing_and_search_options(
        store: TransactionStore,
        refreshing: bool,
        search_options: SearchOptions,
    ) -> Result<Self> {
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
            apply_filters_preview: None,
            should_quit: false,
            active_job: refreshing.then_some(BackgroundJob::Refresh),
            keybind_help_open: false,
            hints_visible: true,
            view_details: false,
            show_sum: false,
            show_category_transactions: false,
            category_transactions: Vec::new(),
            show_account_transactions: false,
            account_transactions: Vec::new(),
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
            tags_by_tx_id: HashMap::new(),
            note_by_tx_id: HashMap::new(),
            tag_options: Vec::new(),
            note_editor: None,
            tag_editor: None,
            category_tx_count: HashMap::new(),
            account_tx_count: HashMap::new(),
            similarity_candidates: HashMap::new(),
            text_prompt: None,
            filter_edit: None,
            confirm_message: None,
            confirm_action: None,
            status: None,
            job_rx: None,
            tab_search_state: HashMap::new(),
            search_options,
        };
        app.rebuild_tx_caches();
        app.rebuild_category_counts();
        app.rebuild_account_counts();
        Ok(app)
    }

    /// How long a transient status message stays on screen.
    const STATUS_DURATION: Duration = Duration::from_secs(5);

    /// Show a transient message in the tab bar's right-aligned status area
    /// for [`Self::STATUS_DURATION`]. It never captures input; it just
    /// disappears on a later redraw.
    pub fn show_status(&mut self, message: String) {
        self.status = Some((message, Instant::now() + Self::STATUS_DURATION));
    }

    /// The status message to display, if one is active and hasn't expired.
    pub fn active_status(&self) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, expires_at)| Instant::now() < *expires_at)
            .map(|(message, _)| message.as_str())
    }

    /// Claim the single background slot for `job`. If another job is already
    /// running, show a consistent "<running> in progress" status and return
    /// false. This is the ONLY place a background-job conflict is reported.
    fn claim_job(&mut self, job: BackgroundJob) -> bool {
        if let Some(running) = self.active_job {
            self.show_status(format!("{} in progress", running.gerund()));
            return false;
        }
        self.active_job = Some(job);
        true
    }

    /// Start importing new bank exports on a background store connection.
    /// This is intentionally available to the Todo → Uncategorised view,
    /// where newly imported transactions are immediately visible.
    pub fn request_refresh(&mut self) {
        if !self.claim_job(BackgroundJob::Refresh) {
            return;
        }

        let Some(db_path) = self.store.db_path().map(std::path::Path::to_path_buf) else {
            let result = self.store.refresh();
            self.finish_refresh(result);
            return;
        };

        let exports_dir = self.store.exports_dir().to_path_buf();
        let search_options = self.search_options;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = TransactionStore::open(&db_path, &exports_dir).and_then(|mut store| {
                store.set_search_options(search_options);
                store.refresh()
            });
            let _ = tx.send(JobOutcome::Refresh(result));
        });
        self.job_rx = Some(rx);
    }

    /// Collect a finished background job (refresh / classify / reindex), if
    /// any. Called every event-loop iteration; cheap no-op while idle or
    /// still running.
    pub fn poll_job(&mut self) {
        let Some(rx) = &self.job_rx else { return };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.job_rx = None;
                let label = self
                    .active_job
                    .map(|j| j.gerund())
                    .unwrap_or("Background job");
                self.active_job = None;
                self.error_message = Some(format!("{label} stopped unexpectedly"));
                return;
            }
        };
        self.job_rx = None;
        match outcome {
            JobOutcome::Refresh(result) => self.finish_refresh(result),
            JobOutcome::Classify(result) => self.finish_classification(result),
            JobOutcome::Reindex(result) => self.finish_reindex(result),
        }
    }

    fn finish_refresh(&mut self, result: Result<crate::RefreshReport>) {
        self.active_job = None;
        self.refresh_data();
        match result {
            Ok(report) => {
                // Updates are the rare case (a feed refining an already
                // imported description), so they earn a slot in the status
                // line only when they happened.
                let updated = match report.transactions_updated {
                    0 => String::new(),
                    n => format!(", updated: {n}"),
                };
                self.show_status(format!(
                    "Refreshed — added: {}{}, skipped: {}, files: {}",
                    report.transactions_added,
                    updated,
                    report.transactions_skipped,
                    report.files_processed,
                ))
            }
            Err(e) => self.error_message = Some(format!("Refresh failed: {e}")),
        }
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

    pub(super) fn current_tab_key(&self) -> TabKey {
        tab_key(self.current_tab, self.todo_subtab)
    }

    pub(super) fn confirm(&mut self, message: String, action: ConfirmAction) {
        self.confirm_message = Some(message);
        self.confirm_action = Some(action);
        self.input_mode = InputMode::Confirm;
    }

    /// Rebuild the per-transaction caches (`tx_by_id`, `category_by_tx_id`,
    /// `transfer_by_tx_id`) from currently-loaded list contents. Cheap: three
    /// bulk lookups — the transactions backing transfer-review sides that aren't
    /// already loaded, plus categories and transfers for the loaded
    /// transactions.
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
        // Pending transfer reviews carry only transaction IDs; load the ones
        // not already cached in a single bulk query.
        let mut missing_ids: Vec<i64> = Vec::new();
        for tr in self.lists.transfer_reviews.items() {
            for id in [tr.from_transaction_id, tr.to_transaction_id] {
                if !self.tx_by_id.contains_key(&id) && !missing_ids.contains(&id) {
                    missing_ids.push(id);
                }
            }
        }
        if !missing_ids.is_empty() {
            let loaded = self.load_or_show("load transfer-review transactions", |s| {
                s.get_transactions_by_ids(&missing_ids)
            });
            for (id, tx) in loaded {
                self.tx_by_id.insert(id, tx);
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

        // Load transfer links straight from the DB for every loaded
        // transaction, so an unlink (which only touches the `transfers` table)
        // is reflected even on tabs that don't reload `linked_transfers`. This
        // is the single source of truth: the DB query already covers every
        // pending transfer-review pair (both endpoints were loaded into
        // `tx_by_id` above), so we must NOT also seed the cache from the
        // `transfer_reviews` list — that list isn't reloaded when another tab
        // is active, and a stale entry would resurrect a transfer the user has
        // just unlinked or recategorised.
        let all_tx_ids: Vec<i64> = self.tx_by_id.keys().copied().collect();
        self.transfer_by_tx_id = self.load_or_show("load transaction transfers", |s| {
            s.get_transfers_for_transactions(&all_tx_ids)
        });

        // Annotations are cached for every loaded transaction, not just the
        // Transactions tab's, because tags render inline on every transaction
        // table and the detail panel can be opened from several of them.
        self.tags_by_tx_id = self.load_or_show("load transaction tags", |s| {
            s.tags_for_transactions(&all_tx_ids)
        });
        self.note_by_tx_id = self.load_or_show("load transaction notes", |s| {
            s.notes_for_transactions(&all_tx_ids)
        });
        self.reload_tag_options();
    }

    /// Refresh the known-tag list and push it into every search bar, so
    /// `tag:` autocomplete reflects a tag that was just created or GC'd.
    pub(super) fn reload_tag_options(&mut self) {
        self.tag_options = self
            .load_or_show("load tags", |s| s.list_tags())
            .into_iter()
            .map(|tag| tag.name)
            .collect();
        self.rebuild_search_configs();
    }

    pub fn get_cached_transaction(&self, id: i64) -> Option<&Transaction> {
        self.tx_by_id.get(&id)
    }

    pub fn get_cached_category(&self, tx_id: i64) -> Option<&str> {
        self.category_by_tx_id.get(&tx_id).map(|s| s.as_str())
    }

    pub fn get_cached_tags(&self, tx_id: i64) -> &[String] {
        self.tags_by_tx_id
            .get(&tx_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn get_cached_note(&self, tx_id: i64) -> Option<&str> {
        self.note_by_tx_id.get(&tx_id).map(String::as_str)
    }

    pub fn tag_options(&self) -> &[String] {
        &self.tag_options
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
        self.current_tab = tabs[next_wrapping(current_idx, tabs.len())];
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
        self.current_tab = tabs[prev_wrapping(current_idx, tabs.len())];
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
        self.todo_subtab = subtabs[next_wrapping(current_idx, subtabs.len())];
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
        self.todo_subtab = subtabs[prev_wrapping(current_idx, subtabs.len())];
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
                self.selected_index = next_wrapping(self.selected_index, len);
            }
        }
        self.reload_category_transactions();
        self.reload_account_transactions();
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
                self.selected_index = prev_wrapping(self.selected_index, len);
            }
        }
        self.reload_category_transactions();
        self.reload_account_transactions();
    }

    fn list_len(&self) -> usize {
        self.lists.len(self.current_tab_key())
    }

    fn clamp_selection(&mut self) {
        let len = self.list_len();
        if len == 0 {
            self.selected_index = 0;
        } else {
            self.selected_index = self.selected_index.min(len - 1);
        }
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

    /// Toggle the inline transaction detail panel (full description, source
    /// file, and metadata) for the selected row.
    pub fn toggle_view_details(&mut self) {
        self.view_details = !self.view_details;
    }

    /// Toggle the row summing the amounts of the currently visible
    /// transactions on the Transactions tab.
    pub fn toggle_sum(&mut self) {
        self.show_sum = !self.show_sum;
    }

    /// Rebuild `transactions_fts` from the transactions table (Ctrl-G).
    ///
    /// Runs on a background store connection (like refresh/classification) so
    /// the UI stays live and a "Reindexing..." indicator can paint. Conflicts
    /// with any other background writer are reported by [`Self::claim_job`].
    /// In-memory stores (tests) have no path to open a second connection on,
    /// so they run inline instead.
    pub fn reindex_fts(&mut self) {
        if !self.claim_job(BackgroundJob::Reindex) {
            return;
        }

        let Some(db_path) = self.store.db_path().map(std::path::Path::to_path_buf) else {
            let result = self.store.rebuild_fts();
            self.finish_reindex(result);
            return;
        };

        let exports_dir = self.store.exports_dir().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = TransactionStore::open(&db_path, &exports_dir)
                .and_then(|store| store.rebuild_fts());
            let _ = tx.send(JobOutcome::Reindex(result));
        });
        self.job_rx = Some(rx);
    }

    /// Clear the in-flight job, then surface the outcome: reload + green
    /// status on success, error popup on failure.
    fn finish_reindex(&mut self, result: Result<usize>) {
        self.active_job = None;
        match result {
            Ok(count) => {
                self.refresh_data();
                self.show_status(format!("Reindexed {count} transactions"));
            }
            Err(e) => self.error_message = Some(format!("Failed to reindex full-text search: {e}")),
        }
    }

    /// Dismiss the error popup if one is showing. Returns whether an error was
    /// present and cleared. Callers (the Esc/Enter interceptor in `run_app`)
    /// use the bool so the path is unit-testable without a terminal.
    pub fn dismiss_error(&mut self) -> bool {
        self.error_message.take().is_some()
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
        self.clamp_selection();
        self.reload_category_transactions();
        self.reload_account_transactions();
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
        let accounts = self.load_or_show("load accounts", |s| s.list_accounts_with_bank());
        self.lists.accounts.set_items(accounts);
        self.rebuild_search_configs();
        self.reload_current_tab();
        self.rebuild_category_counts();
        self.rebuild_account_counts();
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
            TextPromptTarget::AccountRename(account) => {
                self.confirm_account_rename(account, value);
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
        let return_to_text_prompt =
            self.input_mode == InputMode::Confirm && self.text_prompt.is_some();
        let text_prompt_return_mode = match self.input_mode {
            InputMode::TextPrompt => Some(
                self.text_prompt
                    .as_ref()
                    .map(|prompt| prompt.return_mode)
                    .unwrap_or(InputMode::Normal),
            ),
            InputMode::Confirm if return_to_text_prompt => Some(InputMode::TextPrompt),
            _ => None,
        };
        let return_to_filter_edit = self.filter_edit.is_some()
            && matches!(
                self.input_mode,
                InputMode::Category
                    | InputMode::TextPrompt
                    | InputMode::BulkApply
                    | InputMode::Confirm
                    | InputMode::ConfirmApplyFilters
                    | InputMode::TransferNoMatch
            );
        // Declining the "discard this note?" confirm must put the user back in
        // the editor with their text intact, not drop them to Normal.
        let return_to_note_edit =
            self.input_mode == InputMode::Confirm && self.note_editor.is_some();
        self.input_mode = text_prompt_return_mode.unwrap_or(if return_to_note_edit {
            InputMode::Note
        } else if return_to_filter_edit {
            InputMode::FilterEdit
        } else {
            InputMode::Normal
        });
        self.clear_category_popup();
        self.error_message = None;
        self.clear_transfer_mode();
        if !return_to_text_prompt {
            self.clear_text_prompt();
        }
        self.clear_confirm();
        self.bulk_apply = None;
        self.apply_filters_preview = None;
    }

    fn clear_confirm(&mut self) {
        self.confirm_message = None;
        self.confirm_action = None;
    }

    /// Carry out the pending `confirm_action`.
    pub fn confirm_proceed(&mut self) {
        let Some(action) = self.confirm_action.take() else {
            self.cancel_input();
            return;
        };
        self.confirm_message = None;
        self.input_mode = InputMode::Normal;
        match action {
            ConfirmAction::MergeCategory {
                source_id,
                target_id,
            } => {
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
                self.clear_text_prompt();
            }
            ConfirmAction::BreakTransferForCategory {
                transfer_id,
                tx,
                category_path,
            } => {
                if !self.try_mutation("unlink transfer", |s| s.delete_transfer(transfer_id)) {
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
                if applied {
                    self.refresh_data();
                }
            }
            ConfirmAction::UnlinkTransfer { transfer_id } => {
                if self.try_mutation("unlink transfer", |s| s.delete_transfer(transfer_id)) {
                    self.refresh_data();
                }
            }
            ConfirmAction::Uncategorise { tx_id } => {
                if self.try_mutation("remove category", |s| s.delete_enrichment(tx_id)) {
                    self.refresh_data();
                }
            }
            ConfirmAction::DeleteCategory(category_id) => {
                if self.try_mutation("delete category", |s| {
                    s.delete_category(category_id).map(|_| ())
                }) {
                    self.delete_category_after();
                }
            }
            ConfirmAction::DeleteAccount(account_id) => {
                if self.try_mutation("delete account", |s| {
                    s.delete_account(account_id).map(|_| ())
                }) {
                    self.delete_account_after();
                }
            }
            ConfirmAction::DiscardFilterEdit => self.exit_filter_edit(),
            ConfirmAction::DiscardNoteEdit => self.close_note_editor(),
            ConfirmAction::DeleteFilter(filter_id) => {
                if self.try_mutation("delete filter", |s| s.delete_filter(filter_id)) {
                    self.reapply_filters();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::json;
    use tempfile::TempDir;

    use crate::{CategorySource, TransactionStore, TransferSource};

    use super::*;

    #[derive(Clone, Copy)]
    struct FixtureTx {
        description: &'static str,
        amount_cents: i64,
    }

    #[test]
    fn confirm_merge_category_merges_and_returns_to_normal() {
        let (_temp, mut store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        let source_id = store.get_or_create_category("Old").unwrap();
        let target_id = store.get_or_create_category("New").unwrap();
        let tx = tx_by_description(&store, "Coffee");
        store
            .set_category(tx.id, source_id, CategorySource::Manual, true, None)
            .unwrap();

        let mut app = App::new(store).unwrap();
        app.input_mode = InputMode::Confirm;
        app.confirm_message = Some("Merge?".to_string());
        app.confirm_action = Some(ConfirmAction::MergeCategory {
            source_id,
            target_id,
        });

        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.store.get_category(source_id).unwrap().is_none());
        assert_eq!(
            app.store
                .get_transaction_category(tx.id)
                .unwrap()
                .unwrap()
                .id,
            target_id
        );
    }

    /// Type `text` into whichever editor is open, one key at a time.
    fn type_into_editor(app: &mut App, text: &str) {
        for c in text.chars() {
            let key = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            );
            match app.input_mode {
                InputMode::Note => app.handle_note_key(&key),
                InputMode::Tags => app.handle_tag_key(&key),
                other => panic!("no editor open (mode {other:?})"),
            }
        }
    }

    fn press(app: &mut App, code: crossterm::event::KeyCode) {
        let key = crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        match app.input_mode {
            InputMode::Note => app.handle_note_key(&key),
            InputMode::Tags => app.handle_tag_key(&key),
            other => panic!("no editor open (mode {other:?})"),
        }
    }

    fn ctrl_s(app: &mut App) {
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        match app.input_mode {
            InputMode::Note => app.handle_note_key(&key),
            InputMode::Tags => app.handle_tag_key(&key),
            other => panic!("no editor open (mode {other:?})"),
        }
    }

    fn app_with_one_transaction() -> (TempDir, App, i64) {
        let (temp, store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        let tx_id = tx_by_description(&store, "Coffee").id;
        let app = App::new(store).unwrap();
        (temp, app, tx_id)
    }

    #[test]
    fn note_editor_saves_and_caches_the_note() {
        let (_temp, mut app, tx_id) = app_with_one_transaction();

        app.start_note_edit();
        assert_eq!(app.input_mode, InputMode::Note);
        type_into_editor(&mut app, "reimbursable");
        ctrl_s(&mut app);

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.note_editor.is_none());
        assert_eq!(app.get_cached_note(tx_id), Some("reimbursable"));
        assert_eq!(
            app.store.get_note(tx_id).unwrap().as_deref(),
            Some("reimbursable")
        );
    }

    #[test]
    fn reopening_the_note_editor_loads_the_saved_text_and_clearing_removes_it() {
        let (_temp, mut app, tx_id) = app_with_one_transaction();

        app.start_note_edit();
        type_into_editor(&mut app, "first");
        ctrl_s(&mut app);

        app.start_note_edit();
        assert_eq!(app.note_editor.as_ref().unwrap().text(), "first");
        for _ in 0.."first".len() {
            press(&mut app, crossterm::event::KeyCode::Backspace);
        }
        ctrl_s(&mut app);

        assert_eq!(app.get_cached_note(tx_id), None);
        assert_eq!(app.store.get_note(tx_id).unwrap(), None);
    }

    #[test]
    fn escaping_a_dirty_note_confirms_and_declining_returns_to_the_editor() {
        let (_temp, mut app, tx_id) = app_with_one_transaction();

        app.start_note_edit();
        type_into_editor(&mut app, "draft");
        press(&mut app, crossterm::event::KeyCode::Esc);

        // Dirty: the editor must not be thrown away without asking.
        assert_eq!(app.input_mode, InputMode::Confirm);
        assert!(matches!(
            app.confirm_action,
            Some(ConfirmAction::DiscardNoteEdit)
        ));

        cancel_current_confirmation(&mut app);
        assert_eq!(app.input_mode, InputMode::Note);
        assert_eq!(app.note_editor.as_ref().unwrap().text(), "draft");

        // Confirming does discard it.
        press(&mut app, crossterm::event::KeyCode::Esc);
        app.confirm_proceed();
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.note_editor.is_none());
        assert_eq!(app.store.get_note(tx_id).unwrap(), None);
    }

    #[test]
    fn escaping_an_unchanged_note_closes_without_confirming() {
        let (_temp, mut app, _tx_id) = app_with_one_transaction();

        app.start_note_edit();
        press(&mut app, crossterm::event::KeyCode::Esc);

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.confirm_action.is_none());
    }

    #[test]
    fn tag_editor_saves_and_feeds_tag_autocomplete() {
        let (_temp, mut app, tx_id) = app_with_one_transaction();

        app.start_tag_edit();
        assert_eq!(app.input_mode, InputMode::Tags);
        type_into_editor(&mut app, "#work travel");
        press(&mut app, crossterm::event::KeyCode::Enter);

        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.get_cached_tags(tx_id), ["travel", "work"]);
        // A newly created tag must be offered by `tag:` autocomplete at once.
        assert_eq!(app.tag_options().len(), 2);
        assert!(app.tag_options().contains(&"work".to_string()));
    }

    #[test]
    fn clearing_every_tag_removes_it_from_autocomplete() {
        let (_temp, mut app, tx_id) = app_with_one_transaction();

        app.start_tag_edit();
        type_into_editor(&mut app, "work");
        press(&mut app, crossterm::event::KeyCode::Enter);
        assert_eq!(app.tag_options(), ["work".to_string()]);

        app.start_tag_edit();
        for _ in 0.."work ".len() {
            press(&mut app, crossterm::event::KeyCode::Backspace);
        }
        press(&mut app, crossterm::event::KeyCode::Enter);

        assert!(app.get_cached_tags(tx_id).is_empty());
        assert!(app.tag_options().is_empty());
    }

    #[test]
    fn note_footer_hint_says_edit_once_the_row_has_one() {
        let (_temp, mut app, _tx_id) = app_with_one_transaction();
        app.current_tab = Tab::Transactions;
        assert!(crate::tui::keymap::footer_hints(&app).contains(&("n", "note")));

        app.start_note_edit();
        type_into_editor(&mut app, "noted");
        ctrl_s(&mut app);

        assert!(crate::tui::keymap::footer_hints(&app).contains(&("n", "edit note")));
    }

    #[test]
    fn note_and_tag_edits_are_no_ops_without_a_selected_transaction() {
        let (_temp, store) = store_with_transactions(&[]);
        let mut app = App::new(store).unwrap();

        app.start_note_edit();
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.note_editor.is_none());

        app.start_tag_edit();
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.tag_editor.is_none());
    }

    #[test]
    fn cancel_merge_from_rename_restores_rename_prompt() {
        let (_temp, mut store) = store_with_transactions(&[]);
        let source_id = store.get_or_create_category("Old").unwrap();
        store.get_or_create_category("Existing").unwrap();
        let source = store.get_category(source_id).unwrap().unwrap();
        let mut app = App::new(store).unwrap();

        app.confirm_category_rename(source, "Existing".to_string());
        assert_eq!(app.input_mode, InputMode::Confirm);
        assert!(app.confirm_action.is_some());

        cancel_current_confirmation(&mut app);

        assert_eq!(app.input_mode, InputMode::TextPrompt);
        assert_eq!(app.text_prompt_title(), "Rename category");
        assert_eq!(app.text_prompt_value(), "Existing");
        assert!(app.confirm_action.is_none());
    }

    #[test]
    fn confirm_break_transfer_for_category_lands_in_normal() {
        let (_temp, mut store) = store_with_transactions(&[
            FixtureTx {
                description: "Coffee shop",
                amount_cents: -10000,
            },
            FixtureTx {
                description: "Salary deposit",
                amount_cents: 10000,
            },
        ]);
        let tx = tx_by_description(&store, "Coffee shop");
        let other = tx_by_description(&store, "Salary deposit");
        let transfer_id = store
            .create_transfer(tx.id, other.id, TransferSource::Manual, true, None)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.input_mode = InputMode::Confirm;
        app.confirm_message = Some("Break transfer?".to_string());
        app.confirm_action = Some(ConfirmAction::BreakTransferForCategory {
            transfer_id,
            tx: tx.clone(),
            category_path: "Food".to_string(),
        });

        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(
            app.store
                .get_transfer_for_transaction(tx.id)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            app.store
                .get_transaction_category(tx.id)
                .unwrap()
                .unwrap()
                .path,
            "Food"
        );
    }

    #[test]
    fn confirm_break_transfers_for_transfer_lands_in_normal() {
        let (_temp, mut store) = store_with_transactions(&[
            FixtureTx {
                description: "New from",
                amount_cents: -10000,
            },
            FixtureTx {
                description: "New to",
                amount_cents: 10000,
            },
            FixtureTx {
                description: "Old from",
                amount_cents: -20000,
            },
            FixtureTx {
                description: "Old to",
                amount_cents: 20000,
            },
        ]);
        let from = tx_by_description(&store, "New from");
        let to = tx_by_description(&store, "New to");
        let old_from = tx_by_description(&store, "Old from");
        let old_to = tx_by_description(&store, "Old to");
        let first_transfer_id = store
            .create_transfer(from.id, old_to.id, TransferSource::Manual, true, None)
            .unwrap();
        let second_transfer_id = store
            .create_transfer(old_from.id, to.id, TransferSource::Manual, true, None)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.input_mode = InputMode::Confirm;
        app.confirm_message = Some("Break transfers?".to_string());
        app.confirm_action = Some(ConfirmAction::BreakTransfersForTransfer {
            transfer_ids: vec![first_transfer_id, second_transfer_id],
            from_id: from.id,
            to_id: to.id,
        });

        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        let transfer = app
            .store
            .get_transfer_for_transaction(from.id)
            .unwrap()
            .unwrap();
        assert_eq!(transfer.from_transaction_id, from.id);
        assert_eq!(transfer.to_transaction_id, to.id);
        assert!(
            app.store
                .get_transfer_for_transaction(old_from.id)
                .unwrap()
                .is_none()
        );
        assert!(
            app.store
                .get_transfer_for_transaction(old_to.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn confirm_unlink_transfer_lands_in_normal() {
        let (_temp, mut store) = store_with_transactions(&[
            FixtureTx {
                description: "Transfer out",
                amount_cents: -10000,
            },
            FixtureTx {
                description: "Transfer in",
                amount_cents: 10000,
            },
        ]);
        let from = tx_by_description(&store, "Transfer out");
        let to = tx_by_description(&store, "Transfer in");
        let transfer_id = store
            .create_transfer(from.id, to.id, TransferSource::Manual, true, None)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.input_mode = InputMode::Confirm;
        app.confirm_message = Some("Unlink?".to_string());
        app.confirm_action = Some(ConfirmAction::UnlinkTransfer { transfer_id });

        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(
            app.store
                .get_transfer_for_transaction(from.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn unlinking_transfer_clears_indicator_despite_stale_transfer_review_list() {
        let (_temp, mut store) = store_with_transactions(&[
            FixtureTx {
                description: "Move out",
                amount_cents: -10000,
            },
            FixtureTx {
                description: "Move in",
                amount_cents: 10000,
            },
        ]);
        let from = tx_by_description(&store, "Move out");
        let to = tx_by_description(&store, "Move in");
        // An unconfirmed (auto) transfer surfaces on the Transfer Review subtab.
        store
            .create_transfer(from.id, to.id, TransferSource::Auto, false, Some(0.9))
            .unwrap();

        let mut app = App::new(store).unwrap();
        // Visiting Transfer Review loads `transfer_reviews` into memory.
        app.current_tab = Tab::Todo;
        app.todo_subtab = TodoSubTab::TransferReview;
        app.reload_current_tab();
        assert_eq!(app.lists.transfer_reviews.len(), 1);

        // Back on Transactions, the leg correctly reads as a transfer.
        app.current_tab = Tab::Transactions;
        app.reload_current_tab();
        let transfer_id = app
            .get_cached_transfer(from.id)
            .expect("leg reads as a transfer")
            .id;

        // Unlink it. `transfer_reviews` is NOT reloaded while the Transactions
        // tab is active, so it still holds the now-deleted transfer — the cache
        // must reflect the DB, not the stale list.
        app.input_mode = InputMode::Confirm;
        app.confirm_action = Some(ConfirmAction::UnlinkTransfer { transfer_id });
        app.confirm_proceed();

        assert!(app.get_cached_transfer(from.id).is_none());
        assert!(app.get_cached_transfer(to.id).is_none());
    }

    #[test]
    fn bulk_categorise_matching_opens_bulk_apply_and_excludes_transfer_legs() {
        // Four transactions share the FTS term "shop"; two are plain, two are a
        // transfer pair. The bulk apply must categorise the plain ones and skip
        // the transfer legs.
        let (_temp, mut store) = store_with_transactions(&[
            FixtureTx {
                description: "shop groceries",
                amount_cents: -1200,
            },
            FixtureTx {
                description: "shop hardware",
                amount_cents: -3400,
            },
            FixtureTx {
                description: "shop transfer out",
                amount_cents: -5000,
            },
            FixtureTx {
                description: "shop transfer in",
                amount_cents: 5000,
            },
        ]);
        let plain1 = tx_by_description(&store, "shop groceries");
        let plain2 = tx_by_description(&store, "shop hardware");
        let from = tx_by_description(&store, "shop transfer out");
        let to = tx_by_description(&store, "shop transfer in");
        store
            .create_transfer(from.id, to.id, TransferSource::Manual, true, None)
            .unwrap();

        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Transactions;
        app.reload_current_tab();

        // Apply a DB search that matches all four rows.
        app.start_db_search();
        for c in "shop".chars() {
            app.handle_db_search_input(tui_input::InputRequest::InsertChar(c));
        }
        app.confirm_db_search();

        // Drive the bulk-categorise popup: open it, type a category, confirm.
        app.start_bulk_categorise_matching();
        assert_eq!(app.category_target, CategoryTarget::MatchingTransactions);
        for c in "Shopping".chars() {
            app.update_category_input(c);
        }
        app.confirm_category();

        // Lands in the bulk-apply checkbox modal (not a yes/no Confirm).
        assert_eq!(app.input_mode, InputMode::BulkApply);
        let state = app.bulk_apply.as_ref().expect("bulk apply open");
        assert!(matches!(
            &state.action,
            BulkAction::ApplyCategory { category_path } if category_path == "Shopping"
        ));
        let ids: Vec<i64> = state.rows.iter().map(BulkRow::target_id).collect();
        assert!(ids.contains(&plain1.id));
        assert!(ids.contains(&plain2.id));
        assert!(!ids.contains(&from.id));
        assert!(!ids.contains(&to.id));

        app.bulk_apply_confirm();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(
            app.store
                .get_transaction_category(plain1.id)
                .unwrap()
                .unwrap()
                .path,
            "Shopping"
        );
        assert_eq!(
            app.store
                .get_transaction_category(plain2.id)
                .unwrap()
                .unwrap()
                .path,
            "Shopping"
        );
        assert!(
            app.store
                .get_transaction_category(from.id)
                .unwrap()
                .is_none()
        );
        assert!(app.store.get_transaction_category(to.id).unwrap().is_none());
    }

    #[test]
    fn start_accept_matching_builds_rows_for_ai_and_transfer_review_only() {
        let (_temp, mut store) = store_with_transactions(&[
            FixtureTx {
                description: "AI coffee",
                amount_cents: -450,
            },
            FixtureTx {
                description: "AI lunch",
                amount_cents: -1200,
            },
            FixtureTx {
                description: "Xfer out",
                amount_cents: -5000,
            },
            FixtureTx {
                description: "Xfer in",
                amount_cents: 5000,
            },
        ]);
        let coffee = tx_by_description(&store, "AI coffee");
        let lunch = tx_by_description(&store, "AI lunch");
        let from = tx_by_description(&store, "Xfer out");
        let to = tx_by_description(&store, "Xfer in");
        let cat = store.get_or_create_category("Food").unwrap();
        for id in [coffee.id, lunch.id] {
            store
                .set_category(id, cat, CategorySource::Ai, false, Some(0.9))
                .unwrap();
        }
        let transfer_id = store
            .create_transfer(from.id, to.id, TransferSource::Auto, false, Some(0.8))
            .unwrap();

        let mut app = App::new(store).unwrap();

        // AI Review → ConfirmCategories rows.
        app.current_tab = Tab::Todo;
        app.todo_subtab = TodoSubTab::AiReview;
        app.reload_current_tab();
        app.start_accept_matching();
        assert_eq!(app.input_mode, InputMode::BulkApply);
        let state = app.bulk_apply.as_ref().unwrap();
        assert!(matches!(state.action, BulkAction::ConfirmCategories));
        let ids: Vec<i64> = state.rows.iter().map(BulkRow::target_id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&coffee.id) && ids.contains(&lunch.id));
        app.bulk_apply_cancel();

        // Transfer Review → ConfirmTransfers rows.
        app.todo_subtab = TodoSubTab::TransferReview;
        app.reload_current_tab();
        app.start_accept_matching();
        assert_eq!(app.input_mode, InputMode::BulkApply);
        let state = app.bulk_apply.as_ref().unwrap();
        assert!(matches!(state.action, BulkAction::ConfirmTransfers));
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].target_id(), transfer_id);
        app.bulk_apply_cancel();

        // No-op on Transactions / Uncategorised.
        for (tab, subtab) in [
            (Tab::Transactions, TodoSubTab::Uncategorised),
            (Tab::Todo, TodoSubTab::Uncategorised),
        ] {
            app.current_tab = tab;
            app.todo_subtab = subtab;
            app.start_accept_matching();
            assert!(
                app.bulk_apply.is_none(),
                "expected no-op on {tab:?}/{subtab:?}"
            );
            assert_eq!(app.input_mode, InputMode::Normal);
        }
    }

    #[test]
    fn bulk_apply_confirm_applies_selected_only_for_each_action() {
        // Table-driven over the three BulkActions: deselect the middle row,
        // confirm, and assert only selected rows are mutated.
        #[derive(Clone, Copy)]
        enum Flow {
            ApplyCategory,
            ConfirmCategories,
            ConfirmTransfers,
        }

        for flow in [
            Flow::ApplyCategory,
            Flow::ConfirmCategories,
            Flow::ConfirmTransfers,
        ] {
            match flow {
                Flow::ApplyCategory | Flow::ConfirmCategories => {
                    let (_temp, mut store) = store_with_transactions(&[
                        FixtureTx {
                            description: "row a",
                            amount_cents: -100,
                        },
                        FixtureTx {
                            description: "row b",
                            amount_cents: -200,
                        },
                        FixtureTx {
                            description: "row c",
                            amount_cents: -300,
                        },
                    ]);
                    let a = tx_by_description(&store, "row a");
                    let b = tx_by_description(&store, "row b");
                    let c = tx_by_description(&store, "row c");
                    if matches!(flow, Flow::ConfirmCategories) {
                        let cat = store.get_or_create_category("Food").unwrap();
                        for id in [a.id, b.id, c.id] {
                            store
                                .set_category(id, cat, CategorySource::Ai, false, Some(0.7))
                                .unwrap();
                        }
                    }

                    let mut app = App::new(store).unwrap();
                    match flow {
                        Flow::ApplyCategory => {
                            app.bulk_apply = Some(BulkApplyState {
                                action: BulkAction::ApplyCategory {
                                    category_path: "Food".into(),
                                },
                                rows: [a.clone(), b.clone(), c.clone()]
                                    .into_iter()
                                    .map(|tx| BulkRow {
                                        selected: true,
                                        item: BulkItem::Transaction { tx, score: None },
                                    })
                                    .collect(),
                                cursor: 0,
                            });
                            app.input_mode = InputMode::BulkApply;
                        }
                        Flow::ConfirmCategories => {
                            app.current_tab = Tab::Todo;
                            app.todo_subtab = TodoSubTab::AiReview;
                            app.reload_current_tab();
                            app.start_accept_matching();
                        }
                        Flow::ConfirmTransfers => unreachable!(),
                    }

                    let state = app.bulk_apply.as_mut().unwrap();
                    assert_eq!(state.rows.len(), 3);
                    state.rows[1].selected = false;
                    let skipped_id = state.rows[1].target_id();
                    app.bulk_apply_confirm();

                    match flow {
                        Flow::ApplyCategory => {
                            assert_eq!(
                                app.store
                                    .get_transaction_category(a.id)
                                    .unwrap()
                                    .unwrap()
                                    .path,
                                "Food"
                            );
                            assert!(
                                app.store
                                    .get_transaction_category(skipped_id)
                                    .unwrap()
                                    .is_none()
                            );
                            assert_eq!(
                                app.store
                                    .get_transaction_category(c.id)
                                    .unwrap()
                                    .unwrap()
                                    .path,
                                "Food"
                            );
                        }
                        Flow::ConfirmCategories => {
                            // Confirmed rows leave the pending-AI list; skipped stays.
                            let pending: Vec<i64> = app
                                .store
                                .get_pending_ai_reviews(&ParsedQuery::empty(), None)
                                .unwrap()
                                .into_iter()
                                .map(|r| r.transaction.id)
                                .collect();
                            assert_eq!(pending, vec![skipped_id]);
                        }
                        Flow::ConfirmTransfers => unreachable!(),
                    }
                }
                Flow::ConfirmTransfers => {
                    let (_temp, mut store) = store_with_transactions(&[
                        FixtureTx {
                            description: "out1",
                            amount_cents: -1000,
                        },
                        FixtureTx {
                            description: "in1",
                            amount_cents: 1000,
                        },
                        FixtureTx {
                            description: "out2",
                            amount_cents: -2000,
                        },
                        FixtureTx {
                            description: "in2",
                            amount_cents: 2000,
                        },
                        FixtureTx {
                            description: "out3",
                            amount_cents: -3000,
                        },
                        FixtureTx {
                            description: "in3",
                            amount_cents: 3000,
                        },
                    ]);
                    let transfer_ids: Vec<i64> = ["1", "2", "3"]
                        .into_iter()
                        .map(|n| {
                            let out = tx_by_description(&store, &format!("out{n}"));
                            let inn = tx_by_description(&store, &format!("in{n}"));
                            store
                                .create_transfer(
                                    out.id,
                                    inn.id,
                                    TransferSource::Auto,
                                    false,
                                    Some(0.8),
                                )
                                .unwrap()
                        })
                        .collect();

                    let mut app = App::new(store).unwrap();
                    app.current_tab = Tab::Todo;
                    app.todo_subtab = TodoSubTab::TransferReview;
                    app.reload_current_tab();
                    app.start_accept_matching();
                    let state = app.bulk_apply.as_mut().unwrap();
                    // Order is created_at DESC, so rows may not match insert order —
                    // deselect by id instead.
                    let skipped = transfer_ids[1];
                    for row in &mut state.rows {
                        if row.target_id() == skipped {
                            row.selected = false;
                        }
                    }
                    app.bulk_apply_confirm();

                    let pending: Vec<i64> = app
                        .store
                        .get_pending_transfer_reviews(&ParsedQuery::empty(), None)
                        .unwrap()
                        .into_iter()
                        .map(|t| t.id)
                        .collect();
                    assert_eq!(pending, vec![skipped]);
                }
            }
        }
    }

    #[test]
    fn confirm_uncategorise_lands_in_normal() {
        let (_temp, mut store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        let tx = tx_by_description(&store, "Coffee");
        let category_id = store.get_or_create_category("Food").unwrap();
        store
            .set_category(tx.id, category_id, CategorySource::Manual, true, None)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.input_mode = InputMode::Confirm;
        app.confirm_message = Some("Uncategorise?".to_string());
        app.confirm_action = Some(ConfirmAction::Uncategorise { tx_id: tx.id });

        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.store.get_transaction_category(tx.id).unwrap().is_none());
    }

    #[test]
    fn confirm_delete_category_lands_in_normal() {
        let (_temp, mut store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        let tx = tx_by_description(&store, "Coffee");
        let category_id = store.get_or_create_category("Food").unwrap();
        store
            .set_category(tx.id, category_id, CategorySource::Manual, true, None)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.input_mode = InputMode::Confirm;
        app.confirm_message = Some("Delete category?".to_string());
        app.confirm_action = Some(ConfirmAction::DeleteCategory(category_id));

        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.store.get_category(category_id).unwrap().is_none());
        assert!(app.store.get_transaction_category(tx.id).unwrap().is_none());
    }

    #[test]
    fn confirm_discard_filter_edit_lands_in_normal() {
        let (_temp, mut store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        store.create_filter("Coffee", "Coffee").unwrap();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Filters;
        app.open_filter_edit();
        app.input_mode = InputMode::Confirm;
        app.confirm_message = Some("Discard?".to_string());
        app.confirm_action = Some(ConfirmAction::DiscardFilterEdit);

        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.filter_edit.is_none());
    }

    #[test]
    fn confirm_delete_filter_lands_in_normal() {
        let (_temp, mut store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        let filter_id = store.create_filter("Coffee", "Coffee").unwrap();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Filters;
        app.selected_index = 0;
        app.input_mode = InputMode::Confirm;
        app.confirm_message = Some("Delete filter?".to_string());
        app.confirm_action = Some(ConfirmAction::DeleteFilter(filter_id));

        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.store.list_filters().unwrap().is_empty());
    }

    #[test]
    fn toggle_category_transactions_loads_and_clears() {
        let (_temp, mut store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        let tx = tx_by_description(&store, "Coffee");
        let category_id = store.get_or_create_category("Food").unwrap();
        store
            .set_category(tx.id, category_id, CategorySource::Manual, true, None)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Categories;
        app.refresh_data();
        app.move_cursor_to_category("Food");

        app.toggle_category_transactions();
        assert!(app.show_category_transactions);
        assert_eq!(app.category_transactions.len(), 1);
        assert_eq!(app.category_transactions[0].id, tx.id);

        app.toggle_category_transactions();
        assert!(!app.show_category_transactions);
        assert!(app.category_transactions.is_empty());
    }

    #[test]
    fn manage_category_transactions_switches_to_filtered_transactions() {
        let (_temp, mut store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        let tx = tx_by_description(&store, "Coffee");
        let category_id = store.get_or_create_category("Food").unwrap();
        store
            .set_category(tx.id, category_id, CategorySource::Manual, true, None)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Categories;
        app.refresh_data();
        app.move_cursor_to_category("Food");

        app.manage_category_transactions();

        assert_eq!(app.current_tab, Tab::Transactions);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.db_search_active());
        assert_eq!(app.db_search_value(), "category:Food");
    }

    fn cancel_current_confirmation(app: &mut App) {
        match app.input_mode {
            InputMode::Confirm => app.cancel_input(),
            mode => panic!("expected confirmation mode, got {mode:?}"),
        }
    }

    fn store_with_transactions(rows: &[FixtureTx]) -> (TempDir, TransactionStore) {
        let temp = TempDir::new().unwrap();
        let account_dir = temp.path().join("TestBank").join("Checking");
        fs::create_dir_all(&account_dir).unwrap();
        fs::write(account_dir.join("transactions.csv"), "fixture\n").unwrap();

        let imported: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(idx, tx)| {
                json!({
                    "date": format!("2025-01-{:02}", idx + 1),
                    "description": tx.description,
                    "amount_cents": tx.amount_cents,
                    "balance_cents": 50000 + tx.amount_cents,
                    "hash": format!("fixture-{idx}"),
                })
            })
            .collect();
        let payload = serde_json::to_string(&imported).unwrap();
        let import_script = account_dir.join("import");
        fs::write(
            &import_script,
            format!("#!/usr/bin/env bash\ncat <<'JSON'\n{payload}\nJSON\n"),
        )
        .unwrap();
        make_executable(&import_script);

        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();
        (temp, store)
    }

    fn tx_by_description(store: &TransactionStore, description: &str) -> Transaction {
        store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap()
            .into_iter()
            .find(|tx| tx.description == description)
            .unwrap_or_else(|| panic!("missing transaction {description:?}"))
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

    #[test]
    fn background_job_request_skipped_while_another_is_active() {
        // Every ordered pair of distinct jobs: the running one blocks the
        // attempted one with a uniform "<running> in progress" status and
        // leaves active_job unchanged (covers the former refresh↔classify gap).
        let jobs = [
            BackgroundJob::Refresh,
            BackgroundJob::Classify,
            BackgroundJob::Reindex,
        ];
        for running in jobs {
            for attempted in jobs {
                if running == attempted {
                    continue;
                }
                let (_temp, store) = store_with_transactions(&[]);
                let mut app = App::new(store).unwrap();
                app.active_job = Some(running);

                match attempted {
                    BackgroundJob::Refresh => app.request_refresh(),
                    BackgroundJob::Classify => app.request_classify(),
                    BackgroundJob::Reindex => app.reindex_fts(),
                }

                assert_eq!(app.active_job, Some(running));
                assert!(app.job_rx.is_none());
                assert!(app.error_message.is_none());
                let expected = format!("{} in progress", running.gerund());
                assert_eq!(
                    app.active_status(),
                    Some(expected.as_str()),
                    "running={running:?} attempted={attempted:?}"
                );
            }
        }
    }

    #[test]
    fn reindex_fts_rebuilds_when_idle() {
        let (_temp, store) = store_with_transactions(&[
            FixtureTx {
                description: "Coffee",
                amount_cents: -450,
            },
            FixtureTx {
                description: "Rent",
                amount_cents: -120000,
            },
        ]);
        let mut app = App::new(store).unwrap();
        assert!(app.active_job.is_none());

        app.reindex_fts();

        assert!(app.active_job.is_none());
        assert!(app.error_message.is_none());
        assert_eq!(app.active_status(), Some("Reindexed 2 transactions"));
    }

    #[test]
    fn finish_reindex_ok_clears_flag_and_sets_status() {
        let (_temp, store) = store_with_transactions(&[FixtureTx {
            description: "Coffee",
            amount_cents: -450,
        }]);
        let mut app = App::new(store).unwrap();
        app.active_job = Some(BackgroundJob::Reindex);

        app.finish_reindex(Ok(1));

        assert!(app.active_job.is_none());
        assert!(app.error_message.is_none());
        assert_eq!(app.active_status(), Some("Reindexed 1 transactions"));
    }

    #[test]
    fn finish_reindex_err_clears_flag_and_sets_error() {
        let (_temp, store) = store_with_transactions(&[]);
        let mut app = App::new(store).unwrap();
        app.active_job = Some(BackgroundJob::Reindex);

        app.finish_reindex(Err(crate::Error::ImportFailed(
            "simulated reindex failure".into(),
        )));

        assert!(app.active_job.is_none());
        assert!(app.active_status().is_none());
        let msg = app.error_message.as_deref().unwrap_or("");
        assert!(
            msg.contains("Failed to reindex full-text search"),
            "unexpected error_message: {msg:?}"
        );
    }

    #[test]
    fn dismiss_error_clears_message_when_present() {
        let (_temp, store) = store_with_transactions(&[]);
        let mut app = App::new(store).unwrap();
        app.error_message = Some("Database is locked".to_string());

        assert!(app.dismiss_error());
        assert!(app.error_message.is_none());
        // Second dismiss is a no-op.
        assert!(!app.dismiss_error());
        assert!(app.error_message.is_none());
    }

    #[test]
    fn dismiss_error_returns_false_when_none() {
        let (_temp, store) = store_with_transactions(&[]);
        let mut app = App::new(store).unwrap();
        assert!(app.error_message.is_none());
        assert!(!app.dismiss_error());
    }
}
