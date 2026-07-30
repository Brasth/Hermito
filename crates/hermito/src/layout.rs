use crate::document::DocumentId;
use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

/// Landmarks for F6 cycling. Exactly 8 when context collapsed (default first-run), 9 when context open.
/// Order per design: toolbar → authority → left stripe → primary pane → editor → [context] → right stripe → bottom → status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Landmark {
    #[default]
    Toolbar,
    Authority,
    LeftStripe,
    PrimaryPane,
    Editor,
    ContextPane,
    RightStripe,
    BottomPane,
    StatusBar,
}

impl Landmark {
    /// Next visible landmark. Skips ContextPane when !context_visible to guarantee count 8 vs 9.
    pub fn next(self, context_visible: bool) -> Self {
        match self {
            Landmark::Toolbar => Landmark::Authority,
            Landmark::Authority => Landmark::LeftStripe,
            Landmark::LeftStripe => Landmark::PrimaryPane,
            Landmark::PrimaryPane => Landmark::Editor,
            Landmark::Editor => {
                if context_visible {
                    Landmark::ContextPane
                } else {
                    Landmark::RightStripe
                }
            }
            Landmark::ContextPane => Landmark::RightStripe,
            Landmark::RightStripe => Landmark::BottomPane,
            Landmark::BottomPane => Landmark::StatusBar,
            Landmark::StatusBar => Landmark::Toolbar,
        }
    }

    pub fn prev(self, context_visible: bool) -> Self {
        match self {
            Landmark::Toolbar => Landmark::StatusBar,
            Landmark::Authority => Landmark::Toolbar,
            Landmark::LeftStripe => Landmark::Authority,
            Landmark::PrimaryPane => Landmark::LeftStripe,
            Landmark::Editor => Landmark::PrimaryPane,
            Landmark::ContextPane => Landmark::Editor,
            Landmark::RightStripe => {
                if context_visible {
                    Landmark::ContextPane
                } else {
                    Landmark::Editor
                }
            }
            Landmark::BottomPane => Landmark::RightStripe,
            Landmark::StatusBar => Landmark::BottomPane,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Primary,
    Context,
    Bottom,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditorTabState {
    pub doc_id: DocumentId,
    pub scroll_line: u16,
    pub cursor_byte: usize,
    /// Selection anchor (byte). None = point cursor only. Distinct from Landmark focus.
    pub selection_anchor: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkbenchLayout {
    pub width: u16,
    pub height: u16,

    // visibility
    pub primary_visible: bool,
    pub context_visible: bool,
    pub bottom_visible: bool,

    // split sizes in cells
    pub left_width: u16,
    pub context_width: u16,
    pub bottom_height: u16,

    // active tabs for tool panes
    pub primary_active_tab: usize,
    pub context_active_tab: usize,
    pub bottom_active_tab: usize,

    // open editor tabs + per-tab view state (cursor/selection/scroll here, NOT in Buffer)
    pub editor_tabs: Vec<EditorTabState>,
    pub active_editor_tab: usize,

    // fixed sizes - runtime constants; only persistent metadata is serialized (skipped here)
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) toolbar_h: u16,
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) authority_h: u16,
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) status_h: u16,
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) bottom_header_h: u16,
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) stripe_w: u16,
    #[serde(skip_serializing, skip_deserializing)]
    pub(crate) min_editor_w: u16,
}

// (duplicate removed; canonical WorkbenchLayout with serde now at 68)

impl Default for WorkbenchLayout {
    fn default() -> Self {
        Self {
            width: 120,
            height: 36,
            primary_visible: true,
            context_visible: false,
            bottom_visible: false,
            left_width: 28,
            context_width: 24,
            bottom_height: 10,
            primary_active_tab: 0,
            context_active_tab: 0,
            bottom_active_tab: 0,
            editor_tabs: Vec::new(),
            active_editor_tab: 0,
            toolbar_h: 1,
            authority_h: 2,
            status_h: 1,
            bottom_header_h: 1,
            stripe_w: WorkbenchLayout::stripe_width(120),
            min_editor_w: 40,
        }
    }
}

