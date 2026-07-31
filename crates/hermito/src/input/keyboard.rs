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

    if let crate::app::OverlaySnapshot::SshPassphrase { .. } = &snapshot.overlay {
        return match key.code {
            KeyCode::Char(character) => Some(Action::SshPassphraseInput(character)),
            KeyCode::Backspace => Some(Action::SshPassphraseBackspace),
            KeyCode::Enter => Some(Action::SshPassphraseSubmit),
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
    if let crate::app::OverlaySnapshot::RenameInput { .. } = &snapshot.overlay {
        return match key.code {
            KeyCode::Char(c) => Some(Action::RenameOverlayInput(c)),
            KeyCode::Backspace => Some(Action::RenameOverlayBackspace),
            KeyCode::Enter => Some(Action::RenameOverlayConfirm),
            KeyCode::Esc => Some(Action::RenameOverlayCancel),
            _ => None,
        };
    }
    if let crate::app::OverlaySnapshot::Completion { .. } = &snapshot.overlay {
        return match key.code {
            KeyCode::Tab | KeyCode::Down => Some(Action::NextControl),
            KeyCode::BackTab | KeyCode::Up => Some(Action::PrevControl),
            KeyCode::Enter => Some(Action::ActivateFocused),
            KeyCode::Esc => Some(Action::CancelModal),
            _ => None,
        };
    }
    if let crate::app::OverlaySnapshot::Hover { .. } = &snapshot.overlay {
        return match key.code {
            KeyCode::Esc | KeyCode::Enter => Some(Action::CancelModal),
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

    if matches!(key.code, KeyCode::Char('`')) && is_ctrl_or_cmd {
        return Some(Action::OpenTerminal);
    }
    if snapshot.terminal.captured {
        if key.code == KeyCode::Esc {
            return Some(Action::ReleaseTerminalCapture);
        }
        return terminal_key_bytes(key).map(Action::TerminalInput);
    }

    match key.code {
        KeyCode::F(6) => {
            if mods.contains(KeyModifiers::SHIFT) {
                Some(Action::CycleLandmarkBackward)
            } else {
                Some(Action::CycleLandmarkForward)
            }
        }
        KeyCode::F(7) => Some(Action::CycleAuthority),
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
        KeyCode::Char(' ') if is_ctrl_or_cmd => Some(Action::RequestCompletion),
        KeyCode::Char('h') | KeyCode::Char('H')
            if is_ctrl_or_cmd && mods.contains(KeyModifiers::SHIFT) =>
        {
            Some(Action::RequestHover)
        }
        KeyCode::F(12) => Some(Action::RequestDefinition),
        KeyCode::F(2) => Some(Action::RequestRename),
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

fn terminal_key_bytes(key: KeyEvent) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(8);
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }
    match key.code {
        KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if character.is_ascii() {
                bytes.push((character.to_ascii_uppercase() as u8) & 0x1f);
            } else {
                return None;
            }
        }
        KeyCode::Char(character) => {
            let mut encoded = [0_u8; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        }
        KeyCode::Enter => bytes.push(b'\r'),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::BackTab => bytes.extend_from_slice(b"\x1b[Z"),
        KeyCode::Up => bytes.extend_from_slice(b"\x1b[A"),
        KeyCode::Down => bytes.extend_from_slice(b"\x1b[B"),
        KeyCode::Right => bytes.extend_from_slice(b"\x1b[C"),
        KeyCode::Left => bytes.extend_from_slice(b"\x1b[D"),
        KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
        KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
        KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
        KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
        _ => return None,
    }
    Some(bytes)
}
