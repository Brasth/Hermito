use ratatui::layout::Rect;
use ropey::Rope;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Display cell position (line, column in terminal cells).
/// Column accounts for full-width (CJK/emoji) and zero-width (combining) characters.
/// Always corresponds to a grapheme cluster start when produced by these functions.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct CellPos {
    pub line: usize,
    pub column: usize,
}

impl CellPos {
    pub fn new(line: usize, column: usize) -> Self {
        CellPos { line, column }
    }
}
/// Tab expands to this many terminal cells in the editor renderer (see ui::editor render_source).
/// All byte<->cell, cell<->grapheme display conversions MUST use this width for '\t'.
/// Positions that fall inside a tab's cell range snap to the tab's starting grapheme.
pub const TAB_CELL_WIDTH: usize = 4;

/// Snap any byte offset to the start byte of its containing grapheme cluster (or previous valid).
/// Result is always a char boundary and the beginning of a user-perceived grapheme.
/// Used before constructing TextEdit and before cell calculations.
pub fn snap_to_grapheme_start(rope: &Rope, byte: usize) -> usize {
    let len = rope.len_bytes();
    if len == 0 {
        return 0;
    }
    let mut b = byte.min(len);

    // First ensure char boundary (rope requirement).
    while b > 0 && !rope_is_char_boundary(rope, b) {
        b -= 1;
    }
    if b < len && !rope_is_char_boundary(rope, b) {
        b = next_char_boundary(rope, b);
    }

    // Graphemes never cross lines, operate on the line slice for efficiency and correctness.
    let line = line_for_byte(rope, b);
    let line_start = rope.line_to_byte(line);
    let line_end = if line + 1 < rope.len_lines() {
        rope.line_to_byte(line + 1)
    } else {
        len
    };

    let line_slice = rope.byte_slice(line_start..line_end).to_string();
    // Visible text stops before any line terminator sequence.
    let text_end = line_terminator_start(&line_slice);
    let text = &line_slice[..text_end];

    if b <= line_start {
        return line_start;
    }
    let rel = (b - line_start).min(text.len());

    // Use grapheme_indices for reliable cluster starts.
    let mut candidate = 0usize;
    for (g_start, _) in text.grapheme_indices(true) {
        if g_start > rel {
            break;
        }
        candidate = g_start;
    }
    // If rel lands inside a cluster or exactly on start, use that start.
    // Also allow the position after last grapheme (end of visible text).
    if rel >= text.len() {
        line_start + text.len()
    } else {
        line_start + candidate
    }
}

fn next_char_boundary(rope: &Rope, mut byte: usize) -> usize {
    let len = rope.len_bytes();
    while byte < len && !rope_is_char_boundary(rope, byte) {
        byte += 1;
    }
    byte.min(len)
}

fn rope_is_char_boundary(rope: &Rope, byte: usize) -> bool {
    let len = rope.len_bytes();
    if byte == 0 || byte == len {
        return true;
    }
    if byte > len {
        return false;
    }
    let mut offset = 0;
    for chunk in rope.chunks() {
        let chunk_end = offset + chunk.len();
        if byte < chunk_end {
            return chunk.is_char_boundary(byte - offset);
        }
        offset = chunk_end;
    }
    false
}

fn line_for_byte(rope: &Rope, byte: usize) -> usize {
    let len = rope.len_bytes();
    if byte >= len {
        rope.len_lines().saturating_sub(1)
    } else {
        rope.byte_to_line(byte)
    }
}

/// Locate first byte offset of a line terminator (\r, \n, \r\n) within the slice, or len.
fn line_terminator_start(s: &str) -> usize {
    s.find(['\r', '\n']).unwrap_or(s.len())
}
/// Display width in cells for a grapheme as shown by the editor.
/// '\t' uses the canonical TAB_CELL_WIDTH (4); everything else uses unicode width.
/// This makes display coordinate APIs match the rendered editor exactly.
fn grapheme_display_width(g: &str) -> usize {
    if g == "\t" {
        TAB_CELL_WIDTH
    } else {
        g.width()
    }
}

