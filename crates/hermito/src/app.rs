use crate::action::Action;
use crate::buffer::{Buffer, CheckpointPayload};
use crate::document::{BufferPathState, DocumentId, DocumentRevision, Language, WorkspaceEpoch};
use crate::layout::{EditorTabState, Landmark, WorkbenchLayout};
use crate::persistence::journal::{JournalAck, JournalHandle, RecoveredBuffer, Recovery};
use tree_sitter::Tree;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrustLevel {
    Trusted,
    InspectOnly,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthorityKind {
    Local,
    Ssh,
    DevContainer,
}

#[derive(Clone, Debug)]
pub struct AuthorityState {
    pub kind: AuthorityKind,
    pub label: String,
    pub trust: TrustLevel,
}

#[derive(Clone, Debug)]
pub struct EditorTabSnapshot {
    pub id: DocumentId,
    pub revision: DocumentRevision,
    pub title: String,
    pub path_label: String,
    pub language: Language,
    pub text: String,
    pub dirty: bool,
    pub cursor_byte: usize,
    pub selection: Option<(usize, usize)>,
    pub scroll_line: u16,
    pub highlights: Vec<crate::syntax::highlight::HighlightSpan>,
}

#[derive(Clone, Debug)]
pub enum OverlaySnapshot {
    None,
    TrustReview {
        workspace_root: String,
        authority_label: String,
        trust: TrustLevel,
        capabilities: Vec<String>,
        focused_grant: bool,
        invoker: Landmark,
    },
    CommandPalette {
        query: String,
        items: Vec<String>,
        selected: usize,
        invoker: Landmark,
    },
    SaveAs {
        path: String,
        invoker: Landmark,
    },
}

#[derive(Clone, Debug, Default)]
pub struct ProjectTreeSnapshot {
    pub tree: Option<crate::project::tree::ProjectTree>,
    pub loading: bool,
    pub scroll: u16,
    pub selected_row: u16,
}

#[derive(Clone, Debug, Default)]
pub struct StatusSnapshot {
    pub view: String,
    pub branch: Option<String>,
    pub problems: usize,
    pub service: String,
    pub message: Option<String>,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug)]
pub struct AppSnapshot {
    pub epoch: WorkspaceEpoch,
    pub layout: WorkbenchLayout,
    pub focus: Landmark,
    pub overlay: OverlaySnapshot,
    pub authorities: Vec<AuthorityState>,
    pub current_authority_idx: usize,
    pub current_trust: TrustLevel,
    pub current_buffer: Option<EditorTabSnapshot>,
    pub open_editor_tabs: Vec<EditorTabSnapshot>,
    pub active_editor_tab: usize,
    pub project: ProjectTreeSnapshot,
    pub status: StatusSnapshot,
    pub journal_lagging: bool,
    pub workspace_root: String,
    pub workspace_name: String,
}

pub struct App {
    pub epoch: WorkspaceEpoch,
    pub layout: WorkbenchLayout,
    pub buffers: Vec<Buffer>,
    pub current_doc: Option<DocumentId>,
    pub authorities: Vec<AuthorityState>,
    pub current_authority: usize,
    pub focus: Landmark,
    pending_checkpoints: std::collections::HashMap<DocumentId, CheckpointPayload>,
    pending_compactions: std::collections::HashMap<DocumentId, DocumentRevision>,
    pub(crate) overlay: Overlay,
    journal: Option<JournalHandle>,
    syntax_highlights: std::collections::HashMap<
        DocumentId,
        (
            DocumentRevision,
            Vec<crate::syntax::highlight::HighlightSpan>,
        ),
    >,
    retained_syntax:
        std::collections::HashMap<DocumentId, (DocumentRevision, Option<Tree>, String)>,
    project: ProjectTreeSnapshot,
    status_message: String,
    workspace_root: String,
    workspace_name: String,
}
pub(crate) enum Overlay {
    None,
    TrustReview {
        focused_grant: bool,
        invoker: Landmark,
        workspace_root: String,
        authority_label: String,
    },
    CommandPalette {
        query: String,
        items: Vec<String>,
        selected: usize,
        invoker: Landmark,
    },
    SaveAs {
        path: String,
        invoker: Landmark,
        doc_id: DocumentId,
    },
}

fn command_palette_items(trust: TrustLevel, query: &str) -> Vec<String> {
    let mut items = vec!["Review authority trust…".to_string()];
    if trust == TrustLevel::Trusted {
        items.push("Revoke authority trust".to_string());
    }
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        items
    } else {
        items
            .into_iter()
            .filter(|item| item.to_lowercase().contains(&query))
            .collect()
    }
}

