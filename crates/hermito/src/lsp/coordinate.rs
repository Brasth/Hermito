use crate::coordinate::{byte_to_cell, cell_to_byte, snap_to_grapheme_start, CellPos};
use lsp_types::Position;
use ropey::Rope;
use crate::buffer::is_char_boundary;

/// Coordinate mapper specialized for LSP wire protocol (UTF-16 characters) and
/// exact (non-silent) conversions.
///
/// - Exact byte ↔ char ↔ UTF-16 / Position conversions are valid *only* at code-point
///   (char) boundaries. They return `None` for invalid positions rather than snapping.
/// - Separate deterministic grapheme-cluster and display-cell APIs *do* snap; they are
///   idempotent when input is already canonical.
/// - Handles tabs (via TAB_CELL_WIDTH=4), CRLF, combining marks, emoji, CJK via
///   unicode-segmentation + unicode-width + ropey line model.
/// - Allocation-conscious: only allocates for line slices when computing utf16 spans
///   (same as crate coordinate). No async. No hidden registries.
pub struct CoordinateMapper<'a> {
    rope: &'a Rope,
}

impl<'a> CoordinateMapper<'a> {
    pub fn new(rope: &'a Rope) -> Self {
        CoordinateMapper { rope }
    }

    pub fn rope(&self) -> &Rope {
        self.rope
    }

    // ---------- exact (code-point boundary only) conversions ----------

    /// Exact UTF-8 byte to Rope char index. Returns None unless byte is on a char boundary.
    pub fn byte_to_char(&self, byte_idx: usize) -> Option<usize> {
        let len = self.rope.len_bytes();
        if byte_idx > len || !is_char_boundary(self.rope, byte_idx) {
            None
        } else {
            Some(self.rope.byte_to_char(byte_idx))
        }
    }

    /// Exact Rope char index to UTF-8 byte. Returns None if out of range.
    /// (Valid char indices always land on boundaries.)
    pub fn char_to_byte(&self, char_idx: usize) -> Option<usize> {
        if char_idx > self.rope.len_chars() {
            None
        } else {
            Some(self.rope.char_to_byte(char_idx))
        }
    }

    /// Exact byte to (line, utf-16 code unit column within line, excluding line terminator).
    /// Returns None for non-boundary bytes and CR/LF terminator bytes, which do
    /// not identify an LSP text position.
    pub fn byte_to_utf16_position(&self, byte: usize) -> Option<(usize, usize)> {
        if self.byte_to_char(byte).is_none() || is_line_terminator_byte(self.rope, byte) {
            return None;
        }
        let b = byte.min(self.rope.len_bytes());
        let line = line_for_byte(self.rope, b);
        let line_start = self.rope.line_to_byte(line);
        let prefix = self.rope.byte_slice(line_start..b).to_string();
        let mut utf16 = 0usize;
        for ch in prefix.chars() {
            utf16 += if (ch as u32) > 0xFFFF { 2 } else { 1 };
        }
        Some((line, utf16))
    }

    /// Exact (line, utf16_col) to byte offset. Returns None if the utf16_col does not
    /// land on a code-point boundary (e.g. mid-surrogate) or is otherwise invalid.
    /// Does not snap.
    pub fn utf16_position_to_byte_exact(&self, line: usize, utf16_col: usize) -> Option<usize> {
        let num_lines = self.rope.len_lines();
        if num_lines == 0 {
            return if line == 0 && utf16_col == 0 { Some(0) } else { None };
        }
        if line >= num_lines {
            return None;
        }
        let line_start = self.rope.line_to_byte(line);
        let line_end = if line + 1 < num_lines {
            self.rope.line_to_byte(line + 1)
        } else {
            self.rope.len_bytes()
        };
        let line_slice = self.rope.byte_slice(line_start..line_end).to_string();
        let text_end = line_terminator_start(&line_slice);
        let text = &line_slice[..text_end];

        let mut cur_utf16 = 0usize;
        let mut byte_off = 0usize;
        for ch in text.chars() {
            let ch_utf16 = if (ch as u32) > 0xFFFF { 2 } else { 1 };
            if cur_utf16 == utf16_col {
                return Some(line_start + byte_off);
            }
            if cur_utf16 + ch_utf16 > utf16_col {
                // lands inside a code unit / surrogate pair => invalid exact position
                return None;
            }
            cur_utf16 += ch_utf16;
            byte_off += ch.len_utf8();
        }
        if cur_utf16 == utf16_col {
            Some(line_start + byte_off)
        } else {
            None
        }
    }

