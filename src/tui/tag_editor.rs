//! Tag editor.
//!
//! The whole tag set is one space-separated text line, with autocomplete over
//! the token under the cursor. That buys ordinary text editing for free —
//! backspace fixes a typo mid-tag, `Ctrl-Left/Right` moves tag by tag — with no
//! chip model and no separate deletion mode.
//!
//! Key semantics, chosen so nothing is ever committed by surprise:
//!
//! - `Space` commits the token **exactly as typed**. It never takes the
//!   highlighted suggestion: typing `coffee` with `coffee-shop` highlighted
//!   must not silently give you `coffee-shop`, and a new tag that is a prefix
//!   of an existing one has to be typeable.
//! - `Tab` takes the highlighted suggestion (the same convention the search bar
//!   uses) and leaves the cursor after a space, ready for the next tag.
//! - `Enter` and `Ctrl-S` always save. Enter is never "accept the suggestion",
//!   because a mode where it sometimes saves and sometimes completes fails
//!   silently — you press it meaning save and persist a completion you did not
//!   want.
//!
//! This module is deliberately self-contained (state, keys, and the view model
//! the renderer needs) so a different interaction model is a one-file swap.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use std::cmp::Reverse;

use crate::{normalise_tag, validate_tag};

/// What the caller should do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagEditorAction {
    /// Keep editing.
    Continue,
    /// Persist [`TagEditor::tags`].
    Save,
    /// Leave without saving.
    Cancel,
}

/// One suggestion row, with whether that tag is already on the transaction.
pub struct TagSuggestion {
    pub name: String,
    pub applied: bool,
}

pub struct TagEditor {
    pub tx_id: i64,
    /// The raw text line; tags are its whitespace-separated tokens.
    input: String,
    /// Cursor position, in characters.
    cursor: usize,
    /// Every known tag, most-used first.
    known: Vec<String>,
    suggestions: Vec<String>,
    selected: usize,
}

impl TagEditor {
    pub fn new(tx_id: i64, tags: &[String], known: Vec<String>) -> Self {
        // Trailing space so the first keystroke starts a new tag rather than
        // extending the last existing one.
        let input = if tags.is_empty() {
            String::new()
        } else {
            format!("{} ", tags.join(" "))
        };
        let cursor = input.chars().count();
        let mut editor = Self {
            tx_id,
            input,
            cursor,
            known,
            suggestions: Vec::new(),
            selected: 0,
        };
        editor.refresh_suggestions();
        editor
    }