impl WorkbenchLayout {
    pub fn new_default() -> Self {
        let mut l = Self::default();
        l.resize(120, 36);
        l
    }
    pub fn resize(&mut self, w: u16, h: u16) {
        self.width = w.max(60);
        self.height = h.max(20);
        self.bottom_header_h = 1;
        self.stripe_w = Self::stripe_width(self.width);
        self.recompute();
    }
    /// workbench layout split) and mouse mapping derive from this value.
    /// 4 when total width >=70, else 3. Matches previous workbench ad-hoc.
    pub fn stripe_width(total_width: u16) -> u16 {
        if total_width >= 70 {
            4
        } else {
            3
        }
    }

    /// Cell-exact recompute. Enforce min editor width. Collapse priority: context, then bottom, then primary.
    pub(crate) fn recompute(&mut self) {
        self.stripe_w = Self::stripe_width(self.width);
        let stripe = Self::stripe_width(self.width);
        // Horizontal: stripes + primary + context + editor (min 40)
        let mut avail = self.width.saturating_sub(stripe * 2);
        if self.context_visible {
            let c = self
                .context_width
                .min(avail.saturating_sub(self.min_editor_w));
            self.context_width = c;
            avail = avail.saturating_sub(c);
        } else {
            self.context_width = 0;
        }
        if self.primary_visible {
            let p = self.left_width.min(avail.saturating_sub(self.min_editor_w));
            self.left_width = p;
            avail = avail.saturating_sub(p);
        } else {
            self.left_width = 0;
        }
        // If still short, collapse context first
        if avail < self.min_editor_w && self.context_visible {
            self.context_visible = false;
            self.context_width = 0;
            self.recompute();
            return;
        }
        if avail < self.min_editor_w && self.primary_visible {
            self.primary_visible = false;
            self.left_width = 0;
            self.recompute();
            return;
        }

        // Vertical bottom
        let main_h = self
            .height
            .saturating_sub(self.toolbar_h + self.authority_h + self.status_h);
        if self.bottom_visible {
            let b = self.bottom_height.min(main_h.saturating_sub(4)).max(4);
            self.bottom_height = b;
        } else {
            self.bottom_height = 0;
        }
    }

    // All pane rects (cell units, always correct for current size/vis)
    pub fn rect_toolbar(&self) -> Rect {
        Rect::new(0, 0, self.width, self.toolbar_h)
    }
    pub fn rect_authority(&self) -> Rect {
        Rect::new(0, self.toolbar_h, self.width, self.authority_h)
    }
    pub fn rect_left_stripe(&self) -> Rect {
        let stripe = Self::stripe_width(self.width);
        let y = self.toolbar_h + self.authority_h;
        let mut hh = self
            .height
            .saturating_sub(y + self.status_h + self.bottom_header_h);
        if self.bottom_visible {
            hh = hh.saturating_sub(self.bottom_height);
        }
        Rect::new(0, y, stripe, hh)
    }
    pub fn rect_primary(&self) -> Rect {
        if !self.primary_visible {
            return Rect::default();
        }
        let stripe = Self::stripe_width(self.width);
        let x = stripe;
        let y = self.toolbar_h + self.authority_h;
        let mut hh = self
            .height
            .saturating_sub(y + self.status_h + self.bottom_header_h);
        if self.bottom_visible {
            hh = hh.saturating_sub(self.bottom_height);
        }
        Rect::new(x, y, self.left_width, hh)
    }
    pub fn rect_editor(&self) -> Rect {
        let stripe = Self::stripe_width(self.width);
        let x = stripe
            + if self.primary_visible {
                self.left_width
            } else {
                0
            };
        let y = self.toolbar_h + self.authority_h;
        let mut h = self
            .height
            .saturating_sub(y + self.status_h + self.bottom_header_h);
        if self.bottom_visible {
            h = h.saturating_sub(self.bottom_height);
        }
        let right = stripe
            + if self.context_visible {
                self.context_width
            } else {
                0
            };
        let w = self
            .width
            .saturating_sub(x)
            .saturating_sub(right)
            .max(self.min_editor_w);
        Rect::new(x, y, w, h)
    }
    pub fn rect_right_stripe(&self) -> Rect {
        let stripe = Self::stripe_width(self.width);
        let x = self.width.saturating_sub(stripe);
        let y = self.toolbar_h + self.authority_h;
        let mut hh = self
            .height
            .saturating_sub(y + self.status_h + self.bottom_header_h);
        if self.bottom_visible {
            hh = hh.saturating_sub(self.bottom_height);
        }
        Rect::new(x, y, stripe, hh)
    }
    pub fn rect_context(&self) -> Rect {
        if !self.context_visible {
            return Rect::default();
        }
        let stripe = Self::stripe_width(self.width);
        let x = self.width.saturating_sub(stripe + self.context_width);
        let y = self.toolbar_h + self.authority_h;
        let mut hh = self
            .height
            .saturating_sub(y + self.status_h + self.bottom_header_h);
        if self.bottom_visible {
            hh = hh.saturating_sub(self.bottom_height);
        }
        Rect::new(x, y, self.context_width, hh)
    }
    pub fn rect_bottom(&self) -> Rect {
        if !self.bottom_visible {
            return Rect::default();
        }
        let stripe = Self::stripe_width(self.width);
        let y = self
            .height
            .saturating_sub(self.status_h + self.bottom_height);
        Rect::new(
            stripe,
            y,
            self.width.saturating_sub(stripe * 2),
            self.bottom_height,
        )
    }
    pub fn rect_status(&self) -> Rect {
        Rect::new(0, self.height - self.status_h, self.width, self.status_h)
    }
    pub fn toggle_pane(&mut self, p: Pane) {
        match p {
            Pane::Primary => {
                self.primary_visible = !self.primary_visible;
                if self.primary_visible && self.left_width < 4 {
                    self.left_width = 28;
                }
            }
            Pane::Context => {
                self.context_visible = !self.context_visible;
                if self.context_visible && self.context_width < 4 {
                    self.context_width = 24;
                }
            }
            Pane::Bottom => {
                self.bottom_visible = !self.bottom_visible;
                if self.bottom_visible && self.bottom_height < 4 {
                    self.bottom_height = 10;
                }
            }
        }
        self.recompute();
    }

