use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, Widget},
    Frame,
};

use crate::{
    app::{TerminalSnapshot, TerminalViewState},
    terminal::{CellAttrs, TerminalColor, TerminalSurface},
};

use super::theme;

pub fn render(frame: &mut Frame<'_>, area: Rect, terminal: &TerminalSnapshot, focused: bool) {
    if area.is_empty() {
        return;
    }
    let state_label = match terminal.state {
        TerminalViewState::None => "idle",
        TerminalViewState::Starting => "starting",
        TerminalViewState::Running if terminal.captured => "captured · Esc releases",
        TerminalViewState::Running => "running",
        TerminalViewState::Exited => "exited",
        TerminalViewState::Lost => "Lost · request new session",
    };
    let block = Block::new()
        .borders(Borders::ALL)
        .title(format!(
            " Terminal · {} · {} ",
            terminal.authority_label, state_label
        ))
        .border_style(Style::new().fg(if focused { theme::FOCUS } else { theme::RULE }));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match (&terminal.surface, terminal.state) {
        (
            Some(surface),
            TerminalViewState::Running | TerminalViewState::Exited | TerminalViewState::Lost,
        ) => {
            let surface = surface
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            frame.render_widget(SurfaceWidget { surface: &surface }, inner);
        }
        (_, TerminalViewState::Starting) => {
            frame.render_widget(
                Paragraph::new("Starting terminal without blocking the editor…")
                    .style(theme::muted()),
                inner,
            );
        }
        (_, TerminalViewState::Lost) => {
            frame.render_widget(Paragraph::new("Lost – request new session. Hermito never presents a disconnected PTY as resumed.").style(theme::danger()), inner);
        }
        _ => {
            frame.render_widget(Paragraph::new("No terminal session. Ctrl+` opens a new session after execution trust is granted.").style(theme::surface()), inner);
        }
    }
}

struct SurfaceWidget<'a> {
    surface: &'a TerminalSurface,
}

impl Widget for SurfaceWidget<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let rows = area.height.min(self.surface.height());
        let cols = area.width.min(self.surface.width());
        for row in 0..rows {
            for col in 0..cols {
                let Some(source) = self.surface.cell(row, col) else {
                    continue;
                };
                let target = &mut buffer[(area.x + col, area.y + row)];
                target.set_char(source.ch);
                target.set_style(cell_style(
                    source.style.fg,
                    source.style.bg,
                    source.style.attrs,
                ));
            }
        }
    }
}

fn cell_style(fg: TerminalColor, bg: TerminalColor, attrs: CellAttrs) -> Style {
    let mut style = Style::new()
        .fg(color(fg, theme::TEXT))
        .bg(color(bg, theme::CANVAS));
    if attrs.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if attrs.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if attrs.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if attrs.reverse {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn color(color: TerminalColor, default: Color) -> Color {
    match color {
        TerminalColor::Default => default,
        TerminalColor::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
        TerminalColor::Indexed(index) => ansi_color(index),
    }
}

fn ansi_color(index: u8) -> Color {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    if let Some((red, green, blue)) = BASE.get(index as usize).copied() {
        return Color::Rgb(red, green, blue);
    }
    if (16..=231).contains(&index) {
        let value = index - 16;
        let component = |part: u8| if part == 0 { 0 } else { part * 40 + 55 };
        return Color::Rgb(
            component(value / 36),
            component((value / 6) % 6),
            component(value % 6),
        );
    }
    let gray = 8_u8.saturating_add(index.saturating_sub(232).saturating_mul(10));
    Color::Rgb(gray, gray, gray)
}
