use std::ops::Range;

/// A single contiguous text replacement expressed in byte offsets into the current rope.
/// start_byte..old_end_byte is removed then replacement inserted at start_byte.
/// Offsets must be valid char boundaries at application time (enforced by Buffer).
/// This is the canonical edit form for editor input, undo, and LSP textDocument/didChange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub start_byte: usize,
    pub old_end_byte: usize,
    pub replacement: String,
}

impl TextEdit {
    /// Construct an insert (no deletion).
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        let replacement = text.into();
        TextEdit {
            start_byte: at,
            old_end_byte: at,
            replacement,
        }
    }

    /// Construct a pure deletion.
    pub fn delete(range: Range<usize>) -> Self {
        TextEdit {
            start_byte: range.start,
            old_end_byte: range.end,
            replacement: String::new(),
        }
    }

    /// Construct a replacement of a range.
    pub fn replace(range: Range<usize>, text: impl Into<String>) -> Self {
        TextEdit {
            start_byte: range.start,
            old_end_byte: range.end,
            replacement: text.into(),
        }
    }

    /// Length delta in bytes after application (positive = growth).
    pub fn delta(&self) -> isize {
        self.replacement.len() as isize - (self.old_end_byte - self.start_byte) as isize
    }
}
