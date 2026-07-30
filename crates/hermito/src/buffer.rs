use crate::document::{BufferPathState, DocumentId, DocumentRevision, Language, WorkspaceEpoch};
use crate::edit::TextEdit;
use ropey::Rope;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Error returned when apply_edit is called with a revision that does not match the buffer's current revision.
/// Callers (input, undo, LSP) must re-query current revision and retry or discard the operation.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("stale revision")]
pub struct StaleRevision;

/// Immutable payload emitted on every successful mutation.
/// Sent to the journal worker for crash durability. Content is the full current text at that revision.
/// Includes epoch/language/path metadata required for exact recovery, ack stale-tagging, and durable serialization.
/// Only metadata + content needed for persistence are present; rope itself is not serialized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointPayload {
    pub doc_id: DocumentId,
    pub revision: DocumentRevision,
    pub content: String,
    pub language: Language,
    pub path: Option<PathBuf>,
    pub epoch: WorkspaceEpoch,
}

/// Ropey-backed document buffer.
/// - Stable DocumentId for the lifetime of the logical document (survives save-as, reload).
/// - Monotonic DocumentRevision (starts at 0, strictly increases on every content mutation).
/// - apply_edit performs exact stale-revision rejection; only exact match proceeds.
/// - Dirty flag tracks whether the current revision has been durably checkpointed.
/// - Path (via BufferPathState) is retained for recovered missing files so UI can show "Recovered · name"
///   and offer Save As using the original stem. No fs operations performed here.
/// - Cursor/selection/scroll metadata live in layout and App per open tab, never here. This keeps Buffer
///   usable for background LSP/syntax workers without display concepts.
/// - All operations are non-blocking and allocation-aware only where necessary (rope mutations and to_string
///   for the checkpoint payload).
pub struct Buffer {
    id: DocumentId,
    revision: DocumentRevision,
    rope: Rope,
    language: Language,
    path_state: BufferPathState,
    dirty: bool,
    last_checkpointed_revision: Option<DocumentRevision>,
}

impl Buffer {
    /// Create a new buffer. Revision starts at 0, not dirty, no checkpoint yet.
    /// For an untitled buffer pass BufferPathState::Untitled { suggested_name: "untitled".into() } after
    /// or use set_path_state. This ctor defaults to untitled.
    pub fn new(id: DocumentId, language: Language, initial_content: &str) -> Self {
        Buffer {
            id,
            revision: DocumentRevision(0),
            rope: Rope::from_str(initial_content),
            language,
            path_state: BufferPathState::Untitled {
                suggested_name: "untitled".to_owned(),
            },
            dirty: false,
            last_checkpointed_revision: None,
        }
    }

    pub fn restore_clean(
        id: DocumentId,
        language: Language,
        content: &str,
        revision: DocumentRevision,
        path_state: BufferPathState,
    ) -> Self {
        Self {
            id,
            revision,
            rope: Rope::from_str(content),
            language,
            path_state,
            dirty: false,
            last_checkpointed_revision: Some(revision),
        }
    }

    /// Reconstruct a buffer from a journal checkpoint payload.
    /// Revision and last_checkpointed are set to the durable revision.
    /// Buffer starts dirty (unsaved relative to any on-disk original) so that close still offers save.
    /// Path state is taken from payload or set afterwards via set_path_state for recovered case.
    pub fn from_checkpoint(payload: CheckpointPayload, path_state: BufferPathState) -> Self {
        let mut b = Buffer::new(payload.doc_id, payload.language, &payload.content);
        b.revision = payload.revision;
        b.last_checkpointed_revision = Some(payload.revision);
        b.dirty = true;
        b.path_state = path_state;
        b
    }

