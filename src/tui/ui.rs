use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Clear, Paragraph, Row, Tabs},
};

use crate::{
    FilterOverride, Transaction, TransactionWithEnrichment, Transfer, TransferWithTransactions,
};

use super::app::{App, InputMode, Tab, TodoSubTab};
use super::keymap::{self, HelpLine};
use super::modal::{MODAL_CHROME_HEIGHT, Modal, hint_line};
use super::search_bar::SearchBar;
use super::table::{COLUMN_SPACING, ScrollTable, aligned_table, calculate_scroll_offset};

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

const APPLY_FILTERS_COLS: [Constraint; 4] = [
    Constraint::Length(12),
    Constraint::Min(24),
    Constraint::Min(18),
    Constraint::Length(12),
];

fn amount_color(cents: i64) -> Color {
    if cents < 0 { Color::Red } else { Color::Green }
}

fn row_bg(selected: bool) -> Color {
    if selected {
        Color::DarkGray
    } else {
        Color::Reset
    }
}

/// Base style for a table row: the selection background plus bold, so the
/// highlighted row's text pops against the background instead of washing out.
fn row_style(selected: bool) -> Style {
    let style = Style::default().bg(row_bg(selected));
    if selected {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

/// Colour for dimmed contextual text (accounts, queries, counts): DarkGray
/// normally, lifted to Gray on the selected row, where DarkGray would vanish
/// against the DarkGray selection background.
fn dim_fg(selected: bool) -> Color {
    if selected {
        Color::Gray
    } else {
        Color::DarkGray
    }
}

fn bank_account(app: &App, bank_id: i64, account_id: i64, separator: &str) -> String {
    format!(
        "{}{}{}",
        app.bank_name(bank_id),
        separator,
        app.account_name(account_id)
    )
}

fn account_label(app: &App, tx: &Transaction) -> String {
    bank_account(app, tx.bank_id, tx.account_id, " / ")
}

fn compact_account_label(app: &App, tx: &Transaction) -> String {
    bank_account(app, tx.bank_id, tx.account_id, "/")
}

fn transfer_counterpart_id(transfer: &Transfer, tx_id: i64) -> (&'static str, i64) {
    if transfer.from_transaction_id == tx_id {
        ("to", transfer.to_transaction_id)
    } else {
        ("from", transfer.from_transaction_id)
    }
}

fn transfer_counterpart(app: &App, tx: &Transaction) -> Option<(&'static str, String)> {
    let transfer = app.get_cached_transfer(tx.id)?;
    let (label, other_id) = transfer_counterpart_id(transfer, tx.id);
    let account = app
        .get_cached_transaction(other_id)
        .map(|other| compact_account_label(app, other))
        .unwrap_or_default();
    Some((label, account))
}

pub fn draw(f: &mut Frame, app: &App) {
    if app.filter_edit_visible() {
        draw_filter_edit_takeover(f, app);
        return;
    }

    let has_db_search = app.db_search_active() || app.input_mode == InputMode::DbSearch;
    let has_fuzzy_search = app.fuzzy_search_active() || app.input_mode == InputMode::FuzzySearch;

    // The sum row piggybacks on the last active search bar (whichever sits
    // closest to the content) when one is open, so it doesn't cost an extra
    // line; otherwise it gets its own row.
    let show_sum = app.current_tab == Tab::Transactions && app.show_sum;
    let sum_inline = show_sum && (has_db_search || has_fuzzy_search);
    let sum_own_row = show_sum && !sum_inline;

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
    if sum_own_row {
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
        if sum_inline && !has_fuzzy_search {
            draw_sum_overlay(f, app, chunks[idx]);
        }
        idx += 1;
    }
    if has_fuzzy_search {
        draw_fuzzy_search_bar(f, app, chunks[idx]);
        if sum_inline {
            draw_sum_overlay(f, app, chunks[idx]);
        }
        idx += 1;
    }
    if sum_own_row {
        draw_sum_row(f, app, chunks[idx]);
        idx += 1;
    }
    let content = chunks[idx];

    match app.current_tab {
        Tab::Transactions => draw_transactions(f, app, content),
        Tab::Transfers => draw_transfers(f, app, content),
        Tab::Categories => draw_categories(f, app, content),
        Tab::Accounts => draw_accounts(f, app, content),
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

    if app.input_mode == InputMode::Confirm {
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

/// Sum of the amounts of the currently visible (search/fuzzy-filtered)
/// Transactions-tab rows.
fn visible_transactions_sum(app: &App) -> i64 {
    app.lists
        .transactions
        .iter()
        .map(|tx| tx.amount_cents)
        .sum()
}

fn sum_span(sum_cents: i64) -> Span<'static> {
    Span::styled(
        format!("Σ {}", format_cents(sum_cents)),
        Style::default().fg(amount_color(sum_cents)),
    )
}

/// Right-aligned sum overlay on a search bar row, drawn after the bar itself
/// so it never covers the search text (it's skipped if there isn't room).
fn draw_sum_overlay(f: &mut Frame, app: &App, area: Rect) {
    let span = sum_span(visible_transactions_sum(app));
    let w = span.content.chars().count() as u16;
    if area.width <= w {
        return;
    }
    let sum_area = Rect {
        x: area.x + area.width - w,
        width: w,
        ..area
    };
    f.render_widget(Paragraph::new(Line::from(span)), sum_area);
}

/// Dedicated sum row, used when no search bar is open to piggyback on. Kept
/// right-aligned to match the inline overlay on a search bar row.
fn draw_sum_row(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(
        Paragraph::new(Line::from(sum_span(visible_transactions_sum(app))))
            .alignment(Alignment::Right),
        area,
    );
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

    // Render the read-only preview through the shared transaction table so it
    // matches the Transactions tab: category column (showing what the filter
    // would override), `/`-joined account, and standard path truncation.
    let rows: Vec<&Transaction> = preview.iter().collect();
    let selected = app.filter_edit_preview_scroll();
    draw_transaction_table(f, app, &rows, selected, area, true, true);
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

    // Transient status, right-aligned in the tab row: the latest result
    // message (green) and any background-work indicators (DarkGray). Skipped
    // entirely when it wouldn't fit.
    let mut parts: Vec<(String, Color)> = Vec::new();
    if let Some(message) = app.active_status() {
        parts.push((message.to_string(), Color::Green));
    }
    if app.refreshing {
        parts.push(("Refreshing...".to_string(), Color::DarkGray));
    }
    if app.classifying {
        parts.push(("Classifying...".to_string(), Color::DarkGray));
    }
    if parts.is_empty() {
        return;
    }

    let width = (parts
        .iter()
        .map(|(text, _)| text.chars().count())
        .sum::<usize>()
        + 2 * (parts.len() - 1)) as u16;
    if area.width <= width {
        return;
    }

    let mut spans = Vec::new();
    for (i, (text, color)) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(text, Style::default().fg(color)));
    }
    let status_area = Rect::new(area.right().saturating_sub(width), area.y, width, 1);
    f.render_widget(Paragraph::new(Line::from(spans)), status_area);
}

fn draw_key_hints(f: &mut Frame, app: &App, area: Rect) {
    f.render_widget(Paragraph::new(hint_line(&keymap::footer_hints(app))), area);
}

fn draw_transactions(f: &mut Frame, app: &App, area: Rect) {
    let transactions: Vec<_> = app.lists.transactions.iter().collect();
    draw_transaction_table(f, app, &transactions, app.selected_index, area, true, true);
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
            .style(row_style(is_selected))
        });
}

