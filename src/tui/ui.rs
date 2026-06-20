use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Tabs},
};

use crate::{Transaction, TransactionWithEnrichment, TransferWithTransactions};

use super::app::{App, InputMode, Tab, TodoSubTab};
use super::keymap::{self, HelpLine};
use super::table::{ScrollTable, aligned_table, calculate_scroll_offset, column_span};

const DETAILS_HEIGHT: u16 = 8;

/// Inline detail height for the transfer panels: one account line aligned to
/// the description columns, plus one source/status line.
const TRANSFER_DETAIL_HEIGHT: u16 = 2;

/// Column layout shared by the Transfers table and its inline detail, so each
/// account lands directly under the matching description column.
const TRANSFER_COLS: [Constraint; 6] = [
    Constraint::Length(12),
    Constraint::Min(20),
    Constraint::Length(12),
    Constraint::Length(3),
    Constraint::Min(20),
    Constraint::Length(12),
];

/// Same as [`TRANSFER_COLS`] but for the Transfer Review table, whose last
/// column is the narrower confidence percentage.
const TRANSFER_REVIEW_COLS: [Constraint; 6] = [
    Constraint::Length(12),
    Constraint::Min(20),
    Constraint::Length(12),
    Constraint::Length(3),
    Constraint::Min(20),
    Constraint::Length(6),
];

/// Column layout shared by the transaction table and its inline detail, so the
/// account/source/metadata lines land under the description column. The third
/// column holds the category or transfer counterpart.
const TRANSACTION_COLS: [Constraint; 5] = [
    Constraint::Length(12),
    Constraint::Min(20),
    Constraint::Min(16),
    Constraint::Length(12),
    Constraint::Length(12),
];

