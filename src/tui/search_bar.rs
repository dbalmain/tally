//! SearchBar component for the new search system.
//!
//! Provides context-aware key handling based on cursor position and
//! integrates with the Filter trait for completions.
//!
//! # Features
//!
//! - **Context-aware input**: Keys behave differently based on cursor position
//!   - `/` in FTS/whitespace: Creates `//` with cursor between for new regex
//!   - `|` in filter: Inserts and triggers autocomplete for next segment
//!   - Deleting regex delimiters: Removes entire regex token
//!
//! - **Validity display**: Parts colored based on validity state
//!   - Yellow: Valid filters (active) / DarkGray (inactive)
//!   - Red: Invalid filters or regex
//!   - Magenta: Valid regex (active)
//!   - Cyan: FTS text (active)
//!
//! - **Autocomplete**: Shows completions for filters that support them
//!   - AccountFilter and CategoryFilter provide fuzzy-matched suggestions
//!   - Popup anchors at the start of the current segment being typed

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use tui_input::{Input, InputRequest};

use crate::search::{CursorContext, FilterResult, ParsedQuery, QueryPart, SearchConfig, parse};

/// State for filter autocomplete popup.
#[derive(Debug, Clone)]
pub struct AutocompleteState {
    /// Filter name that triggered autocomplete.
    pub filter_name: &'static str,
    /// List of suggestions.
    pub suggestions: Vec<String>,
    /// Currently selected index.
    pub selected: usize,
    /// Character offset within the search input where popup should anchor.
    pub anchor_offset: usize,
}

/// Result of handling a key in the search bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyResult {
    /// Key was handled, search bar consumed it.
    Handled,
    /// Key was not handled, pass to parent.
    NotHandled,
    /// Search was confirmed (Enter pressed).
    Confirmed,
    /// Search was cancelled (Esc pressed).
    Cancelled,
    /// Transition to fuzzy search triggered.
    TransitionToFuzzy,
}

/// A search bar component with context-aware key handling.
pub struct SearchBar {
    /// The text input state.
    input: Input,
    /// The parsed query (updated after each input change).
    parsed: ParsedQuery,
    /// Current cursor context.
    context: CursorContext,
    /// Search configuration with available filters.
    config: SearchConfig,
    /// Autocomplete popup state.
    autocomplete: Option<AutocompleteState>,
}

impl SearchBar {
    /// Create a new search bar with the given configuration.
    pub fn new(config: SearchConfig) -> Self {
        Self {
            input: Input::default(),
            parsed: ParsedQuery::empty(),
            context: CursorContext::Whitespace,
            config,
            autocomplete: None,
        }
    }

    /// Get the current input value.
    pub fn value(&self) -> &str {
        self.input.value()
    }

    /// Get the current cursor position (character index).
    pub fn cursor(&self) -> usize {
        self.input.visual_cursor()
    }

    /// Get the parsed query.
    pub fn parsed(&self) -> &ParsedQuery {
        &self.parsed
    }

    /// Get the current cursor context.
    pub fn context(&self) -> &CursorContext {
        &self.context
    }

    /// Get the autocomplete state if active.
    pub fn autocomplete(&self) -> Option<&AutocompleteState> {
        self.autocomplete.as_ref()
    }

    /// Check if autocomplete popup is active.
    pub fn autocomplete_active(&self) -> bool {
        self.autocomplete.is_some()
    }

    /// Set the input value and reparse.
    pub fn set_value(&mut self, value: &str) {
        self.input = Input::new(value.to_string());
        self.reparse();
    }

    /// Reset the search bar to empty state.
    pub fn reset(&mut self) {
        self.input.reset();
        self.parsed = ParsedQuery::empty();
        self.context = CursorContext::Whitespace;
        self.autocomplete = None;
    }

    /// Update the search configuration.
    pub fn set_config(&mut self, config: SearchConfig) {
        self.config = config;
        self.reparse();
    }

    /// Handle an input request and return whether it was handled.
    pub fn handle_input(&mut self, req: InputRequest) -> KeyResult {
        // Check for special context-aware behavior
        match self.handle_special_input(&req) {
            KeyResult::NotHandled => {}
            res => {
                self.reparse();
                self.update_autocomplete();
                return res;
            }
        }

        // Normal input handling
        self.input.handle(req);
        self.reparse();
        self.update_autocomplete();
        KeyResult::Handled
    }

