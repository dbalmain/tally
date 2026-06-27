//! Transfer actions: marking a pair of transactions as a transfer, and
//! confirming/deleting transfer links.

use crate::TransferSource;

use super::{App, ConfirmAction, InputMode, Tab, TodoSubTab};

impl App {
    pub fn start_transfer_mark(&mut self) {
        let Some(tx) = self.selected_transaction().cloned() else {
            return;
        };

        let candidates = self.load_or_show("find transfer candidates", |s| {
            s.find_matching_transfer_candidates(&tx)
        });

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

        // If this exact transfer already exists, marking it is a no-op — no
        // warning, no churn.
        let already_linked = [from_id, to_id].into_iter().any(|id| {
            self.get_cached_transfer(id).is_some_and(|t| {
                (t.from_transaction_id == from_id && t.to_transaction_id == to_id)
                    || (t.from_transaction_id == to_id && t.to_transaction_id == from_id)
            })
        });
        if already_linked {
            self.clear_transfer_mode();
            return;
        }

        // If either endpoint is already part of a *different* transfer, creating
        // this one would break those links — confirm first.
        let mut transfer_ids: Vec<i64> = [from_id, to_id]
            .into_iter()
            .filter_map(|id| self.get_cached_transfer(id).map(|t| t.id))
            .collect();
        transfer_ids.sort_unstable();
        transfer_ids.dedup();

        if !transfer_ids.is_empty() {
            let n = transfer_ids.len();
            self.confirm_message = Some(format!(
                "{} existing transfer{} will be unlinked. Continue?",
                n,
                if n == 1 { "" } else { "s" }
            ));
            self.confirm_action = Some(ConfirmAction::BreakTransfersForTransfer {
                transfer_ids,
                from_id,
                to_id,
            });
            self.pending_transfer_tx = None;
            self.transfer_candidates.clear();
            self.input_mode = InputMode::Confirm;
            return;
        }

        let created = self.try_mutation("create transfer", |s| {
            s.create_transfer(from_id, to_id, TransferSource::Manual, true, None)
                .map(|_| ())
        });
        if created {
            self.refresh_data();
        }

        self.clear_transfer_mode();
    }

    pub(super) fn clear_transfer_mode(&mut self) {
        self.pending_transfer_tx = None;
        self.transfer_candidates.clear();
        if self.input_mode == InputMode::TransferPending
            || self.input_mode == InputMode::TransferNoMatch
        {
            self.input_mode = InputMode::Normal;
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

    pub fn confirm_transfer_review(&mut self) {
        if self.current_tab != Tab::Todo || self.todo_subtab != TodoSubTab::TransferReview {
            return;
        }
        let Some(transfer_id) = self
            .lists
            .transfer_reviews
            .get(self.selected_index)
            .map(|t| t.id)
        else {
            return;
        };
        if self.try_mutation("confirm transfer", |s| s.confirm_transfer(transfer_id)) {
            self.refresh_data();
        }
    }

    /// On a plain-transaction view, `u` removes the selected transaction's
    /// transfer link, or — if it isn't a transfer — its category, after a
    /// confirmation prompt. (A transaction is never both; transfer takes
    /// precedence so legacy rows that ended up with both can be cleared with two
    /// presses.) The work itself happens in [`App::confirm_proceed`].
    pub fn delete_selected_tx_link(&mut self) {
        let Some(tx_id) = self.selected_transaction().map(|tx| tx.id) else {
            return;
        };
        if let Some(transfer_id) = self.get_cached_transfer(tx_id).map(|t| t.id) {
            self.confirm_message = Some("Unlink this transfer?".to_string());
            self.confirm_action = Some(ConfirmAction::UnlinkTransfer { transfer_id });
            self.input_mode = InputMode::Confirm;
            return;
        }
        if self
            .get_cached_category(tx_id)
            .is_some_and(|c| !c.is_empty())
        {
            self.confirm_message = Some("Uncategorise this transaction?".to_string());
            self.confirm_action = Some(ConfirmAction::Uncategorise { tx_id });
            self.input_mode = InputMode::Confirm;
        }
    }

    pub fn delete_transfer(&mut self) {
        let transfer_id = match (self.current_tab, self.todo_subtab) {
            (Tab::Transfers, _) => self
                .lists
                .linked_transfers
                .get(self.selected_index)
                .map(|twt| twt.transfer.id),
            (Tab::Todo, TodoSubTab::TransferReview) => self
                .lists
                .transfer_reviews
                .get(self.selected_index)
                .map(|t| t.id),
            _ => None,
        };
        let Some(transfer_id) = transfer_id else {
            return;
        };
        if !self.try_mutation("delete transfer", |s| s.delete_transfer(transfer_id)) {
            return;
        }
        self.refresh_data();
        if self.selected_index >= self.lists.len(self.current_tab_key()) && self.selected_index > 0
        {
            self.selected_index -= 1;
        }
    }
}
