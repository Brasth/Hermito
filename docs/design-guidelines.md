# Hermito Design Guidelines

Hermito is an **editor-first terminal workbench for consequential local and remote operations**. Its ergonomics borrow the progressive disclosure and tool-window hierarchy of JetBrains New UI without copying product branding or pretending a terminal is a desktop GUI. The defining Hermito idea is execution authority: the user can always see where a command will run, what trust it carries, and how that authority was reached.

## 1. Product character

### Direction

- **Editor first.** Source text, diffs, environment configuration, or port mappings own the center. Hermito never opens to a dashboard when an editor layout can be restored.
- **Restore, then disclose.** Reopen the last view, tabs, pane visibility, selections, and scroll positions. On first launch, show only Project + editor; right context and bottom tool bodies start collapsed.
- **JetBrains-like hierarchy, terminal-native execution.** Use a simplified project/VCS/run/search toolbar, legible tool-window stripe buttons, compact tabs, and docked panes. Retain cell-aligned geometry and complete text labels in tooltips/help.
- **Calm, not cramped.** Only the active task's supporting panes open. Prefer one-row controls and predictable alignment; recover editor area before reducing text legibility.
- **Authority is the identity.** A slim path directly below the toolbar shows `LOCAL → SSH → DEVCONTAINER`, identifies `CURRENT`, and exposes `TRUSTED` or `INSPECT ONLY`. Blue remains ordinary selection/focus; amber belongs to authority and warnings.

### Terminal constraints

- Design on a character-cell grid. Assume an `xterm-256color` baseline; truecolor themes may refine, never redefine, semantics.
- The TUI inherits the user's monospace face. Do not depend on ligatures, italics, a particular glyph width, or a specific installed font.
- Every state has text or a conventional ASCII mark (`!`, `x`, `+`, `~`). Do not use emoji.
- Test the implementation at 80×24, 120×36, and 160×45 cells. The HTML prototype additionally demonstrates 1280×800 and 1440×900 layouts.

## 2. Design tokens

### Color

| Token | Truecolor | 256-color fallback | Use |
|---|---:|---:|---|
| `canvas` | `#111316` | 233 | Editor and terminal ground |
| `chrome` | `#181B1F` | 234 | Toolbar, stripes, status bar |
| `surface-1` | `#1D2126` | 235 | Tool-window bodies |
| `surface-2` | `#242A30` | 236 | Headers, hovered rows |
| `surface-3` | `#2B323A` | 237 | Inputs and raised controls |
| `rule` | `#353D46` | 238 | Pane and row boundaries |
| `rule-strong` | `#4A5561` | 240 | Active separators and resize handles |
| `text` | `#D7DCE2` | 253 | Primary text |
| `text-dim` | `#AFB7C0` | 249 | Secondary text |
| `text-muted` | `#7F8994` | 245 | Metadata and inactive chrome |
| `selection` | `#2F5F8F` | 24 | Active tab, selected row, active tool button |
| `focus` | `#75B7F0` | 117 | Keyboard focus ring and focused landmark |
| `authority` | `#D6A84B` | 179 | `CURRENT` execution context and warnings only |
| `success` | `#70B580` | 108 | Ready, clean, connected |
| `danger` | `#D96868` | 167 | Failure, conflict, destructive action |
| `info` | `#73A9D8` | 110 | Links, remote state, forwarded leases |

Every CSS color and font in the prototype comes from a named token. TUI color never stands alone: pair it with a label, shape, or ASCII mark. Maintain WCAG AA (4.5:1 for text, 3:1 for focus and meaningful boundaries). High-contrast mode strengthens text and rules without changing semantic assignments.

### Type and density

Use one monospace family. Hierarchy comes from weight, case, alignment, and surface contrast—not large display type.

| Role | Treatment |
|---|---|
| Toolbar/project title | 12–13 px simulation, medium weight, sentence case |
| Tool-window title | 11 px simulation, medium weight, compact one-row header |
| Editor tab | 12 px simulation, full filename before ornament |
| Body/code | 12–13 px simulation, terminal line height |
| Metadata | same minimum size as configured terminal text, dim token |
| Key hint | compact bracketed form: `[F6] Next pane` |

