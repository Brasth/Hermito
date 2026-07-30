use std::borrow::Cow;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{AppSnapshot, OverlaySnapshot, TrustLevel},
    layout::Landmark,
};

use super::{
    authority_path,
    editor::{self, EditorDocument},
    project_tree, status_bar, terminal_pane, theme,
    tool_stripe::{self, StripeSide},
    tool_window, toolbar,
};

pub fn render(frame: &mut Frame<'_>, snapshot: &AppSnapshot) {
    let area = frame.area();
    if area.is_empty() {
        return;
    }
    frame.render_widget(Block::new().style(theme::canvas()), area);

    let l = &snapshot.layout;
    // Derive bottom body/header/main geometry directly from WorkbenchLayout's
    // exact bottom_height (body) + bottom_header_h semantics. No renderer-only cap.
    // This makes the split areas' positions/heights match layout.rect_* used for hit-testing.
    let bottom_body_height = if l.bottom_visible { l.bottom_height } else { 0 };
    let main_height = area.height.saturating_sub(
        l.toolbar_h + l.authority_h + l.status_h + l.bottom_header_h + bottom_body_height,
    );
    let [toolbar_area, authority_area, main_area, bottom_header, bottom_body, status_area] =
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(l.toolbar_h),
                Constraint::Length(l.authority_h),
                Constraint::Length(main_height),
                Constraint::Length(l.bottom_header_h),
                Constraint::Length(bottom_body_height),
                Constraint::Length(l.status_h),
            ])
            .areas(area);

    // Consume layout rects for top (toolbar+authority) so renderer regions == hit-test authority rect
    // (used for CURRENT segment clickability at the visible row). No local height recompute.
    toolbar::render(frame, toolbar_area, snapshot);
    authority_path::render(frame, authority_area, snapshot);
    render_main(frame, main_area, snapshot);
    tool_window::render_bottom_header(
        frame,
        bottom_header,
        l.bottom_active_tab,
        l.bottom_visible,
        snapshot.focus == Landmark::BottomPane,
    );
    if l.bottom_visible {
        render_bottom_body(frame, bottom_body, snapshot);
    }
    status_bar::render(frame, status_area, snapshot);
    match &snapshot.overlay {
        OverlaySnapshot::None => {}
        OverlaySnapshot::TrustReview {
            workspace_root,
            authority_label,
            trust,
            capabilities,
            focused_grant,
            ..
        } => render_trust_review_overlay(
            frame,
            workspace_root,
            authority_label,
            *trust,
            capabilities,
            *focused_grant,
        ),
        OverlaySnapshot::CommandPalette {
            query,
            items,
            selected,
            ..
        } => render_command_palette_overlay(frame, query, items, *selected),
        OverlaySnapshot::SaveAs { path, .. } => render_save_as_overlay(frame, path),
        OverlaySnapshot::SshPassphrase {
            authority_label,
            length,
            ..
        } => render_ssh_passphrase_overlay(frame, authority_label, *length),
    }
}

fn render_main(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    if area.is_empty() {
        return;
    }
    // Renderer must use WorkbenchLayout's rects as the single source of truth
    // for geometry (including reservation of always-visible 1-row bottom_header
    // + body when open). This guarantees rendered editor rect == mouse rect_editor()
    // at 80/120/160 etc, and no overlap with bottom header.
    let layout = &snapshot.layout;

    tool_stripe::render(
        frame,
        layout.rect_left_stripe(),
        StripeSide::Left,
        layout.primary_active_tab,
        snapshot.focus,
    );
    if layout.primary_visible {
        let pa = layout.rect_primary();
        if !pa.is_empty() {
            let inner = tool_window::render_shell(
                frame,
                pa,
                "Project",
                snapshot.focus == Landmark::PrimaryPane,
            );
            project_tree::render(
                frame,
                inner,
                snapshot.project.tree.as_ref(),
                snapshot.project.loading,
                snapshot.project.selected_row as usize,
                snapshot.project.scroll as usize,
                snapshot.focus == Landmark::PrimaryPane,
            );
        }
    }
    let editor_rect = layout.rect_editor();
    render_editor(frame, editor_rect, snapshot);
    if layout.context_visible {
        let ca = layout.rect_context();
        if !ca.is_empty() {
            let inner = tool_window::render_shell(
                frame,
                ca,
                "Context",
                snapshot.focus == Landmark::ContextPane,
            );
            let authority = snapshot.authorities.get(snapshot.current_authority_idx);
            let context = authority.map_or_else(
                || "Current authority unavailable".to_owned(),
                |authority| {
                    format!(
                        "Current authority\n{} · {}\n\nExecution\n{}",
                        super::authority_kind_label(authority.kind),
                        authority.label,
                        super::trust_label(authority.trust)
                    )
                },
            );
            frame.render_widget(Paragraph::new(context).style(theme::surface()), inner);
        }
    }
    tool_stripe::render(
        frame,
        layout.rect_right_stripe(),
        StripeSide::Right,
        layout.context_active_tab,
        snapshot.focus,
    );
}