    pub fn drag_separator(&mut self, sep: Separator, delta: i16) {
        let d = delta as i32;
        match sep {
            Separator::PrimaryRight => {
                if self.primary_visible {
                    self.left_width = (self.left_width as i32 + d).max(4) as u16;
                }
            }
            Separator::ContextLeft => {
                if self.context_visible {
                    self.context_width = (self.context_width as i32 - d).max(4) as u16;
                }
            }
            Separator::BottomTop => {
                if self.bottom_visible {
                    self.bottom_height = (self.bottom_height as i32 - d).max(4) as u16;
                }
            }
        }
        self.recompute();
    }

    pub fn set_active_tab(&mut self, p: Pane, tab: usize) {
        match p {
            Pane::Primary => self.primary_active_tab = tab,
            Pane::Context => self.context_active_tab = tab,
            Pane::Bottom => self.bottom_active_tab = tab,
        }
    }

    // Editor tab management (called from App on open etc; for restore, App clears and rebuilds
    // editor_tabs directly from validated state.tabs with their saved cursor/scroll/selection).
    pub fn open_or_focus_editor(&mut self, id: DocumentId) {
        if let Some(i) = self.editor_tabs.iter().position(|t| t.doc_id == id) {
            self.active_editor_tab = i;
        } else {
            self.editor_tabs.push(EditorTabState {
                doc_id: id,
                scroll_line: 0,
                cursor_byte: 0,
                selection_anchor: None,
            });
            self.active_editor_tab = self.editor_tabs.len() - 1;
        }
    }

    pub fn close_editor_tab(&mut self, idx: usize) {
        if idx < self.editor_tabs.len() {
            self.editor_tabs.remove(idx);
            if !self.editor_tabs.is_empty() && self.active_editor_tab >= self.editor_tabs.len() {
                self.active_editor_tab = self.editor_tabs.len() - 1;
            }
        }
    }

    pub fn current_editor(&self) -> Option<&EditorTabState> {
        self.editor_tabs.get(self.active_editor_tab)
    }
    pub fn current_editor_mut(&mut self) -> Option<&mut EditorTabState> {
        self.editor_tabs.get_mut(self.active_editor_tab)
    }

