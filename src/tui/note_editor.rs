//! Multi-line note editor.
//!
//! Self-contained: it owns the text buffer, the cursor, and its own key
//! handling, and reports back only "keep going / save / cancel". The `App`
//! stores one `Option<NoteEditor>` and persists on `Save`, so nothing about the
//! editing model leaks into app state.
//!
//! Vertical movement is by *source* line, not by wrapped display line. Notes
//! are short prose, so the simpler model costs little and keeps the cursor
//! mapping honest.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the caller should do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteEditorAction {
    /// Keep editing.
    Continue,
    /// Persist [`NoteEditor::text`].
    Save,
    /// Leave without saving (the caller confirms first when dirty).
    Cancel,
}

/// The editor's text laid out for a given width.
pub struct NoteLayout {
    /// Wrapped display lines.
    pub lines: Vec<String>,
    /// Cursor position within `lines`, as (column, row).
    pub cursor: (usize, usize),
}

pub struct NoteEditor {
    pub tx_id: i64,
    /// Source lines; never empty (an empty note is one empty line).
    lines: Vec<String>,
    /// Cursor line, and its character offset within that line.
    row: usize,
    col: usize,
    original: String,
}

impl NoteEditor {
    pub fn new(tx_id: i64, note: &str) -> Self {
        let lines: Vec<String> = if note.is_empty() {
            vec![String::new()]
        } else {
            note.split('\n').map(str::to_string).collect()
        };
        // Start at the end, so appending to an existing note needs no movement.
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        Self {
            tx_id,
            lines,
            row,
            col,
            original: note.to_string(),
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_dirty(&self) -> bool {
        self.text() != self.original
    }

    /// Handle one key. Ctrl-S saves, Esc cancels, Enter inserts a newline —
    /// Enter cannot mean "save" in a multi-line editor.
    pub fn handle_key(&mut self, key: &KeyEvent) -> NoteEditorAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('s') if ctrl => return NoteEditorAction::Save,
            KeyCode::Esc => return NoteEditorAction::Cancel,
            KeyCode::Char(c) => self.insert_char(c),
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_len(self.row),
            _ => {}
        }
        NoteEditorAction::Continue
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    /// Byte offset of character index `col` in row `row`.
    fn byte_offset(&self, row: usize, col: usize) -> usize {
        self.lines[row]
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[row].len())
    }

    fn insert_char(&mut self, c: char) {
        let at = self.byte_offset(self.row, self.col);
        self.lines[self.row].insert(at, c);
        self.col += 1;
    }

    fn insert_newline(&mut self) {
        let at = self.byte_offset(self.row, self.col);
        let tail = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            let at = self.byte_offset(self.row, self.col - 1);
            self.lines[self.row].remove(at);
            self.col -= 1;
        } else if self.row > 0 {
            // Join this line onto the end of the previous one.
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len(self.row);
            self.lines[self.row].push_str(&line);
        }
    }

    fn delete(&mut self) {
        if self.col < self.line_len(self.row) {
            let at = self.byte_offset(self.row, self.col);
            self.lines[self.row].remove(at);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line_len(self.row);
        }
    }

    fn move_right(&mut self) {
        if self.col < self.line_len(self.row) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.line_len(self.row));
        } else {
            self.col = 0;
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.line_len(self.row));
        } else {
            self.col = self.line_len(self.row);
        }
    }

    /// Wrap the buffer to `width` and locate the cursor within the result.
    pub fn layout(&self, width: usize) -> NoteLayout {
        let width = width.max(1);
        let mut lines = Vec::new();
        let mut cursor = (0, 0);

        for (row, line) in self.lines.iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            let segments = wrap_segments(&chars, width);
            for (index, &(start, end)) in segments.iter().enumerate() {
                if row == self.row {
                    // The cursor belongs to the segment containing its offset;
                    // at a segment boundary it belongs to the earlier one only
                    // when that is the last segment (end of line).
                    let last = index + 1 == segments.len();
                    if (self.col >= start && self.col < end) || (last && self.col >= end) {
                        cursor = (self.col - start, lines.len());
                    }
                }
                lines.push(chars[start..end].iter().collect());
            }
        }

        NoteLayout { lines, cursor }
    }
}