pub fn draw(f: &mut Frame, app: &App) {
    let has_db_search = app.db_search_active() || app.input_mode == InputMode::DbSearch;
    let has_fuzzy_search = app.fuzzy_search_active() || app.input_mode == InputMode::FuzzySearch;

    // Header rows top to bottom: main tabs, then (Todo only) subtabs, then
    // any active search bars, then content. Computing the whole layout here
    // keeps it in one place, so popups that anchor to a header row (the
    // filter autocomplete) get handed the rect instead of guessing at
    // y-coordinates.
    let mut constraints = vec![Constraint::Length(2)];
    if app.current_tab == Tab::Todo {
        constraints.push(Constraint::Length(2));
    }
    if has_db_search {
        constraints.push(Constraint::Length(1));
    }
    if has_fuzzy_search {
        constraints.push(Constraint::Length(1));
    }
    let show_hints = app.hints_visible && !app.keybind_help_open;
    constraints.push(Constraint::Min(0));
    if show_hints {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::vertical(constraints).split(f.area());

    let mut idx = 0;
    draw_tabs(f, app, chunks[idx]);
    idx += 1;
    if app.current_tab == Tab::Todo {
        draw_todo_subtabs(f, app, chunks[idx]);
        idx += 1;
    }
    let mut db_search_area = None;
    if has_db_search {
        draw_db_search_bar(f, app, chunks[idx]);
        db_search_area = Some(chunks[idx]);
        idx += 1;
    }
    if has_fuzzy_search {
        draw_fuzzy_search_bar(f, app, chunks[idx]);
        idx += 1;
    }
    let content = chunks[idx];

    match app.current_tab {
        Tab::Transactions => draw_transactions(f, app, content),
        Tab::Transfers => draw_transfers(f, app, content),
        Tab::Categories => draw_categories(f, app, content),
        Tab::Todo => draw_todo(f, app, content),
    }

    if show_hints {
        draw_key_hints(f, app, chunks[idx + 1]);
    }

    if app.input_mode == InputMode::Category {
        draw_category_popup(f, app);
    }

    if app.input_mode == InputMode::CategoryEdit {
        draw_category_edit_popup(f, app);
    }

    if app.input_mode == InputMode::BulkApply {
        draw_bulk_apply_popup(f, app);
    }

    if matches!(app.input_mode, InputMode::ConfirmMerge | InputMode::Confirm) {
        draw_confirm_popup(f, app);
    }

    if app.input_mode == InputMode::TransferNoMatch {
        draw_no_match_popup(f, app);
    }

    if let Some(search_area) = db_search_area
        && app.filter_autocomplete_active()
        && app.input_mode == InputMode::DbSearch
    {
        draw_filter_autocomplete_popup(f, app, search_area);
    }

    if let Some(ref msg) = app.error_message {
        draw_error_popup(f, msg);
    }

    if app.keybind_help_open {
        draw_keybind_popup(f, app);
    }
}

fn draw_db_search_bar(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.input_mode == InputMode::DbSearch;

    if let Some(search_state) = app.current_search_state() {
        let (cursor_x, cursor_y) = search_state.search_bar.render(f, area, "/", is_active);
        if is_active {
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn draw_fuzzy_search_bar(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.input_mode == InputMode::FuzzySearch;
    let search_value = app.fuzzy_search_value();

    if is_active {
        let search_line = Line::from(vec![
            Span::styled("~", Style::default().fg(Color::DarkGray)),
            Span::styled(search_value, Style::default().fg(Color::Yellow)),
        ]);
        f.render_widget(Paragraph::new(search_line), area);

        // Position native terminal cursor after prefix "~" and before_cursor text
        let cursor_x = area.x + 1 + app.fuzzy_search_cursor() as u16;
        f.set_cursor_position((cursor_x, area.y));
    } else {
        let search_line = Line::from(vec![
            Span::styled("~", Style::default().fg(Color::DarkGray)),
            Span::styled(search_value, Style::default().fg(Color::DarkGray)),
        ]);
        f.render_widget(Paragraph::new(search_line), area);
    }
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::all().iter().map(|t| Line::from(t.title())).collect();

    let tabs = Tabs::new(titles)
        .select(
            Tab::all()
                .iter()
                .position(|&t| t == app.current_tab)
                .unwrap_or(0),
        )
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .divider("  ");

    f.render_widget(tabs, area);

    if app.refreshing {
        let label = "Refreshing...";
        let width = label.len() as u16;
        if area.width > width {
            let indicator_area = Rect::new(area.right().saturating_sub(width), area.y, width, 1);
            f.render_widget(
                Paragraph::new(Span::styled(label, Style::default().fg(Color::DarkGray)))
                    .alignment(Alignment::Right),
                indicator_area,
            );
        }
    }
}

fn draw_key_hints(f: &mut Frame, app: &App, area: Rect) {
    let spans = keymap::footer_hints(app)
        .into_iter()
        .flat_map(|(key, desc)| {
            [
                Span::raw("  "),
                Span::styled(key, Style::default().fg(Color::Cyan)),
                Span::styled(format!(" {desc}"), Style::default().fg(Color::DarkGray)),
            ]
        })
        .collect::<Vec<_>>();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_transactions(f: &mut Frame, app: &App, area: Rect) {
    let transactions: Vec<_> = app.lists.transactions.iter().collect();
    draw_transaction_table(f, app, &transactions, app.selected_index, area);
}

fn draw_transfers(f: &mut Frame, app: &App, area: Rect) {
    let transfers: Vec<_> = app.lists.linked_transfers.iter().collect();

    if transfers.is_empty() {
        draw_empty_message(f, "No linked transfers yet.", area);
        return;
    }

    ScrollTable::new(&transfers, app.selected_index, &TRANSFER_COLS)
        .detail(TRANSFER_DETAIL_HEIGHT, |f, twt, area| {
            draw_transfer_details(f, app, twt, area);
        })
        .render(f, area, |i, twt| {
            let is_selected = i == app.selected_index;
            let bg = if is_selected {
                Color::DarkGray
            } else {
                Color::Reset
            };

            let from = &twt.from_transaction;
            let to = &twt.to_transaction;

            Row::new(vec![
                Cell::from(from.date.to_string()),
                Cell::from(from.description.as_str()),
                Cell::from(Line::from(format_cents(from.amount_cents)).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Red)),
                Cell::from("→").style(Style::default().fg(Color::Cyan)),
                Cell::from(to.description.as_str()),
                Cell::from(Line::from(format_cents(to.amount_cents)).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Green)),
            ])
            .style(Style::default().bg(bg))
        });
}

fn draw_todo(f: &mut Frame, app: &App, area: Rect) {
    match app.todo_subtab {
        TodoSubTab::Uncategorised => {
            let uncategorised: Vec<_> = app.lists.uncategorised.iter().collect();
            draw_transaction_table(f, app, &uncategorised, app.selected_index, area);
        }
        TodoSubTab::AiReview => {
            draw_ai_review_table(f, app, area);
        }
        TodoSubTab::TransferReview => {
            draw_transfer_review_table(f, app, area);
        }
    }
}

fn draw_todo_subtabs(f: &mut Frame, app: &App, area: Rect) {
    let subtitles: Vec<Line> = TodoSubTab::all()
        .iter()
        .map(|t| {
            let count = match t {
                TodoSubTab::Uncategorised => app.lists.uncategorised.len(),
                TodoSubTab::AiReview => app.lists.ai_reviews.len(),
                TodoSubTab::TransferReview => app.lists.transfer_reviews.len(),
            };
            Line::from(format!("{} ({})", t.title(), count))
        })
        .collect();

    let subtabs = Tabs::new(subtitles)
        .select(
            TodoSubTab::all()
                .iter()
                .position(|&t| t == app.todo_subtab)
                .unwrap_or(0),
        )
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(Style::default().fg(Color::Cyan))
        .divider("  ");

    f.render_widget(subtabs, area);
}

/// Dimmed placeholder for views with nothing to show.
fn draw_empty_message(f: &mut Frame, message: &str, area: Rect) {
    let text = Paragraph::new(vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            message.to_string(),
            Style::default().fg(Color::DarkGray),
        )]),
    ]);
    f.render_widget(text, area);
}

