//! Domain-agnostic scrollable table rendering for the TUI.
//!
//! This module is the only place these scrolling list views touch ratatui's
//! `Table` widget. Callers provide items, row/detail closures, constraints,
//! and rectangles only; this file must not import application or domain types.
//! If the table implementation is swapped out later, this module is the
//! boundary that changes.

use std::ops::Range;

use ratatui::{
    Frame,
    layout::{Constraint, Flex, Layout, Rect},
    widgets::{Row, Table},
};

const COLUMN_SPACING: u16 = 1;
const TABLE_FLEX: Flex = Flex::Start;

type DetailRenderer<'a, T> = Box<dyn FnMut(&mut Frame, &T, Rect) + 'a>;

struct Detail<'a, T> {
    height: u16,
    render: DetailRenderer<'a, T>,
}

/// Scrollable table with an optional inline detail panel for the selected row.
pub(crate) struct ScrollTable<'a, T> {
    items: &'a [T],
    selected: usize,
    widths: &'a [Constraint],
    detail: Option<Detail<'a, T>>,
}

impl<'a, T> ScrollTable<'a, T> {
    pub(crate) const fn new(items: &'a [T], selected: usize, widths: &'a [Constraint]) -> Self {
        Self {
            items,
            selected,
            widths,
            detail: None,
        }
    }

    pub(crate) fn detail(
        mut self,
        height: u16,
        render: impl FnMut(&mut Frame, &T, Rect) + 'a,
    ) -> Self {
        self.detail = Some(Detail {
            height,
            render: Box::new(render),
        });
        self
    }

    pub(crate) fn render(
        mut self,
        frame: &mut Frame,
        area: Rect,
        mut row: impl FnMut(usize, &'a T) -> Row<'a>,
    ) {
        if area.is_empty() || self.items.is_empty() {
            return;
        }

        let widths = resolve_column_widths(self.widths, area);
        let items = self.items;
        let selected = self.selected.min(items.len().saturating_sub(1));

        let Some(detail) = self.detail.as_mut() else {
            render_plain_table(frame, area, items, selected, &widths, &mut row);
            return;
        };

        let detail_height = clamped_detail_height(area.height as usize, detail.height as usize);
        if detail_height == 0 {
            render_plain_table(frame, area, items, selected, &widths, &mut row);
            return;
        }

        let offset = calculate_scroll_offset_with_detail(
            selected,
            items.len(),
            area.height as usize,
            detail.height as usize,
        );
        let row_capacity = area.height as usize - detail_height;
        let top_height = selected.saturating_sub(offset).saturating_add(1);
        let top_height = top_height.min(row_capacity) as u16;
        let detail_height = detail_height as u16;
        let bottom_height = area
            .height
            .saturating_sub(top_height)
            .saturating_sub(detail_height);

        let top_area = Rect {
            height: top_height,
            ..area
        };
        render_row_range(
            frame,
            top_area,
            items,
            offset..selected.saturating_add(1),
            &widths,
            &mut row,
        );

        let detail_area = Rect {
            y: area.y.saturating_add(top_height),
            height: detail_height,
            ..area
        };
        (detail.render)(frame, &items[selected], detail_area);

        let bottom_area = Rect {
            y: detail_area.y.saturating_add(detail_height),
            height: bottom_height,
            ..area
        };
        let bottom_start = selected.saturating_add(1);
        render_row_range(
            frame,
            bottom_area,
            items,
            bottom_start..bottom_start.saturating_add(bottom_height as usize),
            &widths,
            &mut row,
        );
    }
}

fn render_plain_table<'a, T>(
    frame: &mut Frame,
    area: Rect,
    items: &'a [T],
    selected: usize,
    widths: &[Constraint],
    row: &mut impl FnMut(usize, &'a T) -> Row<'a>,
) {
    let offset = calculate_scroll_offset(selected, items.len(), area.height as usize);
    render_row_range(
        frame,
        area,
        items,
        offset..offset.saturating_add(area.height as usize),
        widths,
        row,
    );
}

fn render_row_range<'a, T>(
    frame: &mut Frame,
    area: Rect,
    items: &'a [T],
    range: Range<usize>,
    widths: &[Constraint],
    row: &mut impl FnMut(usize, &'a T) -> Row<'a>,
) {
    if area.is_empty() || range.start >= items.len() {
        return;
    }

    let rows = items
        .iter()
        .enumerate()
        .skip(range.start)
        .take(range.end.saturating_sub(range.start))
        .take(area.height as usize)
        .map(|(i, item)| row(i, item))
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return;
    }

