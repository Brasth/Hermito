//! Syntax subsystem: tagged requests/results for incremental Tree-sitter
//! parsing and highlighting. Off-thread only. Single coalesced job for current doc.
//! Retains Tree + source per doc; subsequent use exact source-derived InputEdit + edited tree.
//! Stale results filtered by epoch + revision (never apply, but trigger latest retry).
//! Uses pinned grammar crates. Inert span values only.

pub mod highlight;
pub mod tree_sitter;

pub use highlight::{HighlightKind, HighlightSpan};
pub use tree_sitter::{compute_syntax, SyntaxRequest, SyntaxResult};
