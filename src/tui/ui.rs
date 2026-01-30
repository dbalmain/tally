use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs},
    Frame,
};

use crate::{Transaction, TransactionWithEnrichment, TransferWithTransactions};

use super::app::{App, InputMode, Tab, TodoSubTab};

const DETAILS_HEIGHT: u16 = 8;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(f.area());

    draw_tabs(f, app, chunks[0]);

    match app.current_tab {
        Tab::Transactions => draw_transactions(f, app, chunks[1]),
        Tab::Transfers => draw_transfers(f, app, chunks[1]),
        Tab::Todo => draw_todo(f, app, chunks[1]),
    }

    if app.input_mode == InputMode::Category {
        draw_category_popup(f, app);
    }

    if app.input_mode == InputMode::TransferNoMatch {
        draw_no_match_popup(f, app);
    }

    if let Some(ref msg) = app.error_message {
        draw_error_popup(f, msg);
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
}

fn draw_transactions(f: &mut Frame, app: &App, area: Rect) {
    let chunks =
        Layout::vertical([Constraint::Min(0), Constraint::Length(DETAILS_HEIGHT)]).split(area);

    draw_transaction_table(f, app, &app.transactions, app.selected_index, chunks[0]);

    if let Some(tx) = app.transactions.get(app.selected_index) {
        draw_transaction_details(f, app, tx, chunks[1]);
    }
}