fn draw_todo(f: &mut Frame, app: &App, area: Rect) {
    match app.todo_subtab {
        TodoSubTab::Uncategorised => {
            let uncategorised: Vec<_> = app.lists.uncategorised.iter().collect();
            draw_transaction_table(f, app, &uncategorised, app.selected_index, area, true, true);
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

/// Widest visible value, in columns, for each content-sized column.
#[derive(Debug, Clone, Copy, Default)]
struct ColumnNeeds {
    description: u16,
    category: u16,
    account: u16,
}

/// Decide which transaction columns fit into `width` and how wide each is.
///
/// With room to spare, every content-sized column (`needs` carries the widest
/// visible value of each) shows in full and Description absorbs the rest. As
/// width runs short, space is reclaimed in this order: Account and Category
/// shrink to their floors (20 and 24), then Description shrinks to its floor
/// (50), then whole columns drop — Balance, then Account, then Category — and
/// only once those are gone may Description shrink further (to 20) before
/// Amount and Description themselves drop. Spare width flows back in the
/// opposite order: Description grows to its content first, then Category and
/// Account, and any remainder pads Description. The result is in display
/// order. When `include_category` is false the Category column is dropped
/// entirely (the side panel on the Categories tab already knows the category).
fn plan_transaction_columns(
    width: u16,
    include_category: bool,
    needs: ColumnNeeds,
) -> Vec<(TxColumn, u16)> {
    use TxColumn::*;

    const DATE_WIDTH: u16 = 10;
    const AMOUNT_WIDTH: u16 = 12;
    const BALANCE_WIDTH: u16 = 12;
    // Truncation floors: how narrow a column may get before the layout starts
    // dropping columns instead.
    const ACCOUNT_FLOOR: u16 = 20;
    const CATEGORY_FLOOR: u16 = 24;
    const DESCRIPTION_FLOOR: u16 = 50;
    // Hard minimum once every column droppable ahead of Description is gone.
    const DESCRIPTION_MIN: u16 = 20;

    // Candidate column sets in display order, widest first; each next set
    // drops one more column (Balance, then Account, then Category, then
    // Amount), with Date alone as the last resort.
    let mut candidates: Vec<Vec<TxColumn>> = Vec::new();
    let mut current: Vec<TxColumn> = [Date, Description, Account, Category, Amount, Balance]
        .into_iter()
        .filter(|col| include_category || *col != Category)
        .collect();
    candidates.push(current.clone());
    for dropped in [Balance, Account, Category, Amount] {
        if let Some(pos) = current.iter().position(|col| *col == dropped) {
            current.remove(pos);
            candidates.push(current.clone());
        }
    }
    candidates.push(vec![Date]);

    for cols in candidates {
        // Description holds its floor while a droppable column remains; those
        // drop before it shrinks further.
        let description_lo = if cols
            .iter()
            .any(|c| matches!(c, Account | Category | Balance))
        {
            needs.description.clamp(DESCRIPTION_MIN, DESCRIPTION_FLOOR)
        } else {
            DESCRIPTION_MIN
        };
        let lo = |col: TxColumn| match col {
            Date => DATE_WIDTH,
            Description => description_lo,
            Account => needs.account.min(ACCOUNT_FLOOR),
            Category => needs.category.min(CATEGORY_FLOOR),
            Amount => AMOUNT_WIDTH,
            Balance => BALANCE_WIDTH,
        };

        let spacing = COLUMN_SPACING * (cols.len() as u16 - 1);
        let floor_total = cols.iter().map(|&col| lo(col)).sum::<u16>() + spacing;
        if floor_total > width {
            continue;
        }

        // This set fits at its floors: grow the content columns back toward
        // their full content, the description (the column the eye reads)
        // first, and let any remainder pad the description.
        let mut planned: Vec<(TxColumn, u16)> = cols.iter().map(|&col| (col, lo(col))).collect();
        let mut leftover = width - floor_total;
        for (col, need) in [
            (Description, needs.description),
            (Category, needs.category),
            (Account, needs.account),
        ] {
            if let Some(entry) = planned.iter_mut().find(|(c, _)| *c == col) {
                let grow = need.saturating_sub(entry.1).min(leftover);
                entry.1 += grow;
                leftover -= grow;
            }
        }
        if let Some(entry) = planned.iter_mut().find(|(c, _)| *c == Description) {
            entry.1 += leftover;
        }
        return planned;
    }

    Vec::new()
}

fn draw_transaction_table(
    f: &mut Frame,
    app: &App,
    transactions: &[&Transaction],
    selected: usize,
    area: Rect,
    focused: bool,
    include_category: bool,
) {
    // Measure the widest visible values so the content-sized columns truncate
    // only when the width genuinely runs out.
    let needs = ColumnNeeds {
        description: transactions
            .iter()
            .map(|tx| tx.description.chars().count() as u16)
            .max()
            .unwrap_or(0),
        category: if include_category {
            transactions
                .iter()
                .filter_map(|tx| category_cell_text(app, tx, usize::MAX))
                .map(|(text, _)| text.chars().count() as u16)
                .max()
                .unwrap_or(0)
        } else {
            0
        },
        account: transactions
            .iter()
            .map(|tx| compact_account_label(app, tx).chars().count() as u16)
            .max()
            .unwrap_or(0),
    };

    let cols = plan_transaction_columns(area.width, include_category, needs);
    let constraints: Vec<Constraint> = cols.iter().map(|&(_, w)| Constraint::Length(w)).collect();

    // When "view details" (`v`) is on, prebuild the wrapped two-column detail
    // for the selected row. Building it here (where the width is known) lets us
    // reserve exactly as many rows as it occupies, since we wrap by hand. The
    // detail panel only applies to the focused table.
    let detail_lines = if focused && app.view_details {
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
        let is_selected = focused && i == selected;
        let is_pending = app.is_pending_transfer_tx(tx.id);
        let is_candidate = app.is_transfer_candidate(tx.id);
        let is_disabled =
            app.input_mode == InputMode::TransferPending && !is_candidate && !is_pending;

        let base_style = if is_pending {
            Style::default().bg(Color::Blue)
        } else if is_selected && app.input_mode == InputMode::TransferPending {
            Style::default().bg(Color::Green)
        } else {
            row_style(is_selected)
        };

        let fg = if is_disabled {
            Color::DarkGray
        } else {
            Color::Reset
        };

        let amount_fg = if is_disabled {
            Color::DarkGray
        } else {
            amount_color(tx.amount_cents)
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
                        let account = compact_account_label(app, tx);
                        // The account is contextual, so dim it whether or not
                        // the row is disabled.
                        Cell::from(fit_path(&account, w))
                            .style(Style::default().fg(dim_fg(is_selected)))
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
                    .style(Style::default().fg(amount_fg)),
                    TxColumn::Balance => Cell::from(
                        Line::from(format_cents(tx.balance_cents)).alignment(Alignment::Right),
                    )
                    .style(Style::default().fg(fg)),
                }
            })
            .collect::<Vec<_>>();

        Row::new(cells).style(base_style)
    });
}