    /// Navigate to next autocomplete suggestion.
    pub fn autocomplete_next(&mut self) {
        if let Some(ref mut ac) = self.autocomplete {
            if !ac.suggestions.is_empty() {
                ac.selected = (ac.selected + 1) % ac.suggestions.len();
            }
        }
    }

    /// Navigate to previous autocomplete suggestion.
    pub fn autocomplete_prev(&mut self) {
        if let Some(ref mut ac) = self.autocomplete {
            if !ac.suggestions.is_empty() {
                ac.selected = (ac.selected + ac.suggestions.len() - 1) % ac.suggestions.len();
            }
        }
    }

    /// Select current autocomplete suggestion.
    pub fn autocomplete_select(&mut self) -> bool {
        let Some(ac) = self.autocomplete.take() else {
            return false;
        };

        let Some(selected) = ac.suggestions.get(ac.selected).cloned() else {
            return false;
        };

        // Find the filter part that corresponds to this autocomplete
        let filter_part = self
            .parsed
            .parts
            .iter()
            .find(|p| matches!(p, QueryPart::Filter { name, .. } if *name == ac.filter_name));

        let Some(QueryPart::Filter {
            value_span, value, ..
        }) = filter_part
        else {
            return false;
        };

        // Find the segment within the value that we're replacing
        // The anchor_offset is relative to the start of the input
        // We need to find the segment in the value that starts at anchor_offset - value_span.start
        let value_start = value_span.start;
        let segment_start_in_value = ac.anchor_offset.saturating_sub(value_start);

        // Find end of segment (next | or end of value)
        let segment_end_in_value = value[segment_start_in_value..]
            .find('|')
            .map(|i| segment_start_in_value + i)
            .unwrap_or(value.len());

        // Build new value with replaced segment
        let old_input = self.input.value().to_string();
        let segment_start = value_start + segment_start_in_value;
        let segment_end = value_start + segment_end_in_value;

        let mut new_input = String::with_capacity(old_input.len() + selected.len());
        new_input.push_str(&old_input[..segment_start]);
        new_input.push_str(&selected);
        new_input.push_str(&old_input[segment_end..]);

        // Position cursor after the inserted value
        let new_cursor = segment_start + selected.len();
        let tail_len = new_input.len().saturating_sub(new_cursor);

        self.input = Input::new(new_input);
        for _ in 0..tail_len {
            self.input.handle(InputRequest::GoToPrevChar);
        }

        self.reparse();
        true
    }

    /// Close autocomplete popup.
    pub fn autocomplete_close(&mut self) {
        self.autocomplete = None;
    }

    /// Set the input value and position cursor at the given character index.
    fn set_input(&mut self, value: String, cursor_pos: usize) {
        let tail = value.len().saturating_sub(cursor_pos);
        self.input = Input::new(value);
        for _ in 0..tail {
            self.input.handle(InputRequest::GoToPrevChar);
        }
    }

    /// Reparse the input and update context.
    fn reparse(&mut self) {
        let value = self.input.value();
        let cursor = self.input.visual_cursor();
        let (parsed, context) = parse(&self.config, value, cursor);
        self.parsed = parsed;
        self.context = context;
    }

    /// Update autocomplete state based on current context.
    fn update_autocomplete(&mut self) {
        // Only show autocomplete when in a filter context
        let CursorContext::Filter { name, offset } = &self.context else {
            self.autocomplete = None;
            return;
        };

        // Find the filter implementation
        let Some(filter) = self.config.find_filter_by_name(name) else {
            self.autocomplete = None;
            return;
        };

        // Find the filter part to get the value
        let filter_part = self
            .parsed
            .parts
            .iter()
            .find(|p| matches!(p, QueryPart::Filter { name: n, .. } if n == name));

        let Some(QueryPart::Filter {
            value, value_span, ..
        }) = filter_part
        else {
            self.autocomplete = None;
            return;
        };

        // Get completions from the filter
        let Some((mut suggestions, anchor_in_value)) = filter.completions(value, *offset) else {
            self.autocomplete = None;
            return;
        };

        if suggestions.is_empty() {
            self.autocomplete = None;
            return;
        }

        // Make sure suggestions are sorted
        suggestions.sort();

        // Calculate anchor offset in the full input
        let anchor_offset = value_span.start + anchor_in_value;

        // Preserve selection if we already had autocomplete state for the same filter
        let selected = self
            .autocomplete
            .as_ref()
            .filter(|ac| ac.filter_name == *name)
            .map(|ac| ac.selected.min(suggestions.len().saturating_sub(1)))
            .unwrap_or(0);

        self.autocomplete = Some(AutocompleteState {
            filter_name: name,
            suggestions,
            selected,
            anchor_offset,
        });
    }

