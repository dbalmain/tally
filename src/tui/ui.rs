use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Clear, Paragraph, Row, Tabs},
};

use crate::{FilterOverride, Transaction, TransactionWithEnrichment, TransferWithTransactions};

use super::app::{App, InputMode, Tab, TodoSubTab};
use super::keymap::{self, HelpLine};
use super::modal::{Modal, hint_line};
use super::search_bar::SearchBar;
use super::table::{ScrollTable, aligned_table, calculate_scroll_offset};

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

const FILTER_COLS: [Constraint; 5] = [
    Constraint::Min(16),
    Constraint::Min(24),
    Constraint::Min(18),
    Constraint::Length(5),
    Constraint::Length(6),
];

const FILTER_EDIT_PREVIEW_COLS: [Constraint; 5] = [
    Constraint::Length(12),
    Constraint::Min(24),
    Constraint::Min(18),
    Constraint::Length(12),
    Constraint::Length(12),
];

const APPLY_FILTERS_COLS: [Constraint; 4] = [
    Constraint::Length(12),
    Constraint::Min(24),
    Constraint::Min(18),
    Constraint::Length(12),
];

pub fn draw(f: &mut Frame, app: &App) {
    if app.filter_edit_visible() {
        draw_filter_edit_takeover(f, app);
        return;
    }

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
    let modal_open = overlay_open(app);
    let show_hints = app.hints_visible && !modal_open;
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
        Tab::Filters => draw_filters(f, app, content),
    }

    if show_hints {
        draw_key_hints(f, app, chunks[idx + 1]);
    }

    draw_overlays(f, app, db_search_area, None);
}

fn draw_filter_edit_takeover(f: &mut Frame, app: &App) {
    let show_hints = app.hints_visible && !overlay_open(app);
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ];
    if show_hints {
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::vertical(constraints).split(f.area());

    draw_filter_edit_heading(f, app, chunks[0]);
    draw_filter_edit_search_bar(f, app, chunks[1]);
    draw_filter_edit_preview(f, app, chunks[2]);

    if show_hints {
        draw_key_hints(f, app, chunks[3]);
    }

    draw_overlays(f, app, None, Some(chunks[1]));
}

fn overlay_open(app: &App) -> bool {
    matches!(
        app.input_mode,
        InputMode::Category
            | InputMode::TextPrompt
            | InputMode::BulkApply
            | InputMode::Confirm
            | InputMode::ConfirmApplyFilters
            | InputMode::ConfirmMerge
            | InputMode::TransferNoMatch
    ) || app.error_message.is_some()
        || app.keybind_help_open
}