/// Text + colour for the category column: the category path (yellow, like the
/// Categories tab) or, for a transfer, `to:`/`from:` the counterpart
/// `bank/account` (cyan). `None` leaves the cell blank. The value is truncated
/// to `width` via [`fit_path`]; for transfers the `to:`/`from:` label is kept
/// intact and only the account portion is abbreviated.
fn category_cell_text(app: &App, tx: &Transaction, width: usize) -> Option<(String, Color)> {
    if let Some((label, account)) = transfer_counterpart(app, tx) {
        let prefix = label.len() + 1; // "to:" / "from:"
        let account = fit_path(&account, width.saturating_sub(prefix));
        Some((format!("{label}:{account}"), Color::Cyan))
    } else {
        app.get_cached_category(tx.id)
            .filter(|c| !c.is_empty())
            .map(|category| (fit_path(category, width), Color::Yellow))
    }
}

/// Abbreviate a single path segment longer than four characters to its first
/// three characters plus an ellipsis (e.g. `Personal-Rentals` → `Per…`),
/// leaving shorter segments untouched.
fn abbreviate_segment(seg: &str) -> String {
    if seg.chars().count() > 4 {
        let head: String = seg.chars().take(3).collect();
        format!("{head}…")
    } else {
        seg.to_string()
    }
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

/// Fit a `/`-separated value (category path or `bank/account`) into `width`,
/// trimming as little as possible and protecting the last (most specific)
/// segment:
///
/// 1. If it already fits, return it unchanged.
/// 2. Abbreviate leading segments front-to-back (each to three chars + `…`),
///    stopping as soon as the whole value fits.
/// 3. If still too long, truncate only the last segment with a trailing
///    ellipsis to fill the remaining width.
/// 4. If even the abbreviated leading segments overflow, hard-truncate the
///    whole string.
///
/// So `Bankwest/Smart Saver` becomes `Ban…/Smart Saver` at width 16 and
/// `Ban…/Smart Sa…` at width 14.
fn fit_path(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }

    let mut segments: Vec<String> = value.split('/').map(str::to_string).collect();
    let joined_len = |segs: &[String]| segs.join("/").chars().count();
    let last = segments.len().saturating_sub(1);

    // Abbreviate leading segments (all but the last) until the value fits.
    for i in 0..last {
        if joined_len(&segments) <= width {
            break;
        }
        segments[i] = abbreviate_segment(&segments[i]);
    }
    if joined_len(&segments) <= width {
        return segments.join("/");
    }

    // Still too long: truncate only the last segment to the remaining width.
    let lead = segments[..last].join("/");
    let lead_width = lead.chars().count() + usize::from(last > 0);
    segments[last] = truncate_width(&segments[last], width.saturating_sub(lead_width));
    let joined = segments.join("/");
    if joined.chars().count() <= width {
        return joined;
    }

    // Even the abbreviated leading segments overflow: hard-truncate everything.
    truncate_width(&joined, width)
}