    pub fn set_editor_cursor(&mut self, byte: usize) {
        if let Some(t) = self.current_editor_mut() {
            t.cursor_byte = byte;
            t.selection_anchor = None;
        }
    }
    pub fn set_editor_selection(&mut self, anchor: usize, cursor: usize) {
        if let Some(t) = self.current_editor_mut() {
            t.selection_anchor = Some(anchor);
            t.cursor_byte = cursor;
        }
    }
    pub fn extend_or_move_cursor(&mut self, new_byte: usize, extend: bool) {
        if let Some(t) = self.current_editor_mut() {
            if extend {
                if t.selection_anchor.is_none() {
                    t.selection_anchor = Some(t.cursor_byte);
                }
            } else {
                t.selection_anchor = None;
            }
            t.cursor_byte = new_byte;
        }
    }
    /// Restore the fixed non-persisted layout constants after deserializing only the persistent metadata.
    /// Invoked by persistence::state after toml load so that recompute and rects use correct values.
    pub(crate) fn restore_fixed_fields(&mut self) {
        self.toolbar_h = 1;
        self.authority_h = 2;
        self.status_h = 1;
        self.bottom_header_h = 1;
        self.stripe_w = 3;
        self.min_editor_w = 40;
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Separator {
    PrimaryRight,
    ContextLeft,
    BottomTop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripe_width_canonical_at_key_widths() {
        assert_eq!(WorkbenchLayout::stripe_width(60), 3);
        assert_eq!(WorkbenchLayout::stripe_width(69), 3);
        assert_eq!(WorkbenchLayout::stripe_width(70), 4);
        assert_eq!(WorkbenchLayout::stripe_width(80), 4);
        assert_eq!(WorkbenchLayout::stripe_width(120), 4);
        assert_eq!(WorkbenchLayout::stripe_width(160), 4);
        assert_eq!(WorkbenchLayout::stripe_width(1000), 4);
    }

    fn make_layout(w: u16, h: u16) -> WorkbenchLayout {
        let mut l = WorkbenchLayout::default();
        l.resize(w, h);
        // ensure some primary for realistic editor rect
        l.primary_visible = true;
        l.left_width = 24;
        l.context_visible = false;
        l.recompute();
        l
    }

    #[test]
    fn rect_editor_at_80_120_160_matches_stripe_calc() {
        for total_w in [80u16, 120, 160] {
            let l = make_layout(total_w, 40);
            let stripe = WorkbenchLayout::stripe_width(total_w);
            assert_eq!(
                l.stripe_w, stripe,
                "stripe must be canonical at {}",
                total_w
            );

            let r = l.rect_editor();
            // x must be stripe + left (primary on)
            assert_eq!(r.x, stripe + l.left_width);
            // right margin is stripe (no context)
            let expected_w = total_w
                .saturating_sub(stripe)
                .saturating_sub(l.left_width)
                .saturating_sub(stripe)
                .max(l.min_editor_w);
            assert_eq!(r.width, expected_w);
            // y starts after toolbar+authority
            assert_eq!(r.y, l.toolbar_h + l.authority_h);
            // height accounts for status (+bottom if any)
            assert!(r.height > 0);
        }
    }

    #[test]
    fn rect_editor_width_uses_one_formula_no_duplicate_stripe() {
        let mut l = make_layout(120, 36);
        let r1 = l.rect_editor();
        // change stripe via canonical and recompute
        l.stripe_w = WorkbenchLayout::stripe_width(120);
        l.recompute();
        let r2 = l.rect_editor();
        assert_eq!(r1, r2);

        // different width updates stripe and rect consistently
        l.resize(80, 36);
        let r80 = l.rect_editor();
        assert_eq!(l.stripe_w, 4);
        assert!(r80.x > 0);
        assert!(r80.width >= l.min_editor_w);
    }

    #[test]
    fn bottom_visible_at_80x24_uses_exact_height_in_rects_no_cap() {
        let mut l = make_layout(80, 24);
        l.bottom_visible = true;
        l.recompute();
        // default min when visible at this size after recompute
        assert_eq!(l.bottom_height, 4);
        assert_eq!(l.rect_bottom().height, 4);
        // 24 - (1+2+1+1+4) =15 for editor
        assert_eq!(l.rect_editor().height, 15);
        assert_eq!(l.rect_authority().height, 2);
    }

    #[test]
    fn restore_fixed_fields_roundtrip_keeps_authority_detail_and_bottom() {
        let mut l = make_layout(80, 24);
        l.bottom_visible = true;
        l.bottom_height = 6;
        l.recompute();
        let saved_bottom = l.bottom_height;
        // simulate post-deser fixed fields reset (skipped by serde)
        l.toolbar_h = 0;
        l.authority_h = 0;
        l.status_h = 0;
        l.bottom_header_h = 0;
        l.stripe_w = 0;
        l.min_editor_w = 0;
        l.restore_fixed_fields();
        l.recompute();
        assert_eq!(l.toolbar_h, 1);
        assert_eq!(l.authority_h, 2);
        assert_eq!(l.bottom_height, saved_bottom);
        assert_eq!(l.rect_authority().height, 2);
        assert_eq!(l.rect_bottom().height, saved_bottom);
    }

    #[test]
    fn render_split_rects_align_with_layout_hit_test_rects_at_80x24() {
        use ratatui::layout::{Constraint, Direction, Layout};
        let mut l = make_layout(80, 24);
        l.bottom_visible = true;
        l.recompute();
        let area = Rect::new(0, 0, 80, 24);
        let bottom_body_height = if l.bottom_visible { l.bottom_height } else { 0 };
        let main_h = area.height.saturating_sub(
            l.toolbar_h + l.authority_h + l.status_h + l.bottom_header_h + bottom_body_height,
        );
        let [toolbar_a, auth_a, main_a, bheader_a, bbody_a, status_a] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(l.toolbar_h),
                Constraint::Length(l.authority_h),
                Constraint::Length(main_h),
                Constraint::Length(l.bottom_header_h),
                Constraint::Length(bottom_body_height),
                Constraint::Length(l.status_h),
            ])
            .areas(area);
        // render rects (from split using exact bottom_h) align with layout hit-test rects
        assert_eq!(toolbar_a, l.rect_toolbar());
        assert_eq!(auth_a, l.rect_authority());
        assert_eq!(status_a, l.rect_status());
        assert_eq!(main_a.y, l.rect_editor().y);
        assert_eq!(main_a.height, l.rect_editor().height);
        assert_eq!(bbody_a.y, l.rect_bottom().y);
        assert_eq!(bbody_a.height, l.rect_bottom().height);
        assert_eq!(bheader_a.y + bheader_a.height, bbody_a.y);
        assert_eq!(bheader_a.height, 1u16);
        // required authority detail visible
        assert_eq!(l.rect_authority().height, 2);
    }

