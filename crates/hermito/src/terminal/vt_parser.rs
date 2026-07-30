use super::surface::{CellStyle, TerminalColor, TerminalSurface};

const MAX_CSI_BYTES: usize = 128;
const MAX_OSC_BYTES: usize = 4096;
const MAX_HYPERLINK_BYTES: usize = 2048;
const MAX_DCS_BYTES: usize = 4096;

#[derive(Clone, Debug)]
enum ParseState {
    Ground,
    Escape,
    Csi(Vec<u8>),
    Osc(Vec<u8>),
    OscEscape(Vec<u8>),
    Dcs(usize),
    DcsEscape(usize),
}

#[derive(Clone, Debug)]
pub struct VtParser {
    state: ParseState,
    style: CellStyle,
    utf8: Vec<u8>,
}

impl Default for VtParser {
    fn default() -> Self {
        Self {
            state: ParseState::Ground,
            style: CellStyle::default(),
            utf8: Vec::with_capacity(4),
        }
    }
}

impl VtParser {
    pub fn feed(&mut self, bytes: &[u8], surface: &mut TerminalSurface) {
        for &byte in bytes {
            let state = std::mem::replace(&mut self.state, ParseState::Ground);
            self.state = match state {
                ParseState::Ground => self.ground(byte, surface),
                ParseState::Escape => self.escape(byte),
                ParseState::Csi(mut sequence) => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.apply_csi(&sequence, byte, surface);
                        ParseState::Ground
                    } else if sequence.len() < MAX_CSI_BYTES && (0x20..=0x3f).contains(&byte) {
                        sequence.push(byte);
                        ParseState::Csi(sequence)
                    } else {
                        surface.mark_truncated();
                        ParseState::Ground
                    }
                }
                ParseState::Osc(mut sequence) => match byte {
                    0x07 => {
                        self.apply_osc(&sequence, surface);
                        ParseState::Ground
                    }
                    0x1b => ParseState::OscEscape(sequence),
                    _ if sequence.len() < MAX_OSC_BYTES => {
                        sequence.push(byte);
                        ParseState::Osc(sequence)
                    }
                    _ => {
                        surface.mark_truncated();
                        ParseState::Ground
                    }
                },
                ParseState::OscEscape(mut sequence) => {
                    if byte == b'\\' {
                        self.apply_osc(&sequence, surface);
                        ParseState::Ground
                    } else if sequence.len() + 2 <= MAX_OSC_BYTES {
                        sequence.push(0x1b);
                        sequence.push(byte);
                        ParseState::Osc(sequence)
                    } else {
                        surface.mark_truncated();
                        ParseState::Ground
                    }
                }
                ParseState::Dcs(count) => match byte {
                    0x1b => ParseState::DcsEscape(count),
                    _ if count < MAX_DCS_BYTES => ParseState::Dcs(count + 1),
                    _ => {
                        surface.mark_truncated();
                        ParseState::Ground
                    }
                },
                ParseState::DcsEscape(count) => {
                    if byte == b'\\' {
                        ParseState::Ground
                    } else {
                        ParseState::Dcs(count.saturating_add(2))
                    }
                }
            };
        }
    }

    fn ground(&mut self, byte: u8, surface: &mut TerminalSurface) -> ParseState {
        match byte {
            0x1b => {
                self.flush_incomplete_utf8(surface);
                ParseState::Escape
            }
            b'\n' => {
                self.flush_incomplete_utf8(surface);
                surface.newline();
                ParseState::Ground
            }
            b'\r' => {
                self.flush_incomplete_utf8(surface);
                surface.carriage_return();
                ParseState::Ground
            }
            0x08 => {
                self.flush_incomplete_utf8(surface);
                surface.backspace();
                ParseState::Ground
            }
            b'\t' => {
                self.flush_incomplete_utf8(surface);
                surface.tab();
                ParseState::Ground
            }
            0x00..=0x1f | 0x7f => {
                self.flush_incomplete_utf8(surface);
                ParseState::Ground
            }
            _ => {
                self.push_text_byte(byte, surface);
                ParseState::Ground
            }
        }
    }

    fn escape(&mut self, byte: u8) -> ParseState {
        match byte {
            b'[' => ParseState::Csi(Vec::with_capacity(16)),
            b']' => ParseState::Osc(Vec::with_capacity(64)),
            b'P' | b'_' | b'^' => ParseState::Dcs(0),
            _ => ParseState::Ground,
        }
    }

    fn push_text_byte(&mut self, byte: u8, surface: &mut TerminalSurface) {
        self.utf8.push(byte);
        match std::str::from_utf8(&self.utf8) {
            Ok(text) => {
                for ch in text.chars() {
                    surface.put(ch, &self.style);
                }
                self.utf8.clear();
            }
            Err(error) if error.error_len().is_none() && self.utf8.len() < 4 => {}
            Err(_) => {
                surface.put('\u{fffd}', &self.style);
                self.utf8.clear();
            }
        }
    }

    fn flush_incomplete_utf8(&mut self, surface: &mut TerminalSurface) {
        if !self.utf8.is_empty() {
            surface.put('\u{fffd}', &self.style);
            self.utf8.clear();
        }
    }

    fn apply_csi(&mut self, sequence: &[u8], final_byte: u8, surface: &mut TerminalSurface) {
        let private = sequence.first() == Some(&b'?');
        let bytes = if private { &sequence[1..] } else { sequence };
        let params: Vec<u16> = if bytes.is_empty() {
            vec![0]
        } else {
            bytes
                .split(|byte| *byte == b';')
                .map(|part| {
                    std::str::from_utf8(part)
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0)
                })
                .take(16)
                .collect()
        };
        let first = params.first().copied().unwrap_or(0);
        match final_byte {
            b'A' => surface.move_cursor(-i32::from(first.max(1)), 0),
            b'B' => surface.move_cursor(i32::from(first.max(1)), 0),
            b'C' => surface.move_cursor(0, i32::from(first.max(1))),
            b'D' => surface.move_cursor(0, -i32::from(first.max(1))),
            b'H' | b'f' => {
                let row = params.first().copied().unwrap_or(1).max(1) - 1;
                let col = params.get(1).copied().unwrap_or(1).max(1) - 1;
                surface.set_cursor(row, col);
            }
            b'J' => surface.clear_display(first),
            b'K' => surface.clear_line(first),
            b'm' => self.apply_sgr(&params),
            b'h' if private && first == 25 => surface.set_cursor_visible(true),
            b'l' if private && first == 25 => surface.set_cursor_visible(false),
            _ => {}
        }
    }

    fn apply_sgr(&mut self, params: &[u16]) {
        let mut index = 0;
        while index < params.len() {
            match params[index] {
                0 => self.style = CellStyle::default(),
                1 => self.style.attrs.bold = true,
                3 => self.style.attrs.italic = true,
                4 => self.style.attrs.underline = true,
                7 => self.style.attrs.reverse = true,
                22 => self.style.attrs.bold = false,
                23 => self.style.attrs.italic = false,
                24 => self.style.attrs.underline = false,
                27 => self.style.attrs.reverse = false,
                30..=37 => self.style.fg = TerminalColor::Indexed((params[index] - 30) as u8),
                39 => self.style.fg = TerminalColor::Default,
                40..=47 => self.style.bg = TerminalColor::Indexed((params[index] - 40) as u8),
                49 => self.style.bg = TerminalColor::Default,
                90..=97 => self.style.fg = TerminalColor::Indexed((params[index] - 90 + 8) as u8),
                100..=107 => {
                    self.style.bg = TerminalColor::Indexed((params[index] - 100 + 8) as u8)
                }
                38 | 48 => {
                    let foreground = params[index] == 38;
                    if params.get(index + 1) == Some(&5) {
                        if let Some(value) = params.get(index + 2) {
                            if foreground {
                                self.style.fg = TerminalColor::Indexed((*value).min(255) as u8);
                            } else {
                                self.style.bg = TerminalColor::Indexed((*value).min(255) as u8);
                            }
                        }
                        index += 2;
                    } else if params.get(index + 1) == Some(&2) {
                        if let (Some(r), Some(g), Some(b)) = (
                            params.get(index + 2),
                            params.get(index + 3),
                            params.get(index + 4),
                        ) {
                            let color = TerminalColor::Rgb(
                                (*r).min(255) as u8,
                                (*g).min(255) as u8,
                                (*b).min(255) as u8,
                            );
                            if foreground {
                                self.style.fg = color;
                            } else {
                                self.style.bg = color;
                            }
                        }
                        index += 4;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn apply_osc(&mut self, sequence: &[u8], surface: &mut TerminalSurface) {
        let text = String::from_utf8_lossy(sequence);
        let mut parts = text.splitn(3, ';');
        match parts.next() {
            Some("0" | "2") => {
                if let Some(title) = parts.next() {
                    surface.set_title(title);
                }
            }
            Some("8") => {
                let _params = parts.next();
                let uri = parts.next().unwrap_or_default();
                self.style.hyperlink = if uri.is_empty() {
                    None
                } else if uri.len() <= MAX_HYPERLINK_BYTES
                    && (uri.starts_with("https://") || uri.starts_with("http://"))
                {
                    Some(std::sync::Arc::<str>::from(uri))
                } else {
                    None
                };
            }
            Some("52") => {}
            _ => {}
        }
    }
}
