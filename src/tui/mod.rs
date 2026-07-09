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

use crate::search::SearchOptions;
use crate::{RefreshReport, Result, TransactionStore};

use app::InputMode;

/// Launch the interactive TUI application.
pub fn run(
    store: TransactionStore,
    refresh_rx: Receiver<Result<RefreshReport>>,
    search_options: SearchOptions,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new_with_refreshing_and_search_options(store, true, search_options)?;
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

fn handle_tab_switch(app: &mut App, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Tab => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                app.previous_tab();
            } else {
                app.next_tab();
            }
            true
        }
        KeyCode::BackTab => {
            app.previous_tab();
            true
        }
        _ => false,
    }
}

fn handle_autocomplete_nav(
    app: &mut App,
    key: &KeyEvent,
    next: fn(&mut App),
    prev: fn(&mut App),
    select: fn(&mut App) -> bool,
    close: fn(&mut App),
) -> bool {
    match key.code {
        KeyCode::Down => {
            next(app);
            true
        }
        KeyCode::Up => {
            prev(app);
            true
        }
        KeyCode::Tab | KeyCode::Enter if select(app) => true,
        KeyCode::Esc => {
            close(app);
            true
        }
        _ => false,
    }
}

fn handle_filter_edit_ctrl(app: &mut App, key: &KeyEvent) -> bool {
    let Some(action) = keymap::filter_edit_ctrl_action(key) else {
        return false;
    };

    match action {
        keymap::FilterEditCtrlAction::Rename => app.start_filter_rename(),
        keymap::FilterEditCtrlAction::Category => app.start_category_edit(),
        keymap::FilterEditCtrlAction::CycleOverride => app.cycle_filter_override(),
        keymap::FilterEditCtrlAction::ToggleReview => app.toggle_filter_review(),
        keymap::FilterEditCtrlAction::Apply => app.apply_filter_categories(),
    }
    true
}

fn handle_yes_no(app: &mut App, key: &KeyEvent, on_yes: fn(&mut App), on_no: fn(&mut App)) -> bool {
    let yes = matches!(key.code, KeyCode::Enter)
        || matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'y'));
    if yes {
        on_yes(app);
        return true;
    }

    let no = matches!(key.code, KeyCode::Esc)
        || matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'n'));
    if no {
        on_no(app);
        return true;
    }

    false
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

        // The draw above shows the loading modal (`classifying` is set); run the
        // blocking pipeline now, then loop to draw the result modal.
        if app.classify_requested {
            app.classify_requested = false;
            app.run_classification();
            continue;
        }

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if app.classify_report.is_some() {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    app.dismiss_classify_report();
                }
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
                && keymap::help_available(app.input_mode)
            {
                app.keybind_help_open = true;
                continue;
            }

            match app.input_mode {
                InputMode::Normal => keymap::dispatch_normal(app, key),
                InputMode::DbSearch => {
                    if keymap::is_ctrl(&key, 's') {
                        app.start_filter_from_search();
                        continue;
                    }

                    if app.filter_autocomplete_active()
                        && handle_autocomplete_nav(
                            app,
                            &key,
                            App::filter_autocomplete_next,
                            App::filter_autocomplete_prev,
                            App::filter_autocomplete_select,
                            App::filter_autocomplete_close,
                        )
                    {
                        continue;
                    }

                    if handle_tab_switch(app, &key) {
                        continue;
                    }

                    match key.code {
                        KeyCode::Esc => app.clear_db_search(),
                        KeyCode::Enter => app.confirm_db_search(),
                        KeyCode::Down => app.next(),
                        KeyCode::Up => app.previous(),
                        _ => {
                            if let Some(req) = text_edit_request(&key) {
                                app.handle_db_search_input(req);
                            }
                        }
                    }
                }
                InputMode::FuzzySearch => {
                    if handle_tab_switch(app, &key) {
                        continue;
                    }

                    match key.code {
                        KeyCode::Esc => app.clear_fuzzy_search(),
                        KeyCode::Enter => app.confirm_fuzzy_search(),
                        KeyCode::Down => app.next(),
                        KeyCode::Up => app.previous(),
                        _ => {
                            if let Some(req) = text_edit_request(&key) {
                                app.handle_fuzzy_search_input(req);
                            }
                        }
                    }
                }
                InputMode::FilterEdit => {
                    if handle_filter_edit_ctrl(app, &key) {
                        continue;
                    }

                    if app.filter_edit_autocomplete_active()
                        && handle_autocomplete_nav(
                            app,
                            &key,
                            App::filter_edit_autocomplete_next,
                            App::filter_edit_autocomplete_prev,
                            App::filter_edit_autocomplete_select,
                            App::filter_edit_autocomplete_close,
                        )
                    {
                        continue;
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
                InputMode::Confirm => {
                    handle_yes_no(app, &key, App::confirm_proceed, App::cancel_input);
                }
                InputMode::ConfirmApplyFilters => {
                    if handle_yes_no(app, &key, App::apply_filters_confirm, App::cancel_input) {
                        continue;
                    }

                    match key.code {
                        KeyCode::Char('j') | KeyCode::Down => app.apply_filters_preview_next(),
                        KeyCode::Char('k') | KeyCode::Up => app.apply_filters_preview_prev(),
                        _ => {}
                    }
                }
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
