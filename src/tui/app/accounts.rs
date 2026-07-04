//! Account actions on the Accounts tab: rename/move, delete (which also
//! removes the exports folder), the transaction side panel, and jumping to the
//! Transactions tab filtered to an account.
//!
//! Mirrors `categories.rs`, minus merge: accounts never merge, so a rename
//! collision surfaces as a plain error rather than offering a merge flow.

use crate::AccountWithBank;

use super::{App, ConfirmAction, InputMode, Tab, TextPromptTarget};

impl App {
    pub fn selected_account(&self) -> Option<&AccountWithBank> {
        if self.current_tab == Tab::Accounts {
            self.lists.accounts.get(self.selected_index)
        } else {
            None
        }
    }

    pub fn start_account_rename(&mut self) {
        if let Some(account) = self.selected_account().cloned() {
            self.open_text_prompt(
                "Rename account",
                account.path.clone(),
                TextPromptTarget::AccountRename(account),
            );
        }
    }

    pub(super) fn confirm_account_rename(&mut self, account: AccountWithBank, new_path: String) {
        if new_path.is_empty() || new_path == account.path {
            self.cancel_input();
            return;
        }

        // Accounts never merge: a collision or an invalid path is a plain error,
        // surfaced via the error popup with the prompt restored so the user can
        // correct it.
        match self.store.rename_account(account.id, &new_path) {
            Ok(()) => {
                self.reload_accounts();
                self.move_cursor_to_account(&new_path);
                self.input_mode = InputMode::Normal;
                self.clear_text_prompt();
            }
            Err(e) => {
                self.error_message = Some(format!("Failed to rename: {}", e));
                self.restore_text_prompt(
                    "Rename account",
                    new_path,
                    TextPromptTarget::AccountRename(account),
                );
            }
        }
    }

    /// Prompt to delete the selected account, noting the transaction count and
    /// that its exports folder (scripts + CSVs) will be removed.
    pub fn start_account_delete(&mut self) {
        let Some(account) = self.selected_account().cloned() else {
            return;
        };
        let count = self.load_or_show("count transactions in account", |s| {
            s.count_transactions_in_account(account.id)
        });
        self.confirm(
            format!(
                "Delete account {}? Removes its exports/ folder (scripts + CSVs) and hides {} transaction{} (kept in history).",
                account.path,
                count,
                if count == 1 { "" } else { "s" }
            ),
            ConfirmAction::DeleteAccount(account.id),
        );
    }

    /// Reload accounts and keep the cursor in bounds after a deletion.
    pub(super) fn delete_account_after(&mut self) {
        self.reload_accounts();
        self.clamp_selection();
    }

    pub(super) fn reload_accounts(&mut self) {
        let accounts = self.load_or_show("load accounts", |s| s.list_accounts_with_bank());
        self.lists.accounts.set_items(accounts);
        self.rebuild_account_counts();
        self.apply_fuzzy_filter();
        self.clamp_selection();
        self.reload_account_transactions();
    }

    /// Rebuild the per-account transaction count cache in one bulk query.
    pub(super) fn rebuild_account_counts(&mut self) {
        self.account_tx_count = self.load_or_show("load account counts", |s| {
            s.get_account_transaction_counts()
        });
    }

    pub fn account_transaction_count(&self, account_id: i64) -> usize {
        self.account_tx_count.get(&account_id).copied().unwrap_or(0)
    }

    fn move_cursor_to_account(&mut self, path: &str) {
        if let Some(pos) = self.lists.accounts.iter().position(|a| a.path == path) {
            self.selected_index = pos;
        }
        self.reload_account_transactions();
    }

    // ==================== Account Transactions Side Panel ====================

    pub fn toggle_account_transactions(&mut self) {
        self.show_account_transactions = !self.show_account_transactions;
        self.reload_account_transactions();
    }

    /// Reload the side-panel transactions for the selected account, or clear
    /// them when the panel is closed / not on the Accounts tab / no account
    /// selected.
    pub(super) fn reload_account_transactions(&mut self) {
        if self.current_tab == Tab::Accounts
            && self.show_account_transactions
            && let Some(account_id) = self.selected_account().map(|a| a.id)
        {
            self.account_transactions = self.load_or_show("load account transactions", |s| {
                s.query_transactions_in_account(account_id, Some(super::LIST_LIMIT))
            });
            return;
        }
        self.account_transactions.clear();
    }

    /// Jump to the Transactions tab with its DB search set to this account, so
    /// the user can act on its transactions. Focus lands on the first row.
    pub fn manage_account_transactions(&mut self) {
        let Some(path) = self.selected_account().map(|a| a.path.clone()) else {
            return;
        };
        self.save_tab_state();
        self.show_account_transactions = false;
        self.account_transactions.clear();
        self.current_tab = Tab::Transactions;

        let query = format!("account:{path}");
        let state = self.current_search_state_mut();
        state.search_bar.set_value(&query);
        state.db_search_active = true;
        state.editing_db_search = false;
        state.fuzzy_search_active = false;
        state.editing_fuzzy_search = false;
        state.selected_index = 0;

        self.input_mode = InputMode::Normal;
        self.selected_index = 0;
        self.reload_current_tab();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::store_with_two_accounts;
    use crate::tui::app::Tab;

    fn account_pos(app: &App, path: &str) -> usize {
        app.lists
            .accounts
            .iter()
            .position(|a| a.path == path)
            .unwrap()
    }

    #[test]
    fn rename_collision_surfaces_error_without_merge() {
        let (temp, store, _a1, _a2) = store_with_two_accounts();
        // Existing on-disk folder makes the target a collision.
        std::fs::create_dir_all(temp.path().join("TB").join("A2")).unwrap();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Accounts;
        app.selected_index = account_pos(&app, "TB/A1");

        let account = app.selected_account().cloned().unwrap();
        app.confirm_account_rename(account, "TB/A2".to_string());

        // No merge flow: the error popup is shown and the account is untouched.
        assert!(app.error_message.is_some());
        assert!(app.confirm_action.is_none());
        assert!(app.lists.accounts.iter().any(|a| a.path == "TB/A1"));
    }

    #[test]
    fn rename_moves_account_and_focuses_it() {
        let (_temp, store, _a1, _a2) = store_with_two_accounts();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Accounts;
        app.selected_index = account_pos(&app, "TB/A1");

        let account = app.selected_account().cloned().unwrap();
        app.confirm_account_rename(account, "TB/Renamed".to_string());

        assert!(app.error_message.is_none());
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.selected_account().unwrap().path, "TB/Renamed");
    }

    #[test]
    fn delete_account_removes_it_and_clamps_cursor() {
        let (_temp, store, a1, _a2) = store_with_two_accounts();
        let mut app = App::new(store).unwrap();
        app.current_tab = Tab::Accounts;

        app.confirm_action = Some(ConfirmAction::DeleteAccount(a1));
        app.input_mode = InputMode::Confirm;
        app.confirm_proceed();

        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(!app.lists.accounts.iter().any(|a| a.id == a1));
        assert!(app.selected_index < app.lists.accounts.len().max(1));
    }
}