/// Every detail (name, value) pair for a transaction, in display order: the
/// human-friendly fields first, then any metadata entries (sorted by key). This
/// is the data shown by the "view details" (`v`) panel.
fn transaction_detail_pairs(app: &App, tx: &Transaction) -> Vec<(String, String)> {
    let mut pairs = vec![
        ("ID".to_string(), tx.id.to_string()),
        ("Date".to_string(), tx.date.to_string()),
        ("Account".to_string(), compact_account_label(app, tx)),
        ("Description".to_string(), tx.description.clone()),
        ("Amount".to_string(), format_cents(tx.amount_cents)),
        ("Balance".to_string(), format_cents(tx.balance_cents)),
    ];

    // The transaction is either part of a transfer or categorised, never both.
    if let Some((label, account)) = transfer_counterpart(app, tx) {
        if !account.is_empty() {
            pairs.push(("Transfer".to_string(), format!("{label} {account}")));
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

        Row::new(vec![
            Cell::from(tx.date.to_string()),
            Cell::from(tx.description.as_str()),
            Cell::from(Line::from(format_cents(tx.amount_cents)).alignment(Alignment::Right))
                .style(Style::default().fg(amount_color(tx.amount_cents))),
            Cell::from(category_path).style(Style::default().fg(Color::Yellow)),
            Cell::from(confidence).style(Style::default().fg(Color::Cyan)),
        ])
        .style(row_style(is_selected))
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
            .style(row_style(is_selected))
        });
}

/// Gap between the categories list and the transactions side panel.
const CATEGORY_PANEL_GAP: u16 = 2;

fn draw_categories(f: &mut Frame, app: &App, area: Rect) {
    if app.show_category_transactions {
        // It's still the category view, so don't truncate paths: size the list
        // to the longest one and give the rest to the transactions panel.
        let longest = app
            .lists
            .categories
            .iter()
            .map(|c| c.path.chars().count() as u16)
            .max()
            .unwrap_or(0);
        let left = categories_pane_width(longest, area.width);
        let chunks = Layout::horizontal([
            Constraint::Length(left),
            Constraint::Length(CATEGORY_PANEL_GAP),
            Constraint::Min(0),
        ])
        .split(area);
        draw_categories_list(f, app, chunks[0]);
        draw_category_transactions(f, app, chunks[2]);
    } else {
        draw_categories_list(f, app, area);
    }
}

/// Width for the categories list when its side panel is open: wide enough to
/// show the longest category path untruncated (count column + spacing + path),
/// but never so wide it leaves the transactions panel below a usable minimum.
fn categories_pane_width(longest_path: u16, total: u16) -> u16 {
    const COUNT_COL: u16 = 4;
    const MIN_PANEL: u16 = 30;
    let desired = COUNT_COL + COLUMN_SPACING + longest_path;
    let max_left = total.saturating_sub(CATEGORY_PANEL_GAP + MIN_PANEL);
    desired.min(max_left).max(COUNT_COL + COLUMN_SPACING)
}

