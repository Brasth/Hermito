use ratatui::{
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::{
    app::{AppSnapshot, AuthorityConnectionState, TrustLevel},
    layout::Landmark,
};

use super::{authority_kind_label, theme, trust_label};

pub fn render(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    if area.is_empty() {
        return;
    }

    let mut route = vec![
        Span::styled(
            if snapshot.focus == Landmark::Authority {
                ">"
            } else {
                " "
            },
            theme::focused(),
        ),
        Span::styled("AUTHORITY  ", theme::header().add_modifier(Modifier::BOLD)),
    ];
    for (index, authority) in snapshot.authorities.iter().enumerate() {
        if index > 0 {
            route.push(Span::styled(" -> ", theme::chrome()));
        }
        let style = if index == snapshot.current_authority_idx {
            theme::current_authority().add_modifier(Modifier::UNDERLINED)
        } else {
            theme::header()
        };
        route.push(Span::styled(
            format!(
                "{} · {}{}",
                authority_kind_label(authority.kind),
                authority.label,
                connection_suffix(authority.connection),
            ),
            style,
        ));
    }

    let current = snapshot.authorities.get(snapshot.current_authority_idx);
    let detail = match current {
        Some(authority) if authority.connection == AuthorityConnectionState::Lost => {
            Line::from(vec![
                Span::styled(" [x] CURRENT ", theme::danger()),
                Span::styled(
                    format!(
                        "{} · {}",
                        authority_kind_label(authority.kind),
                        authority.label
                    ),
                    theme::current_authority(),
                ),
                Span::styled(
                    "  |  LOST · new session required; no PTY resume",
                    theme::danger(),
                ),
                Span::styled("  [Enter] Review trust", theme::header()),
            ])
        }
        Some(authority) if authority.trust == TrustLevel::Trusted => Line::from(vec![
            Span::styled(" [+] CURRENT ", theme::current_authority()),
            Span::styled(
                format!(
                    "{} · {}",
                    authority_kind_label(authority.kind),
                    authority.label
                ),
                theme::current_authority(),
            ),
            Span::styled("  |  TRUSTED · execution granted", theme::trusted()),
            Span::styled("  [Enter] Review trust", theme::header()),
        ]),
        Some(authority) => Line::from(vec![
            Span::styled(" [!] CURRENT ", theme::current_authority()),
            Span::styled(
                format!(
                    "{} · {}",
                    authority_kind_label(authority.kind),
                    authority.label
                ),
                theme::current_authority(),
            ),
            Span::styled(
                "  |  INSPECT ONLY · execution blocked",
                theme::inspect_only(),
            ),
            Span::styled("  [Enter] Review trust", theme::header()),
        ]),
        None => Line::from(Span::styled(
            format!(
                " [!] CURRENT unavailable  |  {}",
                trust_label(TrustLevel::InspectOnly)
            ),
            theme::inspect_only(),
        )),
    };

    let mut lines = vec![Line::from(route)];
    if area.height > 1 {
        lines.push(detail);
    }
    let style = if snapshot.focus == Landmark::Authority {
        theme::focused()
    } else {
        theme::header()
    };
    frame.render_widget(Paragraph::new(lines).style(style), area);
}

fn connection_suffix(connection: AuthorityConnectionState) -> &'static str {
    match connection {
        AuthorityConnectionState::Disconnected => " [DISCONNECTED]",
        AuthorityConnectionState::Connecting => " [CONNECTING]",
        AuthorityConnectionState::Connected => "",
        AuthorityConnectionState::Lost => " [LOST]",
    }
}