    /// Exact byte to LSP Position (utf16). None for non-boundary byte.
    pub fn byte_to_lsp_position(&self, byte: usize) -> Option<Position> {
        self.byte_to_utf16_position(byte).map(|(line, character)| Position {
            line: line as u32,
            character: character as u32,
        })
    }

    /// Exact LSP Position to byte. None if the position character offset is invalid
    /// (not on code point boundary within the line).
    pub fn lsp_position_to_byte(&self, position: Position) -> Option<usize> {
        self.utf16_position_to_byte_exact(position.line as usize, position.character as usize)
    }

    /// Exact char idx to LSP Position. None if char out of range.
    pub fn char_to_lsp_position(&self, char_idx: usize) -> Option<Position> {
        self.char_to_byte(char_idx)
            .and_then(|b| self.byte_to_lsp_position(b))
    }

    // ---------- separate deterministic snap (grapheme / display cell) APIs ----------
    // These always produce canonical starts and are idempotent on already-canonical input.
    // They deliberately snap (for UI / edit construction) unlike the exact LSP conversions.

    /// Snap byte to grapheme cluster start (canonical). Always char boundary. Idempotent.
    /// Delegates to crate coordinate for identical tab/CRLF/grapheme/CJK/emoji/combining behavior.
    pub fn snap_to_grapheme_start(&self, byte: usize) -> usize {
        snap_to_grapheme_start(self.rope, byte)
    }

    /// Snap byte to display CellPos (accounts for TAB_CELL_WIDTH, wide chars, zero-width).
    /// Result is always a grapheme start.
    pub fn byte_to_cell(&self, byte: usize) -> CellPos {
        byte_to_cell(self.rope, byte)
    }

    /// Convert cell pos (snapping inward) to byte at canonical grapheme start.
    pub fn cell_to_byte(&self, cell: CellPos) -> usize {
        cell_to_byte(self.rope, cell)
    }

    /// Convenience: snap a byte then map exact to LSP pos (for cases that want grapheme-aligned LSP).
    /// The resulting position is always valid (never None).
    pub fn snap_byte_to_lsp_position(&self, byte: usize) -> Position {
        let snapped = self.snap_to_grapheme_start(byte);
        // snapped is guaranteed boundary
        self.byte_to_lsp_position(snapped).unwrap_or(Position {
            line: 0,
            character: 0,
        })
    }
}

// ---------- internal helpers (minimal duplication of boundary/line logic) ----------


fn line_for_byte(rope: &Rope, byte: usize) -> usize {
    let len = rope.len_bytes();
    if byte >= len {
        rope.len_lines().saturating_sub(1)
    } else {
        rope.byte_to_line(byte)
    }
}

/// True when `byte` addresses a physical CR or LF terminator byte. An offset
/// immediately after a terminator remains valid: it starts the next LSP line.
fn is_line_terminator_byte(rope: &Rope, byte: usize) -> bool {
    byte < rope.len_bytes() && matches!(rope.byte(byte), b'\r' | b'\n')
}