fn draw_transaction_table(
    f: &mut Frame,
    app: &App,
    transactions: &[&Transaction],
    selected: usize,
    area: Rect,
) {
    let detail_height = transaction_detail_height(app, transactions.get(selected).copied());

    ScrollTable::new(transactions, selected, &TRANSACTION_COLS)
        .detail(detail_height, |f, tx, area| {
            draw_transaction_details(f, app, tx, area);
        })
        .render(f, area, |i, tx| {
            let is_selected = i == selected;
            let is_pending = app.is_pending_transfer_tx(tx.id);
            let is_candidate = app.is_transfer_candidate(tx.id);
            let is_disabled =
                app.input_mode == InputMode::TransferPending && !is_candidate && !is_pending;

            let bg = if is_pending {
                Color::Blue
            } else if is_selected && app.input_mode == InputMode::TransferPending {
                Color::Green
            } else if is_selected {
                Color::DarkGray
            } else {
                Color::Reset
            };

            let fg = if is_disabled {
                Color::DarkGray
            } else {
                Color::Reset
            };

            let amount_color = if is_disabled {
                Color::DarkGray
            } else if tx.amount_cents < 0 {
                Color::Red
            } else {
                Color::Green
            };

            let amount = format_cents(tx.amount_cents);
            let balance = format_cents(tx.balance_cents);

            // The selected row shows the account here and moves the full
            // (untruncated) description into the detail line below it.
            let middle = if is_selected {
                format!(
                    "{} / {}",
                    app.bank_name(tx.bank_id),
                    app.account_name(tx.account_id)
                )
            } else {
                tx.description.clone()
            };

            Row::new(vec![
                Cell::from(tx.date.to_string()).style(Style::default().fg(fg)),
                Cell::from(middle).style(Style::default().fg(fg)),
                category_or_transfer_cell(app, tx, is_disabled),
                Cell::from(Line::from(amount).alignment(Alignment::Right))
                    .style(Style::default().fg(amount_color)),
                Cell::from(Line::from(balance).alignment(Alignment::Right))
                    .style(Style::default().fg(fg)),
            ])
            .style(Style::default().bg(bg))
        });
}