    /// Handle special input cases based on context.
    /// Returns the result of handling the input.
    fn handle_special_input(&mut self, req: &InputRequest) -> KeyResult {
        match req {
            InputRequest::InsertChar(':') => {
                if self.handle_colon_insert() {
                    KeyResult::Handled
                } else {
                    KeyResult::NotHandled
                }
            }
            InputRequest::InsertChar('/') => {
                if self.handle_slash_insert() {
                    KeyResult::Handled
                } else {
                    KeyResult::NotHandled
                }
            }
            InputRequest::InsertChar('~') => {
                if self.handle_tilde_insert() {
                    KeyResult::TransitionToFuzzy
                } else {
                    KeyResult::NotHandled
                }
            }
            InputRequest::DeletePrevChar | InputRequest::DeleteNextChar => {
                if self.handle_delete_in_regex(req) {
                    KeyResult::Handled
                } else {
                    KeyResult::NotHandled
                }
            }
            _ => KeyResult::NotHandled,
        }
    }

    /// Handle `:` key - filter creation, deduplication, and reordering.
    ///
    /// When `:` is typed in FTS/whitespace context:
    /// 1. Look at the word before cursor (e.g., `d` in `coffee d:`)
    /// 2. Check if it matches a registered filter name or alias
    /// 3. If matching filter already exists → delete typed text, jump cursor to that filter
    /// 4. If no existing filter → create `name:` and move it before FTS text
    fn handle_colon_insert(&mut self) -> bool {
        // Only special behavior in FTS or whitespace context
        if !matches!(
            self.context,
            CursorContext::Fts { .. } | CursorContext::Whitespace
        ) {
            return false;
        }

        let cursor = self.cursor();
        let value = self.input.value().to_string();

        // Find word before cursor (scan back for whitespace)
        let word_start = value[..cursor]
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);

        if word_start == cursor {
            return false; // No word before cursor
        }

        let word = &value[word_start..cursor];

        // Resolve to canonical filter name
        let Some(canonical) = self.config.resolve_filter_name(word) else {
            return false;
        };

        // Check if this filter already exists in the query
        let existing = self
            .parsed
            .parts
            .iter()
            .any(|p| matches!(p, QueryPart::Filter { name, .. } if *name == canonical));

        // Remove the word from input, trimming surrounding whitespace
        let before = value[..word_start].trim_end();
        let after = value[cursor..].trim_start();
        let cleaned = match (before.is_empty(), after.is_empty()) {
            (true, true) => String::new(),
            (true, false) => after.to_string(),
            (false, true) => before.to_string(),
            (false, false) => format!("{} {}", before, after),
        };

