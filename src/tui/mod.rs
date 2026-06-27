mod app;
mod filtered_list;
mod keymap;
mod modal;
pub mod search_bar;
mod table;
mod ui;

pub use app::App;
pub use search_bar::SearchBar;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;
use tui_input::InputRequest;

use crate::{RefreshReport, Result, TransactionStore};

use app::InputMode;

/// Launch the interactive TUI application.
pub fn run(store: TransactionStore, refresh_rx: Receiver<Result<RefreshReport>>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new_with_refreshing(store, true)?;
    let res = run_app(&mut terminal, &mut app, &refresh_rx);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res
}

/// Translate a key event into the text-editing request shared by every
/// text-input mode (DB search, fuzzy search, text prompt). Mode-specific
/// keys (Esc, Enter, Tab, arrows-as-navigation) are handled before falling
/// back to this.
fn text_edit_request(key: &KeyEvent) -> Option<InputRequest> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) => Some(InputRequest::InsertChar(c)),
        KeyCode::Backspace => Some(InputRequest::DeletePrevChar),
        KeyCode::Delete => Some(InputRequest::DeleteNextChar),
        KeyCode::Left if ctrl => Some(InputRequest::GoToPrevWord),
        KeyCode::Left => Some(InputRequest::GoToPrevChar),
        KeyCode::Right if ctrl => Some(InputRequest::GoToNextWord),
        KeyCode::Right => Some(InputRequest::GoToNextChar),
        KeyCode::Home => Some(InputRequest::GoToStart),
        KeyCode::End => Some(InputRequest::GoToEnd),
        _ => None,
    }
}