    /// The canonicalised, deduplicated tag set as currently typed. Invalid
    /// tokens are dropped — [`Self::invalid_tokens`] is what surfaces them to
    /// the user while typing.
    pub fn tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = Vec::new();
        for token in self.input.split_whitespace() {
            if let Some(tag) = validate_tag(token)
                && !tags.contains(&tag)
            {
                tags.push(tag);
            }
        }
        tags
    }

    /// Tokens that aren't usable tags, so the view can render them red.
    pub fn invalid_tokens(&self) -> Vec<&str> {
        self.input
            .split_whitespace()
            .filter(|token| validate_tag(token).is_none())
            .collect()
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The suggestion list for the token under the cursor, each flagged with
    /// whether it is already applied. With an empty token this is every known
    /// tag, so the editor doubles as the tag browser.
    pub fn suggestions(&self) -> Vec<TagSuggestion> {
        let applied = self.tags();
        self.suggestions
            .iter()
            .map(|name| TagSuggestion {
                name: name.clone(),
                applied: applied.contains(name),
            })
            .collect()
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> TagEditorAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return TagEditorAction::Cancel,
            KeyCode::Enter => return TagEditorAction::Save,
            KeyCode::Char('s') if ctrl => return TagEditorAction::Save,
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Tab => self.accept_suggestion(),
            KeyCode::Down => self.select_next(),
            KeyCode::Up => self.select_prev(),
            KeyCode::Left if ctrl => self.cursor = self.prev_word_boundary(),
            KeyCode::Right if ctrl => self.cursor = self.next_word_boundary(),
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                self.refresh_suggestions();
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.len());
                self.refresh_suggestions();
            }
            KeyCode::Home => {
                self.cursor = 0;
                self.refresh_suggestions();
            }
            KeyCode::End => {
                self.cursor = self.len();
                self.refresh_suggestions();
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Char(c) => self.insert_char(c),
            _ => {}
        }
        TagEditorAction::Continue
    }

    fn len(&self) -> usize {
        self.input.chars().count()
    }

    fn byte_offset(&self, index: usize) -> usize {
        self.input
            .char_indices()
            .nth(index)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    /// Character range of the whitespace-separated token containing the cursor.
    /// A cursor sitting on whitespace yields an empty range at that point — a
    /// new, empty token.
    fn token_range(&self) -> (usize, usize) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut start = self.cursor.min(chars.len());
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let mut end = self.cursor.min(chars.len());
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        (start, end)
    }

    fn token(&self) -> String {
        let (start, end) = self.token_range();
        self.input.chars().skip(start).take(end - start).collect()
    }

    fn insert_char(&mut self, c: char) {
        // Collapse a run of spaces: two adjacent separators would just be an
        // empty token.
        if c.is_whitespace() {
            if self.token().is_empty() {
                return;
            }
            self.insert_str(" ");
        } else {
            self.insert_str(&c.to_string());
        }
        self.refresh_suggestions();
    }

    fn insert_str(&mut self, text: &str) {
        let at = self.byte_offset(self.cursor);
        self.input.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let at = self.byte_offset(self.cursor - 1);
        self.input.remove(at);
        self.cursor -= 1;
        self.refresh_suggestions();
    }

    fn delete(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        let at = self.byte_offset(self.cursor);
        self.input.remove(at);
        self.refresh_suggestions();
    }

    /// Delete back to the start of the current (or previous) token — the quick
    /// way to drop a whole tag.
    fn delete_word(&mut self) {
        let start = self.prev_word_boundary();
        if start == self.cursor {
            return;
        }
        let from = self.byte_offset(start);
        let to = self.byte_offset(self.cursor);
        self.input.replace_range(from..to, "");
        self.cursor = start;
        self.refresh_suggestions();
    }

    fn prev_word_boundary(&self) -> usize {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn next_word_boundary(&self) -> usize {
        let chars: Vec<char> = self.input.chars().collect();
        let mut i = self.cursor.min(chars.len());
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    /// Replace the token under the cursor with the highlighted suggestion and
    /// leave the cursor after a following space, ready for the next tag.
    fn accept_suggestion(&mut self) {
        let Some(name) = self.suggestions.get(self.selected).cloned() else {
            return;
        };
        let (start, end) = self.token_range();
        let from = self.byte_offset(start);
        let to = self.byte_offset(end);

        let followed_by_space = self.input.chars().nth(end) == Some(' ');
        let replacement = if followed_by_space {
            name.clone()
        } else {
            format!("{name} ")
        };
        self.input.replace_range(from..to, &replacement);
        self.cursor = start + replacement.chars().count();
        if followed_by_space {
            self.cursor += 1;
        }
        self.refresh_suggestions();
    }

    fn select_next(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected = (self.selected + 1) % self.suggestions.len();
        }
    }

    fn select_prev(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected = self
                .selected
                .checked_sub(1)
                .unwrap_or(self.suggestions.len() - 1);
        }
    }

    /// Rank known tags against the token under the cursor. Tags already used in
    /// *other* tokens are dropped, so the list never offers a duplicate; an
    /// empty token shows everything, in usage order.
    fn refresh_suggestions(&mut self) {
        let needle = normalise_tag(&self.token());
        let (start, _) = self.token_range();
        let others: Vec<String> = self
            .input
            .split_whitespace()
            .scan(0usize, |consumed, token| {
                let text: Vec<char> = self.input.chars().collect();
                let mut at = *consumed;
                while at < text.len() && text[at].is_whitespace() {
                    at += 1;
                }
                let token_start = at;
                *consumed = at + token.chars().count();
                Some((token_start, token))
            })
            .filter(|(token_start, _)| *token_start != start)
            .map(|(_, token)| normalise_tag(token))
            .collect();

        let candidates: Vec<&String> = self
            .known
            .iter()
            .filter(|name| !others.contains(name))
            .collect();

        self.suggestions = if needle.is_empty() {
            candidates.into_iter().cloned().collect()
        } else {
            rank(&candidates, &needle)
        };
        self.selected = 0;
    }
}