/// The third transaction column: the category path (yellow, like the
/// Categories tab) or, for a transfer, `to:`/`from:` the counterpart account
/// (cyan). Dimmed to match a disabled row during transfer marking.
fn category_or_transfer_cell(app: &App, tx: &Transaction, dimmed: bool) -> Cell<'static> {
    if let Some(transfer) = app.get_cached_transfer(tx.id) {
        let (label, other_id) = if transfer.from_transaction_id == tx.id {
            ("to", transfer.to_transaction_id)
        } else {
            ("from", transfer.from_transaction_id)
        };
        let account = app
            .get_cached_transaction(other_id)
            .map(|other| app.account_name(other.account_id))
            .unwrap_or_default();
        let color = if dimmed { Color::DarkGray } else { Color::Cyan };
        Cell::from(Line::from(format!("{label}:{account}")).alignment(Alignment::Right))
            .style(Style::default().fg(color))
    } else if let Some(category) = app.get_cached_category(tx.id).filter(|c| !c.is_empty()) {
        let color = if dimmed {
            Color::DarkGray
        } else {
            Color::Yellow
        };
        Cell::from(Line::from(category.to_string()).alignment(Alignment::Right))
            .style(Style::default().fg(color))
    } else {
        Cell::from("")
    }
}

/// Height of the inline transaction detail: the full description line, the
/// source and metadata lines when expanded (`M`), and a trailing blank. Zero
/// when nothing is selected so the table renders without a detail block.
fn transaction_detail_height(app: &App, selected: Option<&Transaction>) -> u16 {
    match selected {
        Some(tx) => {
            let mut lines = 1; // description
            if app.tx_details_expanded {
                lines += 1; // source
                if !tx.metadata.is_empty() {
                    lines += 1; // metadata
                }
            }
            lines + 1 // trailing blank
        }
        None => 0,
    }
}

fn draw_ai_review_table(f: &mut Frame, app: &App, area: Rect) {
    let ai_reviews: Vec<_> = app.lists.ai_reviews.iter().collect();

    ScrollTable::new(
        &ai_reviews,
        app.selected_index,
        &[
            Constraint::Length(12),
            Constraint::Min(20),
            Constraint::Length(12),
            Constraint::Length(25),
            Constraint::Length(6),
        ],
    )
    .detail(DETAILS_HEIGHT, |f, review, area| {
        draw_ai_review_details(f, app, review, area);
    })
    .render(f, area, |i, review| {
        let is_selected = i == app.selected_index;
        let bg = if is_selected {
            Color::DarkGray
        } else {
            Color::Reset
        };

        let tx = &review.transaction;
        let category_path = review
            .category
            .as_ref()
            .map(|c| c.path.as_str())
            .unwrap_or("-");
        let confidence = review
            .enrichment
            .as_ref()
            .and_then(|e| e.ai_confidence)
            .map(format_confidence_percent)
            .unwrap_or_default();

        let amount_color = if tx.amount_cents < 0 {
            Color::Red
        } else {
            Color::Green
        };

        Row::new(vec![
            Cell::from(tx.date.to_string()),
            Cell::from(tx.description.as_str()),
            Cell::from(Line::from(format_cents(tx.amount_cents)).alignment(Alignment::Right))
                .style(Style::default().fg(amount_color)),
            Cell::from(category_path).style(Style::default().fg(Color::Yellow)),
            Cell::from(confidence).style(Style::default().fg(Color::Cyan)),
        ])
        .style(Style::default().bg(bg))
    });
}

fn draw_transfer_review_table(f: &mut Frame, app: &App, area: Rect) {
    let transfer_reviews: Vec<_> = app.lists.transfer_reviews.iter().collect();

    if transfer_reviews.is_empty() {
        draw_empty_message(f, "No pending transfer reviews.", area);
        return;
    }

    ScrollTable::new(&transfer_reviews, app.selected_index, &TRANSFER_REVIEW_COLS)
        .detail(TRANSFER_DETAIL_HEIGHT, |f, transfer, area| {
            draw_pending_transfer_details(f, app, transfer, area);
        })
        .render(f, area, |i, transfer| {
            let is_selected = i == app.selected_index;
            let bg = if is_selected {
                Color::DarkGray
            } else {
                Color::Reset
            };

            let from = app.get_cached_transaction(transfer.from_transaction_id);
            let to = app.get_cached_transaction(transfer.to_transaction_id);

            // The two legs of a transfer share one magnitude, so show the
            // amount once (on the "from" side) rather than duplicating it.
            let date = from
                .map(|tx| tx.date.to_string())
                .unwrap_or_else(|| format!("#{}", transfer.from_transaction_id));
            let from_desc = from.map(|tx| tx.description.clone()).unwrap_or_default();
            let amount = from
                .map(|tx| format_cents(tx.amount_cents))
                .unwrap_or_default();
            let to_desc = to.map(|tx| tx.description.clone()).unwrap_or_default();
            let confidence = transfer
                .ai_confidence
                .map(format_confidence_percent)
                .unwrap_or_default();

            Row::new(vec![
                Cell::from(date),
                Cell::from(from_desc),
                Cell::from(Line::from(amount).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Red)),
                Cell::from("→").style(Style::default().fg(Color::Cyan)),
                Cell::from(to_desc),
                Cell::from(Line::from(confidence).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Cyan)),
            ])
            .style(Style::default().bg(bg))
        });
}

