//! Tab definitions and per-tab list dispatch.
//!
//! Every piece of per-tab behaviour — what data backs a tab, how it reloads,
//! how fuzzy filtering matches its rows, which tabs expose selectable
//! transactions — lives in this file. Adding a tab means: extend the enums, add
//! a `FilteredList` field, extend each `TabLists` method, then add a draw
//! function in `ui.rs` (see "Adding a New Tab" in `.ai/core.md`).

use std::collections::HashMap;

use crate::search::ParsedQuery;
use crate::tui::filtered_list::FilteredList;
use crate::{
    AccountWithBank, Category, Filter, FuzzyMatcher, Result, Transaction, TransactionStore,
    TransactionWithEnrichment, Transfer, TransferWithTransactions,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Todo,
    Transactions,
    Categories,
    Accounts,
    Transfers,
    Filters,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[
            Tab::Todo,
            Tab::Transactions,
            Tab::Categories,
            Tab::Accounts,
            Tab::Transfers,
            Tab::Filters,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Todo => "Todo",
            Tab::Transactions => "Transactions",
            Tab::Categories => "Categories",
            Tab::Accounts => "Accounts",
            Tab::Transfers => "Transfers",
            Tab::Filters => "Filters",
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
    pub categories: FilteredList<Category>,
    pub accounts: FilteredList<AccountWithBank>,
    pub uncategorised: FilteredList<Transaction>,
    pub ai_reviews: FilteredList<TransactionWithEnrichment>,
    pub transfer_reviews: FilteredList<Transfer>,
    pub filters: FilteredList<Filter>,
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
            categories: FilteredList::new(store.list_categories()?),
            accounts: FilteredList::new(store.list_accounts_with_bank()?),
            uncategorised: FilteredList::new(store.get_uncategorised_transactions(&query, limit)?),
            ai_reviews: FilteredList::new(store.get_pending_ai_reviews(&query, limit)?),
            transfer_reviews: FilteredList::new(store.get_pending_transfer_reviews(&query, limit)?),
            filters: FilteredList::new(store.list_filters()?),
        })
    }

    /// Reload the list behind `key` from the database with the given search
    /// query. On error the existing items are left untouched, so the caller
    /// can surface the error without the user losing their place.
    /// (Categories ignore search queries here; they reload via
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
            (Tab::Accounts, _) => {}
            (Tab::Filters, _) => self.filters.set_items(store.list_filters()?),
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
            (Tab::Accounts, _) => self.accounts.len(),
            (Tab::Filters, _) => self.filters.len(),
            (Tab::Todo, sub) => match sub.unwrap_or(TodoSubTab::Uncategorised) {
                TodoSubTab::Uncategorised => self.uncategorised.len(),
                TodoSubTab::AiReview => self.ai_reviews.len(),
                TodoSubTab::TransferReview => self.transfer_reviews.len(),
            },
        }
    }

    /// Re-apply in-memory filters to the list behind `key`. Non-Categories
    /// tabs ignore `db_query` because their DB search has already been pushed
    /// to SQL during reload; Categories and Accounts apply `db_query` as an
    /// in-memory boundary-prefix filter over the path, then layer fuzzy
    /// matching over it. `tx_by_id` resolves the transaction IDs held by
    /// transfer reviews.
    pub(super) fn apply_fuzzy(
        &mut self,
        key: TabKey,
        db_query: &str,
        pattern: &str,
        matcher: &mut FuzzyMatcher,
        tx_by_id: &HashMap<i64, Transaction>,
    ) {
        let mut any_match = |pattern: &str, fields: &[&str]| {
            fields.iter().any(|f| matcher.fuzzy_matches(pattern, f))
        };

        match key {
            (Tab::Transactions, _) => refilter_by(&mut self.transactions, pattern, |tx| {
                any_match(pattern, &[&tx.description])
            }),
            (Tab::Transfers, _) => refilter_by(&mut self.linked_transfers, pattern, |twt| {
                any_match(
                    pattern,
                    &[
                        &twt.from_transaction.description,
                        &twt.to_transaction.description,
                    ],
                )
            }),
            (Tab::Categories, _) => self
                .categories
                .refilter(|c| boundary_prefix(&c.path, db_query) && any_match(pattern, &[&c.path])),
            (Tab::Accounts, _) => self
                .accounts
                .refilter(|a| boundary_prefix(&a.path, db_query) && any_match(pattern, &[&a.path])),
            (Tab::Filters, _) => self.filters.refilter(|filter| {
                [db_query, pattern]
                    .into_iter()
                    .all(|p| p.is_empty() || any_match(p, &[&filter.name, &filter.query]))
            }),
            (Tab::Todo, sub) => match sub.unwrap_or(TodoSubTab::Uncategorised) {
                TodoSubTab::Uncategorised => refilter_by(&mut self.uncategorised, pattern, |tx| {
                    any_match(pattern, &[&tx.description])
                }),
                TodoSubTab::AiReview => refilter_by(&mut self.ai_reviews, pattern, |r| {
                    any_match(pattern, &[&r.transaction.description])
                }),
                TodoSubTab::TransferReview => {
                    refilter_by(&mut self.transfer_reviews, pattern, |tr| {
                        match (
                            tx_by_id.get(&tr.from_transaction_id),
                            tx_by_id.get(&tr.to_transaction_id),
                        ) {
                            (Some(from), Some(to)) => {
                                any_match(pattern, &[&from.description, &to.description])
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
    /// transactions or wrap one directly (where category/transfer actions
    /// apply).
    pub(super) fn transaction_at(&self, key: TabKey, idx: usize) -> Option<&Transaction> {
        match key {
            (Tab::Transactions, _) => self.transactions.get(idx),
            (Tab::Todo, Some(TodoSubTab::Uncategorised)) => self.uncategorised.get(idx),
            (Tab::Todo, Some(TodoSubTab::AiReview)) => {
                self.ai_reviews.get(idx).map(|r| &r.transaction)
            }
            _ => None,
        }
    }

    /// Visible position of the transaction with `tx_id`, for the same tabs and
    /// transaction-wrapping rows as [`Self::transaction_at`].
    pub(super) fn position_of_tx(&self, key: TabKey, tx_id: i64) -> Option<usize> {
        match key {
            (Tab::Transactions, _) => self.transactions.position(|tx| tx.id == tx_id),
            (Tab::Todo, Some(TodoSubTab::Uncategorised)) => {
                self.uncategorised.position(|tx| tx.id == tx_id)
            }
            (Tab::Todo, Some(TodoSubTab::AiReview)) => {
                self.ai_reviews.position(|r| r.transaction.id == tx_id)
            }
            _ => None,
        }
    }
}

/// True if `query` is a case-insensitive prefix of `path` starting at any
/// boundary: the start of the string or immediately after a non-alphanumeric
/// character. Empty query matches everything.
fn boundary_prefix(path: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }

    let path_lower = path.to_lowercase();
    let query_lower = query.to_lowercase();
    let is_boundary = |start: usize| {
        start == 0
            || !path_lower[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric())
    };

    path_lower
        .char_indices()
        .any(|(i, _)| is_boundary(i) && path_lower[i..].starts_with(&query_lower))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_prefix_matches_start_of_path() {
        assert!(boundary_prefix("tododo", "todo"));
    }

    #[test]
    fn boundary_prefix_matches_after_slash() {
        assert!(boundary_prefix("tax/todos", "todo"));
    }

    #[test]
    fn boundary_prefix_matches_after_hyphen() {
        assert!(boundary_prefix("Personal-todos", "todo"));
    }

    #[test]
    fn boundary_prefix_does_not_match_after_digit() {
        assert!(!boundary_prefix("Food2todo", "todo"));
    }

    #[test]
    fn boundary_prefix_does_not_match_inside_segment() {
        assert!(!boundary_prefix("AllTax/Larbert", "tax"));
    }

    #[test]
    fn boundary_prefix_is_case_insensitive() {
        assert!(boundary_prefix("AllTax/Larbert", "lar"));
    }

    #[test]
    fn boundary_prefix_empty_query_matches() {
        assert!(boundary_prefix("AllTax/Larbert", ""));
    }

    fn account(id: i64, path: &str) -> AccountWithBank {
        let (bank_name, name) = path.split_once('/').unwrap();
        AccountWithBank {
            id,
            bank_id: 1,
            bank_name: bank_name.to_string(),
            name: name.to_string(),
            path: path.to_string(),
        }
    }

    fn lists_with_accounts(paths: &[(i64, &str)]) -> TabLists {
        let accounts = paths.iter().map(|(id, path)| account(*id, path)).collect();
        TabLists {
            transactions: FilteredList::default(),
            linked_transfers: FilteredList::default(),
            categories: FilteredList::default(),
            accounts: FilteredList::new(accounts),
            uncategorised: FilteredList::default(),
            ai_reviews: FilteredList::default(),
            transfer_reviews: FilteredList::default(),
            filters: FilteredList::default(),
        }
    }

    #[test]
    fn accounts_db_search_is_boundary_prefix_over_path() {
        let mut lists = lists_with_accounts(&[(1, "ING/Orange"), (2, "TB/Checking")]);
        let mut matcher = FuzzyMatcher::new();
        let by_id = HashMap::new();

        // `/`-search (db_query) matches at a path boundary (the account segment).
        lists.apply_fuzzy((Tab::Accounts, None), "check", "", &mut matcher, &by_id);
        let visible: Vec<_> = lists.accounts.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(visible, vec!["TB/Checking"]);
    }

    #[test]
    fn accounts_fuzzy_search_scores_over_path() {
        let mut lists = lists_with_accounts(&[(1, "ING/Orange"), (2, "TB/Checking")]);
        let mut matcher = FuzzyMatcher::new();
        let by_id = HashMap::new();

        // `~`-search (pattern) fuzzy-matches the path.
        lists.apply_fuzzy((Tab::Accounts, None), "", "orng", &mut matcher, &by_id);
        let visible: Vec<_> = lists.accounts.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(visible, vec!["ING/Orange"]);
    }
}