fn rank(candidates: &[&String], needle: &str) -> Vec<String> {
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::new(
        needle,
        CaseMatching::Ignore,
        Normalization::Smart,
        nucleo_matcher::pattern::AtomKind::Fuzzy,
    );

    let mut scored: Vec<(u32, &String)> = candidates
        .iter()
        .filter_map(|candidate| {
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(candidate, &mut buf);
            pattern
                .score(haystack, &mut matcher)
                .map(|score| (score, *candidate))
        })
        .collect();

    scored.sort_by_key(|(score, name)| (Reverse(*score), (*name).clone()));
    scored.into_iter().map(|(_, name)| name.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        vec![
            "coffee-shop".to_string(),
            "coffee".to_string(),
            "work".to_string(),
            "work/travel".to_string(),
        ]
    }

    fn editor(tags: &[&str]) -> TagEditor {
        let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        TagEditor::new(1, &tags, known())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(editor: &mut TagEditor, text: &str) {
        for c in text.chars() {
            editor.handle_key(&key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn existing_tags_load_with_a_trailing_space() {
        let editor = editor(&["work", "coffee"]);
        assert_eq!(editor.input(), "work coffee ");
        assert_eq!(editor.cursor(), 12);
        assert_eq!(editor.tags(), vec!["work", "coffee"]);
    }

    #[test]
    fn space_commits_the_literal_token_not_the_highlighted_suggestion() {
        let mut editor = editor(&[]);
        typed(&mut editor, "coffee");
        // `coffee-shop` may well outrank `coffee` in the list; Space must not
        // care.
        assert!(!editor.suggestions().is_empty());
        typed(&mut editor, " ");
        assert_eq!(editor.tags(), vec!["coffee"]);
        assert_eq!(editor.input(), "coffee ");
    }

    #[test]
    fn tab_accepts_the_highlighted_suggestion_and_leaves_a_trailing_space() {
        let mut editor = editor(&[]);
        typed(&mut editor, "cof");
        editor.handle_key(&key(KeyCode::Tab));
        assert_eq!(editor.input(), "coffee ");
        assert_eq!(editor.cursor(), 7);
        assert_eq!(editor.tags(), vec!["coffee"]);
    }

    #[test]
    fn arrow_keys_choose_a_different_suggestion_for_tab() {
        let mut editor = editor(&[]);
        typed(&mut editor, "cof");
        let first = editor.suggestions()[0].name.clone();
        editor.handle_key(&key(KeyCode::Down));
        let second = editor.suggestions()[1].name.clone();
        assert_ne!(first, second);
        editor.handle_key(&key(KeyCode::Tab));
        assert_eq!(editor.tags(), vec![second]);
    }

    #[test]
    fn enter_and_ctrl_s_save_esc_cancels() {
        let mut editor = editor(&[]);
        typed(&mut editor, "cof");
        // Enter saves even mid-token, keeping what was typed.
        assert_eq!(
            editor.handle_key(&key(KeyCode::Enter)),
            TagEditorAction::Save
        );
        assert_eq!(editor.tags(), vec!["cof"]);
        assert_eq!(editor.handle_key(&ctrl('s')), TagEditorAction::Save);
        assert_eq!(
            editor.handle_key(&key(KeyCode::Esc)),
            TagEditorAction::Cancel
        );
    }

    #[test]
    fn a_leading_hash_is_optional() {
        let mut editor = editor(&[]);
        typed(&mut editor, "#work #coffee");
        assert_eq!(editor.tags(), vec!["work", "coffee"]);
    }

    #[test]
    fn tags_are_canonicalised_and_deduplicated() {
        let mut editor = editor(&[]);
        typed(&mut editor, "Work work WORK");
        assert_eq!(editor.tags(), vec!["work"]);
    }

    #[test]
    fn invalid_tokens_are_reported_and_excluded() {
        let mut editor = editor(&[]);
        typed(&mut editor, "ok b@d");
        assert_eq!(editor.tags(), vec!["ok"]);
        assert_eq!(editor.invalid_tokens(), vec!["b@d"]);
    }

    #[test]
    fn suggestions_exclude_tags_used_in_other_tokens() {
        let mut editor = editor(&["work"]);
        typed(&mut editor, "wor");
        let names: Vec<String> = editor.suggestions().into_iter().map(|s| s.name).collect();
        assert!(!names.contains(&"work".to_string()));
        assert!(names.contains(&"work/travel".to_string()));
    }

    #[test]
    fn an_empty_token_offers_every_tag_flagged_with_what_is_applied() {
        let editor = editor(&["work"]);
        let suggestions = editor.suggestions();
        // "work" is used by another token, so it is filtered out; the rest are
        // all offered and none of them is applied.
        let names: Vec<String> = suggestions.iter().map(|s| s.name.clone()).collect();
        assert_eq!(names, vec!["coffee-shop", "coffee", "work/travel"]);
        assert!(suggestions.iter().all(|s| !s.applied));
    }

    #[test]
    fn backspace_edits_mid_tag_without_a_special_mode() {
        let mut editor = editor(&["work", "coffe"]);
        editor.handle_key(&key(KeyCode::Left)); // off the trailing space
        editor.handle_key(&key(KeyCode::Backspace));
        typed(&mut editor, "ee");
        assert_eq!(editor.tags(), vec!["work", "coffee"]);
    }

    #[test]
    fn ctrl_w_deletes_a_whole_tag() {
        let mut editor = editor(&["work", "coffee"]);
        editor.handle_key(&ctrl('w'));
        assert_eq!(editor.tags(), vec!["work"]);
        editor.handle_key(&ctrl('w'));
        assert!(editor.tags().is_empty());
    }

    #[test]
    fn repeated_spaces_do_not_create_empty_tokens() {
        let mut editor = editor(&[]);
        typed(&mut editor, "work   ");
        assert_eq!(editor.input(), "work ");
    }

    #[test]
    fn ctrl_arrows_move_tag_by_tag() {
        let mut editor = editor(&["alpha", "beta"]);
        editor.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(editor.cursor(), 6); // start of "beta"
        editor.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(editor.cursor(), 0);
    }

    #[test]
    fn accepting_a_suggestion_mid_line_does_not_swallow_the_next_tag() {
        let mut editor = editor(&[]);
        typed(&mut editor, "cof work");
        // Move back into the first token.
        for _ in 0.."work".len() + 1 {
            editor.handle_key(&key(KeyCode::Left));
        }
        editor.handle_key(&key(KeyCode::Tab));
        assert_eq!(editor.input(), "coffee work");
        assert_eq!(editor.tags(), vec!["coffee", "work"]);
    }
}
