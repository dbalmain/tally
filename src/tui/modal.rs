use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

pub struct Modal<'a> {
    pub title: &'a str,
    pub hints: &'a [(&'a str, &'a str)],
    pub border: Color,
}

impl Modal<'_> {
    /// Render shared overlay chrome and return the body rect for caller content.
    ///
    /// Shared modal style (applies everywhere via this method): one blank line
    /// under the title, one space of horizontal padding on each side, and — when
    /// hints are present — two blank lines between the body and the bottom hint
    /// row. Size a text modal to `MODAL_CHROME_HEIGHT + lines` so those two
    /// blank lines land exactly.
    pub fn draw(&self, f: &mut Frame, area: Rect) -> Rect {
        f.render_widget(Clear, area);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default()
                    .fg(self.border)
                    .add_modifier(Modifier::BOLD),
            ))
            .style(Style::default().bg(Color::Black).fg(self.border));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Inset the inner rect: a blank top line and a space of padding on each
        // side so body content never hugs the border.
        let padded = Rect {
            x: inner.x.saturating_add(1),
            y: inner.y.saturating_add(1),
            width: inner.width.saturating_sub(2),
            height: inner.height.saturating_sub(1),
        };

        if self.hints.is_empty() {
            return padded;
        }

        // Body, two blank lines, then the hint row pinned to the bottom.
        let chunks = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(padded);

        f.render_widget(Paragraph::new(hint_line(self.hints)), chunks[2]);

        chunks[0]
    }
}

/// Rows of modal chrome around a body of text: top border, blank top line, two
/// blank lines before the hint row, the hint row, and the bottom border. A
/// text modal `lines` tall should be `MODAL_CHROME_HEIGHT + lines` high for the
/// shared spacing to land exactly.
pub const MODAL_CHROME_HEIGHT: u16 = 6;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    /// Render a modal whose body is a single line of text and return the
    /// buffer's rows as trimmed-free strings, so the test can assert on the
    /// exact vertical/horizontal spacing.
    fn render_rows(area_w: u16, area_h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(area_w, area_h)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, area_w, area_h);
                let body = Modal {
                    title: "Confirm",
                    hints: &[("y", "confirm"), ("n", "cancel")],
                    border: Color::Cyan,
                }
                .draw(f, area);
                f.render_widget(Paragraph::new("Message"), body);
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        (0..area_h)
            .map(|y| {
                (0..area_w)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn modal_chrome_spacing_matches_house_style() {
        // MODAL_CHROME_HEIGHT (6) + one body line = 7 rows.
        let rows = render_rows(30, MODAL_CHROME_HEIGHT + 1);

        // Row 0: top border + title. Row 1: blank line under the title.
        assert!(rows[0].contains("Confirm"));
        assert!(rows[1].trim_matches(|c| c == '│' || c == ' ').is_empty());

        // Row 2: the message, indented one space from the left border.
        assert!(rows[2].starts_with("│ Message"));

        // Rows 3 and 4: two blank lines between the body and the hints.
        for blank in &rows[3..5] {
            assert!(blank.trim_matches(|c| c == '│' || c == ' ').is_empty());
        }

        // Row 5: the hint row. Row 6: bottom border.
        assert!(rows[5].contains("confirm") && rows[5].contains("cancel"));
        assert!(rows[6].contains('└'));
    }
}

pub fn hint_line(hints: &[(&str, &str)]) -> Line<'static> {
    let spans = hints
        .iter()
        .flat_map(|(key, desc)| {
            [
                Span::raw("  "),
                Span::styled((*key).to_string(), Style::default().fg(Color::Cyan)),
                Span::styled(format!(" {desc}"), Style::default().fg(Color::DarkGray)),
            ]
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}