fn draw_categories(f: &mut Frame, app: &App, area: Rect) {
    let categories: Vec<_> = app.lists.categories.iter().collect();

    if categories.is_empty() {
        draw_empty_message(f, "No categories yet.", area);
        return;
    }

    ScrollTable::new(
        &categories,
        app.selected_index,
        &[Constraint::Length(4), Constraint::Min(30)],
    )
    .render(f, area, |i, cat| {
        let is_selected = i == app.selected_index;
        let (bg, count_fg) = if is_selected {
            (Color::DarkGray, Color::Gray)
        } else {
            (Color::Reset, Color::DarkGray)
        };

        let tx_count = app.category_transaction_count(cat.id);

        Row::new(vec![
            Cell::from(Line::from(format!("{}", tx_count)).alignment(Alignment::Right))
                .style(Style::default().fg(count_fg)),
            Cell::from(cat.path.as_str()).style(Style::default().fg(Color::Yellow)),
        ])
        .style(Style::default().bg(bg))
    });
}

fn draw_keybind_popup(f: &mut Frame, app: &App) {
    let help = keymap::help_lines(app);
    let screen = f.area();
    let width = 64.min(screen.width.saturating_sub(2)).max(20);
    let desired_height = (help.len() as u16).saturating_add(4).max(8);
    let height = desired_height.min(screen.height.saturating_sub(2).max(1));
    let area = centered_rect_size(width, height, screen);

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Keybinds")
        .style(Style::default().bg(Color::Black).fg(Color::Cyan));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);

    let lines = help.into_iter().map(help_line).collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(Color::White))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        chunks[0],
    );

    let footer = Line::from(vec![
        Span::styled("Esc/Enter", Style::default().fg(Color::Cyan)),
        Span::styled(" close · ", Style::default().fg(Color::DarkGray)),
        Span::styled("Alt-?", Style::default().fg(Color::Cyan)),
        Span::styled(" toggle bar", Style::default().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(footer), chunks[1]);
}

fn help_line(line: HelpLine) -> Line<'static> {
    match line {
        HelpLine::Group(title) => Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        HelpLine::Bind(key, desc) => Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{key:<16}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(desc),
        ]),
        HelpLine::Blank => Line::from(""),
    }
}

fn draw_category_edit_popup(f: &mut Frame, app: &App) {
    let area = centered_rect_fixed_height(50, 4, f.area());

    f.render_widget(Clear, area);

    let block = Block::default().style(Style::default().bg(Color::Black));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let input_width = chunks[1].width as usize;
    let scroll = app.category_edit_scroll(input_width);
    let cursor_pos = app.category_edit_cursor();

    // Display scrolled portion of input
    let input_value = app.category_edit_value();
    let visible: String = input_value.chars().skip(scroll).take(input_width).collect();
    let input = Paragraph::new(visible).style(Style::default().fg(Color::Yellow));
    f.render_widget(input, chunks[1]);

    // Position cursor relative to scroll
    let cursor_x = chunks[1].x + (cursor_pos - scroll) as u16;
    f.set_cursor_position((cursor_x, chunks[1].y));

    let help = Paragraph::new(Line::from(vec![
        Span::styled("Enter", Style::default().fg(Color::Cyan)),
        Span::styled(" save  ", Style::default().fg(Color::White)),
        Span::styled("Esc", Style::default().fg(Color::Cyan)),
        Span::styled(" cancel", Style::default().fg(Color::White)),
    ]));
    f.render_widget(help, chunks[2]);
}