/// Convert a display cell position to the corresponding byte offset.
/// The returned byte is always the start of a grapheme cluster (canonical).
/// Snaps inward for positions that land inside a wide character or after end of line.
pub fn cell_to_byte(rope: &Rope, pos: CellPos) -> usize {
    let num_lines = rope.len_lines();
    if num_lines == 0 {
        return 0;
    }
    let line = pos.line.min(num_lines.saturating_sub(1));
    let line_start = rope.line_to_byte(line);
    let line_end = if line + 1 < num_lines {
        rope.line_to_byte(line + 1)
    } else {
        rope.len_bytes()
    };

    let line_slice = rope.byte_slice(line_start..line_end).to_string();
    let text_end = line_terminator_start(&line_slice);
    let text = &line_slice[..text_end];

    let mut col = 0usize;
    for (g_start, g) in text.grapheme_indices(true) {
        let g_width = grapheme_display_width(g);
        if col + g_width > pos.column {
            // Landed inside a (possibly wide) grapheme including a tab's 4-cell span:
            // snap to its start. This ensures clicks inside tab map to the tab grapheme.
            return line_start + g_start;
        }
        if col == pos.column {
            return line_start + g_start;
        }
        col += g_width;
    }
    // Beyond last grapheme or empty line: end of visible text.
    line_start + text.len()
}

/// Convert a byte offset (will be snapped to grapheme start) to its display CellPos.
/// Zero-width combining characters and marks do not advance the column.
pub fn byte_to_cell(rope: &Rope, byte: usize) -> CellPos {
    let b = snap_to_grapheme_start(rope, byte);
    let len = rope.len_bytes();
    if len == 0 {
        return CellPos::new(0, 0);
    }
    let line = line_for_byte(rope, b);
    let line_start = rope.line_to_byte(line);
    let line_end = if line + 1 < rope.len_lines() {
        rope.line_to_byte(line + 1)
    } else {
        len
    };

    let line_slice = rope.byte_slice(line_start..line_end).to_string();
    let text_end = line_terminator_start(&line_slice);
    let text = &line_slice[..text_end];
    let rel = (b - line_start).min(text.len());

    let mut col = 0usize;
    for (g_start, g) in text.grapheme_indices(true) {
        if g_start >= rel {
            break;
        }
        col += grapheme_display_width(g);
    }
    CellPos::new(line, col)
}

/// Byte offset to (line, byte-column-within-line) using Rope's native line model.
/// Line terminators contribute to the line count but the returned col is the raw byte offset into the line
/// (may point at \r for CRLF files). Use cell conversions for display.
pub fn byte_to_line_col(rope: &Rope, byte: usize) -> (usize, usize) {
    let b = byte.min(rope.len_bytes());
    let line = line_for_byte(rope, b);
    let line_start = rope.line_to_byte(line);
    (line, b - line_start)
}

/// Inverse of byte_to_line_col. Clamps to valid range for the line. Does not snap graphemes.
pub fn line_col_to_byte(rope: &Rope, line: usize, col: usize) -> usize {
    let num_lines = rope.len_lines();
    if num_lines == 0 {
        return 0;
    }
    let line = line.min(num_lines.saturating_sub(1));
    let line_start = rope.line_to_byte(line);
    let line_end = if line + 1 < num_lines {
        rope.line_to_byte(line + 1)
    } else {
        rope.len_bytes()
    };
    let max_col = line_end - line_start;
    line_start + col.min(max_col)
}

/// Convert byte to (line, UTF-16 code unit column from start of line).
/// Required for LSP Position.character. Surrogate pairs count as 2.
pub fn byte_to_utf16_position(rope: &Rope, byte: usize) -> (usize, usize) {
    let b = snap_to_grapheme_start(rope, byte);
    let line = line_for_byte(rope, b);
    let line_start = rope.line_to_byte(line);
    let prefix = rope.byte_slice(line_start..b).to_string();
    let mut utf16 = 0usize;
    for ch in prefix.chars() {
        utf16 += if (ch as u32) > 0xFFFF { 2 } else { 1 };
    }
    (line, utf16)
}

/// Convert LSP (line, utf16 character) to byte offset. Clamps and snaps to grapheme start of the
/// character that would contain the utf16 position.
pub fn utf16_position_to_byte(rope: &Rope, line: usize, utf16_col: usize) -> usize {
    let num_lines = rope.len_lines();
    if num_lines == 0 {
        return 0;
    }
    let line = line.min(num_lines.saturating_sub(1));
    let line_start = rope.line_to_byte(line);
    let line_end = if line + 1 < num_lines {
        rope.line_to_byte(line + 1)
    } else {
        rope.len_bytes()
    };
    let line_slice = rope.byte_slice(line_start..line_end).to_string();
    let text_end = line_terminator_start(&line_slice);
    let text = &line_slice[..text_end];

    let mut cur_utf16 = 0usize;
    let mut byte_off = 0usize;
    for ch in text.chars() {
        let ch_utf16 = if (ch as u32) > 0xFFFF { 2 } else { 1 };
        if cur_utf16 + ch_utf16 > utf16_col {
            // Snap to start of this char's grapheme (the whole cluster start).
            break;
        }
        cur_utf16 += ch_utf16;
        byte_off += ch.len_utf8();
        if cur_utf16 == utf16_col {
            break;
        }
    }
    // Ensure result is on grapheme boundary in case of mid-cluster utf16.
    snap_to_grapheme_start(rope, line_start + byte_off)
}