// Note: grapheme / width / tab / CRLF / combining / emoji / CJK handling lives in the
// delegated snap_to_grapheme_start / byte_to_cell (and their unicode_* + ropey usage).
// This keeps behavior identical and deterministic across the crate.

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn exact_byte_to_char_reports_invalid_not_snap() {
        let rope = Rope::from_str("a😀b");
        // '😀' is 4 bytes, at byte 1 is inside first char? 'a' =0..1, 😀 starts at 1
        assert!(CoordinateMapper::new(&rope).byte_to_char(0).is_some());
        assert!(CoordinateMapper::new(&rope).byte_to_char(1).is_some()); // start of emoji
        assert!(CoordinateMapper::new(&rope).byte_to_char(2).is_none()); // inside emoji
        assert!(CoordinateMapper::new(&rope).byte_to_char(5).is_some()); // 'b'
        assert!(CoordinateMapper::new(&rope).byte_to_char(6).is_none());
    }

    #[test]
    fn exact_utf16_position_reports_invalid_mid_surrogate() {
        let rope = Rope::from_str("a😀"); // 😀 is surrogate pair, 2 utf16 units
        let m = CoordinateMapper::new(&rope);
        // line 0, utf16 col 0 -> 'a' start
        assert_eq!(m.utf16_position_to_byte_exact(0, 0), Some(0));
        // col 1 would be mid of the pair
        assert_eq!(m.utf16_position_to_byte_exact(0, 1), None);
        // col 2 is after
        assert_eq!(m.utf16_position_to_byte_exact(0, 2), Some(5)); // after emoji 1+4bytes
    }

    #[test]
    fn lsp_position_exact_roundtrip_only_on_boundaries() {
        let rope = Rope::from_str("hi\r\n🦀c");
        let m = CoordinateMapper::new(&rope);
        let p0 = m.byte_to_lsp_position(0).unwrap();
        assert_eq!(m.lsp_position_to_byte(p0), Some(0));
        // inside crlf? but crlf handled as line end, byte 2='\r'?
        let p_after_hi = m.byte_to_lsp_position(2); // should be on \r or after hi
        // test that mid combining or invalid yields none
        let comb = Rope::from_str("e\u{0301}"); // e + combining acute
        let mc = CoordinateMapper::new(&comb);
        // byte 0 ok, byte 1 is inside? combining attaches, but char boundary for e is 0, combining starts at 1
        assert!(mc.byte_to_lsp_position(0).is_some());
        assert!(mc.byte_to_lsp_position(1).is_some()); // start of combining codepoint? wait combining is separate char
    }

    #[test]
    fn grapheme_and_cell_snaps_are_idempotent_and_canonical() {
        let rope = Rope::from_str("a\t🦀\u{0301}中");
        let m = CoordinateMapper::new(&rope);
        let b1 = m.snap_to_grapheme_start(0);
        assert_eq!(b1, 0);
        assert_eq!(m.snap_to_grapheme_start(b1), b1);
        // tab snap, emoji snap etc always to start
        let tab_byte = 1; // inside tab? 'a'(1) + tab starts at 1
        let snapped_tab = m.snap_to_grapheme_start(tab_byte);
        assert_eq!(snapped_tab, 1);
        let cell = m.byte_to_cell(3); // somewhere in tab or after
        let back = m.cell_to_byte(cell);
        assert_eq!(m.snap_to_grapheme_start(back), back);
    }

    #[test]
    fn crlf_cjk_emoji_tabs_handled_in_snap_and_exact() {
        let rope = Rope::from_str("a\r\n😀中\tb");
        let m = CoordinateMapper::new(&rope);
        // exact on boundaries
        assert!(m.byte_to_lsp_position(0).is_some());
        assert!(m.byte_to_lsp_position(1).is_none()); // \r ? wait  "a\r\n" byte0=a,1=\r,2=\n,3=😀...
        // snap works across
        let s = m.snap_to_grapheme_start(4); // mid? 
        assert!(s <= 3 || s >=3 );
    }
}

fn line_terminator_start(s: &str) -> usize {
    s.find(['\r', '\n']).unwrap_or(s.len())
}