        if existing {
            // Filter exists: jump cursor to end of its value
            let (parsed, _) = parse(&self.config, &cleaned, 0);

            let target = parsed
                .parts
                .iter()
                .find_map(|p| match p {
                    QueryPart::Filter {
                        name, value_span, ..
                    } if *name == canonical => Some(value_span.end),
                    _ => None,
                })
                .unwrap_or(0);

            self.set_input(cleaned, target);
        } else {
            // New filter: insert before FTS
            let filter_text = format!("{}:", canonical);

            // Parse cleaned string to find FTS position
            let (cleaned_parsed, _) = parse(&self.config, &cleaned, 0);

            let fts_start = cleaned_parsed.parts.iter().find_map(|p| match p {
                QueryPart::Fts { span, .. } => Some(span.start),
                _ => None,
            });

            let (new_value, cursor_pos) = if let Some(pos) = fts_start {
                let prefix = cleaned[..pos].trim_end();
                let suffix = cleaned[pos..].trim_start();
                if prefix.is_empty() {
                    (
                        format!("{} {}", filter_text, suffix),
                        filter_text.len(),
                    )
                } else {
                    let cursor = prefix.len() + 1 + filter_text.len();
                    (format!("{} {} {}", prefix, filter_text, suffix), cursor)
                }
            } else if cleaned.is_empty() {
                (filter_text.clone(), filter_text.len())
            } else {
                let cursor = cleaned.len() + 1 + filter_text.len();
                (format!("{} {}", cleaned, filter_text), cursor)
            };

            self.set_input(new_value, cursor_pos);
        }

