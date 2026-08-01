//! Note and tag actions: opening the editors and persisting what they return.
//!
//! Both editors are self-contained widgets ([`crate::tui::note_editor`],
//! [`crate::tui::tag_editor`]) that own their state and key handling. This file
//! is only the seam: open, hand keys over, and act on the returned
//! [`NoteEditorAction`] / [`TagEditorAction`].

use crossterm::event::KeyEvent;

use crate::tui::note_editor::{NoteEditor, NoteEditorAction};
use crate::tui::tag_editor::{TagEditor, TagEditorAction};

use super::{App, ConfirmAction, InputMode};

impl App {
    // ==================== Notes ====================

    /// Open the note editor for the selected transaction (`n`). A no-op on
    /// tabs whose rows aren't transactions.
    pub fn start_note_edit(&mut self) {
        let Some(tx_id) = self.selected_transaction().map(|tx| tx.id) else {
            return;
        };
        let note = self.get_cached_note(tx_id).unwrap_or_default().to_string();
        self.note_editor = Some(NoteEditor::new(tx_id, &note));
        self.input_mode = InputMode::Note;
    }

    pub fn handle_note_key(&mut self, key: &KeyEvent) {
        let Some(editor) = self.note_editor.as_mut() else {
            return;
        };
        match editor.handle_key(key) {
            NoteEditorAction::Continue => {}
            NoteEditorAction::Save => self.save_note(),
            NoteEditorAction::Cancel => self.request_exit_note_edit(),
        }
    }

    fn save_note(&mut self) {
        let Some((tx_id, text)) = self
            .note_editor
            .as_ref()
            .map(|editor| (editor.tx_id, editor.text()))
        else {
            return;
        };

        let mut kept = false;
        if !self.try_mutation("save note", |s| {
            kept = s.set_note(tx_id, &text)?;
            Ok(())
        }) {
            return;
        }

        self.close_note_editor();
        self.refresh_data();
        self.show_status(if kept {
            "Note saved".to_string()
        } else {
            "Note cleared".to_string()
        });
    }

    /// Esc out of the note editor, confirming first when there are unsaved
    /// edits — the same guard the filter-edit screen uses.
    pub fn request_exit_note_edit(&mut self) {
        let dirty = self.note_editor.as_ref().is_some_and(NoteEditor::is_dirty);
        if dirty {
            self.confirm(
                "Discard unsaved changes to this note?".to_string(),
                ConfirmAction::DiscardNoteEdit,
            );
        } else {
            self.close_note_editor();
        }
    }

    pub(super) fn close_note_editor(&mut self) {
        self.note_editor = None;
        self.input_mode = InputMode::Normal;
    }

    // ==================== Tags ====================

    /// Open the tag editor for the selected transaction (`#`). A no-op on tabs
    /// whose rows aren't transactions.
    pub fn start_tag_edit(&mut self) {
        let Some(tx_id) = self.selected_transaction().map(|tx| tx.id) else {
            return;
        };
        let tags = self.get_cached_tags(tx_id).to_vec();
        let known = self.tag_options().to_vec();
        self.tag_editor = Some(TagEditor::new(tx_id, &tags, known));
        self.input_mode = InputMode::Tags;
    }

    pub fn handle_tag_key(&mut self, key: &KeyEvent) {
        let Some(editor) = self.tag_editor.as_mut() else {
            return;
        };
        match editor.handle_key(key) {
            TagEditorAction::Continue => {}
            TagEditorAction::Save => self.save_tags(),
            TagEditorAction::Cancel => self.close_tag_editor(),
        }
    }

    fn save_tags(&mut self) {
        let Some((tx_id, tags)) = self
            .tag_editor
            .as_ref()
            .map(|editor| (editor.tx_id, editor.tags()))
        else {
            return;
        };

        let mut stored = Vec::new();
        if !self.try_mutation("save tags", |s| {
            stored = s.set_transaction_tags(tx_id, &tags)?;
            Ok(())
        }) {
            return;
        }

        self.close_tag_editor();
        // refresh_data reloads the tag caches and, through them, `tag:`
        // autocomplete — a tag created or GC'd here must show up immediately.
        self.refresh_data();
        self.show_status(if stored.is_empty() {
            "Tags cleared".to_string()
        } else {
            format!("Tagged #{}", stored.join(" #"))
        });
    }

    pub(super) fn close_tag_editor(&mut self) {
        self.tag_editor = None;
        self.input_mode = InputMode::Normal;
    }
}