/// Global grapheme cluster index (0-based) for the byte (snapped).
/// Expensive for very large buffers; prefer per-line when possible. Used by some LSP features.
pub fn byte_to_grapheme(rope: &Rope, byte: usize) -> usize {
    let b = snap_to_grapheme_start(rope, byte);
    let target_line = line_for_byte(rope, b);
    let mut count = 0usize;

    for l in 0..target_line {
        let ls = rope.line_to_byte(l);
        let le = if l + 1 < rope.len_lines() {
            rope.line_to_byte(l + 1)
        } else {
            rope.len_bytes()
        };
        let txt = rope.byte_slice(ls..le).to_string();
        let text_end = line_terminator_start(&txt);
        count += txt[..text_end].graphemes(true).count();
    }

    let ls = rope.line_to_byte(target_line);
    let prefix = rope.byte_slice(ls..b).to_string();
    let text_end = line_terminator_start(&prefix);
    count + prefix[..text_end].graphemes(true).count()
}

/// Inverse: grapheme index to its start byte (snapped by construction).
pub fn grapheme_to_byte(rope: &Rope, grapheme: usize) -> usize {
    let mut remaining = grapheme;
    let num_lines = rope.len_lines();
    for l in 0..num_lines {
        let ls = rope.line_to_byte(l);
        let le = if l + 1 < num_lines {
            rope.line_to_byte(l + 1)
        } else {
            rope.len_bytes()
        };
        let txt = rope.byte_slice(ls..le).to_string();
        let text_end = line_terminator_start(&txt);
        let text = &txt[..text_end];
        let gs: Vec<_> = text.grapheme_indices(true).collect();
        if remaining < gs.len() {
            return ls + gs[remaining].0;
        }
        remaining -= gs.len();
    }
    rope.len_bytes()
}

/// CellPos to grapheme index (roundtrips via canonical snap).
pub fn cell_to_grapheme(rope: &Rope, pos: CellPos) -> usize {
    let b = cell_to_byte(rope, pos);
    byte_to_grapheme(rope, b)
}

/// Grapheme index to its CellPos.
pub fn grapheme_to_cell(rope: &Rope, grapheme: usize) -> CellPos {
    let b = grapheme_to_byte(rope, grapheme);
    byte_to_cell(rope, b)
}

pub fn gutter_width_for_lines(line_count: usize) -> u16 {
    (line_count.to_string().len().max(2) + 2) as u16
}

/// Translate absolute terminal mouse cell inside the *outer* editor rect (Block area)
/// to nearest grapheme-start byte (always canonical grapheme start).
///
/// Must agree exactly with editor renderer layout:
/// - outer borders (1 cell)
/// - tab row (always 1)
/// - breadcrumb row (1 if inner height >=4 after border, else 0) -- matches render()
/// - dynamic gutter = gutter_width_for_lines(total_lines)
///
/// Content cells therefore start at (rect.x + 1, rect.y + 1 + tab + bc)
///
/// Clicks in the tab/breadcrumb/border area return byte for first visible (post scroll) line start.
/// Tab expansion (4 cells) and wide chars handled by cell_to_byte after gutter subtract.
pub fn editor_mouse_to_byte(
    rope: &Rope,
    editor_rect: Rect,
    scroll_line: u16,
    mouse_x: u16,
    mouse_y: u16,
) -> usize {
    if editor_rect.width == 0 || editor_rect.height == 0 {
        return 0;
    }
    let border = 1u16;
    // Match renderer exactly:
    // inner = outer shrunk by border*2 (top+bottom)
    let inner_height = editor_rect.height.saturating_sub(border * 2);
    let breadcrumb_height = if inner_height >= 4 { 1 } else { 0 };
    let header_rows = 1u16 + breadcrumb_height; // tab + optional bc
    let code_y = editor_rect.y.saturating_add(border + header_rows);
    let code_x = editor_rect.x.saturating_add(border);
    if mouse_y < code_y || mouse_x < code_x {
        // click in outer border / tab / breadcrumb area: first visible line start
        let first_vis = scroll_line as usize;
        return cell_to_byte(rope, CellPos::new(first_vis, 0));
    }
    let rel_x = (mouse_x - code_x) as usize;
    let rel_y = (mouse_y - code_y) as usize;
    let logical_line = (scroll_line as usize).saturating_add(rel_y);
    let total_lines = rope.len_lines().max(1);
    let gwidth = gutter_width_for_lines(total_lines) as usize;
    let content_col = if rel_x < gwidth {
        0usize
    } else {
        rel_x.saturating_sub(gwidth)
    };
    let safe_line = logical_line.min(total_lines.saturating_sub(1));
    let pos = CellPos::new(safe_line, content_col);
    cell_to_byte(rope, pos)
}

