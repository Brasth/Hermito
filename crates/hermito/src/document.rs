use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Stable identifier for a document/buffer across renames, reloads and recovery.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct DocumentId(pub Uuid);

impl DocumentId {
    pub fn new() -> Self {
        DocumentId(Uuid::new_v4())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        DocumentId(uuid)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for DocumentId {
    fn default() -> Self {
        Self::new()
    }
}

/// Monotonically increasing revision for a single document. Starts at 0 for new or loaded content.
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default, Serialize, Deserialize,
)]
pub struct DocumentRevision(pub u64);

impl DocumentRevision {
    pub fn new(v: u64) -> Self {
        DocumentRevision(v)
    }

    pub fn increment(self) -> Self {
        DocumentRevision(self.0 + 1)
    }
}

/// Workspace/environment epoch. All async results (local or remote) are tagged with the epoch active at request time.
/// Stale results (mismatched epoch) are discarded.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceEpoch(pub u64);

impl WorkspaceEpoch {
    pub fn new(v: u64) -> Self {
        WorkspaceEpoch(v)
    }

    pub fn increment(self) -> Self {
        WorkspaceEpoch(self.0 + 1)
    }
}

/// Supported languages for syntax, LSP targeting and buffer classification.
/// Matches exactly the first-release set.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Go,
    Python,
    #[default]
    PlainText,
}

impl Language {
    /// Best-effort classification from file extension or name. Never panics; falls back to PlainText.
    pub fn from_path(path: &std::path::Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref()
        {
            Some("rs") => Language::Rust,
            Some("ts") | Some("mts") | Some("cts") => Language::TypeScript,
            Some("tsx") => Language::TypeScript,
            Some("js") | Some("mjs") | Some("cjs") => Language::JavaScript,
            Some("jsx") => Language::JavaScript,
            Some("go") => Language::Go,
            Some("py") | Some("pyi") => Language::Python,
            _ => Language::PlainText,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Go => "go",
            Language::Python => "python",
            Language::PlainText => "plaintext",
        }
    }
}

/// Path semantics for a buffer: supports untitled, saved, and recovered (journaled content whose original
/// on-disk file is now missing). The original path is retained for naming ("Recovered · foo.rs") and Save As
/// target discovery. No filesystem access here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BufferPathState {
    /// Normal on-disk file that may or may not be dirty.
    Saved(PathBuf),
    /// Never had a path (new buffer or Save As target not yet chosen).
    Untitled { suggested_name: String },
    /// Journal recovery of a dirty buffer whose file no longer exists on current authority.
    /// Content is authoritative; path kept only for UI label and as hint for Save As.
    Recovered { original: PathBuf },
}

impl BufferPathState {
    pub fn display_name(&self) -> String {
        match self {
            BufferPathState::Saved(p) => p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_owned(),
            BufferPathState::Untitled { suggested_name } => suggested_name.clone(),
            BufferPathState::Recovered { original } => {
                let name = original
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file");
                format!("Recovered · {}", name)
            }
        }
    }

    pub fn backing_path(&self) -> Option<&PathBuf> {
        match self {
            BufferPathState::Saved(p) | BufferPathState::Recovered { original: p } => Some(p),
            BufferPathState::Untitled { .. } => None,
        }
    }
}

/// Logical selection within a buffer. Stored as byte offsets (char boundaries, preferably grapheme starts).
/// Display (cell) coordinates are computed on demand via coordinate module and never stored here.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Selection {
    /// Byte offset of the anchor (start of selection or caret when empty).
    pub anchor: usize,
    /// Byte offset of the caret / active end.
    pub cursor: usize,
}

impl Selection {
    pub fn new(byte: usize) -> Self {
        Selection {
            anchor: byte,
            cursor: byte,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.cursor
    }

    /// Ordered (min, max) byte range.
    pub fn range(&self) -> (usize, usize) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }
}