/// Side panel listing every transaction in the selected category, in the
/// Transactions table format minus the Category column. Read-only: no row is
/// highlighted because focus stays on the categories list.
fn draw_category_transactions(f: &mut Frame, app: &App, area: Rect) {
    let transactions: Vec<_> = app.category_transactions.iter().collect();
    if transactions.is_empty() {
        draw_empty_message(f, "No transactions in this category.", area);
        return;
    }
    draw_transaction_table(f, app, &transactions, 0, area, false, false);
}

fn draw_categories_list(f: &mut Frame, app: &App, area: Rect) {
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

        let tx_count = app.category_transaction_count(cat.id);

        Row::new(vec![
            Cell::from(Line::from(format!("{}", tx_count)).alignment(Alignment::Right))
                .style(Style::default().fg(dim_fg(is_selected))),
            Cell::from(cat.path.as_str()).style(Style::default().fg(Color::Yellow)),
        ])
        .style(row_style(is_selected))
    });
}

fn draw_accounts(f: &mut Frame, app: &App, area: Rect) {
    if app.show_account_transactions {
        // It's still the account view, so don't truncate paths: size the list to
        // the longest one and give the rest to the transactions panel.
        let longest = app
            .lists
            .accounts
            .iter()
            .map(|a| a.path.chars().count() as u16)
            .max()
            .unwrap_or(0);
        let left = categories_pane_width(longest, area.width);
        let chunks = Layout::horizontal([
            Constraint::Length(left),
            Constraint::Length(CATEGORY_PANEL_GAP),
            Constraint::Min(0),
        ])
        .split(area);
        draw_accounts_list(f, app, chunks[0]);
        draw_account_transactions(f, app, chunks[2]);
    } else {
        draw_accounts_list(f, app, area);
    }
}

/// Side panel listing every transaction in the selected account, in the
/// Transactions table format minus the Category column. Read-only: no row is
/// highlighted because focus stays on the accounts list.
fn draw_account_transactions(f: &mut Frame, app: &App, area: Rect) {
    let transactions: Vec<_> = app.account_transactions.iter().collect();
    if transactions.is_empty() {
        draw_empty_message(f, "No transactions in this account.", area);
        return;
    }
    draw_transaction_table(f, app, &transactions, 0, area, false, false);
}

fn draw_accounts_list(f: &mut Frame, app: &App, area: Rect) {
    let accounts: Vec<_> = app.lists.accounts.iter().collect();

    if accounts.is_empty() {
        draw_empty_message(f, "No accounts yet.", area);
        return;
    }

    const COUNT_COL: u16 = 4;
    // Path column width the Table will lay out, so `fit_path` truncates to the
    // same budget rather than letting ratatui clip mid-word.
    let path_width = area.width.saturating_sub(COUNT_COL + COLUMN_SPACING).max(1) as usize;

    ScrollTable::new(
        &accounts,
        app.selected_index,
        &[Constraint::Length(COUNT_COL), Constraint::Min(30)],
    )
    .render(f, area, |i, account| {
        let is_selected = i == app.selected_index;

        let tx_count = app.account_transaction_count(account.id);

        Row::new(vec![
            Cell::from(Line::from(format!("{}", tx_count)).alignment(Alignment::Right))
                .style(Style::default().fg(dim_fg(is_selected))),
            Cell::from(fit_path(&account.path, path_width)),
        ])
        .style(row_style(is_selected))
    });
}

fn draw_filters(f: &mut Frame, app: &App, area: Rect) {
    let filters: Vec<_> = app.lists.filters.iter().collect();

    if filters.is_empty() {
        draw_empty_message(f, "No filters yet.", area);
        return;
    }

    ScrollTable::new(&filters, app.selected_index, &FILTER_COLS).render(f, area, |i, filter| {
        let is_selected = i == app.selected_index;

        let category = filter
            .category_id
            .and_then(|id| app.category_path(id))
            .map(|path| Cell::from(path.to_string()).style(Style::default().fg(Color::Yellow)))
            .unwrap_or_else(|| dim_dash_cell(is_selected));

        let override_mode = if filter.category_id.is_some() {
            let label = match filter.override_mode {
                FilterOverride::Uncategorised => "new",
                FilterOverride::Ai => "+ai",
                FilterOverride::All => "all",
            };
            Cell::from(label).style(Style::default().fg(Color::Cyan))
        } else {
            dim_dash_cell(is_selected)
        };

        let review = if filter.category_id.is_some() {
            if filter.review_required {
                Cell::from("review").style(Style::default().fg(Color::Yellow))
            } else {
                Cell::from("auto").style(Style::default().fg(dim_fg(is_selected)))
            }
        } else {
            dim_dash_cell(is_selected)
        };

        Row::new(vec![
            Cell::from(filter.name.as_str()),
            Cell::from(filter.query.as_str()).style(Style::default().fg(dim_fg(is_selected))),
            category,
            override_mode,
            review,
        ])
        .style(row_style(is_selected))
    });
}

fn dim_dash_cell(selected: bool) -> Cell<'static> {
    Cell::from("—").style(Style::default().fg(dim_fg(selected)))
}

