use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{AppSnapshot, TrustLevel},
    layout::Landmark,
};

use super::{authority_kind_label, theme, trust_label};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    if area.is_empty() {
        return;
    }
    let [left, right] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .areas(area);

    let view = if snapshot.status.view.is_empty() {
        "Project"
    } else {
        snapshot.status.view.as_str()
    };
    let mut left_spans = vec![
        Span::styled(
            if snapshot.focus == Landmark::StatusBar {
                ">"
            } else {
                " "
            },
            theme::focused(),
        ),
        Span::styled(format!(" {view} "), theme::selected()),
    ];
    if snapshot.journal_lagging {
        left_spans.push(Span::styled("[!] Journal lagging", theme::inspect_only()));
    } else if area.width >= 90 {
        left_spans.push(Span::styled("[+] Journal current", theme::trusted()));
    }
    if area.width >= 120 {
        if let Some(branch) = &snapshot.status.branch {
            left_spans.push(Span::styled(format!(" Git: {branch}"), theme::chrome()));
        }
        if snapshot.status.problems > 0 {
            left_spans.push(Span::styled(
                format!(" [!] {}", snapshot.status.problems),
                theme::danger(),
            ));
        }
    }
    if area.width >= 100 {
        if let Some(message) = &snapshot.status.message {
            left_spans.push(Span::styled(format!("  {message}"), theme::chrome()));
        }
    }
    let left_style = if snapshot.focus == Landmark::StatusBar {
        theme::focused()
    } else {
        theme::chrome()
    };
    frame.render_widget(
        Paragraph::new(Line::from(left_spans))
            .style(left_style)
            .wrap(Wrap { trim: true }),
        left,
    );

    let authority = snapshot.authorities.get(snapshot.current_authority_idx);
    let kind = authority
        .map(|value| authority_kind_label(value.kind))
        .unwrap_or("LOCAL");
    let trust = snapshot.current_trust;
    let summary = if area.width >= 145 {
        format!(
            "{kind} · {} | {} | UTF-8 | Ln {}, Col {} | [F6] Next pane ",
            trust_label(trust),
            snapshot.status.service,
            snapshot.status.line.max(1),
            snapshot.status.column.max(1),
        )
    } else if area.width >= 115 {
        format!(
            "{kind} · {} | UTF-8 | Ln {}, Col {} | [F6] Next pane ",
            trust_label(trust),
            snapshot.status.line.max(1),
            snapshot.status.column.max(1),
        )
    } else {
        format!("{kind} · {} | [F6] ", trust_label(trust))
    };
    let style = if trust == TrustLevel::Trusted {
        theme::trusted()
    } else {
        theme::inspect_only()
    };
    frame.render_widget(Paragraph::new(summary).right_aligned().style(style), right);
}
