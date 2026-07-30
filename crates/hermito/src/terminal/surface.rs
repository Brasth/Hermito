use std::{collections::VecDeque, sync::Arc};

pub const MAX_TERMINAL_DIMENSION: u16 = 500;
pub const MAX_TERMINAL_CELLS: usize = 250_000;
pub const DEFAULT_SCROLLBACK_LINES: usize = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellAttrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellStyle {
    pub fg: TerminalColor,
    pub bg: TerminalColor,
    pub attrs: CellAttrs,
    pub hyperlink: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: CellStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: CellStyle::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalSurface {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
    scrollback: VecDeque<Vec<Cell>>,
    max_scrollback_lines: usize,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    title: String,
    truncated: bool,
}

impl TerminalSurface {
    pub fn new(width: u16, height: u16, max_scrollback_lines: usize) -> Self {
        let (width, height) = bounded_dimensions(width, height);
        Self {
            width,
            height,
            cells: vec![Cell::default(); usize::from(width) * usize::from(height)],
            scrollback: VecDeque::with_capacity(max_scrollback_lines.min(DEFAULT_SCROLLBACK_LINES)),
            max_scrollback_lines: max_scrollback_lines.min(DEFAULT_SCROLLBACK_LINES),
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            title: String::new(),
            truncated: false,
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }
    pub fn height(&self) -> u16 {
        self.height
    }
    pub fn cursor(&self) -> (u16, u16) {
        (self.cursor_row, self.cursor_col)
    }
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row >= self.height || col >= self.width {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.width) + usize::from(col))
    }

    pub fn line(&self, row: u16) -> Option<&[Cell]> {
        if row >= self.height {
            return None;
        }
        let start = usize::from(row) * usize::from(self.width);
        Some(&self.cells[start..start + usize::from(self.width)])
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        let (width, height) = bounded_dimensions(width, height);
        if width == self.width && height == self.height {
            return;
        }
        let mut replacement = vec![Cell::default(); usize::from(width) * usize::from(height)];
        let copy_rows = self.height.min(height);
        let copy_cols = self.width.min(width);
        for row in 0..copy_rows {
            let old_start = usize::from(row) * usize::from(self.width);
            let new_start = usize::from(row) * usize::from(width);
            replacement[new_start..new_start + usize::from(copy_cols)]
                .clone_from_slice(&self.cells[old_start..old_start + usize::from(copy_cols)]);
        }
        self.width = width;
        self.height = height;
        self.cells = replacement;
        self.cursor_row = self.cursor_row.min(height.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(width.saturating_sub(1));
    }

    pub(crate) fn put(&mut self, ch: char, style: &CellStyle) {
        if ch.is_control() {
            return;
        }
        if self.cursor_col >= self.width {
            self.newline();
        }
        let index =
            usize::from(self.cursor_row) * usize::from(self.width) + usize::from(self.cursor_col);
        if let Some(cell) = self.cells.get_mut(index) {
            cell.ch = ch;
            cell.style = style.clone();
        }
        self.cursor_col = self.cursor_col.saturating_add(1);
    }

    pub(crate) fn newline(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row + 1 < self.height {
            self.cursor_row += 1;
        } else {
            self.scroll_up();
        }
    }

    pub(crate) fn carriage_return(&mut self) {
        self.cursor_col = 0;
    }
    pub(crate) fn backspace(&mut self) {
        self.cursor_col = self.cursor_col.saturating_sub(1);
    }

    pub(crate) fn tab(&mut self) {
        let next = (self.cursor_col / 8).saturating_add(1).saturating_mul(8);
        self.cursor_col = next.min(self.width.saturating_sub(1));
    }

    pub(crate) fn move_cursor(&mut self, row_delta: i32, col_delta: i32) {
        self.cursor_row = (i32::from(self.cursor_row) + row_delta)
            .clamp(0, i32::from(self.height.saturating_sub(1))) as u16;
        self.cursor_col = (i32::from(self.cursor_col) + col_delta)
            .clamp(0, i32::from(self.width.saturating_sub(1))) as u16;
    }

    pub(crate) fn set_cursor(&mut self, row: u16, col: u16) {
        self.cursor_row = row.min(self.height.saturating_sub(1));
        self.cursor_col = col.min(self.width.saturating_sub(1));
    }

    pub(crate) fn clear_display(&mut self, mode: u16) {
        match mode {
            0 => {
                let start = usize::from(self.cursor_row) * usize::from(self.width)
                    + usize::from(self.cursor_col);
                self.cells[start..].fill(Cell::default());
            }
            1 => {
                let end = (usize::from(self.cursor_row) * usize::from(self.width)
                    + usize::from(self.cursor_col)
                    + 1)
                .min(self.cells.len());
                self.cells[..end].fill(Cell::default());
            }
            2 | 3 => self.cells.fill(Cell::default()),
            _ => {}
        }
    }

    pub(crate) fn clear_line(&mut self, mode: u16) {
        let start = usize::from(self.cursor_row) * usize::from(self.width);
        let cursor = start + usize::from(self.cursor_col);
        let end = start + usize::from(self.width);
        match mode {
            0 => self.cells[cursor..end].fill(Cell::default()),
            1 => self.cells[start..=cursor.min(end.saturating_sub(1))].fill(Cell::default()),
            2 => self.cells[start..end].fill(Cell::default()),
            _ => {}
        }
    }

    pub(crate) fn set_title(&mut self, title: &str) {
        self.title.clear();
        self.title.extend(title.chars().take(256));
    }

    pub(crate) fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
    }
    pub(crate) fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    fn scroll_up(&mut self) {
        let width = usize::from(self.width);
        if self.max_scrollback_lines > 0 {
            self.scrollback.push_back(self.cells[..width].to_vec());
            while self.scrollback.len() > self.max_scrollback_lines {
                self.scrollback.pop_front();
            }
        }
        self.cells.rotate_left(width);
        let start = self.cells.len().saturating_sub(width);
        self.cells[start..].fill(Cell::default());
    }
}

fn bounded_dimensions(width: u16, height: u16) -> (u16, u16) {
    let width = width.clamp(1, MAX_TERMINAL_DIMENSION);
    let height = height.clamp(1, MAX_TERMINAL_DIMENSION);
    if usize::from(width) * usize::from(height) <= MAX_TERMINAL_CELLS {
        (width, height)
    } else {
        (
            width,
            (MAX_TERMINAL_CELLS / usize::from(width)).max(1) as u16,
        )
    }
}