fn render_editor(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    let active_index = snapshot
        .active_editor_tab
        .min(snapshot.open_editor_tabs.len().saturating_sub(1));
    let active = snapshot
        .current_buffer
        .as_ref()
        .or_else(|| snapshot.open_editor_tabs.get(active_index));
    if let Some(active) = active {
        let document = EditorDocument {
            title: &active.title,
            path_label: &active.path_label,
            language: active.language,
            text: &active.text,
            cursor_byte: active.cursor_byte,
            selection: active.selection,
            scroll_line: active.scroll_line as usize,
            highlights: &active.highlights,
        };
        editor::render(
            frame,
            area,
            &snapshot.open_editor_tabs,
            active_index,
            &document,
            snapshot.focus == Landmark::Editor,
        );
    } else {
        let document = EditorDocument {
            title: "Untitled",
            path_label: "Untitled",
            language: crate::document::Language::PlainText,
            text: "",
            cursor_byte: 0,
            selection: None,
            scroll_line: 0,
            highlights: &[],
        };
        editor::render(
            frame,
            area,
            &snapshot.open_editor_tabs,
            0,
            &document,
            snapshot.focus == Landmark::Editor,
        );
    }
}

fn render_bottom_body(frame: &mut Frame<'_>, area: Rect, snapshot: &AppSnapshot) {
    if area.is_empty() {
        return;
    }
    let trusted = snapshot.current_trust == TrustLevel::Trusted;
    if snapshot.layout.bottom_active_tab == 0
        && (trusted || snapshot.terminal.state != crate::app::TerminalViewState::None)
    {
        terminal_pane::render(
            frame,
            area,
            &snapshot.terminal,
            snapshot.focus == Landmark::BottomPane,
        );
        return;
    }
    let (text, style) = match snapshot.layout.bottom_active_tab {
        1 if snapshot.status.problems == 0 => (
            Cow::Borrowed("No problems. Diagnostics will appear here with a file, location, and next action."),
            theme::surface(),
        ),
        1 => (
            Cow::Owned(format!(
                "{} problems. Open a diagnostic from the editor gutter.",
                snapshot.status.problems
            )),
            theme::inspect_only(),
        ),
        2 => (
            Cow::Owned(format!("Services: {}", snapshot.status.service)),
            theme::surface(),
        ),
        _ if trusted => (
            Cow::Borrowed("No terminal session. Bottom tools remain available while the editor keeps focus."),
            theme::surface(),
        ),
        _ => (
            Cow::Borrowed(
                "[x] Terminal blocked: this authority is inspect only. Review trust to allow execution.",
            ),
            theme::inspect_only(),
        ),
    };
    frame.render_widget(
        Paragraph::new(text.as_ref() as &str).style(style).block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(if snapshot.focus == Landmark::BottomPane {
                    theme::FOCUS
                } else {
                    theme::RULE
                })),
        ),
        area,
    );
}

