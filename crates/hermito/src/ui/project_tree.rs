use ratatui::{
    layout::Rect,
    style::Modifier,
    text::Line,
    widgets::{List, ListItem, Paragraph},
    Frame,
};

// flatten rebuilds visible rows on the fly using each entry.is_expanded (toggled via
// ProjectToggleDir from dir Enter in keyboard + app apply; selection preserved by
// name path not row in tree helpers).
use crate::project::tree::{EntryKind, ProjectEntry, ProjectTree};

use super::theme;

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    tree: Option<&ProjectTree>,
    loading: bool,
    selected_row: usize,
    scroll: usize,
    focused: bool,
) {
    if area.is_empty() {
        return;
    }
    let Some(tree) = tree else {
        let message = if loading {
            "Project files are loading..."
        } else {
            "Project is empty. Open or create a file."
        };
        frame.render_widget(Paragraph::new(message).style(theme::surface()), area);
        return;
    };
    if tree.entries.is_empty() {
        frame.render_widget(
            Paragraph::new("Project is empty. Open or create a file.").style(theme::surface()),
            area,
        );
        return;
    }

    let mut rows = Vec::new();
    flatten(&tree.entries, 0, &mut rows);
    let items = rows
        .into_iter()
        .enumerate()
        .skip(scroll)
        .map(|(row, (depth, entry))| {
            let mark = match entry.kind {
                EntryKind::Dir if entry.is_expanded => "-",
                EntryKind::Dir => "+",
                EntryKind::File => " ",
            };
            let suffix = if entry.kind == EntryKind::Dir {
                "/"
            } else {
                ""
            };
            let focus_mark = if focused && row == selected_row {
                ">"
            } else {
                " "
            };
            let text = format!(
                "{focus_mark}{}{mark} {}{suffix}",
                "  ".repeat(depth),
                entry.name
            );
            let style = if row == selected_row {
                theme::selected()
            } else if entry.kind == EntryKind::Dir {
                theme::surface().add_modifier(Modifier::BOLD)
            } else {
                theme::surface()
            };
            ListItem::new(Line::from(text)).style(style)
        });
    frame.render_widget(List::new(items).style(theme::surface()), area);
}

fn flatten<'a>(
    entries: &'a [ProjectEntry],
    depth: usize,
    rows: &mut Vec<(usize, &'a ProjectEntry)>,
) {
    for entry in entries {
        rows.push((depth, entry));
        if entry.is_expanded {
            flatten(&entry.children, depth.saturating_add(1), rows);
        }
    }
}
