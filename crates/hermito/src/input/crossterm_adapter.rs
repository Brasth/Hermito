use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};

/// Adapter layer. Pure classification + passthrough. Handler owns Action mapping.
pub fn is_f6(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Key(KeyEvent {
            code: KeyCode::F(6),
            ..
        })
    )
}
pub fn is_shift_f6(ev: &Event) -> bool {
    matches!(ev, Event::Key(KeyEvent { code: KeyCode::F(6), modifiers, .. }) if modifiers.contains(KeyModifiers::SHIFT))
}
pub fn is_alt_tool(ev: &Event) -> Option<u8> {
    if let Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers,
        ..
    }) = ev
    {
        if modifiers.contains(KeyModifiers::ALT) && ('1'..='4').contains(c) {
            return Some(c.to_digit(10).unwrap() as u8);
        }
    }
    None
}
pub fn is_command_k(ev: &Event) -> bool {
    if let Event::Key(KeyEvent {
        code: KeyCode::Char('k') | KeyCode::Char('K'),
        modifiers,
        ..
    }) = ev
    {
        return modifiers.contains(KeyModifiers::CONTROL)
            || modifiers.contains(KeyModifiers::SUPER);
    }
    false
}

pub fn as_key(ev: &Event) -> Option<KeyEvent> {
    if let Event::Key(k) = ev {
        Some(*k)
    } else {
        None
    }
}
pub fn as_mouse(ev: &Event) -> Option<MouseEvent> {
    if let Event::Mouse(m) = ev {
        Some(*m)
    } else {
        None
    }
}
pub fn as_paste(ev: &Event) -> Option<String> {
    if let Event::Paste(s) = ev {
        Some(s.clone())
    } else {
        None
    }
}
pub fn as_resize(ev: &Event) -> Option<(u16, u16)> {
    if let Event::Resize(w, h) = ev {
        Some((*w, *h))
    } else {
        None
    }
}