fn draw_transfers(f: &mut Frame, app: &App, area: Rect) {
    if app.linked_transfers.is_empty() {
        let text = Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                "No linked transfers yet.",
                Style::default().fg(Color::DarkGray),
            )]),
        ]);
        f.render_widget(text, area);
        return;
    }

    let chunks =
        Layout::vertical([Constraint::Min(0), Constraint::Length(DETAILS_HEIGHT)]).split(area);

    let visible_height = chunks[0].height as usize;
    let total = app.linked_transfers.len();
    let offset = calculate_scroll_offset(app.selected_index, total, visible_height);

    let rows: Vec<Row> = app
        .linked_transfers
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_height)
        .map(|(i, twt)| {
            let is_selected = i == app.selected_index;
            let bg = if is_selected {
                Color::DarkGray
            } else {
                Color::Reset
            };

            let from = &twt.from_transaction;
            let to = &twt.to_transaction;

            Row::new(vec![
                Cell::from(from.date.to_string()).style(Style::default().bg(bg)),
                Cell::from(from.description.clone()).style(Style::default().bg(bg)),
                Cell::from(Line::from(format_cents(from.amount_cents)).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Red).bg(bg)),
                Cell::from("→").style(Style::default().fg(Color::Cyan).bg(bg)),
                Cell::from(to.description.clone()).style(Style::default().bg(bg)),
                Cell::from(Line::from(format_cents(to.amount_cents)).alignment(Alignment::Right))
                    .style(Style::default().fg(Color::Green).bg(bg)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Min(20),
            Constraint::Length(12),
            Constraint::Length(3),
            Constraint::Min(20),
            Constraint::Length(12),
        ],
    );

    f.render_widget(table, chunks[0]);

    if let Some(twt) = app.linked_transfers.get(app.selected_index) {
        draw_transfer_details(f, app, twt, chunks[1]);
    }
}

fn draw_todo(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);

    let subtitles: Vec<Line> = TodoSubTab::all()
        .iter()
        .map(|t| {
            let count = match t {
                TodoSubTab::Uncategorized => app.uncategorized.len(),
                TodoSubTab::AiReview => app.ai_reviews.len(),
                TodoSubTab::TransferReview => app.transfer_reviews.len(),
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

    f.render_widget(subtabs, chunks[0]);

    let content_chunks =
        Layout::vertical([Constraint::Min(0), Constraint::Length(DETAILS_HEIGHT)]).split(chunks[1]);

    match app.todo_subtab {
        TodoSubTab::Uncategorized => {
            draw_transaction_table(
                f,
                app,
                &app.uncategorized,
                app.selected_index,
                content_chunks[0],
            );
            if let Some(tx) = app.uncategorized.get(app.selected_index) {
                draw_transaction_details(f, app, tx, content_chunks[1]);
            }
        }
        TodoSubTab::AiReview => {
            draw_ai_review_table(f, app, content_chunks[0]);
            if let Some(review) = app.ai_reviews.get(app.selected_index) {
                draw_ai_review_details(f, app, review, content_chunks[1]);
            }
        }
        TodoSubTab::TransferReview => {
            draw_transfer_review_table(f, app, content_chunks[0]);
            if let Some(transfer) = app.transfer_reviews.get(app.selected_index) {
                draw_pending_transfer_details(f, app, transfer, content_chunks[1]);
            }
        }
    }
}

fn draw_transaction_table(
    f: &mut Frame,
    app: &App,
    transactions: &[crate::Transaction],
    selected: usize,
    area: Rect,
) {
    let visible_height = area.height as usize;
    let total = transactions.len();

    let offset = calculate_scroll_offset(selected, total, visible_height);

    let rows: Vec<Row> = transactions
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_height)
        .map(|(i, tx)| {
            let is_selected = i == selected;
            let is_pending = app.is_pending_transfer_tx(tx.id);
            let is_candidate = app.is_transfer_candidate(tx.id);
            let is_disabled = app.input_mode == InputMode::TransferPending
                && !is_candidate
                && !is_pending;

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

            Row::new(vec![
                Cell::from(tx.date.to_string()).style(Style::default().fg(fg).bg(bg)),
                Cell::from(tx.description.clone()).style(Style::default().fg(fg).bg(bg)),
                Cell::from(Line::from(amount).alignment(Alignment::Right))
                    .style(Style::default().fg(amount_color).bg(bg)),
                Cell::from(Line::from(balance).alignment(Alignment::Right))
                    .style(Style::default().fg(fg).bg(bg)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Min(30),
            Constraint::Length(12),
            Constraint::Length(12),
        ],
    );

    f.render_widget(table, area);
}

fn draw_ai_review_table(f: &mut Frame, app: &App, area: Rect) {
    let visible_height = area.height as usize;
    let total = app.ai_reviews.len();
    let offset = calculate_scroll_offset(app.selected_index, total, visible_height);

    let rows: Vec<Row> = app
        .ai_reviews
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_height)
        .map(|(i, review)| {
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
                .map(|c| format!("{:.0}%", c * 100.0))
                .unwrap_or_default();

            let amount_color = if tx.amount_cents < 0 {
                Color::Red
            } else {
                Color::Green
            };

            Row::new(vec![
                Cell::from(tx.date.to_string()).style(Style::default().bg(bg)),
                Cell::from(tx.description.clone()).style(Style::default().bg(bg)),
                Cell::from(Line::from(format_cents(tx.amount_cents)).alignment(Alignment::Right))
                    .style(Style::default().fg(amount_color).bg(bg)),
                Cell::from(category_path.to_string()).style(Style::default().fg(Color::Yellow).bg(bg)),
                Cell::from(confidence).style(Style::default().fg(Color::Cyan).bg(bg)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Min(20),
            Constraint::Length(12),
            Constraint::Length(25),
            Constraint::Length(6),
        ],
    );

    f.render_widget(table, area);
}

fn draw_transfer_review_table(f: &mut Frame, app: &App, area: Rect) {
    if app.transfer_reviews.is_empty() {
        let text = Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                "No pending transfer reviews.",
                Style::default().fg(Color::DarkGray),
            )]),
        ]);
        f.render_widget(text, area);
        return;
    }

    let visible_height = area.height as usize;
    let total = app.transfer_reviews.len();
    let offset = calculate_scroll_offset(app.selected_index, total, visible_height);

    let rows: Vec<Row> = app
        .transfer_reviews
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_height)
        .map(|(i, transfer)| {
            let is_selected = i == app.selected_index;
            let bg = if is_selected {
                Color::DarkGray
            } else {
                Color::Reset
            };

            let source = transfer.source.as_str();
            let from_id = transfer.from_transaction_id.to_string();
            let to_id = transfer.to_transaction_id.to_string();

            Row::new(vec![
                Cell::from(format!("From: {}", from_id)).style(Style::default().bg(bg)),
                Cell::from(format!("To: {}", to_id)).style(Style::default().bg(bg)),
                Cell::from(source).style(Style::default().fg(Color::Cyan).bg(bg)),
                Cell::from(transfer.created_at.format("%Y-%m-%d").to_string())
                    .style(Style::default().bg(bg)),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(15),
            Constraint::Length(15),
            Constraint::Length(10),
            Constraint::Length(12),
        ],
    );

    f.render_widget(table, area);
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

fn calculate_scroll_offset(selected: usize, total: usize, visible_height: usize) -> usize {
    if total <= visible_height {
        0
    } else if selected < visible_height / 2 {
        0
    } else if selected > total - visible_height / 2 {
        total.saturating_sub(visible_height)
    } else {
        selected.saturating_sub(visible_height / 2)
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let popup_height = r.height * percent_y / 100;
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

fn draw_transaction_details(f: &mut Frame, app: &App, tx: &Transaction, area: Rect) {
    let bank_name = app.bank_name(tx.bank_id);
    let account_name = app.account_name(tx.account_id);

    let amount_style = if tx.amount_cents < 0 {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };

    let metadata_str = if tx.metadata.is_empty() {
        String::new()
    } else {
        serde_json::to_string(&tx.metadata).unwrap_or_default()
    };

    let transfer_info = app
        .store
        .get_transfer_for_transaction(tx.id)
        .ok()
        .flatten()
        .and_then(|transfer| {
            let other_id = if transfer.from_transaction_id == tx.id {
                transfer.to_transaction_id
            } else {
                transfer.from_transaction_id
            };
            app.store.get_transaction_by_id(other_id).ok().flatten()
        });

    let category = app
        .store
        .get_transaction_category(tx.id)
        .ok()
        .flatten()
        .map(|c| c.path)
        .unwrap_or_default();

    let mut lines = vec![
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
    ];

    if let Some(other_tx) = transfer_info {
        let other_bank = app.bank_name(other_tx.bank_id);
        let other_account = app.account_name(other_tx.account_id);
        lines.push(Line::from(vec![
            Span::styled("Transfer: ", Style::default().fg(Color::Cyan)),
            Span::raw(format!(
                "{} / {} — {}",
                other_bank, other_account, other_tx.description
            )),
        ]));
    } else if !category.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Category: ", Style::default().fg(Color::DarkGray)),
            Span::styled(category, Style::default().fg(Color::Yellow)),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("Source: ", Style::default().fg(Color::DarkGray)),
        Span::raw(&tx.source_file),
    ]));

    if !metadata_str.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Metadata: ", Style::default().fg(Color::DarkGray)),
            Span::raw(metadata_str),
        ]));
    }

    let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(paragraph, area);
}