impl App {
    /// Construct from startup recovery (journal first, before any authority use).
    pub fn new_from_recovery(recovery: Recovery, journal: JournalHandle) -> Self {
        let epoch = WorkspaceEpoch(0);
        let mut buffers = Vec::new();
        for RecoveredBuffer {
            id,
            revision,
            content,
            path,
            language,
        } in recovery.buffers
        {
            let path_state = match path {
                Some(p) => BufferPathState::Recovered { original: p },
                None => BufferPathState::Untitled {
                    suggested_name: "recovered".into(),
                },
            };
            let b = Buffer::recover(id, language, &content, revision, path_state);
            buffers.push(b);
        }
        if buffers.is_empty() {
            let id = DocumentId::new();
            let buffer = Buffer::new(id, crate::document::Language::PlainText, "fn main() {}\n");
            buffers.push(buffer);
        }
        let current_doc = buffers.first().map(|b| b.id());
        let mut layout = WorkbenchLayout::default();
        if let Some(id) = current_doc {
            layout.open_or_focus_editor(id);
        }
        App {
            epoch,
            layout,
            buffers,
            current_doc,
            authorities: vec![AuthorityState {
                kind: AuthorityKind::Local,
                label: "host".into(),
                trust: TrustLevel::InspectOnly,
            }],
            current_authority: 0,
            focus: Landmark::Editor,
            overlay: Overlay::None,
            journal: Some(journal),
            pending_checkpoints: std::collections::HashMap::new(),
            pending_compactions: std::collections::HashMap::new(),
            syntax_highlights: std::collections::HashMap::new(),
            retained_syntax: std::collections::HashMap::new(),
            project: ProjectTreeSnapshot {
                tree: None,
                loading: true,
                scroll: 0,
                selected_row: 0,
            },
            status_message: String::new(),
            workspace_root: "/workspace".into(),
            workspace_name: "workspace".into(),
        }
    }

