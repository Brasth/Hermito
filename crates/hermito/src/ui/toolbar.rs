use ratatui::{
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{AppSnapshot, TrustLevel},
    layout::Landmark,
};

use super::{authority_kind_label, theme};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    if area.is_empty() {
        return;
    }

    let authority = snapshot.authorities.get(snapshot.current_authority_idx);
    let kind = authority
        .map(|authority| authority_kind_label(authority.kind))
        .unwrap_or("LOCAL");
    let trusted = authority.is_some_and(|authority| authority.trust == TrustLevel::Trusted);

    let mut spans = vec![
        Span::styled(
            if snapshot.focus == Landmark::Toolbar {
                ">"
            } else {
                " "
            },
            theme::focused(),
        ),
        Span::styled(
            format!(" {} ", snapshot.workspace_name),
            theme::header().add_modifier(Modifier::BOLD),
        ),
        Span::styled("| [PR] Project ", theme::chrome()),
    ];
    if area.width >= 100 {
        spans.push(Span::styled("| [GT] Git ", theme::chrome()));
    }
    spans.push(Span::styled("| ", theme::chrome()));
    if trusted {
        spans.push(Span::styled(format!("[>] Run · {kind}"), theme::trusted()));
    } else {
        spans.push(Span::styled(
            "[x] Run blocked: inspect only",
            theme::inspect_only(),
        ));
    }
    if area.width >= 74 {
        spans.push(Span::styled("  Search/command [Ctrl+K] ", theme::chrome()));
    }

    let style = if snapshot.focus == Landmark::Toolbar {
        theme::focused()
    } else {
        theme::chrome()
    };
    frame.render_widget(Paragraph::new(Line::from(spans)).style(style), area);
}