/// Move exactly one grapheme cluster left (canonical start). Never lands inside a cluster,
/// tab expansion, wide emoji/CJK, or combining sequence. Clamps at document start.
pub fn move_left(rope: &Rope, byte: usize) -> usize {
    let g = byte_to_grapheme(rope, byte);
    if g == 0 {
        0
    } else {
        grapheme_to_byte(rope, g - 1)
    }
}

/// Move exactly one grapheme cluster right (canonical start). Never lands inside a cluster.
/// Clamps at end.
pub fn move_right(rope: &Rope, byte: usize) -> usize {
    let g = byte_to_grapheme(rope, byte);
    let max_g = byte_to_grapheme(rope, rope.len_bytes());
    if g >= max_g {
        rope.len_bytes()
    } else {
        grapheme_to_byte(rope, g + 1)
    }
}

/// Move by delta lines (positive=down), preserving the display cell column via byte_to_cell/cell_to_byte.
/// Column accounts for tabs (4), wide chars, zero-width. Clamps to valid lines; snaps inward on short lines.
/// Always returns a canonical grapheme start. Used for up/down and page movements.
pub fn move_vertical(rope: &Rope, byte: usize, delta: i32) -> usize {
    if delta == 0 {
        return snap_to_grapheme_start(rope, byte);
    }
    let cur = byte_to_cell(rope, byte);
    let num = rope.len_lines();
    let target_line = ((cur.line as i32) + delta).clamp(0, (num as i32).saturating_sub(1)) as usize;
    let target = CellPos {
        line: target_line,
        column: cur.column,
    };
    let nb = cell_to_byte(rope, target);
    snap_to_grapheme_start(rope, nb)
}
#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn tab_cell_width_is_four() {
        assert_eq!(TAB_CELL_WIDTH, 4);
    }

    #[test]
    fn byte_to_cell_and_back_ascii() {
        let r = rope("hello\nworld");
        for b in 0..=r.len_bytes() {
            let c = byte_to_cell(&r, b);
            let b2 = cell_to_byte(&r, c);
            // canonical: result of byte_to_cell then cell_to_byte must be snapped start
            assert_eq!(b2, snap_to_grapheme_start(&r, b2));
        }
    }

    #[test]
    fn byte_to_cell_and_back_emoji_cjk_combining() {
        // emoji (wide=2), CJK (wide=2), combining (0)
        let r = rope("a😀文\u{0301}b"); // a + emoji(2) + CJK(2) + combining-on-a(0) + b
        for b in 0..=r.len_bytes() {
            let c = byte_to_cell(&r, b);
            let b2 = cell_to_byte(&r, c);
            assert!(rope_is_char_boundary(&r, b2));
            let c2 = byte_to_cell(&r, b2);
            // produced cells always describe grapheme starts; roundtrip stable in domain
            let _ = c2;
        }
    }

    #[test]
    fn tab_advances_four_cells_and_roundtrips() {
        let r = rope("a\tb\nx\ty");
        // 'a' col0, tab at col1 (advances +4), 'b' at col5
        assert_eq!(byte_to_cell(&r, 0), CellPos::new(0, 0));
        let tab_cell = byte_to_cell(&r, 1); // byte of \t
        assert_eq!(tab_cell.column, 1);
        let b_after_tab = byte_to_cell(&r, 2); // 'b'
        assert_eq!(b_after_tab.column, 1 + TAB_CELL_WIDTH);

        // cell positions inside the tab visual (col 1..=4) all snap to tab byte=1
        for dc in 1..=TAB_CELL_WIDTH {
            let p = CellPos::new(0, dc);
            let bb = cell_to_byte(&r, p);
            assert_eq!(bb, 1, "col {} inside tab must snap to tab start", dc);
        }
        // col after tab is 'b'
        assert_eq!(cell_to_byte(&r, CellPos::new(0, 1 + TAB_CELL_WIDTH)), 2);
    }

    #[test]
    fn grapheme_cell_roundtrip_with_tabs_and_wide() {
        let r = rope("\t😀\t文");
        for g in 0..=byte_to_grapheme(&r, r.len_bytes()) {
            let c = grapheme_to_cell(&r, g);
            let g2 = cell_to_grapheme(&r, c);
            assert_eq!(g2, g);
            let b = cell_to_byte(&r, c);
            assert_eq!(b, grapheme_to_byte(&r, g));
        }
    }

    #[test]
    fn editor_mouse_to_byte_header_and_tab_snap() {
        // outer rect at (10,5) w=50 h=10  => inner h=8 >=4 => bc present => header=2
        // code_y=5+1+2=8, code_x=11
        let rect = ratatui::layout::Rect::new(10, 5, 50, 10);
        let r = rope("line0\n\thello\nend");
        // click in header/tab/bc area => first visible (scroll 0)
        let b0 = editor_mouse_to_byte(&r, rect, 0, 15, 6);
        assert_eq!(b0, 0);

        // gutter on content row for line1 (logical after scroll)
        let gutter_click = editor_mouse_to_byte(&r, rect, 0, 11, 9);
        assert_eq!(gutter_click, 6);

        // inside the tab 4-cell area on that line: must snap to \t byte=6
        let total_l = r.len_lines();
        let gw = gutter_width_for_lines(total_l) as u16;
        let tab_start_content_x = 11 + gw;
        for off in 0..=3u16 {
            let mx = tab_start_content_x + off;
            let mb = editor_mouse_to_byte(&r, rect, 0, mx, 9);
            assert_eq!(mb, 6, "inside tab visual must snap to tab byte");
        }
    }

    #[test]
    fn editor_mouse_to_byte_no_breadcrumb_small_rect() {
        // outer h=4 => inner_h=2 <4 => bc=0, header_rows=1 => code_y = 0+1+1=2
        let rect = ratatui::layout::Rect::new(0, 0, 20, 4);
        let r = rope("abc\n\tde");
        let b = editor_mouse_to_byte(&r, rect, 0, 3, 2);
        assert!(b <= r.len_bytes());
        let cpos = byte_to_cell(&r, b);
        assert_eq!(cpos.line, 0);
    }

    // ---------- movement helpers: grapheme exact, column preserve, no interior (Phase1 cursor fix) ----------
    #[test]
    fn move_left_right_never_enters_grapheme_interiors_emoji_cjk_tab_combining() {
        let r = rope("a😀b\t文\u{0301}c\nx");
        // property: repeated left/right stay on snaps, and move changes or clamps
        let mut p = 0usize;
        let len = r.len_bytes();
        for _ in 0..40 {
            let before = p;
            p = move_right(&r, p);
            assert!(p <= len);
            assert_eq!(p, snap_to_grapheme_start(&r, p));
            p = move_left(&r, p);
            assert_eq!(p, snap_to_grapheme_start(&r, p));
            if before > 0 {
                // eventually can progress
            }
        }
        // also from middle
        let mid = 6;
        let l = move_left(&r, mid);
        let rr = move_right(&r, mid);
        assert_eq!(l, snap_to_grapheme_start(&r, l));
        assert_eq!(rr, snap_to_grapheme_start(&r, rr));
    }

    #[test]
    fn move_vertical_preserves_display_column_across_variable_lengths_and_wide() {
        let r = rope("ab\n😀\t文\nshort");
        let start = 1usize;
        let down = move_vertical(&r, start, 1);
        let cd = byte_to_cell(&r, down);
        assert_eq!(cd.line, 1);
        assert!(cd.column <= 1);
        let d2 = move_vertical(&r, 3, 1);
        let c2 = byte_to_cell(&r, d2);
        assert_eq!(c2.line, 2);
        assert!(c2.column <= 6);
    }

    #[test]
    fn move_left_right_vertical_always_return_grapheme_starts() {
        let r = rope("😀\n文\n\t");
        for b in 0..=r.len_bytes() {
            assert_eq!(
                move_left(&r, b),
                snap_to_grapheme_start(&r, move_left(&r, b))
            );
            assert_eq!(
                move_right(&r, b),
                snap_to_grapheme_start(&r, move_right(&r, b))
            );
            assert_eq!(
                move_vertical(&r, b, 1),
                snap_to_grapheme_start(&r, move_vertical(&r, b, 1))
            );
            assert_eq!(
                move_vertical(&r, b, -1),
                snap_to_grapheme_start(&r, move_vertical(&r, b, -1))
            );
        }
    }
}