pub fn render_trust_review_overlay(
    frame: &mut Frame<'_>,
    workspace_root: &str,
    authority: &str,
    trust: TrustLevel,
    capabilities: &[String],
    grant_or_revoke_focused: bool,
) {
    let area = centered(frame.area(), 72, 14);
    frame.render_widget(Clear, area);
    let action = if trust == TrustLevel::Trusted {
        "Revoke execution"
    } else {
        "Grant execution"
    };
    let action_style = if grant_or_revoke_focused {
        theme::focused()
    } else {
        theme::header()
    };
    let cancel_style = if grant_or_revoke_focused {
        theme::header()
    } else {
        theme::focused()
    };
    let capability_summary = if capabilities.is_empty() {
        "inspect reads only".to_owned()
    } else {
        capabilities.join(", ")
    };
    let lines = vec![
        Line::from(Span::styled(
            "Workspace trust",
            theme::header().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Workspace  ", theme::chrome()),
            Span::raw(workspace_root.to_owned()),
        ]),
        Line::from(vec![
            Span::styled("Authority  ", theme::chrome()),
            Span::raw(authority.to_owned()),
        ]),
        Line::from(vec![
            Span::styled("State      ", theme::chrome()),
            Span::styled(
                super::trust_label(trust),
                if trust == TrustLevel::Trusted {
                    theme::trusted()
                } else {
                    theme::inspect_only()
                },
            ),
        ]),
        Line::from(""),
        Line::from(format!("Capabilities: {capability_summary}")),
        Line::from("Opening this review does not change trust."),
        Line::from(""),
        Line::from(vec![
            Span::styled(format!(" [Enter] {action} "), action_style),
            Span::raw("   "),
            Span::styled(" Cancel [Esc] ", cancel_style),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::surface())
            .wrap(Wrap { trim: false })
            .block(
                Block::new()
                    .borders(Borders::ALL)
                    .border_style(Style::new().fg(theme::FOCUS))
                    .style(theme::surface()),
            ),
        area,
    );
}

pub fn render_command_palette_overlay(
    frame: &mut Frame<'_>,
    query: &str,
    items: &[String],
    selected: usize,
) {
    let area = centered(frame.area(), 68, 12);
    frame.render_widget(Clear, area);
    let mut lines = vec![
        Line::from(Span::styled(
            "Command palette",
            theme::header().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(" > ", theme::focused()),
            Span::raw(query.to_owned()),
        ]),
        Line::from(""),
    ];
    for (index, item) in items
        .iter()
        .take(area.height.saturating_sub(5) as usize)
        .enumerate()
    {
        lines.push(Line::from(Span::styled(
            format!("{} {item}", if index == selected { ">" } else { " " }),
            if index == selected {
                theme::selected()
            } else {
                theme::surface()
            },
        )));
    }
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            "No matching commands. Try 'trust', 'project', or 'terminal'.",
            theme::chrome(),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).style(theme::surface()).block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(theme::FOCUS))
                .style(theme::surface()),
        ),
        area,
    );
}

fn centered(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = max_width.min(area.width.saturating_sub(4)).max(1);
    let height = max_height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

pub fn render_save_as_overlay(frame: &mut Frame<'_>, path: &str) {
    let area = centered(frame.area(), 60, 5);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title("Save As")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::FOCUS))
        .style(theme::surface());
    let display = if path.is_empty() {
        "Type full path (e.g. /Users/you/file.rs or ./note.txt)\nEnter: save  •  Esc: cancel  •  Backspace".to_string()
    } else {
        format!("> {}", path)
    };
    let p = Paragraph::new(display)
        .block(block)
        .style(theme::surface())
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

pub fn render_ssh_passphrase_overlay(frame: &mut Frame<'_>, authority_label: &str, length: usize) {
    let area = centered(frame.area(), 60, 7);
    frame.render_widget(Clear, area);
    let lines = vec![
        Line::from(Span::styled(
            "Encrypted SSH key",
            theme::header().add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("Authority  {authority_label}")),
        Line::from(format!("Passphrase > {}", "•".repeat(length))),
        Line::from(""),
        Line::from("Enter: connect  •  Esc: cancel  •  Backspace"),
    ];
    frame.render_widget(
        Paragraph::new(lines).style(theme::surface()).block(
            Block::new()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(theme::FOCUS))
                .style(theme::surface()),
        ),
        area,
    );
}