    /// Convenience for journal recovery when path state is already known (including Recovered variant).
    pub fn recover(
        id: DocumentId,
        language: Language,
        content: &str,
        revision: DocumentRevision,
        path_state: BufferPathState,
    ) -> Self {
        let payload = CheckpointPayload {
            doc_id: id,
            revision,
            content: content.to_owned(),
            language,
            path: path_state.backing_path().cloned(),
            epoch: WorkspaceEpoch(0),
        };
        Self::from_checkpoint(payload, path_state)
    }

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn revision(&self) -> DocumentRevision {
        self.revision
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn path(&self) -> Option<&Path> {
        self.path_state.backing_path().map(|p| p.as_path())
    }

    pub fn path_state(&self) -> &BufferPathState {
        &self.path_state
    }

    pub fn set_path_state(&mut self, state: BufferPathState) {
        self.path_state = state;
        // path change alone does not affect dirty/revision (content unchanged)
    }

    /// Update the logical path after Save As or first save. Does not clear dirty.
    pub fn set_path(&mut self, path: Option<PathBuf>) {
        self.path_state = match path {
            Some(p) => BufferPathState::Saved(p),
            None => BufferPathState::Untitled {
                suggested_name: "untitled".to_owned(),
            },
        };
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn last_checkpointed_revision(&self) -> Option<DocumentRevision> {
        self.last_checkpointed_revision
    }

    /// Called after a successful durable save acknowledgement for this exact revision.
    /// Idempotent: only affects state when the rev matches current.
    pub fn mark_clean(&mut self, at: DocumentRevision) {
        if at == self.revision {
            self.dirty = false;
            self.last_checkpointed_revision = Some(at);
        }
    }

    /// Record journal checkpoint ack for rev. Crash-recoverable only; never clears dirty
    /// (dirty cleared only on durable disk save). Idempotent on current rev match.
    pub fn record_last_checkpoint(&mut self, at: DocumentRevision) {
        if at == self.revision {
            self.last_checkpointed_revision = Some(at);
        }
    }

    /// Borrow the current rope for read-only use by syntax workers, coordinate calculations, rendering
    /// snapshots etc. Cloning the returned &Rope is cheap (internal refcounted nodes) when a full
    /// owned snapshot is required by a worker.
    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    /// Full text copy. Used for checkpoint payloads and worker requests that need owned data.
    /// Callers should prefer rope() when only & is required.
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Apply a text edit.
    ///
    /// Rejects with StaleRevision unless expected exactly equals current revision.
    /// On success:
    /// - rope is mutated
    /// - revision is incremented (monotonic, starts from 0)
    /// - dirty is set
    /// - returns the new revision together with an immutable CheckpointPayload containing the full
    ///   text at the new revision (for journal).
    ///
    /// Edit offsets must be char boundaries. If they are not, they are snapped to the nearest
    /// previous valid boundary (no silent data loss, no panic). Out-of-range offsets are clamped.
    /// This keeps the contract "exact" while remaining robust for coordinate-derived input.
    pub fn apply_edit(
        &mut self,
        expected: DocumentRevision,
        edit: TextEdit,
        epoch: WorkspaceEpoch,
    ) -> Result<(DocumentRevision, CheckpointPayload), StaleRevision> {
        if expected != self.revision {
            return Err(StaleRevision);
        }

        let len = self.rope.len_bytes();
        let mut start = edit.start_byte.min(len);
        let mut old_end = edit.old_end_byte.min(len).max(start);

        // Validate the canonical byte-domain edit, then convert to Ropey's char-index domain.
        // This conversion is required for multibyte UTF-8; passing bytes directly to Ropey
        // would interpret them as character indices and panic or edit the wrong text.
        if !is_char_boundary(&self.rope, start) {
            start = prev_char_boundary(&self.rope, start);
        }
        if !is_char_boundary(&self.rope, old_end) {
            old_end = prev_char_boundary(&self.rope, old_end).max(start);
        }
        let start_char = self.rope.byte_to_char(start);
        let old_end_char = self.rope.byte_to_char(old_end);

        self.rope.remove(start_char..old_end_char);
        self.rope.insert(start_char, &edit.replacement);

        self.revision = self.revision.increment();
        self.dirty = true;

        let payload = CheckpointPayload {
            doc_id: self.id,
            revision: self.revision,
            content: self.rope.to_string(),
            language: self.language,
            path: self.path().map(|p| p.to_path_buf()),
            epoch,
        };

        Ok((self.revision, payload))
    }

    /// Snapshot used by background tasks. Returns an owned cheap clone of the rope at current revision.
    /// Workers must also carry the revision (and epoch from caller) and re-validate on result.
    pub fn snapshot_rope(&self) -> Rope {
        self.rope.clone()
    }
}

/// Ropey-supported char boundary check: walk chunks (each a &str at char bndry) and delegate to str.
fn is_char_boundary(rope: &Rope, byte_idx: usize) -> bool {
    let len = rope.len_bytes();
    if byte_idx == 0 || byte_idx == len {
        return true;
    }
    if byte_idx > len {
        return false;
    }
    let mut offset = 0;
    for chunk in rope.chunks() {
        let clen = chunk.len();
        if byte_idx < offset + clen {
            let local = byte_idx - offset;
            return chunk.is_char_boundary(local);
        }
        offset += clen;
    }
    false
}

/// Find the greatest char boundary <= byte (inclusive of 0), using Ropey-supported is_char_boundary.
fn prev_char_boundary(rope: &Rope, mut byte: usize) -> usize {
    let len = rope.len_bytes();
    byte = byte.min(len);
    while byte > 0 && !is_char_boundary(rope, byte) {
        byte -= 1;
    }
    byte
}