fn draw_keybind_popup(f: &mut Frame, app: &App) {
    let help = keymap::help_lines(app);
    let screen = f.area();
    let width = 64.min(screen.width.saturating_sub(2)).max(20);
    let desired_height = (help.len() as u16)
        .saturating_add(MODAL_CHROME_HEIGHT)
        .max(8);
    let height = desired_height.min(screen.height.saturating_sub(2).max(1));
    let area = center(width, height, screen);
    let hints = [("Esc/Enter", "close"), ("Alt-?", "toggle hints?")];
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
    // One input line plus the shared modal chrome.
    let screen = f.area();
    let area = center(screen.width * 50 / 100, MODAL_CHROME_HEIGHT + 1, screen);
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
    let message = app.confirm_message.as_deref().unwrap_or("Confirm action?");
    let hints = keymap::footer_hints(app);
    let body = message_modal(f, "Confirm", Color::Cyan, message, &hints, 60);

    let msg_line = Paragraph::new(message)
        .style(Style::default().fg(Color::Yellow))
        .wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(msg_line, body);
}

fn draw_category_popup(f: &mut Frame, app: &App) {
    let screen = f.area();
    let area = center(screen.width * 50 / 100, screen.height * 60 / 100, screen);
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
            let bg = row_bg(selected);
            if cat.id == 0 {
                // The synthetic "what you typed" entry: a new category.
                Line::from(vec![
                    Span::styled(&cat.path, Style::default().fg(Color::Green).bg(bg)),
                    Span::styled(" (new)", Style::default().fg(dim_fg(selected)).bg(bg)),
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

    let screen = f.area();
    let area = center(screen.width * 60 / 100, screen.height * 70 / 100, screen);

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
        let is_cursor = i == state.cursor;
        let checkbox = if row.selected { "[x]" } else { "[ ]" };
        let checkbox_color = if row.selected {
            Color::Green
        } else {
            dim_fg(is_cursor)
        };

        Row::new(vec![
            Cell::from(checkbox).style(Style::default().fg(checkbox_color)),
            Cell::from(row.tx.date.to_string()),
            Cell::from(row.tx.description.as_str()),
            Cell::from(Line::from(format_cents(row.tx.amount_cents)).alignment(Alignment::Right))
                .style(Style::default().fg(amount_color(row.tx.amount_cents))),
            Cell::from(format_confidence_percent(f64::from(row.score)))
                .style(Style::default().fg(Color::Cyan)),
        ])
        .style(row_style(is_cursor))
    });
}

fn draw_apply_filters_popup(f: &mut Frame, app: &App) {
    let screen = f.area();
    let area = center(screen.width * 60 / 100, screen.height * 70 / 100, screen);
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
        let is_selected = i == selected;
        let account = account_label(app, tx);

        Row::new(vec![
            Cell::from(tx.date.to_string()),
            Cell::from(tx.description.as_str()),
            Cell::from(account).style(Style::default().fg(dim_fg(is_selected))),
            Cell::from(Line::from(format_cents(tx.amount_cents)).alignment(Alignment::Right))
                .style(Style::default().fg(amount_color(tx.amount_cents))),
        ])
        .style(row_style(is_selected))
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

    let hints = keymap::footer_hints(app);
    let body = message_modal(f, "No match", Color::Cyan, &msg, &hints, 40);

    let paragraph = Paragraph::new(msg)
        .style(Style::default().fg(Color::White))
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(paragraph, body);
}

fn draw_error_popup(f: &mut Frame, msg: &str) {
    let hints = [("Esc", "dismiss")];
    let body = message_modal(f, "Error", Color::Red, msg, &hints, 40);

    let paragraph = Paragraph::new(msg)
        .style(Style::default().fg(Color::White))
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(paragraph, body);
}

fn center(width: u16, height: u16, r: Rect) -> Rect {
    let popup_width = width.min(r.width);
    let popup_height = height.min(r.height);
    let x = (r.width - popup_width) / 2;
    let y = (r.height - popup_height) / 2;
    Rect::new(r.x + x, r.y + y, popup_width, popup_height)
}

/// Number of rows `text` occupies once wrapped to `width`, honouring embedded
/// newlines. Used to size text modals so the shared spacing lands exactly.
fn count_wrapped_lines(text: &str, width: usize) -> usize {
    text.split('\n')
        .map(|line| wrap_text(line, width).len())
        .sum()
}

/// Draw a short text modal sized to its (wrapped) message, following the shared
/// modal style, and return the body rect the caller should render the message
/// into. `width_pct` is the modal width as a percentage of the screen.
fn message_modal(
    f: &mut Frame,
    title: &str,
    border: Color,
    message: &str,
    hints: &[(&str, &str)],
    width_pct: u16,
) -> Rect {
    let screen = f.area();
    let width = (screen.width * width_pct / 100).clamp(20.min(screen.width), screen.width);
    let body_width = width.saturating_sub(4).max(1) as usize;
    let lines = count_wrapped_lines(message, body_width) as u16;
    let height = lines.saturating_add(MODAL_CHROME_HEIGHT).min(screen.height);
    let area = center(width, height, screen);
    Modal {
        title,
        hints,
        border,
    }
    .draw(f, area)
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
    let from_account = account_label(app, from);
    let to_account = account_label(app, to);

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
    let amount_style = Style::default().fg(amount_color(tx.amount_cents));

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
            Span::raw(account_label(app, tx)),
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
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use crate::search::ParsedQuery;
    use crate::{TransactionStore, TransferSource};

    use super::*;

    #[test]
    fn amount_color_uses_current_signed_threshold() {
        assert_eq!(amount_color(-1), Color::Red);
        assert_eq!(amount_color(0), Color::Green);
        assert_eq!(amount_color(1), Color::Green);
    }

    #[test]
    fn row_bg_matches_selected_row_colors() {
        assert_eq!(row_bg(true), Color::DarkGray);
        assert_eq!(row_bg(false), Color::Reset);
    }

    #[test]
    fn row_style_bolds_only_the_selected_row() {
        assert!(row_style(true).add_modifier.contains(Modifier::BOLD));
        assert!(!row_style(false).add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn dim_fg_lifts_dimmed_text_on_the_selected_row() {
        // DarkGray text would vanish on the DarkGray selection background.
        assert_eq!(dim_fg(true), Color::Gray);
        assert_eq!(dim_fg(false), Color::DarkGray);
    }

    #[test]
    fn center_clamps_to_screen_and_centers() {
        let screen = Rect::new(10, 20, 100, 40);
        assert_eq!(center(50, 10, screen), Rect::new(35, 35, 50, 10));

        let small = Rect::new(5, 7, 30, 10);
        assert_eq!(center(80, 20, small), Rect::new(5, 7, 30, 10));
    }

    #[test]
    fn transfer_counterpart_uses_cached_counterpart_account() {
        let (_temp, app, from, to) = app_with_transfer();

        assert_eq!(
            transfer_counterpart(&app, &from),
            Some(("to", "TestBank/Savings".to_string()))
        );
        assert_eq!(
            transfer_counterpart(&app, &to),
            Some(("from", "TestBank/Checking".to_string()))
        );
    }

    #[test]
    fn abbreviate_segment_shortens_long_segments_only() {
        // Segments longer than four chars collapse to three chars + ellipsis;
        // "Bond" (exactly four) is left alone.
        assert_eq!(abbreviate_segment("Personal-Rentals"), "Per…");
        assert_eq!(abbreviate_segment("Bond"), "Bond");
    }

    #[test]
    fn fit_path_protects_last_segment_and_trims_minimally() {
        // Fits as-is.
        assert_eq!(fit_path("Bankwest/Smart Saver", 20), "Bankwest/Smart Saver");
        // Abbreviating the leading segment alone is enough; last segment intact.
        assert_eq!(fit_path("Bankwest/Smart Saver", 16), "Ban…/Smart Saver");
        // Leading segment abbreviated, then the last segment trimmed just enough.
        assert_eq!(fit_path("Bankwest/Smart Saver", 14), "Ban…/Smart Sa…");
        // Single segment with no separators: a plain hard truncation.
        assert_eq!(fit_path("Groceries", 5), "Groc…");
        // Last segment uses all remaining width before truncating.
        assert_eq!(fit_path("Food/Groceries", 10), "Food/Groc…");
        // Even the abbreviated leading segments overflow, so hard-truncate.
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

    /// Widths of the planned columns keyed by column, for assertions.
    fn width_of(cols: &[(TxColumn, u16)], col: TxColumn) -> Option<u16> {
        cols.iter().find(|&&(c, _)| c == col).map(|&(_, w)| w)
    }

    #[test]
    fn plan_columns_drops_columns_rather_than_squeeze_description() {
        let needs = ColumnNeeds {
            description: 60,
            category: 24,
            account: 20,
        };

        // The full set at its floors needs 10+50+20+24+12+12 + 5 spacing = 133.
        let cols: Vec<_> = plan_transaction_columns(140, true, needs)
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

        // One short of the full set's floor total: Balance drops rather than
        // Description shrinking below its 50-column floor.
        let cols = plan_transaction_columns(132, true, needs);
        assert!(width_of(&cols, TxColumn::Balance).is_none());
        assert!(width_of(&cols, TxColumn::Description).unwrap() >= 50);

        // Narrower still: Account goes next, then Category — Description holds
        // its floor throughout.
        let cols = plan_transaction_columns(110, true, needs);
        assert!(width_of(&cols, TxColumn::Account).is_none());
        assert!(width_of(&cols, TxColumn::Category).is_some());
        assert!(width_of(&cols, TxColumn::Description).unwrap() >= 50);

        // Once only Date/Description/Amount remain, Description may shrink
        // below 50 (down to 20) before anything else gives way.
        let cols = plan_transaction_columns(60, true, needs);
        assert_eq!(
            cols.iter().map(|&(c, _)| c).collect::<Vec<_>>(),
            vec![TxColumn::Date, TxColumn::Description, TxColumn::Amount]
        );
        assert_eq!(width_of(&cols, TxColumn::Description), Some(36));

        // Very narrow: only Date and Description survive.
        let cols: Vec<_> = plan_transaction_columns(31, true, needs)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        assert_eq!(cols, vec![TxColumn::Date, TxColumn::Description]);
    }

    #[test]
    fn plan_columns_sizes_category_and_account_to_content() {
        // Plenty of width: Category and Account grow to exactly their widest
        // value on screen (not a fixed cap), and Description absorbs the rest.
        let needs = ColumnNeeds {
            description: 50,
            category: 37,
            account: 29,
        };
        let cols = plan_transaction_columns(200, true, needs);
        assert_eq!(width_of(&cols, TxColumn::Category), Some(37));
        assert_eq!(width_of(&cols, TxColumn::Account), Some(29));

        // Content narrower than the floor keeps the column at its content.
        let needs = ColumnNeeds {
            description: 50,
            category: 4,
            account: 4,
        };
        let cols = plan_transaction_columns(200, true, needs);
        assert_eq!(width_of(&cols, TxColumn::Account), Some(4));
        assert_eq!(width_of(&cols, TxColumn::Category), Some(4));
    }

    #[test]
    fn plan_columns_grows_description_before_category_and_account() {
        // 7 columns of slack over the floor total (133): all of it goes to the
        // Description, while Category and Account stay at their floors.
        let needs = ColumnNeeds {
            description: 80,
            category: 37,
            account: 29,
        };
        let cols = plan_transaction_columns(140, true, needs);
        assert_eq!(width_of(&cols, TxColumn::Description), Some(57));
        assert_eq!(width_of(&cols, TxColumn::Category), Some(24));
        assert_eq!(width_of(&cols, TxColumn::Account), Some(20));

        // With the description content fully shown, further slack grows
        // Category (then Account) toward their content.
        let cols = plan_transaction_columns(170, true, needs);
        assert_eq!(width_of(&cols, TxColumn::Description), Some(80));
        assert_eq!(width_of(&cols, TxColumn::Category), Some(31));
        assert_eq!(width_of(&cols, TxColumn::Account), Some(20));
    }

    #[test]
    fn plan_columns_excludes_category_when_requested() {
        // With the Category column suppressed it never appears, but the rest of
        // the layout is unaffected (Account still shows at a wide width).
        let needs = ColumnNeeds {
            description: 60,
            category: 0,
            account: 20,
        };
        let cols: Vec<_> = plan_transaction_columns(140, false, needs)
            .into_iter()
            .map(|(c, _)| c)
            .collect();
        assert!(!cols.contains(&TxColumn::Category));
        assert!(cols.contains(&TxColumn::Account));
    }

    #[test]
    fn categories_pane_width_fits_longest_without_starving_panel() {
        // Wide terminal: the list is exactly count + spacing + longest path.
        assert_eq!(categories_pane_width(20, 120), 4 + COLUMN_SPACING + 20);
        // Narrow terminal: clamped so the transactions panel keeps its minimum.
        assert_eq!(categories_pane_width(60, 80), 80 - CATEGORY_PANEL_GAP - 30);
    }

    #[test]
    fn plan_columns_widths_fit_within_area() {
        let needs = ColumnNeeds {
            description: 60,
            category: 24,
            account: 20,
        };
        for width in [10u16, 31, 44, 55, 66, 79, 100, 133, 200] {
            let cols = plan_transaction_columns(width, true, needs);
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

    fn app_with_transfer() -> (TempDir, App, Transaction, Transaction) {
        let temp = TempDir::new().unwrap();
        write_import_account(
            temp.path(),
            "Checking",
            "Transfer out",
            -10000,
            "fixture-from",
        );
        write_import_account(temp.path(), "Savings", "Transfer in", 10000, "fixture-to");

        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();
        let from = tx_by_description(&store, "Transfer out");
        let to = tx_by_description(&store, "Transfer in");
        store
            .create_transfer(from.id, to.id, TransferSource::Manual, true, None)
            .unwrap();

        let app = App::new(store).unwrap();
        (temp, app, from, to)
    }

    fn write_import_account(
        root: &Path,
        account: &str,
        description: &str,
        amount_cents: i64,
        hash: &str,
    ) {
        let account_dir = root.join("TestBank").join(account);
        fs::create_dir_all(&account_dir).unwrap();
        fs::write(account_dir.join("transactions.csv"), "fixture\n").unwrap();

        let payload = format!(
            r#"[{{"date":"2025-01-01","description":"{description}","amount_cents":{amount_cents},"balance_cents":50000,"hash":"{hash}"}}]"#
        );
        let import_script = account_dir.join("import");
        fs::write(
            &import_script,
            format!("#!/usr/bin/env bash\ncat <<'JSON'\n{payload}\nJSON\n"),
        )
        .unwrap();
        make_executable(&import_script);
    }

    fn tx_by_description(store: &TransactionStore, description: &str) -> Transaction {
        store
            .query_transactions(&ParsedQuery::empty(), None)
            .unwrap()
            .into_iter()
            .find(|tx| tx.description == description)
            .unwrap_or_else(|| panic!("missing transaction {description:?}"))
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        #[cfg(not(unix))]
        let _ = path;
    }
}
