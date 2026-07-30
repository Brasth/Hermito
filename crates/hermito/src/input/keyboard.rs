use crate::action::Action;
use crate::app::AppSnapshot;
use crate::layout::Landmark;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Map a key event to Action(s). Modal state from snapshot decides grant/cancel vs normal.
/// Tab in TrustReview flips grant focus (via NextControl). Enter only emits GrantTrust if grant focused in snapshot.
pub fn map_key(key: KeyEvent, snapshot: &AppSnapshot) -> Option<Action> {
    let mods = key.modifiers;
    let is_ctrl_or_cmd = mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::SUPER);

    // Modal handling first (TrustReview sole grant path)
    if let crate::app::OverlaySnapshot::TrustReview {
        focused_grant,
        trust,
        ..
    } = &snapshot.overlay
    {
        return match key.code {
            KeyCode::Tab => Some(Action::NextControl),
            KeyCode::BackTab => Some(Action::PrevControl),
            KeyCode::Enter => {
                if *focused_grant {
                    if *trust == crate::app::TrustLevel::Trusted {
                        Some(Action::RevokeTrust)
                    } else {
                        Some(Action::GrantTrust)
                    }
                } else {
                    Some(Action::CancelModal)
                }
            }
            KeyCode::Esc => Some(Action::CancelModal),
            _ => None,
        };
    }

    if let crate::app::OverlaySnapshot::CommandPalette { .. } = &snapshot.overlay {
        return match key.code {
            KeyCode::Tab | KeyCode::Down => Some(Action::NextControl),
            KeyCode::BackTab | KeyCode::Up => Some(Action::PrevControl),
            KeyCode::Enter => Some(Action::ActivateFocused),
            KeyCode::Backspace => Some(Action::PaletteBackspace),
            KeyCode::Esc => Some(Action::CancelModal),
            KeyCode::Char(c) => Some(Action::PaletteInput(c)),
            _ => None,
        };
    }
    if let crate::app::OverlaySnapshot::SaveAs { .. } = &snapshot.overlay {
        return match key.code {
            KeyCode::Char(c) => Some(Action::SaveAsOverlayInput(c)),
            KeyCode::Backspace => Some(Action::SaveAsOverlayBackspace),
            KeyCode::Enter => Some(Action::SaveAsOverlayConfirm),
            KeyCode::Esc => Some(Action::SaveAsOverlayCancel),
            _ => None,
        };
    }

    match key.code {
        KeyCode::F(6) => {
            if mods.contains(KeyModifiers::SHIFT) {
                Some(Action::CycleLandmarkBackward)
            } else {
                Some(Action::CycleLandmarkForward)
            }
        }
        KeyCode::Tab => Some(Action::NextControl),
        KeyCode::BackTab => Some(Action::PrevControl),
        KeyCode::Char('1') if mods.contains(KeyModifiers::ALT) => Some(Action::ActivateLeftTool(1)),
        KeyCode::Char('2') if mods.contains(KeyModifiers::ALT) => Some(Action::ActivateLeftTool(2)),
        KeyCode::Char('3') if mods.contains(KeyModifiers::ALT) => Some(Action::ActivateLeftTool(3)),
        KeyCode::Char('4') if mods.contains(KeyModifiers::ALT) => Some(Action::ActivateLeftTool(4)),
        KeyCode::Char('k') | KeyCode::Char('K') if is_ctrl_or_cmd => {
            Some(Action::OpenCommandPalette)
        }
        KeyCode::Char('s') | KeyCode::Char('S') if is_ctrl_or_cmd => Some(Action::Save),
        KeyCode::Esc => Some(Action::CancelModal),
        KeyCode::Enter => {
            if snapshot.focus == Landmark::Authority {
                Some(Action::ReviewTrust)
            } else if snapshot.focus == Landmark::PrimaryPane {
                if let Some(tree) = &snapshot.project.tree {
                    let row = snapshot.project.selected_row as usize;
                    if let Some(names) = tree.entry_path_at_row(row) {
                        let segs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                        if let Some(entry) = tree.find_entry(&segs) {
                            if entry.is_dir() {
                                if let Some(p) = tree.resolve_path(&segs) {
                                    return Some(Action::ProjectToggleDir { path: p });
                                }
                            } else if let Some(p) = tree.resolve_path(&segs) {
                                return Some(Action::RequestProjectFile { path: p });
                            }
                        }
                    }
                }
                None
            } else {
                Some(Action::ActivateFocused)
            }
        }
        KeyCode::Char(c) => {
            if snapshot.focus == Landmark::Editor {
                Some(Action::EditorInsert(c))
            } else {
                None
            }
        }
        KeyCode::Backspace => {
            if snapshot.focus == Landmark::Editor {
                Some(Action::EditorDeleteBackward)
            } else {
                None
            }
        }
        KeyCode::Up => {
            if snapshot.focus == Landmark::PrimaryPane {
                Some(Action::ProjectMoveSelection { delta: -1 })
            } else {
                Some(Action::EditorMoveCursor {
                    line_delta: -1,
                    col_delta: 0,
                    extend_selection: mods.contains(KeyModifiers::SHIFT),
                })
            }
        }
        KeyCode::Down => {
            if snapshot.focus == Landmark::PrimaryPane {
                Some(Action::ProjectMoveSelection { delta: 1 })
            } else {
                Some(Action::EditorMoveCursor {
                    line_delta: 1,
                    col_delta: 0,
                    extend_selection: mods.contains(KeyModifiers::SHIFT),
                })
            }
        }
        KeyCode::Left => Some(Action::EditorMoveCursor {
            line_delta: 0,
            col_delta: -1,
            extend_selection: mods.contains(KeyModifiers::SHIFT),
        }),
        KeyCode::Right => Some(Action::EditorMoveCursor {
            line_delta: 0,
            col_delta: 1,
            extend_selection: mods.contains(KeyModifiers::SHIFT),
        }),
        KeyCode::PageUp => Some(Action::EditorPage {
            up: true,
            extend: mods.contains(KeyModifiers::SHIFT),
        }),
        KeyCode::PageDown => Some(Action::EditorPage {
            up: false,
            extend: mods.contains(KeyModifiers::SHIFT),
        }),
        _ => None,
    }
}