Use a 4 px base spacing scale. Dense rows are 24–28 px in the prototype; tool headers are 30 px; the main toolbar is 38 px. Controls use square to 3 px corners. Borders consume one cell and must establish hierarchy or afford resizing.

## 3. Workbench anatomy

Order and persistence are intentional:

1. **Main toolbar** — project widget, VCS widget, run target, command search, and compact layout controls.
2. **Authority path** — always visible below the toolbar. It shows the reachable local/SSH/devcontainer chain, marks `CURRENT`, states trust, and controls where execution is routed.
3. **Left tool-window stripe** — Project, Git, Environments, Ports. Buttons use one coherent icon family, retain accessible names, and open the primary vertical tool window.
4. **Primary tool window** — Project tree, changelists, environment chain, or port leases. Open on first launch; restored thereafter.
5. **Editor-centered work area** — code editor, diff, environment inspector, or port table. Workspace defaults to a real source buffer with tabs, breadcrumbs, gutter, cursor, diagnostics, and scroll.
6. **Right tool-window stripe** — Context, Structure, and Services remain available while the contextual pane is closed by default.
7. **Contextual tool window** — outline, hunk detail, capabilities, or lease policy. Opens on demand and collapses before the primary pane.
8. **Bottom tool stripe/header** — Terminal, Problems, and Services tabs remain discoverable; their body is collapsed by default and opens on demand.
9. **Status bar** — current view, branch, problems, service state, authority shorthand, encoding, position, and keyboard help.

The center is never wrapped in decorative cards. Tool windows share straight boundaries with the work area. Active tabs use blue; keyboard focus uses a brighter blue outline; amber never indicates ordinary selection.

### Distinctive authority treatment

- Render authority as an ordered path, not three unrelated badges. Connectors communicate delegation: host process → remote helper → container process.
- The `CURRENT` node receives the strongest amber edge and literal text. Non-current trusted nodes remain quiet; unavailable or inspect-only nodes explain why.
- The run widget repeats the current target and authority, for example `cargo run · DEVCONTAINER hermito-dev`.
- Switching authority only proposes context; it never executes. `Enter` on focused Authority Path or click CURRENT opens Trust Review showing workspace root, authority identity, effective config/tool hashes where applicable, and exact capabilities. `Tab` reaches `Grant execution`/`Cancel`; only focused grant + `Enter` changes state. Trusted state offers immediate `Revoke execution`.
- Revocation disables run/task, terminal, Git mutation/network, lifecycle, and forwarding controls immediately; browse/edit and audited inspect reads remain. Cancel/Esc never changes trust.

## 4. Tool-window behavior

- Clicking a stripe button opens and selects its tool window. Clicking the selected button again collapses the pane.
- Left and right separators support pointer drag and keyboard arrows. Bottom separation supports pointer drag and keyboard up/down.
- Persist the active view, open editor tabs, pane visibility, pane sizes, active tool tabs, selections, and scroll positions. Restore them on the next launch only after validating that referenced files and authorities still exist.
- First-run fallback: Project tool window open, editor open, right context body closed, bottom body closed. Do not pre-open terminal, environment, or Git surfaces.
- Tool-window bodies are independent wheel-ready scroll regions. The pane under the pointer scrolls; hover never moves keyboard focus.
- At 1440×900 and 1280×800, the default Project + editor composition must feel spacious. Open panes reduce the editor deliberately; context collapses first, then bottom body, then the primary pane.
- A collapsed pane remains available from its stripe/header. The authority path, focused control, and central work area never disappear.

## 5. Selection, focus, and keyboard model

- Keyboard operation is complete; mouse support is additive.
- `[F6]` and `[Shift+F6]` cycle visible landmarks: toolbar → authority → left stripe → primary pane → editor/work area → context → right stripe → bottom pane → status.
- `Tab` moves through controls within a landmark. Arrow keys move within tablists, trees, result lists, and radio-like groups. `Home` and `End` jump to the first and last tab. `Enter` or `Space` activates.
- `Alt+1` through `Alt+4` open Workspace, Git, Environments, and Ports.
- `Ctrl+K` / `Cmd+K` opens command and file search. Search results expose their kind and shortcut; arrows move the active result and Enter executes it.
- `Esc` closes the topmost dialog, closes command search, or releases terminal capture. It never silently changes authority or discards work.
- Focus is visually separate from selection: selection is a blue fill/edge; focus is a high-contrast blue outline and optional `>` marker.
- After a dialog closes, restore focus to its invoker. After shortcut view switching, focus the new central panel heading or first meaningful control.
- Reduced-motion mode removes nonessential motion. Busy state may update discretely no faster than twice per second.