    #[test]
    fn required_sizes_render_current_and_align_after_restore() {
        use ratatui::layout::{Constraint, Direction, Layout};
        let mut l = WorkbenchLayout::default();
        l.resize(80, 24);
        // zero fixed then restore
        l.toolbar_h = 0;
        l.authority_h = 0;
        l.status_h = 0;
        l.bottom_header_h = 0;
        l.stripe_w = 0;
        l.min_editor_w = 0;
        l.restore_fixed_fields();
        l.bottom_visible = true;
        l.recompute();
        assert_eq!(l.toolbar_h, 1);
        assert_eq!(l.authority_h, 2);
        assert_eq!(l.rect_authority().height, 2);
        let area = Rect::new(0, 0, 80, 24);
        let bottom_body_height = if l.bottom_visible { l.bottom_height } else { 0 };
        let main_h = area.height.saturating_sub(
            l.toolbar_h + l.authority_h + l.status_h + l.bottom_header_h + bottom_body_height,
        );
        let [_toolbar_a, auth_a, main_a, _bheader_a, bbody_a, _status_a] = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(l.toolbar_h),
                Constraint::Length(l.authority_h),
                Constraint::Length(main_h),
                Constraint::Length(l.bottom_header_h),
                Constraint::Length(bottom_body_height),
                Constraint::Length(l.status_h),
            ])
            .areas(area);
        assert_eq!(auth_a.height, 2);
        assert_eq!(main_a.height, l.rect_editor().height);
        assert_eq!(bbody_a.height, l.rect_bottom().height);
    }
}
