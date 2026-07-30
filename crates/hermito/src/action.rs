use crate::document::{DocumentId, DocumentRevision, WorkspaceEpoch};
use crate::edit::TextEdit;
use crate::layout::Landmark;

/// All high-level actions. Input maps raw crossterm to these. App applies (validating epoch/rev).
/// Trust grant ONLY via focused GrantTrust action (from modal Enter when grant focused).
#[derive(Clone, Debug)]
pub enum Action {
    // Landmark navigation (F6 / Shift+F6)
    CycleLandmarkForward,
    CycleLandmarkBackward,
    FocusLandmark(Landmark),

    // Within-landmark / modal (Tab / Shift+Tab)
    NextControl,
    PrevControl,
    ActivateFocused,

    // Direct tool activation (Alt+1..4)
    ActivateLeftTool(u8),

    // Palette (Ctrl/Cmd + K)
    OpenCommandPalette,
    PaletteInput(char),
    PaletteBackspace,

    // Editor (typing, paste, cursor, selection) - focus or direct mouse
    EditorInsert(char),
    EditorPaste(String),
    EditorDeleteBackward,
    EditorMoveCursor {
        line_delta: i32,
        col_delta: i32,
        extend_selection: bool,
    },
    EditorSetCursor {
        byte: usize,
    }, // from click
    EditorSetSelection {
        anchor: usize,
        cursor: usize,
    }, // from drag
    EditorPage {
        up: bool,
        extend: bool,
    },

    // Mouse
    MouseDown {
        x: u16,
        y: u16,
        button: u8,
    },
    MouseDrag {
        x: u16,
        y: u16,
    },
    MouseUp,
    Wheel {
        landmark: Landmark,
        lines: i32,
    },

    // Terminal
    TerminalResize {
        width: u16,
        height: u16,
    },
    Quit,

    // Authority / trust modal (sole grant path)
    ReviewTrust, // Enter on authority or CURRENT click
    GrantTrust,  // ONLY when modal grant button focused + Enter
    RevokeTrust, // immediate
    CancelModal, // Esc - restores invoker focus, no state change

    JournalAck {
        doc_id: DocumentId,
        revision: DocumentRevision,
        epoch: WorkspaceEpoch,
    },
    ApplySyntaxHighlights {
        doc_id: DocumentId,
        revision: DocumentRevision,
        spans: Vec<crate::syntax::highlight::HighlightSpan>,
        epoch: WorkspaceEpoch,
    },
    UpdateProjectState {
        tree: Option<crate::project::tree::ProjectTree>,
        epoch: WorkspaceEpoch,
    },

    // Project tree navigation + open (selection in PrimaryPane; Enter activates; read off-thread)
    ProjectMoveSelection {
        delta: i32,
    },
    ProjectActivateSelected,
    ProjectToggleDir {
        path: std::path::PathBuf,
    },
    RequestProjectFile {
        path: std::path::PathBuf,
    },
    ProjectFileLoaded {
        path: std::path::PathBuf,
        content: Option<String>,
        epoch: WorkspaceEpoch,
    },
    // Save: Ctrl/Cmd+S on Saved path does direct off-thread atomic write.
    // Untitled/Recovered opens Save As overlay (path entry).
    Save,
    // Keyboard-driven Save As overlay (typing, backspace, Enter confirm, Esc cancel).
    SaveAsOverlayInput(char),
    SaveAsOverlayBackspace,
    SaveAsOverlayConfirm,
    SaveAsOverlayCancel,
    // Result from off-thread durable atomic save (temp+fsync+rename). Only exact rev match clears dirty.
    SaveCompleted {
        doc_id: DocumentId,
        revision: DocumentRevision,
        path: std::path::PathBuf,
        success: bool,
        epoch: WorkspaceEpoch,
    },

    // Internal for apply_edit path from editor actions
    ApplyBufferEdit {
        doc_id: DocumentId,
        expected_rev: DocumentRevision,
        edit: TextEdit,
    },
}
