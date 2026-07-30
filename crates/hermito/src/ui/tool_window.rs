use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::theme;

pub fn render_shell(frame: &mut Frame<'_>, area: Rect, title: &str, focused: bool) -> Rect {
    if area.width < 2 || area.height < 2 {
        return Rect::default();
    }
    let border_style = Style::new().fg(if focused { theme::FOCUS } else { theme::RULE });
    let title_style = if focused {
        theme::focused()
    } else {
        theme::header().add_modifier(Modifier::BOLD)
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(vec![Span::styled(
            format!(" {title} "),
            title_style,
        )]))
        .style(theme::surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

pub fn render_context_collapsed(frame: &mut Frame<'_>, area: Rect) {
    if area.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(" Context closed · [C] on right stripe ")
            .style(theme::chrome())
            .block(
                Block::new()
                    .borders(Borders::TOP)
                    .border_style(Style::new().fg(theme::RULE)),
            ),
        area,
    );
}

pub fn render_bottom_header(
    frame: &mut Frame<'_>,
    area: Rect,
    active: usize,
    open: bool,
    focused: bool,
) {
    if area.is_empty() {
        return;
    }
    let labels = ["Terminal", "Problems", "Services"];
    let mut spans = Vec::with_capacity(labels.len().saturating_mul(2).saturating_add(2));
    spans.push(Span::styled(
        if focused { " > " } else { "   " },
        if focused {
            theme::focused()
        } else {
            theme::chrome()
        },
    ));
    for (index, label) in labels.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" | ", theme::chrome()));
        }
        let style = if index == active {
            theme::selected()
        } else {
            theme::chrome()
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }
    spans.push(Span::styled(
        if open {
            "  [v] Collapse"
        } else {
            "  [^] Open bottom tools"
        },
        theme::header(),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme::chrome()),
        area,
    );
}