fn ctrl_char(key: &KeyEvent, expected: char) -> bool {
    matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&expected))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    refresh_rx: &Receiver<Result<RefreshReport>>,
) -> Result<()> {
    loop {
        if app.should_quit {
            return Ok(());
        }

        match refresh_rx.try_recv() {
            Ok(Ok(_report)) => {
                app.refreshing = false;
                app.refresh_data();
            }
            Ok(Err(e)) => {
                app.refreshing = false;
                app.error_message = Some(format!("Refresh failed: {e}"));
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                app.refreshing = false;
            }
        }

        terminal.draw(|f| ui::draw(f, app))?;

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if app.keybind_help_open {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?')) {
                    app.keybind_help_open = false;
                }
                continue;
            }

            if key.code == KeyCode::Char('?') && key.modifiers.contains(KeyModifiers::ALT) {
                app.hints_visible = !app.hints_visible;
                continue;
            }

            if key.code == KeyCode::Char('?')
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && matches!(
                    app.input_mode,
                    InputMode::Normal
                        | InputMode::FilterEdit
                        | InputMode::ConfirmMerge
                        | InputMode::Confirm
                        | InputMode::ConfirmApplyFilters
                        | InputMode::BulkApply
                        | InputMode::TransferPending
                        | InputMode::TransferNoMatch
                )
            {
                app.keybind_help_open = true;
                continue;
            }

            match app.input_mode {
                InputMode::Normal => keymap::dispatch_normal(app, key),
                InputMode::DbSearch => {
                    if ctrl_char(&key, 's') {
                        app.start_filter_from_search();
                        continue;
                    }

                    // Handle autocomplete popup navigation when active
                    if app.filter_autocomplete_active() {
                        match key.code {
                            KeyCode::Down => {
                                app.filter_autocomplete_next();
                                continue;
                            }
                            KeyCode::Up => {
                                app.filter_autocomplete_prev();
                                continue;
                            }
                            KeyCode::Tab | KeyCode::Enter if app.filter_autocomplete_select() => {
                                continue;
                                // If no selection made, fall through to normal behavior
                            }
                            KeyCode::Esc => {
                                app.filter_autocomplete_close();
                                continue;
                            }
                            _ => {
                                // Other keys close popup and proceed normally
                                // (the popup will re-open if still in a filter value)
                            }
                        }
                    }

                    match key.code {
                        KeyCode::Esc => app.clear_db_search(),
                        KeyCode::Enter => app.confirm_db_search(),
                        KeyCode::Tab => {
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                app.previous_tab();
                            } else {
                                app.next_tab();
                            }
                        }
                        KeyCode::BackTab => app.previous_tab(),
                        KeyCode::Down => app.next(),
                        KeyCode::Up => app.previous(),
                        _ => {
                            if let Some(req) = text_edit_request(&key) {
                                app.handle_db_search_input(req);
                            }
                        }
                    }
                }
                InputMode::FuzzySearch => match key.code {
                    KeyCode::Esc => app.clear_fuzzy_search(),
                    KeyCode::Enter => app.confirm_fuzzy_search(),
                    KeyCode::Tab => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            app.previous_tab();
                        } else {
                            app.next_tab();
                        }
                    }
                    KeyCode::BackTab => app.previous_tab(),
                    KeyCode::Down => app.next(),
                    KeyCode::Up => app.previous(),
                    _ => {
                        if let Some(req) = text_edit_request(&key) {
                            app.handle_fuzzy_search_input(req);
                        }
                    }
                },
                InputMode::FilterEdit => {
                    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                    if ctrl {
                        match key.code {
                            KeyCode::Char('r') | KeyCode::Char('R') => {
                                app.start_filter_rename();
                                continue;
                            }
                            KeyCode::Char('c') | KeyCode::Char('C') => {
                                app.start_category_edit();
                                continue;
                            }
                            KeyCode::Char('o') | KeyCode::Char('O') => {
                                app.cycle_filter_override();
                                continue;
                            }
                            KeyCode::Char('v') | KeyCode::Char('V') => {
                                app.toggle_filter_review();
                                continue;
                            }
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                app.apply_filter_categories();
                                continue;
                            }
                            _ => {}
                        }
                    }

                    if app.filter_edit_autocomplete_active() {
                        match key.code {
                            KeyCode::Down => {
                                app.filter_edit_autocomplete_next();
                                continue;
                            }
                            KeyCode::Up => {
                                app.filter_edit_autocomplete_prev();
                                continue;
                            }
                            KeyCode::Tab | KeyCode::Enter
                                if app.filter_edit_autocomplete_select() =>
                            {
                                continue;
                            }
                            KeyCode::Esc => {
                                app.filter_edit_autocomplete_close();
                                continue;
                            }
                            _ => {}
                        }
                    }

                    match key.code {
                        KeyCode::Esc => app.request_exit_filter_edit(),
                        KeyCode::Down => app.filter_edit_preview_next(),
                        KeyCode::Up => app.filter_edit_preview_prev(),
                        KeyCode::Enter => app.save_filter_edit(),
                        _ => {
                            if let Some(req) = text_edit_request(&key) {
                                app.filter_edit_input(req);
                            }
                        }
                    }
                }
                InputMode::Category => match key.code {
                    KeyCode::Esc => app.cancel_input(),
                    KeyCode::Enter => app.confirm_category(),
                    KeyCode::Backspace => app.backspace_category_input(),
                    KeyCode::Down => app.category_next(),
                    KeyCode::Up => app.category_previous(),
                    KeyCode::Char(c) => app.update_category_input(c),
                    _ => {}
                },
                InputMode::BulkApply => match key.code {
                    KeyCode::Esc => app.bulk_apply_cancel(),
                    KeyCode::Enter => app.bulk_apply_confirm(),
                    KeyCode::Char(' ') => app.bulk_apply_toggle(),
                    KeyCode::Char('a') => app.bulk_apply_toggle_all(),
                    KeyCode::Char('j') | KeyCode::Down => app.bulk_apply_next(),
                    KeyCode::Char('k') | KeyCode::Up => app.bulk_apply_prev(),
                    _ => {}
                },
                InputMode::TextPrompt => match key.code {
                    KeyCode::Esc => app.cancel_input(),
                    KeyCode::Enter => app.confirm_text_prompt(),
                    _ => {
                        if let Some(req) = text_edit_request(&key) {
                            app.handle_text_prompt_input(req);
                        }
                    }
                },
                InputMode::ConfirmMerge => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        app.confirm_merge();
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        app.cancel_merge();
                    }
                    _ => {}
                },
                InputMode::Confirm => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        app.confirm_proceed();
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        app.cancel_input();
                    }
                    _ => {}
                },
                InputMode::ConfirmApplyFilters => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        app.apply_filters_confirm();
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        app.cancel_input();
                    }
                    KeyCode::Char('j') | KeyCode::Down => app.apply_filters_preview_next(),
                    KeyCode::Char('k') | KeyCode::Up => app.apply_filters_preview_prev(),
                    _ => {}
                },
                InputMode::TransferPending => match key.code {
                    KeyCode::Esc => app.cancel_input(),
                    KeyCode::Char('t') => app.start_transfer_mark(),
                    KeyCode::Char('T') | KeyCode::Enter => app.complete_transfer(),
                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                    _ => {}
                },
                InputMode::TransferNoMatch => {
                    if key.code == KeyCode::Esc {
                        app.cancel_input();
                    }
                }
            }

            if app.should_quit {
                return Ok(());
            }
        }
    }
}