fn draw_transfer_details(f: &mut Frame, app: &App, twt: &TransferWithTransactions, area: Rect) {
    let from = &twt.from_transaction;
    let to = &twt.to_transaction;

    let from_bank = app.bank_name(from.bank_id);
    let from_account = app.account_name(from.account_id);
    let to_bank = app.bank_name(to.bank_id);
    let to_account = app.account_name(to.account_id);

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("From: ", Style::default().fg(Color::Red)),
            Span::raw(format!(
                "{} / {} — {} — {} — {}",
                from_bank,
                from_account,
                from.date,
                format_cents(from.amount_cents),
                &from.description,
            )),
        ]),
        Line::from(vec![
            Span::styled("To:   ", Style::default().fg(Color::Green)),
            Span::raw(format!(
                "{} / {} — {} — {} — {}",
                to_bank,
                to_account,
                to.date,
                format_cents(to.amount_cents),
                &to.description,
            )),
        ]),
        Line::from(vec![
            Span::styled("Created: ", Style::default().fg(Color::DarkGray)),
            Span::raw(twt.transfer.created_at.format("%Y-%m-%d %H:%M").to_string()),
            Span::raw("  "),
            Span::styled("Source: ", Style::default().fg(Color::DarkGray)),
            Span::raw(twt.transfer.source.as_str()),
        ]),
    ];

    let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(paragraph, area);
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
        .map(|c| format!("{:.0}%", c * 100.0))
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

fn draw_pending_transfer_details(
    f: &mut Frame,
    app: &App,
    transfer: &crate::Transfer,
    area: Rect,
) {
    let from_tx = app.store.get_transaction_by_id(transfer.from_transaction_id);
    let to_tx = app.store.get_transaction_by_id(transfer.to_transaction_id);

    let (from_tx, to_tx) = match (from_tx, to_tx) {
        (Ok(Some(f)), Ok(Some(t))) => (f, t),
        _ => {
            let lines = vec![Line::from(vec![
                Span::styled("Error: ", Style::default().fg(Color::Red)),
                Span::raw("Could not load transfer transactions"),
            ])];
            f.render_widget(Paragraph::new(lines), area);
            return;
        }
    };

    let from_bank = app.bank_name(from_tx.bank_id);
    let from_account = app.account_name(from_tx.account_id);
    let to_bank = app.bank_name(to_tx.bank_id);
    let to_account = app.account_name(to_tx.account_id);

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("From: ", Style::default().fg(Color::Red)),
            Span::raw(format!(
                "{} / {} — {} — {} — {}",
                from_bank,
                from_account,
                from_tx.date,
                format_cents(from_tx.amount_cents),
                &from_tx.description,
            )),
        ]),
        Line::from(vec![
            Span::styled("To:   ", Style::default().fg(Color::Green)),
            Span::raw(format!(
                "{} / {} — {} — {} — {}",
                to_bank,
                to_account,
                to_tx.date,
                format_cents(to_tx.amount_cents),
                &to_tx.description,
            )),
        ]),
        Line::from(vec![
            Span::styled("Source: ", Style::default().fg(Color::DarkGray)),
            Span::raw(transfer.source.as_str()),
            Span::raw("  "),
            Span::styled("Status: ", Style::default().fg(Color::Yellow)),
            Span::raw("Pending review"),
        ]),
    ];

    let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(paragraph, area);
}
