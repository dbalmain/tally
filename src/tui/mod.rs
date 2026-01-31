mod app;
mod ui;

pub use app::App;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
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

    let mut app = App::new(store);
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

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('j') | KeyCode::Down => app.next(),
                        KeyCode::Char('k') | KeyCode::Up => app.previous(),
                        KeyCode::Tab => {
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                app.previous_tab();
                            } else {
                                app.next_tab();
                            }
                        }
                        KeyCode::BackTab => app.previous_tab(),
                        KeyCode::Char('[') => app.previous_subtab(),
                        KeyCode::Char(']') => app.next_subtab(),
                        KeyCode::Char('/') => app.start_db_search(),
                        KeyCode::Char('~') => app.start_fuzzy_search(),
                        KeyCode::Char('c') => app.start_category_edit(),
                        KeyCode::Char('t') => app.start_transfer_mark(),
                        KeyCode::Char('d') => app.delete_transfer(),
                        KeyCode::Enter => {
                            app.confirm_ai_category();
                            app.confirm_transfer_review();
                        }
                        _ => {}
                    },
                    InputMode::DbSearch => match key.code {
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
                        KeyCode::Char(c) => {
                            app.handle_db_search_input(InputRequest::InsertChar(c));
                        }
                        KeyCode::Backspace => {
                            app.handle_db_search_input(InputRequest::DeletePrevChar);
                        }
                        KeyCode::Delete => {
                            app.handle_db_search_input(InputRequest::DeleteNextChar);
                        }
                        KeyCode::Left => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                app.handle_db_search_input(InputRequest::GoToPrevWord);
                            } else {
                                app.handle_db_search_input(InputRequest::GoToPrevChar);
                            }
                        }
                        KeyCode::Right => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                app.handle_db_search_input(InputRequest::GoToNextWord);
                            } else {
                                app.handle_db_search_input(InputRequest::GoToNextChar);
                            }
                        }
                        KeyCode::Home => {
                            app.handle_db_search_input(InputRequest::GoToStart);
                        }
                        KeyCode::End => {
                            app.handle_db_search_input(InputRequest::GoToEnd);
                        }
                        _ => {}
                    },
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
                        KeyCode::Char(c) => {
                            app.handle_fuzzy_search_input(InputRequest::InsertChar(c));
                        }
                        KeyCode::Backspace => {
                            app.handle_fuzzy_search_input(InputRequest::DeletePrevChar);
                        }
                        KeyCode::Delete => {
                            app.handle_fuzzy_search_input(InputRequest::DeleteNextChar);
                        }
                        KeyCode::Left => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                app.handle_fuzzy_search_input(InputRequest::GoToPrevWord);
                            } else {
                                app.handle_fuzzy_search_input(InputRequest::GoToPrevChar);
                            }
                        }
                        KeyCode::Right => {
                            if key.modifiers.contains(KeyModifiers::CONTROL) {
                                app.handle_fuzzy_search_input(InputRequest::GoToNextWord);
                            } else {
                                app.handle_fuzzy_search_input(InputRequest::GoToNextChar);
                            }
                        }
                        KeyCode::Home => {
                            app.handle_fuzzy_search_input(InputRequest::GoToStart);
                        }
                        KeyCode::End => {
                            app.handle_fuzzy_search_input(InputRequest::GoToEnd);
                        }
                        _ => {}
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
        }
    }
}