fn draw_confirm_popup(f: &mut Frame, app: &App) {
    let area = centered_rect_fixed_height(60, 6, f.area());

    f.render_widget(Clear, area);

    let block = Block::default().style(Style::default().bg(Color::Black));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };

    let message = app.confirm_message.as_deref().unwrap_or("Confirm action?");

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let msg_line = Paragraph::new(message)
        .style(Style::default().fg(Color::Yellow))
        .wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(msg_line, chunks[1]);

    let help = Paragraph::new(Line::from(vec![
        Span::styled("y", Style::default().fg(Color::Green)),
        Span::styled(" yes  ", Style::default().fg(Color::White)),
        Span::styled("n", Style::default().fg(Color::Red)),
        Span::styled(" no", Style::default().fg(Color::White)),
    ]));
    f.render_widget(help, chunks[2]);
}

fn draw_category_popup(f: &mut Frame, app: &App) {
    let area = centered_rect(50, 60, f.area());

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Category")
        .style(Style::default().bg(Color::Black));

    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);

    let input_style = Style::default().fg(Color::Yellow);
    let input = Paragraph::new(app.category_input.as_str()).style(input_style);
    f.render_widget(input, chunks[0]);

    let suggestions: Vec<Line> = app
        .category_suggestions
        .iter()
        .enumerate()
        .take(chunks[1].height as usize)
        .map(|(i, cat)| {
            let style = if i == app.category_selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::styled(&cat.path, style)
        })
        .collect();

    let suggestions_widget = Paragraph::new(suggestions);
    f.render_widget(suggestions_widget, chunks[1]);
}

fn draw_bulk_apply_popup(f: &mut Frame, app: &App) {
    let Some(state) = app.bulk_apply.as_ref() else {
        return;
    };

    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let selected = state.rows.iter().filter(|row| row.selected).count();
    let title = format!(
        "Apply \"{}\" to similar ({} selected)",
        state.category_path, selected
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().bg(Color::Black));
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let rows: Vec<_> = state.rows.iter().collect();
    ScrollTable::new(
        &rows,
        state.cursor,
        &[
            Constraint::Length(4),
            Constraint::Length(12),
            Constraint::Min(20),
            Constraint::Length(12),
            Constraint::Length(6),
        ],
    )
    .render(f, inner, |i, row| {
        let bg = if i == state.cursor {
            Color::DarkGray
        } else {
            Color::Reset
        };
        let amount_color = if row.tx.amount_cents < 0 {
            Color::Red
        } else {
            Color::Green
        };
        let checkbox = if row.selected { "[x]" } else { "[ ]" };
        let checkbox_color = if row.selected {
            Color::Green
        } else {
            Color::DarkGray
        };

        Row::new(vec![
            Cell::from(checkbox).style(Style::default().fg(checkbox_color)),
            Cell::from(row.tx.date.to_string()),
            Cell::from(row.tx.description.as_str()),
            Cell::from(Line::from(format_cents(row.tx.amount_cents)).alignment(Alignment::Right))
                .style(Style::default().fg(amount_color)),
            Cell::from(format_confidence_percent(f64::from(row.score)))
                .style(Style::default().fg(Color::Cyan)),
        ])
        .style(Style::default().bg(bg))
    });
}

/// Render the filter autocomplete popup anchored below the DB search bar.
/// `search_area` is the rect the search bar was drawn into.
fn draw_filter_autocomplete_popup(f: &mut Frame, app: &App, search_area: Rect) {
    let Some(search_state) = app.current_search_state() else {
        return;
    };
    let Some(ac_state) = search_state.search_bar.autocomplete() else {
        return;
    };

    if ac_state.suggestions.is_empty() {
        return;
    }

    let y = search_area.y + 1;

    let max_items = 8.min(ac_state.suggestions.len());
    let popup_height = max_items as u16;
    let popup_width = 40.min(f.area().width.saturating_sub(4));

    // Align with the anchor offset (after the ":"), accounting for the "/"
    // prefix the search bar renders before the input text.
    let x = (search_area.x + 1 + ac_state.anchor_offset as u16)
        .min(f.area().width.saturating_sub(popup_width));

    let area = Rect::new(x, y, popup_width, popup_height);

    f.render_widget(Clear, area);

    // No border, just a background
    let block = Block::default().style(Style::default().bg(Color::Black));
    f.render_widget(block, area);

    // Calculate scroll offset for suggestions
    let visible_height = area.height as usize;
    let offset = calculate_scroll_offset(
        ac_state.selected,
        ac_state.suggestions.len(),
        visible_height,
    );

    let suggestions: Vec<Line> = ac_state
        .suggestions
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_height)
        .map(|(i, suggestion)| {
            let style = if i == ac_state.selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::styled(suggestion.as_str(), style)
        })
        .collect();

    let suggestions_widget = Paragraph::new(suggestions);
    f.render_widget(suggestions_widget, area);
}