fn draw_overlays(
    f: &mut Frame,
    app: &App,
    db_search_area: Option<Rect>,
    filter_edit_search_area: Option<Rect>,
) {
    if app.input_mode == InputMode::Category {
        draw_category_popup(f, app);
    }

    if app.input_mode == InputMode::TextPrompt {
        draw_text_prompt_popup(f, app);
    }

    if app.input_mode == InputMode::BulkApply {
        draw_bulk_apply_popup(f, app);
    }

    if matches!(app.input_mode, InputMode::ConfirmMerge | InputMode::Confirm) {
        draw_confirm_popup(f, app);
    }

    if app.input_mode == InputMode::ConfirmApplyFilters {
        draw_apply_filters_popup(f, app);
    }

    if app.input_mode == InputMode::TransferNoMatch {
        draw_no_match_popup(f, app);
    }

    if let Some(search_area) = db_search_area
        && app.filter_autocomplete_active()
        && app.input_mode == InputMode::DbSearch
        && let Some(search_state) = app.current_search_state()
    {
        draw_search_autocomplete_popup(f, &search_state.search_bar, search_area);
    }

    if let Some(search_area) = filter_edit_search_area
        && app.filter_edit_autocomplete_active()
        && app.input_mode == InputMode::FilterEdit
        && let Some(search_bar) = app.filter_edit_search_bar()
    {
        draw_search_autocomplete_popup(f, search_bar, search_area);
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

fn draw_filter_edit_heading(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        app.filter_edit_name().to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(path) = app.filter_edit_category_path() {
        spans.push(Span::styled(
            "  category ",
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(
            path.to_string(),
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(label) = app.filter_edit_override_label() {
        spans.push(Span::styled(
            "  override ",
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(label, Style::default().fg(Color::Cyan)));
    }
    if let Some(review_required) = app.filter_edit_review_required() {
        spans.push(Span::styled(
            "  require review ",
            Style::default().fg(Color::DarkGray),
        ));
        let (text, color) = if review_required {
            ("review", Color::Yellow)
        } else {
            ("commit", Color::DarkGray)
        };
        spans.push(Span::styled(text, Style::default().fg(color)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_filter_edit_search_bar(f: &mut Frame, app: &App, area: Rect) {
    let is_active = app.input_mode == InputMode::FilterEdit;
    if let Some(search_bar) = app.filter_edit_search_bar() {
        let (cursor_x, cursor_y) = search_bar.render(f, area, "/", is_active);
        if is_active {
            f.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn draw_filter_edit_preview(f: &mut Frame, app: &App, area: Rect) {
    let preview = app.filter_edit_preview();
    if preview.is_empty() {
        draw_empty_message(f, "No matching transactions.", area);
        return;
    }

    let selected = app.filter_edit_preview_scroll();
    ScrollTable::new(preview, selected, &FILTER_EDIT_PREVIEW_COLS).render(f, area, |i, tx| {
        let bg = if i == selected {
            Color::DarkGray
        } else {
            Color::Reset
        };
        let amount_color = if tx.amount_cents < 0 {
            Color::Red
        } else {
            Color::Green
        };
        let account = format!(
            "{} / {}",
            app.bank_name(tx.bank_id),
            app.account_name(tx.account_id)
        );

        Row::new(vec![
            Cell::from(tx.date.to_string()),
            Cell::from(tx.description.as_str()),
            Cell::from(account).style(Style::default().fg(Color::DarkGray)),
            Cell::from(Line::from(format_cents(tx.amount_cents)).alignment(Alignment::Right))
                .style(Style::default().fg(amount_color)),
            Cell::from(Line::from(format_cents(tx.balance_cents)).alignment(Alignment::Right)),
        ])
        .style(Style::default().bg(bg))
    });
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
    f.render_widget(Paragraph::new(hint_line(&keymap::footer_hints(app))), area);
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

/// Transaction-table columns, in no particular order. The display order and
/// which columns are visible at a given width are decided by
/// [`plan_transaction_columns`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxColumn {
    Date,
    Description,
    Account,
    Category,
    Amount,
    Balance,
}

/// Decide which transaction columns fit into `width` and how wide each is.
///
/// Columns are included greedily in priority order (Date, Description, Amount,
/// Category, Account, Balance), each at a minimum width; the first that does
/// not fit hides itself and every lower-priority column. Leftover width then
/// grows Category and Account up to a comfortable cap, and Description takes
/// whatever remains. The result is returned in display order.
fn plan_transaction_columns(width: u16) -> Vec<(TxColumn, u16)> {
    use TxColumn::*;

    // (column, minimum width) in priority order, highest priority first.
    let priority = [
        (Date, 10u16),
        (Description, 20),
        (Amount, 12),
        (Category, 10),
        (Account, 10),
        (Balance, 12),
    ];

    let mut included: Vec<(TxColumn, u16)> = Vec::new();
    let mut used = 0u16;
    for (col, min) in priority {
        let need = if included.is_empty() {
            min
        } else {
            min + COLUMN_SPACING
        };
        if used + need <= width {
            used += need;
            included.push((col, min));
        } else {
            break;
        }
    }

    // Grow the truncatable columns up to a cap, then give the rest to the
    // description (the elastic, highest-priority text column).
    let mut leftover = width.saturating_sub(used);
    for (col, cap) in [(Category, 24u16), (Account, 20u16)] {
        if let Some(entry) = included.iter_mut().find(|(c, _)| *c == col) {
            let grow = cap.saturating_sub(entry.1).min(leftover);
            entry.1 += grow;
            leftover -= grow;
        }
    }
    if let Some(entry) = included.iter_mut().find(|(c, _)| *c == Description) {
        entry.1 += leftover;
    }

    let display = [Date, Description, Account, Category, Amount, Balance];
    display
        .into_iter()
        .filter_map(|col| included.iter().find(|(c, _)| *c == col).copied())
        .collect()
}

const COLUMN_SPACING: u16 = 1;

fn draw_transaction_table(
    f: &mut Frame,
    app: &App,
    transactions: &[&Transaction],
    selected: usize,
    area: Rect,
) {
    let cols = plan_transaction_columns(area.width);
    let constraints: Vec<Constraint> = cols.iter().map(|&(_, w)| Constraint::Length(w)).collect();

    // When "view details" (`v`) is on, prebuild the wrapped two-column detail
    // for the selected row. Building it here (where the width is known) lets us
    // reserve exactly as many rows as it occupies, since we wrap by hand.
    let detail_lines = if app.view_details {
        transactions
            .get(selected)
            .copied()
            .map(|tx| build_detail_lines(app, tx, area.width))
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut table = ScrollTable::new(transactions, selected, &constraints);
    if !detail_lines.is_empty() {
        table = table.detail(detail_lines.len() as u16, |f, _tx, area| {
            f.render_widget(Paragraph::new(detail_lines.clone()), area);
        });
    }

    table.render(f, area, |i, tx| {
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

        let cells = cols
            .iter()
            .map(|&(col, w)| {
                let w = w as usize;
                match col {
                    TxColumn::Date => {
                        Cell::from(tx.date.to_string()).style(Style::default().fg(fg))
                    }
                    TxColumn::Description => {
                        Cell::from(tx.description.clone()).style(Style::default().fg(fg))
                    }
                    TxColumn::Account => {
                        let account = format!(
                            "{}/{}",
                            app.bank_name(tx.bank_id),
                            app.account_name(tx.account_id)
                        );
                        // The account is contextual, so dim it whether or not
                        // the row is disabled.
                        Cell::from(fit_path(&account, w))
                            .style(Style::default().fg(Color::DarkGray))
                    }
                    TxColumn::Category => match category_cell_text(app, tx, w) {
                        Some((text, color)) => {
                            let color = if is_disabled { Color::DarkGray } else { color };
                            Cell::from(text).style(Style::default().fg(color))
                        }
                        None => Cell::from(""),
                    },
                    TxColumn::Amount => Cell::from(
                        Line::from(format_cents(tx.amount_cents)).alignment(Alignment::Right),
                    )
                    .style(Style::default().fg(amount_color)),
                    TxColumn::Balance => Cell::from(
                        Line::from(format_cents(tx.balance_cents)).alignment(Alignment::Right),
                    )
                    .style(Style::default().fg(fg)),
                }
            })
            .collect::<Vec<_>>();

        Row::new(cells).style(Style::default().bg(bg))
    });
}

/// Text + colour for the category column: the category path (yellow, like the
/// Categories tab) or, for a transfer, `to:`/`from:` the counterpart
/// `bank/account` (cyan). `None` leaves the cell blank. The value is truncated
/// to `width` via [`fit_path`]; for transfers the `to:`/`from:` label is kept
/// intact and only the account portion is abbreviated.
fn category_cell_text(app: &App, tx: &Transaction, width: usize) -> Option<(String, Color)> {
    if let Some(transfer) = app.get_cached_transfer(tx.id) {
        let (label, other_id) = if transfer.from_transaction_id == tx.id {
            ("to", transfer.to_transaction_id)
        } else {
            ("from", transfer.from_transaction_id)
        };
        let account = app
            .get_cached_transaction(other_id)
            .map(|other| {
                format!(
                    "{}/{}",
                    app.bank_name(other.bank_id),
                    app.account_name(other.account_id)
                )
            })
            .unwrap_or_default();
        let prefix = label.len() + 1; // "to:" / "from:"
        let account = fit_path(&account, width.saturating_sub(prefix));
        Some((format!("{label}:{account}"), Color::Cyan))
    } else {
        app.get_cached_category(tx.id)
            .filter(|c| !c.is_empty())
            .map(|category| (fit_path(category, width), Color::Yellow))
    }
}

/// Abbreviate each `/`-separated segment longer than four characters to its
/// first three characters plus an ellipsis (e.g. `Personal-Rentals` → `Per…`),
/// leaving shorter segments untouched.
fn abbreviate_path(value: &str) -> String {
    value
        .split('/')
        .map(|seg| {
            if seg.chars().count() > 4 {
                let head: String = seg.chars().take(3).collect();
                format!("{head}…")
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Hard-truncate `value` to `width` columns, adding a trailing ellipsis when
/// the text is clipped.
fn truncate_width(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    match width {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let head: String = value.chars().take(width - 1).collect();
            format!("{head}…")
        }
    }
}

/// Fit a `/`-separated value (category path or `bank/account`) into `width`:
/// first try it as-is, then per-segment abbreviation, then a hard ellipsis
/// truncation.
fn fit_path(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    truncate_width(&abbreviate_path(value), width)
}

/// Every detail (name, value) pair for a transaction, in display order: the
/// human-friendly fields first, then any metadata entries (sorted by key). This
/// is the data shown by the "view details" (`v`) panel.
fn transaction_detail_pairs(app: &App, tx: &Transaction) -> Vec<(String, String)> {
    let mut pairs = vec![
        ("ID".to_string(), tx.id.to_string()),
        ("Date".to_string(), tx.date.to_string()),
        (
            "Account".to_string(),
            format!(
                "{}/{}",
                app.bank_name(tx.bank_id),
                app.account_name(tx.account_id)
            ),
        ),
        ("Description".to_string(), tx.description.clone()),
        ("Amount".to_string(), format_cents(tx.amount_cents)),
        ("Balance".to_string(), format_cents(tx.balance_cents)),
    ];

    // The transaction is either part of a transfer or categorised, never both.
    if let Some(transfer) = app.get_cached_transfer(tx.id) {
        let (label, other_id) = if transfer.from_transaction_id == tx.id {
            ("to", transfer.to_transaction_id)
        } else {
            ("from", transfer.from_transaction_id)
        };
        if let Some(other) = app.get_cached_transaction(other_id) {
            pairs.push((
                "Transfer".to_string(),
                format!(
                    "{label} {}/{}",
                    app.bank_name(other.bank_id),
                    app.account_name(other.account_id)
                ),
            ));
        }
    } else if let Some(category) = app.get_cached_category(tx.id).filter(|c| !c.is_empty()) {
        pairs.push(("Category".to_string(), category.to_string()));
    }

    pairs.push(("Source".to_string(), tx.source_file.clone()));
    pairs.push(("Hash".to_string(), tx.hash.clone()));
    pairs.push(("Import batch".to_string(), tx.import_batch_id.to_string()));

    let mut keys: Vec<&String> = tx.metadata.keys().collect();
    keys.sort();
    for key in keys {
        let value = match &tx.metadata[key] {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        pairs.push((key.clone(), value));
    }

    pairs
}

/// Build the "view details" panel as a two-column layout: dimmed names on the
/// left, values on the right wrapped (never truncated) to fill the remaining
/// width. Returns one [`Line`] per rendered row so the caller can reserve the
/// exact height.
fn build_detail_lines(app: &App, tx: &Transaction, width: u16) -> Vec<Line<'static>> {
    let pairs = transaction_detail_pairs(app, tx);
    let width = width as usize;

    // Size the name column to the longest label, but cap it so a stray long
    // metadata key can't starve the value column.
    let longest = pairs
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let label_width = longest.min(24).min(width.saturating_sub(8)).max(3);
    const GAP: usize = 1;
    let value_width = width.saturating_sub(label_width + GAP).max(1);

    let mut lines = Vec::new();
    for (label, value) in pairs {
        for (i, chunk) in wrap_text(&value, value_width).into_iter().enumerate() {
            let label_cell = if i == 0 {
                let label = truncate_width(&label, label_width);
                format!("{label:<label_width$}")
            } else {
                " ".repeat(label_width)
            };
            lines.push(Line::from(vec![
                Span::styled(label_cell, Style::default().fg(Color::DarkGray)),
                Span::raw(" ".repeat(GAP)),
                Span::raw(chunk),
            ]));
        }
    }
    lines
}

/// Word-wrap `text` to `width` columns, hard-breaking any single token longer
/// than the width. Always returns at least one (possibly empty) line.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        // Break a token that can't fit on one line on its own.
        while word.chars().count() > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let head: String = word.chars().take(width).collect();
            lines.push(head);
            word = word.chars().skip(width).collect();
        }
        if current.is_empty() {
            current = word;
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(&word);
        } else {
            lines.push(std::mem::take(&mut current));
            current = word;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
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

fn draw_filters(f: &mut Frame, app: &App, area: Rect) {
    let filters: Vec<_> = app.lists.filters.iter().collect();

    if filters.is_empty() {
        draw_empty_message(f, "No filters yet.", area);
        return;
    }

    ScrollTable::new(&filters, app.selected_index, &FILTER_COLS).render(f, area, |i, filter| {
        let bg = if i == app.selected_index {
            Color::DarkGray
        } else {
            Color::Reset
        };

        let category = filter
            .category_id
            .and_then(|id| app.category_path(id))
            .map(|path| Cell::from(path.to_string()).style(Style::default().fg(Color::Yellow)))
            .unwrap_or_else(dim_dash_cell);

        let override_mode = if filter.category_id.is_some() {
            let label = match filter.override_mode {
                FilterOverride::Uncategorised => "new",
                FilterOverride::Ai => "+ai",
                FilterOverride::All => "all",
            };
            Cell::from(label).style(Style::default().fg(Color::Cyan))
        } else {
            dim_dash_cell()
        };

        let review = if filter.category_id.is_some() {
            if filter.review_required {
                Cell::from("review").style(Style::default().fg(Color::Yellow))
            } else {
                Cell::from("auto").style(Style::default().fg(Color::DarkGray))
            }
        } else {
            dim_dash_cell()
        };

        Row::new(vec![
            Cell::from(filter.name.as_str()),
            Cell::from(filter.query.as_str()).style(Style::default().fg(Color::DarkGray)),
            category,
            override_mode,
            review,
        ])
        .style(Style::default().bg(bg))
    });
}

fn dim_dash_cell() -> Cell<'static> {
    Cell::from("—").style(Style::default().fg(Color::DarkGray))
}

fn draw_keybind_popup(f: &mut Frame, app: &App) {
    let help = keymap::help_lines(app);
    let screen = f.area();
    let width = 64.min(screen.width.saturating_sub(2)).max(20);
    let desired_height = (help.len() as u16).saturating_add(4).max(8);
    let height = desired_height.min(screen.height.saturating_sub(2).max(1));
    let area = centered_rect_size(width, height, screen);
    let hints = [("Esc/Enter", "close"), ("Alt-?", "toggle bar")];
    let body = Modal {
        title: "Keybinds",
        hints: &hints,
        border: Color::Cyan,
    }
    .draw(f, area);

    let lines = help.into_iter().map(help_line).collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(Color::White))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        body,
    );
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

fn draw_text_prompt_popup(f: &mut Frame, app: &App) {
    let area = centered_rect_fixed_height(50, 6, f.area());
    let hints = keymap::footer_hints(app);
    let body = Modal {
        title: app.text_prompt_title(),
        hints: &hints,
        border: Color::Cyan,
    }
    .draw(f, area);

    let input_width = body.width as usize;
    let scroll = app.text_prompt_scroll(input_width);
    let cursor_pos = app.text_prompt_cursor();

    // Display scrolled portion of input
    let input_value = app.text_prompt_value();
    let visible: String = input_value.chars().skip(scroll).take(input_width).collect();
    let input = Paragraph::new(visible).style(Style::default().fg(Color::Yellow));
    f.render_widget(input, body);

    // Position cursor relative to scroll
    let cursor_x = body.x + (cursor_pos - scroll) as u16;
    f.set_cursor_position((cursor_x, body.y));
}

fn draw_confirm_popup(f: &mut Frame, app: &App) {
    let area = centered_rect_fixed_height(60, 8, f.area());
    let hints = keymap::footer_hints(app);
    let body = Modal {
        title: "Confirm",
        hints: &hints,
        border: Color::Cyan,
    }
    .draw(f, area);
    let message = app.confirm_message.as_deref().unwrap_or("Confirm action?");

    let msg_line = Paragraph::new(message)
        .style(Style::default().fg(Color::Yellow))
        .wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(msg_line, body);
}

fn draw_category_popup(f: &mut Frame, app: &App) {
    let area = centered_rect(50, 60, f.area());
    let hints = keymap::footer_hints(app);
    let body = Modal {
        title: "Category",
        hints: &hints,
        border: Color::Cyan,
    }
    .draw(f, area);

    let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(body);

    let input_style = Style::default().fg(Color::Yellow);
    let input = Paragraph::new(app.category_input.as_str()).style(input_style);
    f.render_widget(input, chunks[0]);

    let suggestions: Vec<Line> = app
        .category_suggestions
        .iter()
        .enumerate()
        .take(chunks[1].height as usize)
        .map(|(i, cat)| {
            let selected = i == app.category_selected;
            let bg = if selected {
                Color::DarkGray
            } else {
                Color::Reset
            };
            if cat.id == 0 {
                // The synthetic "what you typed" entry: a new category.
                Line::from(vec![
                    Span::styled(&cat.path, Style::default().fg(Color::Green).bg(bg)),
                    Span::styled(" (new)", Style::default().fg(Color::DarkGray).bg(bg)),
                ])
            } else {
                let fg = if selected { Color::White } else { Color::Gray };
                Line::styled(&cat.path, Style::default().fg(fg).bg(bg))
            }
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

    let selected = state.rows.iter().filter(|row| row.selected).count();
    let title = format!(
        "Apply \"{}\" to similar ({} selected)",
        state.category_path, selected
    );
    let hints = keymap::footer_hints(app);
    let body = Modal {
        title: &title,
        hints: &hints,
        border: Color::Cyan,
    }
    .draw(f, area);

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
    .render(f, body, |i, row| {
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

fn draw_apply_filters_popup(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 70, f.area());
    let rows = app.apply_filters_preview_rows();
    let title = format!("Apply filters ({} to update)", rows.len());
    let hints = keymap::footer_hints(app);
    let body = Modal {
        title: &title,
        hints: &hints,
        border: Color::Cyan,
    }
    .draw(f, area);

    if rows.is_empty() {
        draw_empty_message(f, "No transactions to update.", body);
        return;
    }

    let selected = app.apply_filters_preview_scroll();
    ScrollTable::new(rows, selected, &APPLY_FILTERS_COLS).render(f, body, |i, tx| {
        let bg = if i == selected {
            Color::DarkGray
        } else {
            Color::Reset
        };
        let amount_color = if tx.amount_cents < 0 {
            Color::Red
        } else {
            Color::Green
        };
        let account = format!(
            "{} / {}",
            app.bank_name(tx.bank_id),
            app.account_name(tx.account_id)
        );

        Row::new(vec![
            Cell::from(tx.date.to_string()),
            Cell::from(tx.description.as_str()),
            Cell::from(account).style(Style::default().fg(Color::DarkGray)),
            Cell::from(Line::from(format_cents(tx.amount_cents)).alignment(Alignment::Right))
                .style(Style::default().fg(amount_color)),
        ])
        .style(Style::default().bg(bg))
    });
}

/// Render a search-filter autocomplete popup anchored below a search bar.
fn draw_search_autocomplete_popup(f: &mut Frame, search_bar: &SearchBar, search_area: Rect) {
    let Some(ac_state) = search_bar.autocomplete() else {
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
    let hints = keymap::footer_hints(app);
    let body = Modal {
        title: "No match",
        hints: &hints,
        border: Color::Cyan,
    }
    .draw(f, area);

    let tx = app.pending_transfer_tx.as_ref();
    let msg = if let Some(tx) = tx {
        format!(
            "No matching transaction found for\n{} ({})",
            format_cents(tx.amount_cents),
            tx.description
        )
    } else {
        "No matching transaction found.".to_string()
    };

    let paragraph = Paragraph::new(msg).style(Style::default().fg(Color::White));
    f.render_widget(paragraph, body);
}

fn draw_error_popup(f: &mut Frame, msg: &str) {
    let area = centered_rect(40, 15, f.area());
    let hints = [("Esc", "dismiss")];
    let body = Modal {
        title: "Error",
        hints: &hints,
        border: Color::Red,
    }
    .draw(f, area);

    let paragraph = Paragraph::new(msg).style(Style::default().fg(Color::White));
    f.render_widget(paragraph, body);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviate_path_shortens_long_segments_only() {
        // Segments longer than four chars collapse to three chars + ellipsis;
        // "Bond" (exactly four) is left alone.
        assert_eq!(
            abbreviate_path("AllTax/Personal-Rentals/Campbell-Unit/Campbell/Bond"),
            "All…/Per…/Cam…/Cam…/Bond"
        );
    }

    #[test]
    fn fit_path_tries_full_then_abbreviated_then_hard_truncates() {
        // Fits as-is.
        assert_eq!(fit_path("Food/Groceries", 20), "Food/Groceries");
        // Needs abbreviation to fit.
        assert_eq!(fit_path("Food/Groceries", 10), "Food/Gro…");
        // Abbreviation still too long, so hard-truncate with an ellipsis.
        assert_eq!(
            fit_path("AllTax/Personal-Rentals/Campbell-Unit/Campbell/Bond", 8),
            "All…/Pe…"
        );
    }

    #[test]
    fn wrap_text_word_wraps_and_hard_breaks_long_tokens() {
        // Word wrapping at a boundary.
        assert_eq!(
            wrap_text("the quick brown fox", 10),
            vec!["the quick", "brown fox"]
        );
        // A single token longer than the width is hard-broken.
        assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        // Empty input still yields one (empty) line so the label row renders.
        assert_eq!(wrap_text("", 10), vec![""]);
    }

    #[test]
    fn plan_columns_hides_lowest_priority_first() {
        // Wide enough for everything: all six, in display order.
        let cols: Vec<_> = plan_transaction_columns(120)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        assert_eq!(
            cols,
            vec![
                TxColumn::Date,
                TxColumn::Description,
                TxColumn::Account,
                TxColumn::Category,
                TxColumn::Amount,
                TxColumn::Balance,
            ]
        );

        // Balance is the lowest priority, so it is the first to be dropped.
        let cols: Vec<_> = plan_transaction_columns(70)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        assert!(!cols.contains(&TxColumn::Balance));
        assert!(cols.contains(&TxColumn::Account));

        // Very narrow: only Date and Description survive (Description is second
        // priority), and Amount/Category/Account/Balance are all gone.
        let cols: Vec<_> = plan_transaction_columns(31)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        assert_eq!(cols, vec![TxColumn::Date, TxColumn::Description]);
    }

    #[test]
    fn plan_columns_widths_fit_within_area() {
        for width in [10u16, 31, 44, 55, 66, 79, 100, 200] {
            let cols = plan_transaction_columns(width);
            if cols.is_empty() {
                continue;
            }
            let spacing = COLUMN_SPACING * (cols.len() as u16 - 1);
            let total: u16 = cols.iter().map(|&(_, w)| w).sum::<u16>() + spacing;
            assert!(
                total <= width,
                "columns total {total} exceed width {width}: {cols:?}"
            );
        }
    }
}