        true
    }

    /// Handle `~` key - transition to fuzzy search if at word boundary.
    fn handle_tilde_insert(&mut self) -> bool {
        let cursor = self.cursor();
        let value = self.input.value();

        // Only transition at word boundary (start of input or preceded by whitespace)
        cursor == 0
            || value
                .chars()
                .nth(cursor.saturating_sub(1))
                .map(|c| c.is_whitespace())
                .unwrap_or(false)
    }

    /// Handle `/` key - context-aware regex handling.
    fn handle_slash_insert(&mut self) -> bool {
        // Only special behavior in FTS or whitespace context
        if !matches!(
            self.context,
            CursorContext::Fts { .. } | CursorContext::Whitespace
        ) {
            return false;
        }

        // Check if there's already a regex in the input
        let has_regex = self
            .parsed
            .parts
            .iter()
            .any(|p| matches!(p, QueryPart::Regex { .. }));

        if has_regex {
            // If cursor is inside an existing regex, move to end
            if let Some(QueryPart::Regex { span, .. }) = self.parsed.parts.iter().find(
                |p| matches!(p, QueryPart::Regex { span, .. } if span.contains(self.cursor())),
            ) {
                // Move cursor past closing slash
                let move_to = span.end;
                let current = self.cursor();
                for _ in current..move_to {
                    self.input.handle(InputRequest::GoToNextChar);
                }
                return true;
            }
            // Otherwise, just insert normally
            return false;
        }

        // No regex exists - insert `//` and place cursor between
        let value = self.input.value().to_string();
        let cursor = self.cursor();

        let mut new_value = String::with_capacity(value.len() + 2);
        new_value.push_str(&value[..cursor]);
        new_value.push_str("//");
        new_value.push_str(&value[cursor..]);

        // Place cursor between the slashes
        let new_cursor = cursor + 1;
        let tail_len = new_value.len() - new_cursor;

        self.input = Input::new(new_value);
        for _ in 0..tail_len {
            self.input.handle(InputRequest::GoToPrevChar);
        }

        true
    }

    /// Handle delete in regex - delete entire regex when deleting a delimiter.
    fn handle_delete_in_regex(&mut self, req: &InputRequest) -> bool {
        let cursor = self.cursor();
        let value = self.input.value().to_string();

        // Find regex that cursor is in or adjacent to
        let regex_part = self.parsed.parts.iter().find(|p| {
            matches!(p, QueryPart::Regex { span, .. } if {
                match req {
                    InputRequest::DeletePrevChar => cursor > span.start && cursor <= span.end,
                    InputRequest::DeleteNextChar => cursor >= span.start && cursor < span.end,
                    _ => false,
                }
            })
        });

        let Some(QueryPart::Regex { span, .. }) = regex_part else {
            return false;
        };

        // Check if we're deleting a slash delimiter
        let deleting_delimiter = match req {
            InputRequest::DeletePrevChar => {
                // Deleting opening slash or closing slash
                cursor == span.start + 1 || {
                    // Check if cursor is just after closing slash
                    let content = &value[span.start..span.end];
                    if let Some(closing_pos) = content.rfind('/') {
                        cursor == span.start + closing_pos + 1
                    } else {
                        false
                    }
                }
            }
            InputRequest::DeleteNextChar => {
                // Deleting opening slash
                cursor == span.start || {
                    // Check if cursor is at closing slash
                    let content = &value[span.start..span.end];
                    if let Some(closing_pos) = content[1..].rfind('/') {
                        cursor == span.start + 1 + closing_pos
                    } else {
                        false
                    }
                }
            }
            _ => false,
        };

        if !deleting_delimiter {
            return false;
        }

        // Delete entire regex
        let mut new_value = String::with_capacity(value.len());
        new_value.push_str(&value[..span.start]);

        // Handle surrounding whitespace
        let after = &value[span.end..];
        let after = after.strip_prefix(' ').unwrap_or(after);
        if !new_value.is_empty() && !after.is_empty() && !new_value.ends_with(' ') {
            new_value.push(' ');
        }
        new_value.push_str(after);

        let new_cursor = span.start.min(new_value.len());
        let tail_len = new_value.len().saturating_sub(new_cursor);

        self.input = Input::new(new_value);
        for _ in 0..tail_len {
            self.input.handle(InputRequest::GoToPrevChar);
        }

        true
    }

    /// Render the search bar.
    ///
    /// Returns the cursor position for the terminal (x, y).
    pub fn render(&self, f: &mut Frame, area: Rect, prefix: &str, active: bool) -> (u16, u16) {
        let value = self.input.value();
        let cursor = self.cursor();

        // Build styled spans for each token
        let mut spans = vec![Span::styled(prefix, Style::default().fg(Color::DarkGray))];

        if value.is_empty() {
            // Empty input - just show prefix
        } else if active {
            spans.extend(self.styled_spans_active(cursor));
        } else {
            spans.extend(self.styled_spans_inactive());
        }

        let line = Line::from(spans);
        f.render_widget(Paragraph::new(line), area);

        // Calculate cursor position
        let (before_cursor, _) = split_at_char_index(value, cursor);
        let cursor_x = area.x + prefix.len() as u16 + before_cursor.len() as u16;

        (cursor_x, area.y)
    }

    /// Generate styled spans for active (editing) state.
    fn styled_spans_active(&self, cursor: usize) -> Vec<Span<'_>> {
        let value = self.input.value();
        let mut spans = Vec::new();

        // Find which part the cursor is in
        let cursor_part_idx = self.parsed.parts.iter().position(|p| {
            let span = p.span();
            cursor >= span.start && cursor <= span.end
        });

        // Also check if cursor is in FTS block (all FTS parts highlighted together)
        let cursor_in_fts = matches!(self.context, CursorContext::Fts { .. });

        for (idx, part) in self.parsed.parts.iter().enumerate() {
            let span = part.span();
            let text = &value[span.start..span.end];

            let is_active = if cursor_in_fts {
                matches!(part, QueryPart::Fts { .. })
            } else {
                Some(idx) == cursor_part_idx
            };

            let color = self.color_for_part(part, is_active);
            spans.push(Span::styled(text, Style::default().fg(color)));
        }

        // Handle any trailing content not covered by parts
        if let Some(last) = self.parsed.parts.last() {
            let end = last.span().end;
            if end < value.len() {
                let text = &value[end..];
                spans.push(Span::styled(text, Style::default().fg(Color::Cyan)));
            }
        } else if !value.is_empty() {
            // No parts - show all as FTS
            spans.push(Span::styled(value, Style::default().fg(Color::Cyan)));
        }

        spans
    }

    /// Generate styled spans for inactive state.
    fn styled_spans_inactive(&self) -> Vec<Span<'_>> {
        let value = self.input.value();
        let mut spans = Vec::new();

        for part in &self.parsed.parts {
            let span = part.span();
            let text = &value[span.start..span.end];
            let color = self.color_for_part(part, false);
            spans.push(Span::styled(text, Style::default().fg(color)));
        }

        // Handle trailing content
        if let Some(last) = self.parsed.parts.last() {
            let end = last.span().end;
            if end < value.len() {
                let text = &value[end..];
                spans.push(Span::styled(text, Style::default().fg(Color::DarkGray)));
            }
        } else if !value.is_empty() {
            spans.push(Span::styled(value, Style::default().fg(Color::DarkGray)));
        }

        spans
    }

    /// Get the color for a query part.
    fn color_for_part(&self, part: &QueryPart, active: bool) -> Color {
        match (part, active) {
            (QueryPart::Whitespace { .. }, _) => Color::Reset,
            (QueryPart::Filter { result, .. }, true) => {
                // Empty/Invalid = Red, Valid = Yellow
                if matches!(result, FilterResult::Valid { .. }) {
                    Color::Yellow
                } else {
                    Color::Red
                }
            }
            (QueryPart::Filter { result, .. }, false) => {
                // Empty/Invalid = Red, Valid = DarkGray
                if matches!(result, FilterResult::Valid { .. }) {
                    Color::DarkGray
                } else {
                    Color::Red
                }
            }
            (QueryPart::Regex { valid, .. }, true) => {
                if *valid {
                    Color::Magenta
                } else {
                    Color::Red
                }
            }
            (QueryPart::Regex { valid, .. }, false) => {
                if *valid {
                    Color::DarkGray
                } else {
                    Color::Red
                }
            }
            (QueryPart::Fts { .. }, true) => Color::Cyan,
            (QueryPart::Fts { .. }, false) => Color::DarkGray,
        }
    }
}