/// Greedy word-wrap `chars` to `width`, as (start, end) character ranges.
/// Always returns at least one (possibly empty) range so an empty line still
/// occupies a row.
fn wrap_segments(chars: &[char], width: usize) -> Vec<(usize, usize)> {
    if chars.len() <= width {
        return vec![(0, chars.len())];
    }

    let mut segments = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let remaining = chars.len() - start;
        if remaining <= width {
            segments.push((start, chars.len()));
            break;
        }
        // Break at the last space that fits; fall back to a hard break for a
        // token longer than the whole width.
        let limit = start + width;
        let split = chars[start..limit]
            .iter()
            .rposition(|c| *c == ' ')
            .map(|i| start + i + 1)
            .filter(|&i| i > start)
            .unwrap_or(limit);
        segments.push((start, split));
        start = split;
    }

    if segments.is_empty() {
        segments.push((0, 0));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(editor: &mut NoteEditor, text: &str) {
        for c in text.chars() {
            editor.handle_key(&key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn starts_at_the_end_of_an_existing_note() {
        let editor = NoteEditor::new(1, "first\nsecond");
        assert_eq!((editor.row, editor.col), (1, 6));
        assert!(!editor.is_dirty());
    }

    #[test]
    fn enter_splits_the_line_and_backspace_rejoins_it() {
        let mut editor = NoteEditor::new(1, "");
        typed(&mut editor, "abcd");
        editor.handle_key(&key(KeyCode::Left));
        editor.handle_key(&key(KeyCode::Enter));
        assert_eq!(editor.text(), "abc\nd");
        assert_eq!((editor.row, editor.col), (1, 0));

        editor.handle_key(&key(KeyCode::Backspace));
        assert_eq!(editor.text(), "abcd");
        assert_eq!((editor.row, editor.col), (0, 3));
    }

    #[test]
    fn delete_at_end_of_line_pulls_up_the_next() {
        let mut editor = NoteEditor::new(1, "ab\ncd");
        editor.handle_key(&key(KeyCode::Up));
        editor.handle_key(&key(KeyCode::End));
        editor.handle_key(&key(KeyCode::Delete));
        assert_eq!(editor.text(), "abcd");
    }

    #[test]
    fn vertical_movement_clamps_to_the_shorter_line() {
        let mut editor = NoteEditor::new(1, "long line here\nab");
        editor.handle_key(&key(KeyCode::Up));
        assert_eq!(editor.col, 2);
        editor.handle_key(&key(KeyCode::End));
        assert_eq!(editor.col, 14);
    }

    #[test]
    fn ctrl_s_saves_esc_cancels_and_enter_never_saves() {
        let mut editor = NoteEditor::new(1, "");
        assert_eq!(
            editor.handle_key(&key(KeyCode::Enter)),
            NoteEditorAction::Continue
        );
        assert_eq!(editor.handle_key(&ctrl('s')), NoteEditorAction::Save);
        assert_eq!(
            editor.handle_key(&key(KeyCode::Esc)),
            NoteEditorAction::Cancel
        );
    }

    #[test]
    fn dirty_tracks_the_original_text() {
        let mut editor = NoteEditor::new(1, "note");
        assert!(!editor.is_dirty());
        typed(&mut editor, "!");
        assert!(editor.is_dirty());
        editor.handle_key(&key(KeyCode::Backspace));
        assert!(!editor.is_dirty());
    }

    #[test]
    fn layout_wraps_on_word_boundaries_and_places_the_cursor() {
        let editor = NoteEditor::new(1, "alpha beta gamma");
        let layout = editor.layout(11);
        assert_eq!(layout.lines, vec!["alpha beta ", "gamma"]);
        // The cursor sits at end-of-text, on the last visual row.
        assert_eq!(layout.cursor, (5, 1));
    }

    #[test]
    fn layout_hard_breaks_a_token_longer_than_the_width() {
        let editor = NoteEditor::new(1, "aaaaaaaa");
        let layout = editor.layout(3);
        assert_eq!(layout.lines, vec!["aaa", "aaa", "aa"]);
        assert_eq!(layout.cursor, (2, 2));
    }

    #[test]
    fn layout_keeps_blank_lines_as_rows() {
        let editor = NoteEditor::new(1, "a\n\nb");
        let layout = editor.layout(10);
        assert_eq!(layout.lines, vec!["a", "", "b"]);
        assert_eq!(layout.cursor, (1, 2));
    }

    #[test]
    fn multibyte_text_edits_by_character_not_byte() {
        let mut editor = NoteEditor::new(1, "café");
        editor.handle_key(&key(KeyCode::Backspace));
        assert_eq!(editor.text(), "caf");
        typed(&mut editor, "é!");
        assert_eq!(editor.text(), "café!");
        editor.handle_key(&key(KeyCode::Left));
        editor.handle_key(&key(KeyCode::Backspace));
        assert_eq!(editor.text(), "caf!");
    }
}
