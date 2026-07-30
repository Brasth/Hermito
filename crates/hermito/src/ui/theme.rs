use ratatui::style::{Color, Modifier, Style};

pub const CANVAS: Color = Color::Rgb(0x11, 0x13, 0x16);
pub const CHROME: Color = Color::Rgb(0x18, 0x1b, 0x1f);
pub const SURFACE_1: Color = Color::Rgb(0x1d, 0x21, 0x26);
pub const SURFACE_2: Color = Color::Rgb(0x24, 0x2a, 0x30);
pub const SURFACE_3: Color = Color::Rgb(0x2b, 0x32, 0x3a);
pub const RULE: Color = Color::Rgb(0x35, 0x3d, 0x46);
pub const RULE_STRONG: Color = Color::Rgb(0x4a, 0x55, 0x61);
pub const TEXT: Color = Color::Rgb(0xd7, 0xdc, 0xe2);
pub const TEXT_DIM: Color = Color::Rgb(0xaf, 0xb7, 0xc0);
pub const TEXT_MUTED: Color = Color::Rgb(0x7f, 0x89, 0x94);
pub const SELECTION: Color = Color::Rgb(0x2f, 0x5f, 0x8f);
pub const FOCUS: Color = Color::Rgb(0x75, 0xb7, 0xf0);
pub const AUTHORITY: Color = Color::Rgb(0xd6, 0xa8, 0x4b);
pub const SUCCESS: Color = Color::Rgb(0x70, 0xb5, 0x80);
pub const DANGER: Color = Color::Rgb(0xd9, 0x68, 0x68);
pub const INFO: Color = Color::Rgb(0x73, 0xa9, 0xd8);

pub const fn canvas() -> Style {
    Style::new().fg(TEXT).bg(CANVAS)
}

pub const fn chrome() -> Style {
    Style::new().fg(TEXT_DIM).bg(CHROME)
}

pub const fn surface() -> Style {
    Style::new().fg(TEXT).bg(SURFACE_1)
}

pub const fn header() -> Style {
    Style::new().fg(TEXT).bg(SURFACE_2)
}

pub const fn muted() -> Style {
    Style::new().fg(TEXT_MUTED).bg(CANVAS)
}

pub const fn selected() -> Style {
    Style::new().fg(TEXT).bg(SELECTION)
}

pub const fn focused() -> Style {
    Style::new()
        .fg(FOCUS)
        .bg(CHROME)
        .add_modifier(Modifier::BOLD)
}

pub const fn current_authority() -> Style {
    Style::new()
        .fg(AUTHORITY)
        .bg(SURFACE_2)
        .add_modifier(Modifier::BOLD)
}

pub const fn trusted() -> Style {
    Style::new()
        .fg(SUCCESS)
        .bg(SURFACE_2)
        .add_modifier(Modifier::BOLD)
}

pub const fn inspect_only() -> Style {
    Style::new()
        .fg(AUTHORITY)
        .bg(SURFACE_2)
        .add_modifier(Modifier::BOLD)
}

pub const fn danger() -> Style {
    Style::new().fg(DANGER).bg(CANVAS)
}

pub const fn info() -> Style {
    Style::new().fg(INFO).bg(CANVAS)
}

pub const fn syntax_keyword() -> Style {
    Style::new()
        .fg(INFO)
        .bg(CANVAS)
        .add_modifier(Modifier::BOLD)
}

pub const fn syntax_string() -> Style {
    Style::new().fg(SUCCESS).bg(CANVAS)
}

pub const fn syntax_comment() -> Style {
    Style::new().fg(TEXT_MUTED).bg(CANVAS)
}

pub const fn syntax_type() -> Style {
    Style::new().fg(AUTHORITY).bg(CANVAS)
}

pub const fn syntax_function() -> Style {
    Style::new().fg(FOCUS).bg(CANVAS)
}
