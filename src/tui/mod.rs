mod app;
mod filtered_list;
mod keymap;
pub mod search_bar;
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
use tui_input::InputRequest;

use crate::{Result, TransactionStore};

use app::InputMode;

/// Launch the interactive TUI application.
pub fn run(store: TransactionStore) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(store)?;
    let res = run_app(&mut terminal, &mut app);

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
/// text-input mode (DB search, fuzzy search, category rename). Mode-specific
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

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        if app.should_quit {
            return Ok(());
        }

        terminal.draw(|f| ui::draw(f, app))?;

        if let Event::Key(key) = event::read()? {
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
                        | InputMode::ConfirmMerge
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
                InputMode::Category => match key.code {
                    KeyCode::Esc => app.cancel_input(),
                    KeyCode::Enter => app.confirm_category(),
                    KeyCode::Backspace => app.backspace_category_input(),
                    KeyCode::Down => app.category_next(),
                    KeyCode::Up => app.category_previous(),
                    KeyCode::Char(c) => app.update_category_input(c),
                    _ => {}
                },
                InputMode::CategoryEdit => match key.code {
                    KeyCode::Esc => app.cancel_input(),
                    KeyCode::Enter => app.confirm_category_rename(),
                    _ => {
                        if let Some(req) = text_edit_request(&key) {
                            app.handle_category_edit_input(req);
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
