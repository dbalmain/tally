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
        self.input.cursor()
    }

    /// Get the parsed query.
    pub fn parsed(&self) -> &ParsedQuery {
        &self.parsed
    }

    /// Get the current cursor context.
    pub fn context(&self) -> &CursorContext {
        &self.context
    }

    /// First validation error relevant to the cursor (invalid filter or
    /// regex), prioritising the part containing the cursor and falling back
    /// to the leftmost invalid part. `None` when everything parses cleanly.
    pub fn error_message(&self) -> Option<&str> {
        self.parsed.error_at_cursor(self.cursor())
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
        self.update_autocomplete();
    }

    /// Replace the search configuration and reparse the current input.
    pub fn set_config(&mut self, config: SearchConfig) {
        self.config = config;
        self.reparse();
        self.update_autocomplete();
    }

    /// Reset the search bar to empty state.
    pub fn reset(&mut self) {
        self.input.reset();
        self.parsed = ParsedQuery::empty();
        self.context = CursorContext::Whitespace;
        self.autocomplete = None;
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
        if let Some(ref mut ac) = self.autocomplete
            && !ac.suggestions.is_empty()
        {
            ac.selected = (ac.selected + 1) % ac.suggestions.len();
        }
    }

    /// Navigate to previous autocomplete suggestion.
    pub fn autocomplete_prev(&mut self) {
        if let Some(ref mut ac) = self.autocomplete
            && !ac.suggestions.is_empty()
        {
            ac.selected = (ac.selected + ac.suggestions.len() - 1) % ac.suggestions.len();
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
        let filter_part = self.parsed.parts.iter().find(|p| {
            matches!(p, QueryPart::Filter { name, value_span, .. }
                    if *name == ac.filter_name
                        && ac.anchor_offset >= value_span.start
                        && ac.anchor_offset <= value_span.end)
        });

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

        let segment_end_in_value = value
            .chars()
            .enumerate()
            .skip(segment_start_in_value)
            .find_map(|(idx, c)| (c == '|').then_some(idx))
            .unwrap_or(char_len(value));

        // Build new value with replaced segment
        let old_input = self.input.value().to_string();
        let segment_start = value_start + segment_start_in_value;
        let segment_end = value_start + segment_end_in_value;

        let mut new_input = String::with_capacity(old_input.len() + selected.len());
        new_input.push_str(char_slice(&old_input, 0, segment_start));
        new_input.push_str(&selected);
        new_input.push_str(char_slice(&old_input, segment_end, char_len(&old_input)));

        self.input = Input::new(new_input);
        self.set_cursor(segment_start + char_len(&selected));

        self.reparse();
        true
    }

    /// Close autocomplete popup.
    pub fn autocomplete_close(&mut self) {
        self.autocomplete = None;
    }

    /// Set the input value and position cursor at the given character index.
    fn set_input(&mut self, value: String, cursor_pos: usize) {
        self.input = Input::new(value);
        self.set_cursor(cursor_pos);
    }

    /// Position cursor at the given character index.
    fn set_cursor(&mut self, cursor_pos: usize) {
        let value = self.input.value();
        let tail = char_len(value).saturating_sub(cursor_pos);
        for _ in 0..tail {
            self.input.handle(InputRequest::GoToPrevChar);
        }
    }

    /// Reparse the input and update context.
    fn reparse(&mut self) {
        let value = self.input.value();
        let cursor = self.cursor();
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
        let filter_part = self.parsed.parts.iter().find(|p| {
            matches!(p, QueryPart::Filter { name: n, value_span, .. }
                    if n == name
                        && self.cursor() >= value_span.start
                        && self.cursor() <= value_span.end)
        });

        let Some(QueryPart::Filter {
            value, value_span, ..
        }) = filter_part
        else {
            self.autocomplete = None;
            return;
        };

        // Get completions from the filter
        let Some((suggestions, anchor_in_value)) = filter.completions(value, *offset) else {
            self.autocomplete = None;
            return;
        };

        if suggestions.is_empty() {
            self.autocomplete = None;
            return;
        }

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

        let word_start = word_start_before_cursor(&value, cursor);

        if word_start == cursor {
            return false; // No word before cursor
        }

        let word = char_slice(&value, word_start, cursor);

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
        let before = char_slice(&value, 0, word_start).trim_end();
        let after = char_slice(&value, cursor, char_len(&value)).trim_start();
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
                let prefix = char_slice(&cleaned, 0, pos).trim_end();
                let suffix = char_slice(&cleaned, pos, char_len(&cleaned)).trim_start();
                if prefix.is_empty() {
                    let cursor = char_len(&filter_text);
                    (format!("{} {}", filter_text, suffix), cursor)
                } else {
                    let cursor = char_len(prefix) + 1 + char_len(&filter_text);
                    (format!("{} {} {}", prefix, filter_text, suffix), cursor)
                }
            } else if cleaned.is_empty() {
                let cursor = char_len(&filter_text);
                (filter_text.clone(), cursor)
            } else {
                let cursor = char_len(&cleaned) + 1 + char_len(&filter_text);
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
        new_value.push_str(char_slice(&value, 0, cursor));
        new_value.push_str("//");
        new_value.push_str(char_slice(&value, cursor, char_len(&value)));

        self.input = Input::new(new_value);
        self.set_cursor(cursor + 1);

        true
    }

    /// Handle delete in regex - delete entire regex when deleting a delimiter.
    fn handle_delete_in_regex(&mut self, req: &InputRequest) -> bool {
        let cursor = self.cursor();

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

        // Check if we're deleting the first or last slash
        let deleting_delimiter = match req {
            InputRequest::DeletePrevChar => cursor == span.start + 1 || cursor == span.end,
            InputRequest::DeleteNextChar => cursor == span.start || cursor == span.end - 1,
            _ => false,
        };

        if !deleting_delimiter {
            return false;
        }

        // Delete entire regex
        let value = self.input.value();
        let mut new_value = String::with_capacity(value.len());
        new_value.push_str(char_slice(value, 0, span.start));

        // Handle surrounding whitespace
        let after = char_slice(value, span.end, char_len(value));
        let after = after.strip_prefix(' ').unwrap_or(after);
        if !new_value.is_empty() && !after.is_empty() && !new_value.ends_with(' ') {
            new_value.push(' ');
        }
        new_value.push_str(after);

        let new_cursor = span.start.min(char_len(&new_value));
        self.input = Input::new(new_value);
        self.set_cursor(new_cursor);

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

        if !value.is_empty() {
            spans.extend(self.styled_spans(active.then_some(cursor)));
        }

        let line = Line::from(spans);
        f.render_widget(Paragraph::new(line), area);

        // Right-aligned error hint, only when there's enough room without
        // covering the user's input. When the input gets wide enough to
        // collide, the red token colouring inside the bar is the only signal
        // — better that than overwriting what the user is typing.
        if active && let Some(err) = self.error_message() {
            let err_text = format!(" ⚠ {}", err);
            let err_w = char_len(&err_text) as u16;
            let used_w = (prefix.chars().count() + char_len(value)) as u16;
            if area.width > used_w + err_w + 1 {
                let err_area = Rect {
                    x: area.x + area.width - err_w,
                    width: err_w,
                    ..area
                };
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        err_text,
                        Style::default().fg(Color::Red),
                    ))),
                    err_area,
                );
            }
        }

        // Calculate cursor position
        let cursor_x = area.x + prefix.chars().count() as u16 + self.input.visual_cursor() as u16;

        (cursor_x, area.y)
    }

    /// Generate styled spans for the search bar content.
    fn styled_spans(&self, cursor: Option<usize>) -> Vec<Span<'_>> {
        let value = self.input.value();
        let mut spans = Vec::new();

        // Determine which parts are "active" (cursor is in them)
        let active_part_idx = cursor.and_then(|c| {
            if matches!(self.context, CursorContext::Fts { .. }) {
                // When in FTS, all FTS parts are active
                None
            } else {
                self.parsed.parts.iter().position(|p| {
                    let span = p.span();
                    c >= span.start && c <= span.end
                })
            }
        });

        for (idx, part) in self.parsed.parts.iter().enumerate() {
            let span = part.span();
            let text = char_slice(value, span.start, span.end);

            let is_active = match active_part_idx {
                Some(active_idx) => idx == active_idx,
                None => {
                    // When in FTS context, all FTS parts are active
                    cursor.is_some() && matches!(part, QueryPart::Fts { .. })
                }
            };

            let color = self.color_for_part(part, is_active);
            spans.push(Span::styled(text, Style::default().fg(color)));
        }

        // Handle trailing content not covered by parts
        if let Some(last) = self.parsed.parts.last() {
            let end = last.span().end;
            if end < char_len(value) {
                let text = char_slice(value, end, char_len(value));
                let color = if cursor.is_some() {
                    Color::Cyan
                } else {
                    Color::DarkGray
                };
                spans.push(Span::styled(text, Style::default().fg(color)));
            }
        } else if !value.is_empty() {
            // No parts - show all as FTS
            let color = if cursor.is_some() {
                Color::Cyan
            } else {
                Color::DarkGray
            };
            spans.push(Span::styled(value, Style::default().fg(color)));
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

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn char_slice(s: &str, start: usize, end: usize) -> &str {
    let start_byte = char_to_byte_index(s, start);
    let end_byte = char_to_byte_index(s, end);
    &s[start_byte..end_byte]
}

fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn word_start_before_cursor(s: &str, cursor: usize) -> usize {
    let chars: Vec<char> = s.chars().take(cursor).collect();
    chars
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, c)| c.is_whitespace().then_some(idx + 1))
        .unwrap_or(0)
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
    fn test_unicode_input_uses_character_indices() {
        let mut bar = SearchBar::new(SearchConfig::new(vec![
            Box::new(DateFilter),
            Box::new(CategoryFilter::with_options(vec![
                "Food/Cafe".to_string(),
                "Food/Café".to_string(),
            ])),
        ]));

        for c in "café d".chars() {
            bar.handle_input(InputRequest::InsertChar(c));
        }
        bar.handle_input(InputRequest::InsertChar(':'));
        assert_eq!(bar.value(), "date: café");
        assert_eq!(bar.cursor(), "date:".chars().count());

        bar.set_value("category:Caf");
        assert!(bar.autocomplete_active());
        assert!(bar.autocomplete_select());
        assert!(bar.value().starts_with("category:Food/Caf"));
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
