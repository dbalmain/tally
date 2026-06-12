//! Tab definitions and per-tab list dispatch.
//!
//! Every piece of per-tab behaviour — what data backs a tab, how it reloads,
//! how fuzzy filtering matches its rows, which tabs expose plain transactions
//! — lives in this file. Adding a tab means: extend the enums, add a
//! `FilteredList` field, extend each `TabLists` method, then add a draw
//! function in `ui.rs` (see "Adding a New Tab" in CLAUDE.md).

use std::collections::HashMap;

use crate::search::ParsedQuery;
use crate::tui::filtered_list::FilteredList;
use crate::{
    Category, FuzzyMatcher, Result, Transaction, TransactionStore, TransactionWithEnrichment,
    Transfer, TransferWithTransactions,
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

/// Key for per-tab search state storage. The subtab is only meaningful on the
/// Todo tab (where each subtab gets its own state); all other tabs use `None`
/// so that switching the Todo subtab while on, say, Transactions doesn't
/// silently fork the saved search state.
pub type TabKey = (Tab, Option<TodoSubTab>);

pub(super) fn tab_key(tab: Tab, subtab: TodoSubTab) -> TabKey {
    match tab {
        Tab::Todo => (Tab::Todo, Some(subtab)),
        other => (other, None),
    }
}

/// Human-readable name for a tab key, for error messages.
pub(super) fn tab_title(key: TabKey) -> &'static str {
    match key {
        (Tab::Todo, Some(sub)) => sub.title(),
        (tab, _) => tab.title(),
    }
}

/// The data behind every tab. `ui.rs` reads the typed lists directly; all
/// key-driven behaviour goes through the dispatch methods below so that
/// per-tab logic stays in this file.
pub struct TabLists {
    pub transactions: FilteredList<Transaction>,
    pub linked_transfers: FilteredList<TransferWithTransactions>,
    pub categories: Vec<Category>,
    pub uncategorised: FilteredList<Transaction>,
    pub ai_reviews: FilteredList<TransactionWithEnrichment>,
    pub transfer_reviews: FilteredList<Transfer>,
}

impl TabLists {
    /// Load every tab's data with no filters applied.
    pub(super) fn load(store: &TransactionStore, limit: Option<usize>) -> Result<Self> {
        let query = ParsedQuery::empty();
        Ok(Self {
            transactions: FilteredList::new(store.query_transactions(&query, limit)?),
            linked_transfers: FilteredList::new(
                store.list_transfers_with_transactions(true, &query, limit)?,
            ),
            categories: store.list_categories()?,
            uncategorised: FilteredList::new(store.get_uncategorised_transactions(&query, limit)?),
            ai_reviews: FilteredList::new(store.get_pending_ai_reviews(&query, limit)?),
            transfer_reviews: FilteredList::new(store.get_pending_transfer_reviews(&query, limit)?),
        })
    }

    /// Reload the list behind `key` from the database with the given search
    /// query. On error the existing items are left untouched, so the caller
    /// can surface the error without the user losing their place.
    /// (Categories ignore search queries; they reload via
    /// `App::reload_categories`.)
    pub(super) fn reload(
        &mut self,
        key: TabKey,
        store: &TransactionStore,
        query: &ParsedQuery,
        limit: Option<usize>,
    ) -> Result<()> {
        match key {
            (Tab::Transactions, _) => self
                .transactions
                .set_items(store.query_transactions(query, limit)?),
            (Tab::Transfers, _) => self
                .linked_transfers
                .set_items(store.list_transfers_with_transactions(true, query, limit)?),
            (Tab::Categories, _) => {}
            (Tab::Todo, sub) => match sub.unwrap_or(TodoSubTab::Uncategorised) {
                TodoSubTab::Uncategorised => self
                    .uncategorised
                    .set_items(store.get_uncategorised_transactions(query, limit)?),
                TodoSubTab::AiReview => self
                    .ai_reviews
                    .set_items(store.get_pending_ai_reviews(query, limit)?),
                TodoSubTab::TransferReview => self
                    .transfer_reviews
                    .set_items(store.get_pending_transfer_reviews(query, limit)?),
            },
        }
        Ok(())
    }

    /// Number of visible rows in the list behind `key`.
    pub(super) fn len(&self, key: TabKey) -> usize {
        match key {
            (Tab::Transactions, _) => self.transactions.len(),
            (Tab::Transfers, _) => self.linked_transfers.len(),
            (Tab::Categories, _) => self.categories.len(),
            (Tab::Todo, sub) => match sub.unwrap_or(TodoSubTab::Uncategorised) {
                TodoSubTab::Uncategorised => self.uncategorised.len(),
                TodoSubTab::AiReview => self.ai_reviews.len(),
                TodoSubTab::TransferReview => self.transfer_reviews.len(),
            },
        }
    }

    /// Re-apply the fuzzy `pattern` to the list behind `key`. An empty
    /// pattern shows all rows. `tx_by_id` resolves the transaction IDs held
    /// by transfer reviews.
    pub(super) fn apply_fuzzy(
        &mut self,
        key: TabKey,
        pattern: &str,
        matcher: &mut FuzzyMatcher,
        tx_by_id: &HashMap<i64, Transaction>,
    ) {
        let mut any_match =
            |fields: &[&str]| fields.iter().any(|f| matcher.fuzzy_matches(pattern, f));

        match key {
            (Tab::Transactions, _) => refilter_by(&mut self.transactions, pattern, |tx| {
                any_match(&[&tx.description])
            }),
            (Tab::Transfers, _) => refilter_by(&mut self.linked_transfers, pattern, |twt| {
                any_match(&[
                    &twt.from_transaction.description,
                    &twt.to_transaction.description,
                ])
            }),
            (Tab::Categories, _) => {
                // Categories don't use fuzzy filtering (for now)
            }
            (Tab::Todo, sub) => match sub.unwrap_or(TodoSubTab::Uncategorised) {
                TodoSubTab::Uncategorised => refilter_by(&mut self.uncategorised, pattern, |tx| {
                    any_match(&[&tx.description])
                }),
                TodoSubTab::AiReview => refilter_by(&mut self.ai_reviews, pattern, |r| {
                    any_match(&[&r.transaction.description])
                }),
                TodoSubTab::TransferReview => {
                    refilter_by(&mut self.transfer_reviews, pattern, |tr| {
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
                    })
                }
            },
        }
    }

    /// The transaction at visible index `idx`, for tabs whose rows are plain
    /// transactions (where category/transfer actions apply).
    pub(super) fn transaction_at(&self, key: TabKey, idx: usize) -> Option<&Transaction> {
        match key {
            (Tab::Transactions, _) => self.transactions.get(idx),
            (Tab::Todo, Some(TodoSubTab::Uncategorised)) => self.uncategorised.get(idx),
            _ => None,
        }
    }

    /// Visible position of the transaction with `tx_id`, for the same tabs
    /// as [`Self::transaction_at`].
    pub(super) fn position_of_tx(&self, key: TabKey, tx_id: i64) -> Option<usize> {
        match key {
            (Tab::Transactions, _) => self.transactions.position(|tx| tx.id == tx_id),
            (Tab::Todo, Some(TodoSubTab::Uncategorised)) => {
                self.uncategorised.position(|tx| tx.id == tx_id)
            }
            _ => None,
        }
    }
}

/// Apply `visible` as the list's filter, or show everything when the pattern
/// is empty.
fn refilter_by<T>(list: &mut FilteredList<T>, pattern: &str, visible: impl FnMut(&T) -> bool) {
    if pattern.is_empty() {
        list.show_all();
    } else {
        list.refilter(visible);
    }
}
