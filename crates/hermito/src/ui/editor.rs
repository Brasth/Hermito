use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    app::EditorTabSnapshot,
    document::Language,
    syntax::highlight::{HighlightKind, HighlightSpan},
};

use super::theme;

pub struct EditorDocument<'a> {
    pub title: &'a str,
    pub path_label: &'a str,
    pub language: Language,
    pub text: &'a str,
    pub cursor_byte: usize,
    pub selection: Option<(usize, usize)>,
    pub scroll_line: usize,
    pub highlights: &'a [HighlightSpan],
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    tabs: &[EditorTabSnapshot],
    active_tab: usize,
    document: &EditorDocument<'_>,
    focused: bool,
) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let border_style = Style::new().fg(if focused { theme::FOCUS } else { theme::RULE });
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style)
        .style(theme::canvas());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let breadcrumb_height = if inner.height >= 4 { 1 } else { 0 };
    let [tab_area, breadcrumb_area, code_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(breadcrumb_height),
            Constraint::Min(1),
        ])
        .areas(inner);
    render_tabs(frame, tab_area, tabs, active_tab, focused);
    if breadcrumb_height > 0 {
        render_breadcrumb(frame, breadcrumb_area, document);
    }
    render_source(frame, code_area, document, focused);
}

fn render_tabs(
    frame: &mut Frame<'_>,
    area: Rect,
    tabs: &[EditorTabSnapshot],
    active_tab: usize,
    focused: bool,
) {
    let mut spans = Vec::with_capacity(tabs.len().saturating_mul(2));
    for (index, tab) in tabs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("|", theme::chrome()));
        }
        let state = if tab.dirty { " *" } else { "" };
        let focus_mark = if focused && index == active_tab {
            ">"
        } else {
            " "
        };
        let style = if index == active_tab {
            theme::selected()
        } else {
            theme::header()
        };
        spans.push(Span::styled(
            format!("{focus_mark} {}{state} x ", tab.title),
            style,
        ));
    }
    if spans.is_empty() {
        spans.push(Span::styled(" Untitled ", theme::selected()));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme::header()),
        area,
    );
}

fn render_breadcrumb(frame: &mut Frame<'_>, area: Rect, document: &EditorDocument<'_>) {
    let language = match document.language {
        Language::Rust => "Rust",
        Language::TypeScript => "TypeScript",
        Language::JavaScript => "JavaScript",
        Language::Go => "Go",
        Language::Python => "Python",
        Language::PlainText => "Plain text",
    };
    let path = if document.path_label.is_empty() {
        document.title.to_owned()
    } else {
        document.path_label.replace(['/', '\\'], " > ")
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ", theme::header()),
            Span::styled(path, theme::header()),
            Span::styled(format!("  [{language}]"), theme::chrome()),
        ]))
        .style(theme::header()),
        area,
    );
}

fn render_source(frame: &mut Frame<'_>, area: Rect, document: &EditorDocument<'_>, focused: bool) {
    if area.is_empty() {
        return;
    }
    let line_count = document.text.split('\n').count().max(1);
    let number_width = line_count.to_string().len().max(2);
    let scroll_line = document.scroll_line.min(line_count.saturating_sub(1));
    let selection = document
        .selection
        .map(|(anchor, active)| (anchor.min(active), anchor.max(active)));
    let mut rendered = Vec::with_capacity(area.height as usize);
    let mut line_start = 0usize;
    for (line_index, line_text) in document.text.split('\n').enumerate() {
        let line_end = line_start.saturating_add(line_text.len());
        if line_index >= scroll_line && rendered.len() < area.height as usize {
            let cursor_line =
                document.cursor_byte >= line_start && document.cursor_byte <= line_end;
            let gutter_style = if focused && cursor_line {
                theme::info().add_modifier(Modifier::BOLD)
            } else {
                theme::muted()
            };
            let mut spans = vec![Span::styled(
                format!("{:>number_width$}  ", line_index.saturating_add(1)),
                gutter_style,
            )];
            let mut drew_cursor = false;
            for (relative_byte, grapheme) in line_text.grapheme_indices(true) {
                let byte = line_start.saturating_add(relative_byte);
                let display = if grapheme == "\t" {
                    // Contract: must match coordinate::TAB_CELL_WIDTH (single source for display geometry)
                    &"    "[..crate::coordinate::TAB_CELL_WIDTH]
                } else {
                    grapheme
                };
                let mut style = syntax_style(byte, document.highlights);
                if selection.is_some_and(|(start, end)| byte >= start && byte < end) {
                    style = style.bg(theme::SELECTION);
                }
                if focused && byte == document.cursor_byte {
                    style = style
                        .fg(theme::CANVAS)
                        .bg(theme::FOCUS)
                        .add_modifier(Modifier::BOLD);
                    drew_cursor = true;
                }
                spans.push(Span::styled(display, style));
            }
            if focused && cursor_line && !drew_cursor && document.cursor_byte == line_end {
                spans.push(Span::styled(
                    " ",
                    Style::new()
                        .fg(theme::CANVAS)
                        .bg(theme::FOCUS)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            rendered.push(Line::from(spans));
        }
        if rendered.len() >= area.height as usize {
            break;
        }
        line_start = line_end.saturating_add(1);
    }
    if rendered.is_empty() {
        let cursor = if focused {
            Style::new().fg(theme::CANVAS).bg(theme::FOCUS)
        } else {
            theme::canvas()
        };
        rendered.push(Line::from(vec![
            Span::styled(format!("{:>number_width$}  ", 1), theme::muted()),
            Span::styled(" ", cursor),
        ]));
    }
    frame.render_widget(Paragraph::new(rendered).style(theme::canvas()), area);
}

fn syntax_style(byte: usize, highlights: &[HighlightSpan]) -> Style {
    let index = highlights.partition_point(|span| span.end_byte <= byte);
    let Some(span) = highlights.get(index).filter(|span| span.start_byte <= byte) else {
        return theme::canvas();
    };
    match span.kind {
        HighlightKind::Keyword => theme::syntax_keyword(),
        HighlightKind::String => theme::syntax_string(),
        HighlightKind::Comment => theme::syntax_comment(),
        HighlightKind::Function => theme::syntax_function(),
        HighlightKind::Type => theme::syntax_type(),
        HighlightKind::Number => theme::info(),
        HighlightKind::Other => theme::canvas(),
    }
}