fn draw_no_match_popup(f: &mut Frame, app: &App) {
    let area = centered_rect(40, 20, f.area());

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("No Match")
        .style(Style::default().bg(Color::Black).fg(Color::Red));

    let tx = app.pending_transfer_tx.as_ref();
    let msg = if let Some(tx) = tx {
        format!(
            "No matching transaction found for\n{} ({})\n\nPress Escape to cancel",
            format_cents(tx.amount_cents),
            tx.description
        )
    } else {
        "No matching transaction found.\n\nPress Escape to cancel".to_string()
    };

    let paragraph = Paragraph::new(msg)
        .block(block)
        .style(Style::default().fg(Color::White));

    f.render_widget(paragraph, area);
}

fn draw_error_popup(f: &mut Frame, msg: &str) {
    let area = centered_rect(40, 15, f.area());

    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("Error")
        .style(Style::default().bg(Color::Black).fg(Color::Red));

    let paragraph = Paragraph::new(msg)
        .block(block)
        .style(Style::default().fg(Color::White));

    f.render_widget(paragraph, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let popup_height = r.height * percent_y / 100;
    let x = (r.width - popup_width) / 2;
    let y = (r.height - popup_height) / 2;
    Rect::new(r.x + x, r.y + y, popup_width, popup_height)
}

fn centered_rect_fixed_height(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let popup_height = height.min(r.height);
    let x = (r.width - popup_width) / 2;
    let y = (r.height - popup_height) / 2;
    Rect::new(r.x + x, r.y + y, popup_width, popup_height)
}

fn centered_rect_size(width: u16, height: u16, r: Rect) -> Rect {
    let popup_width = width.min(r.width);
    let popup_height = height.min(r.height);
    let x = (r.width - popup_width) / 2;
    let y = (r.height - popup_height) / 2;
    Rect::new(r.x + x, r.y + y, popup_width, popup_height)
}

fn format_cents(cents: i64) -> String {
    let dollars = cents.abs() / 100;
    let remainder = cents.abs() % 100;
    let sign = if cents < 0 { "-" } else { "" };
    format!("{}${}.{:02}", sign, dollars, remainder)
}

fn format_confidence_percent(confidence: f64) -> String {
    format!("{:.0}%", confidence * 100.0)
}

/// Inline transaction detail: the full (untruncated) description directly under
/// the row's account, then (when expanded with `M`) the source file and
/// metadata. Each line starts under the description column and spans the rest
/// of the width; the final detail line is left blank.
fn draw_transaction_details(f: &mut Frame, app: &App, tx: &Transaction, area: Rect) {
    // The description is primary (default colour); source/metadata are dimmed.
    let mut values = vec![(tx.description.clone(), Color::Reset)];

    if app.tx_details_expanded {
        values.push((tx.source_file.clone(), Color::DarkGray));
        if !tx.metadata.is_empty() {
            let metadata = serde_json::to_string(&tx.metadata).unwrap_or_default();
            values.push((metadata, Color::DarkGray));
        }
    }

    // Start each line under the description column (index 1) and span the rest
    // of the row width, so long descriptions and metadata stay readable.
    let (x, width) = column_span(&TRANSACTION_COLS, area, 1);
    for (i, (value, color)) in values.into_iter().enumerate() {
        if i as u16 >= area.height {
            break;
        }
        let line_area = Rect {
            x,
            y: area.y + i as u16,
            width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Span::styled(value, Style::default().fg(color))),
            line_area,
        );
    }
}

fn draw_transfer_details(f: &mut Frame, app: &App, twt: &TransferWithTransactions, area: Rect) {
    draw_transfer_pair(
        f,
        app,
        &twt.from_transaction,
        &twt.to_transaction,
        twt.transfer.source.as_str(),
        &TRANSFER_COLS,
        area,
    );
}