    pub fn apply_action(&mut self, action: Action) {
        match action {
            Action::Quit => {}
            Action::CycleLandmarkForward => {
                self.focus = self.focus.next(self.layout.context_visible);
            }
            Action::CycleLandmarkBackward => {
                self.focus = self.focus.prev(self.layout.context_visible);
            }
            Action::FocusLandmark(l) => {
                self.focus = l;
            }
            Action::NextControl => {
                if let Overlay::TrustReview { focused_grant, .. } = &mut self.overlay {
                    *focused_grant = !*focused_grant;
                } else if let Overlay::CommandPalette {
                    selected, items, ..
                } = &mut self.overlay
                {
                    if !items.is_empty() {
                        *selected = (*selected + 1) % items.len();
                    }
                }
            }
            Action::PrevControl => {
                if let Overlay::TrustReview { focused_grant, .. } = &mut self.overlay {
                    *focused_grant = !*focused_grant;
                } else if let Overlay::CommandPalette {
                    selected, items, ..
                } = &mut self.overlay
                {
                    if !items.is_empty() {
                        *selected = if *selected == 0 {
                            items.len() - 1
                        } else {
                            *selected - 1
                        };
                    }
                }
            }
            Action::ActivateFocused => {
                if let Overlay::CommandPalette {
                    selected, items, ..
                } = &self.overlay
                {
                    if let Some(item) = items.get(*selected) {
                        let item = item.clone();
                        self.overlay = Overlay::None;
                        if item == "Review authority trust…" {
                            self.apply_action(Action::ReviewTrust);
                        } else if item == "Revoke authority trust" {
                            self.apply_action(Action::RevokeTrust);
                        }
                        return;
                    }
                }
                self.overlay = Overlay::None;
            }
            Action::ActivateLeftTool(n) => {
                self.layout.primary_visible = true;
                self.layout
                    .set_active_tab(crate::layout::Pane::Primary, (n.saturating_sub(1)) as usize);
                self.focus = Landmark::PrimaryPane;
            }
            Action::OpenCommandPalette => {
                self.overlay = Overlay::CommandPalette {
                    query: String::new(),
                    items: command_palette_items(self.current_trust(), ""),
                    selected: 0,
                    invoker: self.focus,
                };
            }
            Action::PaletteInput(character) => {
                let trust = self.current_trust();
                if let Overlay::CommandPalette {
                    query,
                    items,
                    selected,
                    ..
                } = &mut self.overlay
                {
                    query.push(character);
                    *items = command_palette_items(trust, query);
                    *selected = 0;
                }
            }
            Action::PaletteBackspace => {
                let trust = self.current_trust();
                if let Overlay::CommandPalette {
                    query,
                    items,
                    selected,
                    ..
                } = &mut self.overlay
                {
                    query.pop();
                    *items = command_palette_items(trust, query);
                    *selected = 0;
                }
            }
            Action::MouseDown { x, y, button: _ } => {
                let ar = self.layout.rect_authority();
                if x >= ar.x && x < ar.x + ar.width && y >= ar.y && y < ar.y + ar.height {
                    self.apply_action(Action::ReviewTrust);
                }
                // Editor mouse translated upstream in input/mouse.rs -> EditorSetCursor / EditorSetSelection
                // using coordinate::editor_mouse_to_byte (Rect + gutter + scroll + Rope -> CellPos -> cell_to_byte)
            }
            Action::MouseDrag { x: _, y: _ } => {
                // pre-mapped via coordinate in mouse layer
            }
            Action::MouseUp => {}
            Action::Wheel { landmark, lines } => {
                if landmark == Landmark::Editor {
                    if let Some(t) = self.layout.current_editor_mut() {
                        t.scroll_line = ((t.scroll_line as i32) + lines).max(0) as u16;
                    }
                } else if landmark == Landmark::PrimaryPane {
                    self.project.scroll = ((self.project.scroll as i32) + lines).max(0) as u16;
                }
            }
            Action::TerminalResize { width, height } => {
                self.layout.resize(width, height);
            }
            Action::ReviewTrust => {
                let label = self
                    .authorities
                    .get(self.current_authority)
                    .map(|a| a.label.clone())
                    .unwrap_or_default();
                self.overlay = Overlay::TrustReview {
                    focused_grant: false,
                    invoker: self.focus,
                    workspace_root: self.workspace_root.clone(),
                    authority_label: label,
                };
                self.focus = Landmark::Authority;
            }
            Action::GrantTrust => {
                // SOLE GRANT PATH: only when the modal is TrustReview and grant is focused.
                let invoker = match &self.overlay {
                    Overlay::TrustReview {
                        focused_grant: true,
                        invoker,
                        ..
                    } => Some(*invoker),
                    _ => None,
                };
                if let Some(invoker) = invoker {
                    if let Some(authority) = self.authorities.get_mut(self.current_authority) {
                        authority.trust = TrustLevel::Trusted;
                    }
                    self.focus = invoker;
                    self.overlay = Overlay::None;
                    self.status_message = "Execution granted for current authority.".into();
                }
            }
            Action::RevokeTrust => {
                let invoker = match &self.overlay {
                    Overlay::TrustReview { invoker, .. }
                    | Overlay::CommandPalette { invoker, .. }
                    | Overlay::SaveAs { invoker, .. } => Some(*invoker),
                    Overlay::None => None,
                };
                if let Some(authority) = self.authorities.get_mut(self.current_authority) {
                    authority.trust = TrustLevel::InspectOnly;
                }
                if let Some(invoker) = invoker {
                    self.focus = invoker;
                }
                self.overlay = Overlay::None;
                self.status_message = "Execution revoked. Now INSPECT ONLY.".into();
            }
            Action::CancelModal => {
                if let Overlay::TrustReview { invoker, .. } = &self.overlay {
                    self.focus = *invoker;
                } else if let Overlay::CommandPalette { invoker, .. } = &self.overlay {
                    self.focus = *invoker;
                } else if let Overlay::SaveAs { invoker, .. } = &self.overlay {
                    self.focus = *invoker;
                }
                self.overlay = Overlay::None;
            }
            Action::JournalAck {
                doc_id,
                revision,
                epoch,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                    if buf.revision() == revision {
                        buf.record_last_checkpoint(revision);
                    }
                }
                // retain newer pending (N+1) when ack arrives for older N
                if let Some(p) = self.pending_checkpoints.get(&doc_id) {
                    if p.revision <= revision {
                        self.pending_checkpoints.remove(&doc_id);
                    }
                }
            }
            Action::ApplySyntaxHighlights {
                doc_id,
                revision,
                spans,
                epoch,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                    if buf.revision() == revision {
                        self.syntax_highlights.insert(doc_id, (revision, spans));
                    }
                }
            }
            Action::UpdateProjectState { tree, epoch } => {
                if epoch != self.epoch {
                    return;
                }
                self.project.tree = tree;
                self.project.loading = false;
                if let Some(t) = &self.project.tree {
                    let n = t.visible_entry_count();
                    if n > 0 && (self.project.selected_row as usize) >= n {
                        self.project.selected_row = (n - 1) as u16;
                    }
                }
            }
            Action::ProjectMoveSelection { delta } => {
                if let Some(t) = &self.project.tree {
                    let n = t.visible_entry_count();
                    if n > 0 {
                        let mut r = self.project.selected_row as i32 + delta;
                        if r < 0 {
                            r = 0;
                        }
                        if r >= n as i32 {
                            r = (n - 1) as i32;
                        }
                        self.project.selected_row = r as u16;
                    }
                }
            }
            Action::ProjectActivateSelected => {
                // Selection change only; actual open triggered via RequestProjectFile from keyboard
                // (to keep request/result boundary explicit and spawn off-thread).
                if self.focus != Landmark::PrimaryPane {
                    self.focus = Landmark::PrimaryPane;
                }
            }
            Action::ProjectToggleDir { path } => {
                if let Some(t) = &mut self.project.tree {
                    // capture logical selection by name path (numeric row shifts when visible rows rebuild)
                    let prev_names = t.entry_path_at_row(self.project.selected_row as usize);
                    if t.toggle_path(&path) {
                        let mut new_row = None;
                        if let Some(names) = &prev_names {
                            let segs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                            new_row = t.row_for_entry_path(&segs);
                        }
                        if new_row.is_none() {
                            // fallback select the dir we toggled (if selection was inside subtree now hidden)
                            if let Ok(rel) = path.strip_prefix(&t.root) {
                                let tnames: Vec<String> = rel
                                    .components()
                                    .filter_map(|c| {
                                        if let std::path::Component::Normal(s) = c {
                                            Some(s.to_string_lossy().into_owned())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                let tsegs: Vec<&str> = tnames.iter().map(|s| s.as_str()).collect();
                                new_row = t.row_for_entry_path(&tsegs);
                            }
                        }
                        if let Some(r) = new_row {
                            self.project.selected_row = r as u16;
                        } else {
                            let n = t.visible_entry_count();
                            if n > 0 && (self.project.selected_row as usize) >= n {
                                self.project.selected_row = (n - 1) as u16;
                            }
                        }
                    }
                }
            }
            Action::RequestProjectFile { path } => {
                self.status_message = format!("Opening {}…", path.to_string_lossy());
                // Spawn of off-thread read + result action happens in event_loop plumbing.
            }
            Action::ProjectFileLoaded {
                path,
                content,
                epoch,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(c) = content {
                    // canonical/safe path match: focus existing buffer (recovered or prior open) rather than dup
                    if let Some(existing) = self
                        .buffers
                        .iter()
                        .find(|buffer| buffer.path() == Some(path.as_path()))
                    {
                        let id = existing.id();
                        self.layout.open_or_focus_editor(id);
                        self.current_doc = Some(id);
                        self.focus = Landmark::Editor;
                        self.status_message = format!("Focused {}", path.to_string_lossy());
                    } else {
                        let id = DocumentId::new();
                        let language = crate::document::Language::from_path(&path);
                        let buffer = Buffer::restore_clean(
                            id,
                            language,
                            &c,
                            DocumentRevision(0),
                            BufferPathState::Saved(path.clone()),
                        );
                        self.buffers.push(buffer);
                        self.layout.open_or_focus_editor(id);
                        self.current_doc = Some(id);
                        self.focus = Landmark::Editor;
                        self.status_message = format!("Opened {}", path.to_string_lossy());
                    }
                } else {
                    self.status_message = format!("Failed to open {}", path.to_string_lossy());
                }
            }
            Action::Save => {
                if let Some(doc_id) = self.current_doc {
                    let has_saved_path = self.buffers.iter().any(|b| {
                        b.id() == doc_id && matches!(b.path_state(), BufferPathState::Saved(_))
                    });
                    if !has_saved_path {
                        // Untitled or Recovered: open path-entry overlay (never silent overwrite of old path)
                        self.overlay = Overlay::SaveAs {
                            path: String::new(),
                            invoker: self.focus,
                            doc_id,
                        };
                        self.status_message =
                            "Save As: type path, Enter to save, Esc to cancel".to_string();
                    } else {
                        self.status_message = "Saving…".to_string();
                    }
                }
            }
            Action::SaveAsOverlayInput(c) => {
                if let Overlay::SaveAs { path, .. } = &mut self.overlay {
                    path.push(c);
                }
            }
            Action::SaveAsOverlayBackspace => {
                if let Overlay::SaveAs { path, .. } = &mut self.overlay {
                    path.pop();
                }
            }
            Action::SaveAsOverlayConfirm => {
                // spawn of off-thread write happens in event_loop before this apply
                if let Overlay::SaveAs { invoker, .. } = &self.overlay {
                    self.focus = *invoker;
                }
                self.overlay = Overlay::None;
            }
            Action::SaveAsOverlayCancel => {
                if let Overlay::SaveAs { invoker, .. } = &self.overlay {
                    self.focus = *invoker;
                }
                self.overlay = Overlay::None;
                self.status_message = "Save cancelled".to_string();
            }
            Action::SaveCompleted {
                doc_id,
                revision,
                path,
                success,
                epoch,
            } => {
                if epoch != self.epoch {
                    return;
                }
                if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                    if success && buf.revision() == revision {
                        buf.mark_clean(revision);
                        buf.set_path_state(BufferPathState::Saved(path.clone()));
                        self.status_message = format!("Saved {}", path.to_string_lossy());
                        // schedule compaction (try; retain on backpressure for lossless)
                        if let Some(j) = &self.journal {
                            if j.try_ack_save(doc_id, revision).is_err() {
                                self.retain_pending_compact(doc_id, revision);
                            }
                        }
                    } else if !success {
                        self.status_message = format!("Save failed: {}", path.to_string_lossy());
                        // dirty + path unchanged
                    } else {
                        self.status_message =
                            "Save stale (concurrent edit); left dirty".to_string();
                        // dirty + path unchanged
                    }
                }
            }
            Action::ApplyBufferEdit {
                doc_id,
                expected_rev,
                edit,
            } => {
                if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                    if let Ok((_r, payload)) = buf.apply_edit(expected_rev, edit, self.epoch) {
                        self.try_checkpoint(doc_id, payload);
                        // syntax dispatch happens in event loop after apply (tagged result)
                    }
                }
            }
            Action::EditorInsert(c) => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rev = buf.revision();
                        let cur = self
                            .layout
                            .current_editor()
                            .map(|t| t.cursor_byte)
                            .unwrap_or(0);
                        let edit = crate::edit::TextEdit::insert(cur, c.to_string());
                        if let Ok((_nr, payload)) = buf.apply_edit(rev, edit, self.epoch) {
                            self.layout.set_editor_cursor(cur + c.len_utf8());
                            self.try_checkpoint(doc_id, payload);
                        }
                    }
                }
            }
            Action::EditorPaste(text) => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rev = buf.revision();
                        let cur = self
                            .layout
                            .current_editor()
                            .map(|t| t.cursor_byte)
                            .unwrap_or(0);
                        let edit = crate::edit::TextEdit::insert(cur, text.clone());
                        if let Ok((_nr, payload)) = buf.apply_edit(rev, edit, self.epoch) {
                            self.layout.set_editor_cursor(cur + text.len());
                            self.try_checkpoint(doc_id, payload);
                        }
                    }
                }
            }
            Action::EditorDeleteBackward => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rope = buf.rope();
                        let rev = buf.revision();
                        if let Some(tab) = self.layout.current_editor() {
                            let (del_start, del_end) = if let Some(anchor) = tab.selection_anchor {
                                let a = crate::coordinate::snap_to_grapheme_start(rope, anchor);
                                let c = crate::coordinate::snap_to_grapheme_start(
                                    rope,
                                    tab.cursor_byte,
                                );
                                if a != c {
                                    (a.min(c), a.max(c))
                                } else {
                                    let cur = crate::coordinate::snap_to_grapheme_start(
                                        rope,
                                        tab.cursor_byte,
                                    );
                                    let prev = crate::coordinate::move_left(rope, cur);
                                    (prev, cur)
                                }
                            } else {
                                let cur = crate::coordinate::snap_to_grapheme_start(
                                    rope,
                                    tab.cursor_byte,
                                );
                                let prev = crate::coordinate::move_left(rope, cur);
                                (prev, cur)
                            };
                            if del_start != del_end {
                                let edit = crate::edit::TextEdit::delete(del_start..del_end);
                                if let Ok((_nr, payload)) = buf.apply_edit(rev, edit, self.epoch) {
                                    self.layout.set_editor_cursor(del_start);
                                    if let Some(t) = self.layout.current_editor_mut() {
                                        t.selection_anchor = None;
                                    }
                                    self.try_checkpoint(doc_id, payload);
                                }
                            }
                        }
                    }
                }
            }
            Action::EditorMoveCursor {
                line_delta,
                col_delta,
                extend_selection,
            } => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rope = buf.rope();
                        let cur = self
                            .layout
                            .current_editor()
                            .map(|t| t.cursor_byte)
                            .unwrap_or(0);
                        let cur = crate::coordinate::snap_to_grapheme_start(rope, cur);
                        let nb = if line_delta == 0 && col_delta != 0 {
                            let mut p = cur;
                            let steps = col_delta.unsigned_abs() as usize;
                            for _ in 0..steps {
                                p = if col_delta > 0 {
                                    crate::coordinate::move_right(rope, p)
                                } else {
                                    crate::coordinate::move_left(rope, p)
                                };
                            }
                            p
                        } else {
                            crate::coordinate::move_vertical(rope, cur, line_delta)
                        };
                        let nb = crate::coordinate::snap_to_grapheme_start(rope, nb);
                        self.layout.extend_or_move_cursor(nb, extend_selection);
                        // normalize selection anchor if present
                        if let Some(t) = self.layout.current_editor_mut() {
                            if let Some(a) = t.selection_anchor {
                                t.selection_anchor =
                                    Some(crate::coordinate::snap_to_grapheme_start(rope, a));
                            }
                        }
                    }
                }
            }
            Action::EditorSetCursor { byte } => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter().find(|b| b.id() == doc_id) {
                        let snapped = crate::coordinate::snap_to_grapheme_start(buf.rope(), byte);
                        self.layout.set_editor_cursor(snapped);
                    } else {
                        self.layout.set_editor_cursor(byte);
                    }
                } else {
                    self.layout.set_editor_cursor(byte);
                }
                self.focus = Landmark::Editor;
            }
            Action::EditorSetSelection { anchor, cursor } => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter().find(|b| b.id() == doc_id) {
                        let rope = buf.rope();
                        let a = crate::coordinate::snap_to_grapheme_start(rope, anchor);
                        let c = crate::coordinate::snap_to_grapheme_start(rope, cursor);
                        self.layout.set_editor_selection(a, c);
                    } else {
                        self.layout.set_editor_selection(anchor, cursor);
                    }
                } else {
                    self.layout.set_editor_selection(anchor, cursor);
                }
            }
            Action::EditorPage { up, extend } => {
                if let Some(doc_id) = self.current_doc {
                    if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == doc_id) {
                        let rope = buf.rope();
                        let cur = self
                            .layout
                            .current_editor()
                            .map(|t| t.cursor_byte)
                            .unwrap_or(0);
                        let cur = crate::coordinate::snap_to_grapheme_start(rope, cur);
                        let h = self.layout.rect_editor().height as i32;
                        let page = if h > 2 { h - 2 } else { 1 };
                        let d = if up { -page } else { page };
                        let nb = crate::coordinate::move_vertical(rope, cur, d);
                        let nb = crate::coordinate::snap_to_grapheme_start(rope, nb);
                        self.layout.extend_or_move_cursor(nb, extend);
                        if let Some(t) = self.layout.current_editor_mut() {
                            if let Some(a) = t.selection_anchor {
                                t.selection_anchor =
                                    Some(crate::coordinate::snap_to_grapheme_start(rope, a));
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn journal_handle_mut(&mut self) -> Option<&mut JournalHandle> {
        self.journal.as_mut()
    }

    fn try_checkpoint(&mut self, _doc_id: DocumentId, payload: CheckpointPayload) {
        if let Some(journal) = &self.journal {
            if journal.try_checkpoint(payload.clone()).is_err() {
                self.retain_pending_checkpoint(payload);
            }
        }
    }

    pub fn retain_pending_checkpoint(&mut self, payload: CheckpointPayload) {
        self.pending_checkpoints
            .entry(payload.doc_id)
            .and_modify(|pending| {
                if payload.revision > pending.revision {
                    *pending = payload.clone();
                }
            })
            .or_insert(payload);
    }

    pub fn pending_checkpoint_revision(&self, doc_id: DocumentId) -> Option<DocumentRevision> {
        self.pending_checkpoints
            .get(&doc_id)
            .map(|payload| payload.revision)
    }

    pub fn apply_journal_ack(&mut self, ack: JournalAck) {
        if ack.epoch != self.epoch {
            return;
        }
        if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == ack.doc_id) {
            if buf.revision() == ack.revision {
                buf.record_last_checkpoint(ack.revision);
            }
        }
        // retain newer pending checkpoint N+1 when ack for older N arrives
        if let Some(p) = self.pending_checkpoints.get(&ack.doc_id) {
            if p.revision <= ack.revision {
                self.pending_checkpoints.remove(&ack.doc_id);
            }
        }
    }

    pub fn retry_pending_checkpoints(&mut self) {
        if let Some(j) = &self.journal {
            let mut still = std::collections::HashMap::new();
            for (id, p) in self.pending_checkpoints.drain() {
                if j.try_checkpoint(p.clone()).is_err() {
                    still.insert(id, p);
                }
            }
            self.pending_checkpoints = still;
        }
    }

    pub fn retain_pending_compact(&mut self, id: DocumentId, rev: DocumentRevision) {
        self.pending_compactions
            .entry(id)
            .and_modify(|r| {
                if rev > *r {
                    *r = rev;
                }
            })
            .or_insert(rev);
    }

    pub fn retry_pending_compacts(&mut self) {
        if let Some(j) = &self.journal {
            let mut still = std::collections::HashMap::new();
            for (id, rev) in self.pending_compactions.drain() {
                if j.try_ack_save(id, rev).is_err() {
                    still.insert(id, rev);
                }
            }
            self.pending_compactions = still;
        }
    }

    /// Blocking submit of *all* retained latest checkpoints + pending compacts.
    /// Called before flush on every shutdown/panic return path while App owned.
    /// Blocking send allowed only here.
    pub fn submit_all_retained_blocking(&mut self) {
        if let Some(j) = &self.journal {
            for (_, p) in self.pending_checkpoints.drain() {
                j.submit_checkpoint_blocking(p);
            }
            for (id, rev) in self.pending_compactions.drain() {
                j.submit_ack_blocking(id, rev);
            }
        }
    }

    pub fn snapshot(&self) -> AppSnapshot {
        let open_tabs: Vec<EditorTabSnapshot> = self
            .layout
            .editor_tabs
            .iter()
            .filter_map(|tab| {
                self.buffers
                    .iter()
                    .find(|b| b.id() == tab.doc_id)
                    .map(|buf| {
                        let (hl_rev, hls) = self
                            .syntax_highlights
                            .get(&tab.doc_id)
                            .cloned()
                            .unwrap_or((buf.revision(), vec![]));
                        let sel = tab.selection_anchor.map(|a| (a, tab.cursor_byte));
                        let ps = buf.path_state();
                        let title = ps.display_name();
                        let path_label = match ps {
                            BufferPathState::Saved(p) => p.to_string_lossy().into_owned(),
                            BufferPathState::Recovered { original } => {
                                format!("Recovered · {}", original.to_string_lossy())
                            }
                            BufferPathState::Untitled { suggested_name } => {
                                format!("<{}>", suggested_name)
                            }
                        };
                        EditorTabSnapshot {
                            id: tab.doc_id,
                            revision: buf.revision(),
                            title,
                            path_label,
                            language: buf.language(),
                            text: buf.text(),
                            dirty: buf.is_dirty(),
                            cursor_byte: tab.cursor_byte,
                            selection: sel,
                            scroll_line: tab.scroll_line,
                            highlights: if hl_rev == buf.revision() {
                                hls
                            } else {
                                vec![]
                            },
                        }
                    })
            })
            .collect();
        let current_buffer = self
            .current_doc
            .and_then(|id| open_tabs.iter().find(|t| t.id == id).cloned());
        let cur_trust = self
            .authorities
            .get(self.current_authority)
            .map(|a| a.trust)
            .unwrap_or(TrustLevel::InspectOnly);
        let overlay = match &self.overlay {
            Overlay::None => OverlaySnapshot::None,
            Overlay::TrustReview {
                focused_grant,
                invoker,
                workspace_root,
                authority_label,
            } => OverlaySnapshot::TrustReview {
                workspace_root: workspace_root.clone(),
                authority_label: authority_label.clone(),
                trust: cur_trust,
                capabilities: vec!["execution".into(), "git".into(), "terminal".into()],
                focused_grant: *focused_grant,
                invoker: *invoker,
            },
            Overlay::CommandPalette {
                query,
                items,
                selected,
                invoker,
            } => OverlaySnapshot::CommandPalette {
                query: query.clone(),
                items: items.clone(),
                selected: *selected,
                invoker: *invoker,
            },
            Overlay::SaveAs { path, invoker, .. } => OverlaySnapshot::SaveAs {
                path: path.clone(),
                invoker: *invoker,
            },
        };
        AppSnapshot {
            epoch: self.epoch,
            layout: self.layout.clone(),
            focus: self.focus,
            overlay,
            authorities: self.authorities.clone(),
            current_authority_idx: self.current_authority,
            current_trust: cur_trust,
            current_buffer,
            open_editor_tabs: open_tabs,
            active_editor_tab: self.layout.active_editor_tab,
            project: self.project.clone(),
            status: StatusSnapshot {
                view: "editor".into(),
                branch: None,
                problems: 0,
                service: "idle".into(),
                message: if self.status_message.is_empty() {
                    None
                } else {
                    Some(self.status_message.clone())
                },
                line: 1,
                column: 1,
            },
            journal_lagging: !self.pending_checkpoints.is_empty()
                || !self.pending_compactions.is_empty(),
            workspace_root: self.workspace_root.clone(),
            workspace_name: self.workspace_name.clone(),
        }
    }
    /// Restore full persisted state + journal recovery (called by lib::run after load+validate).
    /// Dirty recovery buffers first (journal), then validated clean tabs supplied by state (content read
    /// off-thread at startup in validate before UI). Rebuild layout.editor_tabs/current/selection/cursor/scroll
    /// strictly from validated state.tabs (not unvalidated paths or old layout tabs).
    /// Missing clean dropped (by validate), recovered dirty missing files kept. First-run welcome always
    /// gets an editor tab entry so typing visible.
    pub fn restore_state(
        state: crate::persistence::state::AppState,
        recovery: Recovery,
        journal: JournalHandle,
    ) -> Self {
        let epoch = state.epoch;
        let mut buffers: Vec<Buffer> = Vec::new();
        let first_recovered = recovery.buffers.first().map(|recovered| recovered.id);
        // Dirty first from journal recovery (may be missing on disk).
        for recovered in &recovery.buffers {
            let path_state = match &recovered.path {
                Some(path) => BufferPathState::Recovered {
                    original: path.clone(),
                },
                None => BufferPathState::Untitled {
                    suggested_name: "recovered".into(),
                },
            };
            buffers.push(Buffer::recover(
                recovered.id,
                recovered.language,
                &recovered.content,
                recovered.revision,
                path_state,
            ));
        }
        // Clean tabs from validated state (content loaded off-thread before event loop).
        for t in &state.tabs {
            if buffers.iter().any(|b| b.id() == t.id) {
                continue;
            }
            let content = match (&t.path, &t.content) {
                (Some(_), Some(content)) => content.clone(),
                (Some(_), None) => continue,
                (None, Some(content)) => content.clone(),
                (None, None) => String::new(),
            };
            let lang = t.language;
            let path_state = match &t.path {
                Some(p) => BufferPathState::Saved(p.clone()),
                None => BufferPathState::Untitled {
                    suggested_name: "untitled".into(),
                },
            };
            buffers.push(Buffer::restore_clean(
                t.id,
                lang,
                &content,
                t.last_known_revision,
                path_state,
            ));
        }
        if buffers.is_empty() {
            let id = DocumentId::new();
            buffers.push(Buffer::new(id, Language::PlainText, "fn main() {}\n"));
        }
        // current: prefer a valid persisted current_tab if present in buffers (recovered or clean),
        // else recovered (first) or first buffer.
        let current_doc = if let Some(cid) = state.current_tab {
            if buffers.iter().any(|b| b.id() == cid) {
                Some(cid)
            } else {
                first_recovered.or_else(|| buffers.first().map(|buffer| buffer.id()))
            }
        } else {
            first_recovered.or_else(|| buffers.first().map(|buffer| buffer.id()))
        };

        // Rebuild editor_tabs from recovered dirty buffers FIRST, then validated clean state tabs
        // without duplicates. Use default view state for recovered (no persisted view if path was
        // missing at validate); preserve view state from state.tabs for clean. This makes dirty
        // missing-file recoveries always visible as tabs; dropped clean missing stay out.
        let mut layout = state.layout;
        layout.editor_tabs.clear();
        layout.active_editor_tab = 0;
        // recovered dirties first
        for rec in &recovery.buffers {
            if !layout.editor_tabs.iter().any(|t| t.doc_id == rec.id) {
                layout.editor_tabs.push(EditorTabState {
                    doc_id: rec.id,
                    scroll_line: 0,
                    cursor_byte: 0,
                    selection_anchor: None,
                });
            }
        }
        // then additional clean from state (with their saved views)
        for t in &state.tabs {
            if buffers.iter().any(|buffer| buffer.id() == t.id)
                && !layout.editor_tabs.iter().any(|tab| tab.doc_id == t.id)
            {
                let idx = layout.editor_tabs.len();
                layout.editor_tabs.push(EditorTabState {
                    doc_id: t.id,
                    scroll_line: t.scroll_top_line as u16,
                    cursor_byte: t.cursor_byte,
                    selection_anchor: t.selection_start_byte,
                });
                if Some(t.id) == current_doc {
                    layout.active_editor_tab = idx;
                }
            }
        }
        // ensure active for current if from recovered prefix
        if let Some(cur) = current_doc {
            if let Some(pos) = layout.editor_tabs.iter().position(|et| et.doc_id == cur) {
                layout.active_editor_tab = pos;
            }
        }
        if layout.editor_tabs.is_empty() {
            if let Some(id) = current_doc {
                layout.open_or_focus_editor(id);
            }
        }

        let mut authorities = Vec::new();
        for tr in &state.trust {
            let level = if tr.level == "trusted" {
                TrustLevel::Trusted
            } else {
                TrustLevel::InspectOnly
            };
            authorities.push(AuthorityState {
                kind: AuthorityKind::Local,
                label: tr.authority.clone(),
                trust: level,
            });
        }
        if authorities.is_empty() {
            authorities.push(AuthorityState {
                kind: AuthorityKind::Local,
                label: "host".into(),
                trust: TrustLevel::InspectOnly,
            });
        }
        let workspace_root = state
            .trust
            .first()
            .map(|t| t.workspace_root.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/workspace".into());
        let focus = match state.focus.as_str() {
            "Toolbar" => Landmark::Toolbar,
            "Authority" => Landmark::Authority,
            "LeftStripe" => Landmark::LeftStripe,
            "PrimaryPane" => Landmark::PrimaryPane,
            "Editor" => Landmark::Editor,
            "ContextPane" => Landmark::ContextPane,
            "RightStripe" => Landmark::RightStripe,
            "BottomPane" => Landmark::BottomPane,
            "StatusBar" => Landmark::StatusBar,
            _ => Landmark::Editor,
        };
        App {
            epoch,
            layout,
            buffers,
            current_doc,
            authorities,
            current_authority: 0,
            focus,
            overlay: Overlay::None,
            journal: Some(journal),
            pending_checkpoints: std::collections::HashMap::new(),
            pending_compactions: std::collections::HashMap::new(),
            syntax_highlights: std::collections::HashMap::new(),
            retained_syntax: std::collections::HashMap::new(),
            project: ProjectTreeSnapshot {
                tree: None,
                loading: true,
                scroll: 0,
                selected_row: 0,
            },
            status_message: String::new(),
            workspace_root,
            workspace_name: "workspace".into(),
        }
    }

    /// Produce versioned state for durable save at shutdown.
    pub fn to_state(&self) -> crate::persistence::state::AppState {
        use std::path::PathBuf;
        let trust: Vec<crate::persistence::state::TrustRecord> = self
            .authorities
            .iter()
            .map(|a| crate::persistence::state::TrustRecord {
                workspace_root: PathBuf::from(&self.workspace_root),
                authority: a.label.clone(),
                level: match a.trust {
                    TrustLevel::Trusted => "trusted".into(),
                    TrustLevel::InspectOnly => "inspect_only".into(),
                },
            })
            .collect();
        let tabs: Vec<crate::persistence::state::TabMetadata> = self
            .layout
            .editor_tabs
            .iter()
            .filter_map(|t| {
                self.buffers.iter().find(|b| b.id() == t.doc_id).map(|buf| {
                    crate::persistence::state::TabMetadata {
                        id: t.doc_id,
                        path: buf.path().map(|p| p.to_path_buf()),
                        last_known_revision: buf.revision(),
                        cursor_byte: t.cursor_byte,
                        scroll_top_line: t.scroll_line as usize,
                        selection_start_byte: t.selection_anchor,
                        selection_end_byte: Some(t.cursor_byte),
                        content: None,
                        language: buf.language(),
                    }
                })
            })
            .collect();
        crate::persistence::state::AppState {
            version: 1,
            epoch: self.epoch,
            layout: self.layout.clone(),
            tabs,
            current_tab: self.current_doc,
            focus: match self.focus {
                Landmark::Toolbar => "Toolbar".into(),
                Landmark::Authority => "Authority".into(),
                Landmark::LeftStripe => "LeftStripe".into(),
                Landmark::PrimaryPane => "PrimaryPane".into(),
                Landmark::Editor => "Editor".into(),
                Landmark::ContextPane => "ContextPane".into(),
                Landmark::RightStripe => "RightStripe".into(),
                Landmark::BottomPane => "BottomPane".into(),
                Landmark::StatusBar => "StatusBar".into(),
            },
            trust,
        }
    }
    pub fn workspace_root(&self) -> &str {
        &self.workspace_root
    }

    pub fn syntax_is_current(&self, doc_id: DocumentId, revision: DocumentRevision) -> bool {
        self.syntax_highlights
            .get(&doc_id)
            .is_some_and(|(highlight_revision, _)| *highlight_revision == revision)
    }
    pub fn syntax_retained(
        &self,
        doc_id: DocumentId,
    ) -> Option<(DocumentRevision, Option<Tree>, String)> {
        self.retained_syntax.get(&doc_id).cloned()
    }

    /// Update highlights + retain Tree+source for the rev if still current (epoch/rev checked).
    /// Used by event_loop syntax result drain to support incremental next parses.
    pub(crate) fn apply_syntax_result(&mut self, sres: crate::syntax::SyntaxResult) {
        if sres.epoch != self.epoch {
            return;
        }
        if let Some(buf) = self.buffers.iter_mut().find(|b| b.id() == sres.doc_id) {
            if buf.revision() == sres.revision {
                self.syntax_highlights
                    .insert(sres.doc_id, (sres.revision, sres.highlights));
                self.retained_syntax
                    .insert(sres.doc_id, (sres.revision, sres.tree, sres.source));
            }
        }
    }

    pub fn current_trust(&self) -> TrustLevel {
        self.authorities
            .get(self.current_authority)
            .map(|a| a.trust)
            .unwrap_or(TrustLevel::InspectOnly)
    }
}
