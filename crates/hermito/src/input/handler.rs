use crate::action::Action;
use crate::app::AppSnapshot;
use crate::input::{crossterm_adapter as adapter, keyboard, mouse};
use crossterm::event::Event;
/// Map any crossterm event to zero or more Actions.
/// Snapshot supplies current focus, overlay (modal) state, layout rects, current buffer, *and project tree/selection*.
/// Keyboard now routes PrimaryPane arrows/Enter for tree nav/activate using snapshot.project.
pub fn handle_event(event: Event, snapshot: &AppSnapshot) -> Vec<Action> {
    let mut out = Vec::with_capacity(2);

    // Paste is routed only to the currently captured input surface.
    if let Some(text) = adapter::as_paste(&event) {
        if snapshot.terminal.captured {
            out.push(Action::TerminalInput(text.into_bytes()));
        } else if snapshot.focus == crate::layout::Landmark::Editor {
            out.push(Action::EditorPaste(text));
        }
        return out;
    }

    // Resize
    if let Some((w, h)) = adapter::as_resize(&event) {
        out.push(Action::TerminalResize {
            width: w,
            height: h,
        });
        return out;
    }

    // Global quit shortcuts apply only outside terminal capture. Captured control
    // keys, including Ctrl-C, are bytes for the child process.
    if !snapshot.terminal.captured {
        if let Event::Key(k) = &event {
            if (k.code == crossterm::event::KeyCode::Char('c')
                || k.code == crossterm::event::KeyCode::Char('q'))
                && k.modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
            {
                out.push(Action::Quit);
                return out;
            }
        }
    }

    // Keyboard
    if let Some(key) = adapter::as_key(&event) {
        if let Some(a) = keyboard::map_key(key, snapshot) {
            out.push(a);
        }
    }

    // Mouse
    if let Some(m) = adapter::as_mouse(&event) {
        if let Some(a) = mouse::map_mouse(m, snapshot) {
            out.push(a);
        }
    }

    // F6 direct (in case keyboard path missed modifiers)
    if adapter::is_f6(&event) && !adapter::is_shift_f6(&event) && out.is_empty() {
        out.push(Action::CycleLandmarkForward);
    }
    if adapter::is_shift_f6(&event)
        && (out.is_empty() || !matches!(out.last(), Some(Action::CycleLandmarkBackward)))
    {
        out.push(Action::CycleLandmarkBackward);
    }

    // Alt tool direct
    if let Some(n) = adapter::is_alt_tool(&event) {
        out.push(Action::ActivateLeftTool(n));
    }

    // Ctrl/Cmd+K
    if adapter::is_command_k(&event) {
        out.push(Action::OpenCommandPalette);
    }

    out
}