/// Glyph shown under the `→` to indicate how the transfer was linked.
/// `auto` (classifier-detected) is the default and shows nothing; `ai` =
/// AI-suggested, `manual` = linked by the user.
fn source_glyph(source: &str) -> &'static str {
    match source {
        "ai" => "✦",
        "manual" => "☝",
        _ => "",
    }
}

/// Render the shared inline transfer detail used by both the confirmed-transfer
/// panel and the pending-review one. Each account sits directly under its
/// description column and the source glyph under the `→` (the panel reuses the
/// row table's `widths` via [`aligned_table`]); the line below is left blank.
fn draw_transfer_pair(
    f: &mut Frame,
    app: &App,
    from: &crate::Transaction,
    to: &crate::Transaction,
    source: &str,
    widths: &[Constraint],
    area: Rect,
) {
    let from_account = format!(
        "{} / {}",
        app.bank_name(from.bank_id),
        app.account_name(from.account_id)
    );
    let to_account = format!(
        "{} / {}",
        app.bank_name(to.bank_id),
        app.account_name(to.account_id)
    );

    // Empty cells in the date/amount columns push each account under its
    // description column (indices 1 and 4) and the source glyph under the
    // arrow (index 3), matching the row above. The remaining detail line is
    // left blank.
    let account_row = Row::new(vec![
        Cell::from(""),
        Cell::from(from_account).style(Style::default().fg(Color::DarkGray)),
        Cell::from(""),
        Cell::from(source_glyph(source)).style(Style::default().fg(Color::Cyan)),
        Cell::from(to_account).style(Style::default().fg(Color::DarkGray)),
        Cell::from(""),
    ]);
    let row_area = Rect { height: 1, ..area };
    f.render_widget(aligned_table(vec![account_row], widths), row_area);
}

fn draw_ai_review_details(
    f: &mut Frame,
    app: &App,
    review: &TransactionWithEnrichment,
    area: Rect,
) {
    let tx = &review.transaction;
    let bank_name = app.bank_name(tx.bank_id);
    let account_name = app.account_name(tx.account_id);

    let amount_style = if tx.amount_cents < 0 {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };

    let category_path = review
        .category
        .as_ref()
        .map(|c| c.path.as_str())
        .unwrap_or("-");

    let confidence = review
        .enrichment
        .as_ref()
        .and_then(|e| e.ai_confidence)
        .map(format_confidence_percent)
        .unwrap_or_else(|| "-".to_string());

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Date: ", Style::default().fg(Color::DarkGray)),
            Span::raw(tx.date.to_string()),
            Span::raw("  "),
            Span::styled("Amount: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format_cents(tx.amount_cents), amount_style),
            Span::raw("  "),
            Span::styled("Balance: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format_cents(tx.balance_cents)),
        ]),
        Line::from(vec![
            Span::styled("Account: ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{} / {}", bank_name, account_name)),
        ]),
        Line::from(vec![
            Span::styled("Description: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&tx.description),
        ]),
        Line::from(vec![
            Span::styled("AI Category: ", Style::default().fg(Color::Yellow)),
            Span::raw(category_path),
            Span::raw("  "),
            Span::styled("Confidence: ", Style::default().fg(Color::Cyan)),
            Span::raw(confidence),
        ]),
        Line::from(vec![
            Span::styled("Source: ", Style::default().fg(Color::DarkGray)),
            Span::raw(&tx.source_file),
        ]),
    ];

    let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_pending_transfer_details(f: &mut Frame, app: &App, transfer: &crate::Transfer, area: Rect) {
    let (from_tx, to_tx) = match (
        app.get_cached_transaction(transfer.from_transaction_id),
        app.get_cached_transaction(transfer.to_transaction_id),
    ) {
        (Some(f), Some(t)) => (f, t),
        _ => {
            let lines = vec![Line::from(vec![
                Span::styled("Error: ", Style::default().fg(Color::Red)),
                Span::raw("Could not load transfer transactions"),
            ])];
            f.render_widget(Paragraph::new(lines), area);
            return;
        }
    };

    draw_transfer_pair(
        f,
        app,
        from_tx,
        to_tx,
        transfer.source.as_str(),
        &TRANSFER_REVIEW_COLS,
        area,
    );
}
