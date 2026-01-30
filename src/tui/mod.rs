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

use crate::{Result, TransactionStore};

use app::InputMode;

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
            if key.kind == KeyEventKind::Press {
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
                        KeyCode::Char('c') => app.start_category_edit(),
                        KeyCode::Char('t') => app.start_transfer_mark(),
                        KeyCode::Char('d') => app.delete_transfer(),
                        KeyCode::Enter => {
                            app.confirm_ai_category();
                            app.confirm_transfer_review();
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
                    InputMode::TransferNoMatch => match key.code {
                        KeyCode::Esc => app.cancel_input(),
                        _ => {}
                    },
                }
            }
        }
    }
}
