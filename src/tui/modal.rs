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

        if self.hints.is_empty() {
            return inner;
        }

        let chunks = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

        f.render_widget(Paragraph::new(hint_line(self.hints)), chunks[2]);

        chunks[0]
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
