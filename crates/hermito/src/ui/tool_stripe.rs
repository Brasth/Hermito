use ratatui::{
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Borders, List, ListItem},
    Frame,
};

use crate::layout::Landmark;

use super::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripeSide {
    Left,
    Right,
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    side: StripeSide,
    selected: usize,
    focus: Landmark,
) {
    if area.is_empty() {
        return;
    }

    let focused = matches!(
        (side, focus),
        (StripeSide::Left, Landmark::LeftStripe) | (StripeSide::Right, Landmark::RightStripe)
    );
    let labels: &[&str] = match side {
        StripeSide::Left => &["P", "G", "E", "O"],
        StripeSide::Right => &["C", "S", "V"],
    };
    let items = labels.iter().enumerate().map(|(index, label)| {
        let mark = if focused && index == selected {
            ">"
        } else {
            " "
        };
        let style = if index == selected {
            theme::selected()
        } else {
            theme::chrome()
        };
        ListItem::new(Line::from(format!("{mark}{label}"))).style(style)
    });
    let border_style = if focused {
        Style::new().fg(theme::FOCUS)
    } else {
        Style::new().fg(theme::RULE)
    };
    let block = Block::new()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(border_style)
        .style(theme::chrome());
    frame.render_widget(List::new(items).block(block), area);
}
