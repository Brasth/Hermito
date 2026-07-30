use crate::action::Action;
use crate::app::AppSnapshot;
use crate::layout::{Landmark, WorkbenchLayout};
use crossterm::event::{MouseButton as CtMouseButton, MouseEvent, MouseEventKind};
use ropey::Rope;

/// Map mouse event using snapshot layout rects + focus. CURRENT click on authority emits ReviewTrust.
/// Editor drags produce selection. Wheel tagged with exact landmark under pointer.
pub fn map_mouse(ev: MouseEvent, snapshot: &AppSnapshot) -> Option<Action> {
    let layout = &snapshot.layout;
    let (x, y) = (ev.column, ev.row);

    match ev.kind {
        MouseEventKind::Down(btn) => {
            let btn_code = match btn {
                CtMouseButton::Left => 0,
                CtMouseButton::Right => 1,
                CtMouseButton::Middle => 2,
            };
            if contains(layout.rect_authority(), x, y) {
                return Some(Action::ReviewTrust);
            }
            // Editor click: use canonical layout.rect_editor() + coordinate mapper.
            // The rect, border(1), tab row, optional breadcrumb, dynamic gutter, and
            // 4-cell tab policy are identical between renderer and this path.
            // Therefore clicks (incl. inside expanded tabs) map to displayed graphemes.
            if contains(layout.rect_editor(), x, y) {
                let text = snapshot
                    .current_buffer
                    .as_ref()
                    .map(|b| b.text.as_str())
                    .unwrap_or("");
                let rope = Rope::from_str(text);
                let scroll = snapshot
                    .current_buffer
                    .as_ref()
                    .map(|b| b.scroll_line)
                    .unwrap_or(0);
                let byte = crate::coordinate::editor_mouse_to_byte(
                    &rope,
                    layout.rect_editor(),
                    scroll,
                    x,
                    y,
                );
                return Some(Action::EditorSetCursor { byte });
            }
            // Stripes / panes set focus (primary also for tree)
            if contains(layout.rect_left_stripe(), x, y) {
                return Some(Action::FocusLandmark(Landmark::LeftStripe));
            }
            if contains(layout.rect_primary(), x, y) {
                return Some(Action::FocusLandmark(Landmark::PrimaryPane));
            }
            if contains(layout.rect_right_stripe(), x, y) {
                return Some(Action::FocusLandmark(Landmark::RightStripe));
            }
            if contains(layout.rect_bottom(), x, y) {
                return Some(Action::FocusLandmark(Landmark::BottomPane));
            }
            if contains(layout.rect_context(), x, y) {
                return Some(Action::FocusLandmark(Landmark::ContextPane));
            }
            Some(Action::MouseDown {
                x,
                y,
                button: btn_code,
            })
        }
        MouseEventKind::Drag(_) => {
            if contains(layout.rect_editor(), x, y) {
                let text = snapshot
                    .current_buffer
                    .as_ref()
                    .map(|b| b.text.as_str())
                    .unwrap_or("");
                let rope = Rope::from_str(text);
                let scroll = snapshot
                    .current_buffer
                    .as_ref()
                    .map(|b| b.scroll_line)
                    .unwrap_or(0);
                let byte = crate::coordinate::editor_mouse_to_byte(
                    &rope,
                    layout.rect_editor(),
                    scroll,
                    x,
                    y,
                );
                let anchor = snapshot
                    .current_buffer
                    .as_ref()
                    .and_then(|b| b.selection.map(|(a, _)| a))
                    .or_else(|| snapshot.current_buffer.as_ref().map(|b| b.cursor_byte))
                    .unwrap_or(0);
                Some(Action::EditorSetSelection {
                    anchor,
                    cursor: byte,
                })
            } else {
                None
            }
        }
        MouseEventKind::Up(_) => Some(Action::MouseUp),
        MouseEventKind::ScrollUp => {
            let lm = landmark_at(layout, x, y, snapshot.focus);
            Some(Action::Wheel {
                landmark: lm,
                lines: -3,
            })
        }
        MouseEventKind::ScrollDown => {
            let lm = landmark_at(layout, x, y, snapshot.focus);
            Some(Action::Wheel {
                landmark: lm,
                lines: 3,
            })
        }
        _ => None,
    }
}

fn contains(r: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

fn landmark_at(layout: &WorkbenchLayout, x: u16, y: u16, default: Landmark) -> Landmark {
    if contains(layout.rect_toolbar(), x, y) {
        return Landmark::Toolbar;
    }
    if contains(layout.rect_authority(), x, y) {
        return Landmark::Authority;
    }
    if contains(layout.rect_left_stripe(), x, y) {
        return Landmark::LeftStripe;
    }
    if contains(layout.rect_primary(), x, y) {
        return Landmark::PrimaryPane;
    }
    if contains(layout.rect_editor(), x, y) {
        return Landmark::Editor;
    }
    if contains(layout.rect_context(), x, y) {
        return Landmark::ContextPane;
    }
    if contains(layout.rect_right_stripe(), x, y) {
        return Landmark::RightStripe;
    }
    if contains(layout.rect_bottom(), x, y) {
        return Landmark::BottomPane;
    }
    if contains(layout.rect_status(), x, y) {
        return Landmark::StatusBar;
    }
    default
}