    frame.render_widget(
        Table::new(rows, widths.to_vec())
            .column_spacing(COLUMN_SPACING)
            .flex(TABLE_FLEX),
        area,
    );
}

/// Build a table with this module's standard column spacing and flex, so an
/// overlay (e.g. an inline detail line) lines up under the same columns a
/// [`ScrollTable`] rendered. Pass the same `widths` the rows used.
pub(crate) fn aligned_table<'a>(rows: Vec<Row<'a>>, widths: &[Constraint]) -> Table<'a> {
    Table::new(rows, widths.to_vec())
        .column_spacing(COLUMN_SPACING)
        .flex(TABLE_FLEX)
}

fn resolve_column_widths(widths: &[Constraint], area: Rect) -> Vec<Constraint> {
    if widths.is_empty() {
        return Vec::new();
    }

    // Inline detail rendering splits the visible rows into two ratatui tables:
    // the rows above the detail and the rows below it. Passing flexible widths
    // (for example `Min`) to each segment would make each table resolve its
    // geometry independently. Resolve once against the full table width, then
    // pass concrete `Length` widths plus the same spacing to both segments.
    // Because both segment areas keep the same x-offset and width, ratatui has
    // no remaining flexible column decision that can diverge between them.
    Layout::horizontal(widths)
        .flex(TABLE_FLEX)
        .spacing(COLUMN_SPACING)
        .split(Rect::new(area.x, area.y, area.width, 1))
        .iter()
        .map(|rect| Constraint::Length(rect.width))
        .collect()
}

pub(crate) fn calculate_scroll_offset(
    selected: usize,
    total: usize,
    visible_height: usize,
) -> usize {
    if total == 0 || visible_height == 0 {
        return 0;
    }

    let selected = selected.min(total - 1);
    if total <= visible_height || selected < visible_height / 2 {
        0
    } else if selected > total.saturating_sub(visible_height / 2) {
        total.saturating_sub(visible_height)
    } else {
        selected.saturating_sub(visible_height / 2)
    }
}

fn calculate_scroll_offset_with_detail(
    selected: usize,
    total: usize,
    area_height: usize,
    detail_height: usize,
) -> usize {
    let detail_height = clamped_detail_height(area_height, detail_height);
    let visible_rows = area_height.saturating_sub(detail_height);
    calculate_scroll_offset(selected, total, visible_rows)
}

fn clamped_detail_height(area_height: usize, detail_height: usize) -> usize {
    if area_height == 0 {
        0
    } else {
        detail_height.min(area_height.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_scroll_offset_reserves_room_for_detail_panel() {
        assert_eq!(calculate_scroll_offset_with_detail(10, 30, 12, 4), 6);
        assert_eq!(calculate_scroll_offset_with_detail(27, 30, 12, 4), 22);
    }

    #[test]
    fn detail_scroll_offset_handles_short_and_tiny_areas() {
        assert_eq!(calculate_scroll_offset_with_detail(1, 3, 12, 4), 0);
        assert_eq!(clamped_detail_height(5, 8), 4);
        assert_eq!(calculate_scroll_offset_with_detail(4, 10, 5, 8), 4);
        assert_eq!(calculate_scroll_offset_with_detail(0, 10, 0, 8), 0);
    }
}
