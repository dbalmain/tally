//! Transfer actions: marking a pair of transactions as a transfer, and
//! confirming/deleting transfer links.

use crate::TransferSource;

use super::{App, InputMode, Tab, TodoSubTab};

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

    pub fn delete_transfer(&mut self) {
        if self.current_tab != Tab::Transfers {
            return;
        }
        let Some(transfer_id) = self
            .lists
            .linked_transfers
            .get(self.selected_index)
            .map(|twt| twt.transfer.id)
        else {
            return;
        };
        if !self.try_mutation("delete transfer", |s| s.delete_transfer(transfer_id)) {
            return;
        }
        self.refresh_data();
        if self.selected_index >= self.lists.linked_transfers.len() && self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }
}