## 6. Authority and trust

The authority strip is load-bearing, not status decoration. It is always legible and immediately below the main toolbar.

- The active authority segment includes the literal word **`CURRENT`** and an amber edge/fill treatment.
- Each segment names both kind and object: `LOCAL · huy-mac`, `SSH · build-07.internal`, `DEVCONTAINER · hermito-rust`.
- Trust state is explicit text:
  - `TRUSTED · execution granted`
  - `INSPECT ONLY · execution blocked`
- Trust is scoped per workspace and authority. Switching authority loads that authority's trust decision; it never implies trust merely because a neighbor is trusted.
- Inspect-only mode permits navigation and editing but blocks tasks, lifecycle hooks, environment-provided tools, terminal execution, port forwarding, and credential/agent forwarding.
- Disabled execution controls remain legible and expose the reason in adjacent status feedback.
- The status bar repeats a compact authority/trust summary, but never replaces the full strip.

## 7. Operational screen content

### Workspace

Show a project tree and a restored real source buffer—not a readiness dashboard. Include editor tabs, breadcrumbs, gutter line numbers, cursor/selection, syntax, diagnostics, outline on demand, modification markers, language-service state, and terminal access from the collapsed bottom tool header.

### Git

Show branch divergence, conflicts, staged/unstaged groups, recent commits, selected hunk metadata, and a realistic unified diff. The acting authority and Git binary context remain visible. Stage/commit are execution actions and obey trust.

### Environments

Show the ordered local → SSH → devcontainer chain, host verification, environment epoch, image/source, rebuild state, capabilities, trust scope, and an event log. A rebuilding final node must not imply that earlier authority layers are unavailable.

### Ports

Show service, source port, host listener, bind scope, lease, state, and action. Default host binds are loopback-only. Bind failures say what address collided and what to do next; no public listener is inferred or substituted.

## 8. Terminal, Problems, and Services

- Bottom tabs behave as a real tablist and switch visible tabpanels.
- Terminal capture reads exactly: `TERMINAL CAPTURED · Esc releases`. Enter or click captures; Esc releases. Capture state is repeated in the mode/status area.
- Problems show severity, file/location, cause, and a next action where available.
- Services show operational states for language services, SSH, devcontainer lifecycle, and port forwarding.
- Hidden bottom panels continue to report critical failure/capture state in the status bar.

## 9. Destructive and trust-boundary actions

Confirm only destructive actions and trust-boundary grants. The modal names object, authority, effective hash, capability, and effect:

> Grant `Git commit + hooks` on `DEVCONTAINER hermito-rust` for config `7bd1…`?

The safe choice starts focused. Exact verbs only (`Grant execution`, `Discard hunk`, `Revoke execution`), never “OK”. Trust Review is keyboard complete and does not grant by opening. Re-type only for irreversible remote deletion or overwriting uncommitted work; reversible Git actions prefer Undo.

## 10. Status feedback and copy

Use short operational sentences: what happened, where, and the next available action.

| Situation | Copy pattern |
|---|---|
| View switch | `Git opened. Authority: DEVCONTAINER hermito-rust.` |
| Busy | `Rebuilding dev container on SSH build-07… 18s.` |
| Trust block | `Commit blocked: this authority is inspect only. Review trust to allow execution.` |
| Connection loss | `SSH connection lost. Buffers are safe locally; remote terminals are Lost. Reconnect, then start a new terminal.` |
| Empty Git | `Working tree clean. No changes to stage.` |
| Terminal capture | `Terminal captured input. Press Esc to return to the workbench.` |
| Port success | `Forwarding container 3000 → host 127.0.0.1:43127.` |
| Port error | `Port 9090 could not bind to 127.0.0.1: already in use. Choose another local port.` |

Avoid “Something went wrong”, “Are you sure?”, unexplained codes, blame, celebratory toasts, and punctuation-heavy alerts. Persistent status feedback is preferred; temporary announcements are reserved for completed direct actions.