/// Split a string at a character index.
fn split_at_char_index(s: &str, char_idx: usize) -> (&str, &str) {
    let byte_idx = s
        .char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    s.split_at(byte_idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{AccountFilter, AmountFilter, CategoryFilter, DateFilter};

    fn test_config() -> SearchConfig {
        SearchConfig::new(vec![
            Box::new(DateFilter),
            Box::new(AmountFilter),
            Box::new(AccountFilter::with_options(vec![
                "ING/Orange".to_string(),
                "NAB/Classic".to_string(),
            ])),
            Box::new(CategoryFilter::with_options(vec![
                "Food".to_string(),
                "Transport".to_string(),
            ])),
        ])
    }

    #[test]
    fn test_new_search_bar() {
        let bar = SearchBar::new(test_config());
        assert!(bar.value().is_empty());
        assert_eq!(bar.cursor(), 0);
        assert!(bar.parsed().is_empty());
    }

    #[test]
    fn test_set_value() {
        let mut bar = SearchBar::new(test_config());
        bar.set_value("date:2024");
        assert_eq!(bar.value(), "date:2024");
        assert!(!bar.parsed().is_empty());
    }

    #[test]
    fn test_handle_input() {
        let mut bar = SearchBar::new(test_config());
        bar.handle_input(InputRequest::InsertChar('d'));
        bar.handle_input(InputRequest::InsertChar(':'));
        bar.handle_input(InputRequest::InsertChar('2'));
        bar.handle_input(InputRequest::InsertChar('0'));
        bar.handle_input(InputRequest::InsertChar('2'));
        bar.handle_input(InputRequest::InsertChar('4'));
        // d: gets expanded to date: by colon handler
        assert_eq!(bar.value(), "date:2024");
    }

    #[test]
    fn test_colon_expands_alias() {
        let mut bar = SearchBar::new(test_config());
        // Type "am:" - should expand to "amount:"
        for c in "am".chars() {
            bar.handle_input(InputRequest::InsertChar(c));
        }
        bar.handle_input(InputRequest::InsertChar(':'));
        assert_eq!(bar.value(), "amount:");
        assert_eq!(bar.cursor(), 7);
    }

    #[test]
    fn test_colon_reorders_before_fts() {
        let mut bar = SearchBar::new(test_config());
        // Type "coffee d:" - should become "date: coffee"
        for c in "coffee d".chars() {
            bar.handle_input(InputRequest::InsertChar(c));
        }
        bar.handle_input(InputRequest::InsertChar(':'));
        assert_eq!(bar.value(), "date: coffee");
        assert_eq!(bar.cursor(), 5); // After "date:"
    }

    #[test]
    fn test_colon_dedup_jumps_to_existing() {
        let mut bar = SearchBar::new(test_config());
        // Type "date:2024 " then "d:"
        for c in "date:2024 d".chars() {
            bar.handle_input(InputRequest::InsertChar(c));
        }
        bar.handle_input(InputRequest::InsertChar(':'));
        assert_eq!(bar.value(), "date:2024");
        assert_eq!(bar.cursor(), 9); // After "2024"
    }

    #[test]
    fn test_colon_dedup_with_fts() {
        let mut bar = SearchBar::new(test_config());
        // Type "date:2024 coffee d:" - should remove "d", keep FTS, jump to existing filter
        for c in "date:2024 coffee d".chars() {
            bar.handle_input(InputRequest::InsertChar(c));
        }
        bar.handle_input(InputRequest::InsertChar(':'));
        assert_eq!(bar.value(), "date:2024 coffee");
        assert_eq!(bar.cursor(), 9); // After "2024"
    }

    #[test]
    fn test_colon_unknown_filter_passes_through() {
        let mut bar = SearchBar::new(test_config());
        // Type "foo:" - "foo" isn't a filter, so ':' inserted normally
        for c in "foo".chars() {
            bar.handle_input(InputRequest::InsertChar(c));
        }
        bar.handle_input(InputRequest::InsertChar(':'));
        assert_eq!(bar.value(), "foo:");
    }

    #[test]
    fn test_colon_reorders_with_existing_filter() {
        let mut bar = SearchBar::new(test_config());
        // Type "amount:>100 coffee d:" - should become "amount:>100 date: coffee"
        for c in "amount:>100 coffee d".chars() {
            bar.handle_input(InputRequest::InsertChar(c));
        }
        bar.handle_input(InputRequest::InsertChar(':'));
        assert_eq!(bar.value(), "amount:>100 date: coffee");
        assert_eq!(bar.cursor(), 17); // After "date:"
    }

    #[test]
    fn test_slash_insert_creates_regex() {
        let mut bar = SearchBar::new(test_config());
        bar.handle_input(InputRequest::InsertChar('/'));
        assert_eq!(bar.value(), "//");
        assert_eq!(bar.cursor(), 1); // Between the slashes
    }

    #[test]
    fn test_autocomplete_in_filter() {
        let mut bar = SearchBar::new(test_config());
        bar.set_value("account:I");
        // Move cursor to end
        for _ in 0..10 {
            bar.handle_input(InputRequest::GoToNextChar);
        }
        bar.reparse();
        bar.update_autocomplete();
        assert!(bar.autocomplete_active());
        let ac = bar.autocomplete().unwrap();
        assert!(ac.suggestions.iter().any(|s| s.contains("ING")));
    }

    #[test]
    fn test_autocomplete_filter_before_fts() {
        // Autocomplete must work when filter appears before FTS terms
        let mut bar = SearchBar::new(test_config());
        // Type "account:I coffee" with cursor right after "I"
        for c in "account:I".chars() {
            bar.handle_input(InputRequest::InsertChar(c));
        }
        // Add " coffee" after cursor
        let value = bar.value().to_string();
        let cursor = bar.cursor();
        bar.set_input(format!("{} coffee", value), cursor);
        bar.reparse();
        bar.update_autocomplete();
        assert!(
            bar.autocomplete_active(),
            "autocomplete should be active when filter is before FTS, context: {:?}",
            bar.context()
        );
        let ac = bar.autocomplete().unwrap();
        assert!(ac.suggestions.iter().any(|s| s.contains("ING")));
    }

    #[test]
    fn test_tilde_transition_to_fuzzy() {
        let mut bar = SearchBar::new(test_config());
        // At start of input
        let res = bar.handle_input(InputRequest::InsertChar('~'));
        assert_eq!(res, KeyResult::TransitionToFuzzy);
        assert_eq!(bar.value(), ""); // Should NOT have inserted it

        // After space
        bar.set_value("date:2024 ");
        // Move cursor to end
        for _ in 0..10 {
            bar.handle_input(InputRequest::GoToNextChar);
        }
        let res = bar.handle_input(InputRequest::InsertChar('~'));
        assert_eq!(res, KeyResult::TransitionToFuzzy);
        assert_eq!(bar.value(), "date:2024 "); // Should NOT have inserted it
    }

    #[test]
    fn test_reset() {
        let mut bar = SearchBar::new(test_config());
        bar.set_value("date:2024");
        bar.reset();
        assert!(bar.value().is_empty());
        assert!(bar.parsed().is_empty());
    }
}
