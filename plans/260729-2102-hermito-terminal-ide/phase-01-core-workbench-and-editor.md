---
phase: 1
title: Core Workbench and Editor
status: completed
priority: P1
dependencies: []
effort: L
---

# Phase 1: Core Workbench and Editor

## Overview

Bootstrap the Cargo workspace and implement the host-side core of the Hermito modular monolith. Deliver a non-blocking Ratatui/Crossterm TUI application that restores and persists a full workbench layout (toolbar, authority path, left/right tool stripes, primary and contextual tool windows, editor area, bottom tool header, status bar), manages editor buffers with Ropey-backed storage and monotonically increasing document revisions, performs exact byte/cell/grapheme coordinate conversions, integrates Tree-sitter for incremental syntax highlighting using curated pinned grammars, renders a project tree backed by host filesystem reads, and provides a trust-aware UI shell that renders authority state and disables execution affordances when in inspect-only mode. Add atomic revisioned dirty-buffer journal/checkpoint for saved and untitled dirty buffers (with recovery, acknowledgement, and compaction) that is restored before any authority reconnect. All workspaces and authorities start InspectOnly; first-run trust defaults to InspectOnly with explicit `GrantTrust` action required before execution affordances are enabled. Implement platform signal (SIGINT/SIGTERM/SIGHUP on Unix, console close on Windows) and explicit shutdown path that restores Crossterm raw mode, alternate screen, and mouse capture exactly once via a single owned guard; do not rely on Drop for restoration.

## Context Links

- `/Users/huynguyen/Personal/Hermito/docs/technology-stack.md` (approved crates, architecture rules, release gates)
- `/Users/huynguyen/Personal/Hermito/docs/design-guidelines.md` (workbench anatomy, authority treatment, selection/focus/keyboard model, tool-window behavior, color tokens, terminal constraints)
- `/Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/plan.md`
- `/Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-02-terminal-and-execution-transport.md` (downstream dependency)

## Requirements

- Cargo workspace must be created at `/Users/huynguyen/Personal/Hermito/Cargo.toml` with a single host crate entry point under `crates/hermito` (modular monolith pattern; `hermito-protocol` and `hermito-remote` crates added only in later phases where transport boundaries require them).
- Application must launch under native macOS, Linux, and Windows using only approved stack components.
- Crossterm exclusively owns raw mode, alternate screen, mouse capture, resize events, bracketed paste, and all terminal writes. Ratatui renders only to Crossterm backend.
- Workbench layout (pane visibility, sizes in cells, active tool tabs, open editor tabs with per-tab cursor/selection/scroll, last focused landmark) must be persisted to versioned app-state records and restored on subsequent launches only after validating that referenced files still exist on the current authority.
- Editor buffers use Ropey for UTF-8 storage. Every mutation produces a new document revision. Every background result carries workspace epoch; document-derived results additionally carry document revision. Handlers reject a stale epoch or, where applicable, revision.
- Coordinate APIs explicitly distinguish canonical grapheme-start positions from arbitrary byte/UTF-16 text positions. Conversions are exact and invertible on each declared domain; display-cell-to-text conversion snaps deterministically to a grapheme start. Display coordinates never leak into the document model.
- Tree-sitter integration provides incremental parse trees and syntax highlighting for at minimum Rust, TypeScript/JavaScript, Go, and Python using pinned grammar versions. Highlighting is produced off the UI thread and applied atomically with revision checks.
- Project tree performs host filesystem directory walks (respecting `.gitignore` patterns via the `ignore` crate where it does not violate external-tool policy) and renders a collapsible tree with file icons via conventional ASCII marks. Opening a file loads it into a new or reused editor buffer.
- Trust-aware UI renders the ordered authority path with `CURRENT` plus explicit `TRUSTED`/`INSPECT ONLY`. Every authority starts InspectOnly. With Authority Path focused, `Enter` (or click on CURRENT) opens a keyboard-complete Trust Review modal naming workspace root, authority identity, and exact capability scope; `Tab` reaches `Grant execution` and `Cancel`, and granting requires focused `Grant execution` + `Enter`. A trusted authority instead offers `Revoke execution`, which takes effect immediately. The command palette exposes the same `Review authority trust…` and `Revoke authority trust` actions. No direct key silently grants trust.
- Complete keyboard model: `[F6]`/`[Shift+F6]` landmark cycling, `Tab` within landmark/modal, `Alt+1`–`Alt+4` for left tools, `Ctrl+K`/`Cmd+K` command palette, `Enter` on Authority Path for trust review, and `Esc` for topmost dismissal. Mouse supports the equivalent CURRENT-segment trust review, cursor placement, drag selection, and wheel scrolling under pointer. Focus and selection remain distinct.
- First-run defaults: Project tool window open, one editor buffer open (or empty welcome buffer), right contextual body collapsed, bottom body collapsed. No pre-opening of terminal or other execution surfaces. Atomic dirty-buffer journal is initialized empty (or with welcome buffer checkpoint) on first run.
- Layout must remain legible and functional at 80×24, 120×36, 160×45 cells and at the prototype reference sizes (1280×800, 1440×900). Dense rows use 24–28 px equivalent; tool headers 30 px equivalent.
- All colors come from the exact token table in design-guidelines.md. No direct ANSI codes outside Ratatui theme mapping.
- UI thread never blocks on filesystem, parsing, or I/O. Heavy work runs on Tokio tasks; bounded result channels carry workspace epoch and an optional document revision required for document-derived work.
- On abnormal termination (panic, SIGTERM, etc.) and on normal signal/console-close the terminal must be restored to cooked mode before process exit. Restoration occurs exactly once via the owned shutdown path.
- Dirty-buffer durability uses a dedicated journal worker. Every mutation publishes the latest `(doc_id, revision, content/delta, workspace_epoch)` without filesystem work on the UI thread. If the bounded worker queue is full, `App` retains/replaces one pending checkpoint per document and retries on subsequent event-loop ticks; it never drops the newest revision or marks it durable. A revision becomes crash-recoverable only when the worker acknowledges the durable replace sequence. Clean/signal shutdown stops new edits and awaits pending-map plus worker-queue flush; SIGKILL/power loss guarantees only the latest acknowledged revision. A file save carries its revision; only a matching durable save permits compaction.
- Explicit platform shutdown path: Unix signals and Windows console close are registered at startup. Clean exit, signal/console-close, top-level event-loop panic, and supervised fatal-task failure all converge on one owner that consumes `TerminalGuard`, disables raw mode, leaves the alternate screen, disables mouse exactly once, flushes pending journal acknowledgements, then exits or propagates the panic. No `Drop` implementation performs restoration.

## Architecture

The host executable is one Tokio runtime with `App` exclusively owned and mutated by the main event loop—no shared `Arc<Mutex<App>>`. Crossterm input and all filesystem/parser workers send bounded typed messages; workers receive immutable Rope/layout snapshots and return epoch/revision-tagged results. Ratatui draws borrow the current event-loop state directly.

Key subsystems:
- `layout` owns the persistent geometry (pane rects in cells, visibility flags, active tab indices) and produces a `WorkbenchLayout` snapshot that survives across environment epochs.
- `buffer` owns `ropey::Rope` instances keyed by document id, each carrying a `u64` revision counter. Edits go through a transactional `apply_edit(rev, edit: TextEdit)` that rejects stale revisions. Dirty state is additionally written to an atomic revisioned journal (see below).
- `coordinate` provides pure functions: `cell_to_byte`, `byte_to_cell`, `grapheme_to_cell`, `cell_to_grapheme`, `lsp_position_to_byte` (baseline; full LSP later), all operating on a `Rope` snapshot plus syntax tree metrics.
- `syntax` owns a `tree_sitter::Parser` pool per language. On buffer edit, an incremental `InputEdit` is computed and the parse is updated on a worker task. Highlight queries produce `Vec<HighlightSpan>` tagged with revision.
- `project` performs read-only host fs walks under the current workspace root, caching a tree model updated only on explicit refresh or fs events (later).
- `ui` contains Ratatui `Widget` implementations: `Workbench`, `EditorView`, `ProjectTree`, `AuthorityPath`, `ToolStripe`, `StatusBar`. Rendering is a pure function of current `App` snapshot.
- `input` maps raw `crossterm::event::Event` to high-level `Action` (cursor move, selection extend, pane resize drag, focus landmark, open file, etc.).
- `persistence` serializes the minimal layout + open buffers metadata (paths, revs, cursors, scroll offsets, pane state) to a versioned TOML or JSON file under the platform config dir (e.g. `~/.config/hermito/state.v1.toml`). On restore, each referenced path is stat'ed; missing files drop the tab silently. Dirty-buffer journal is restored before layout and before any authority reconnect.
- `event_loop` owns the main `select!` over input, timer ticks, and background result channels. Every handler validates workspace epoch and validates document revision when the result derives from a buffer.
- `journal` owns a dedicated bounded writer and acknowledgement channel. It exposes `try_checkpoint`, `ack_save`, `flush`, `compact`, and startup-only `recover`. `App` owns a latest-per-document pending map for queue backpressure and retries without blocking. The worker may coalesce queued entries per document, acknowledges only after durable replace, and never performs filesystem I/O on the UI thread.
- `terminal_guard` owns the single Crossterm state (raw mode, alternate screen, mouse). Exposes `restore(self)` (consuming) that performs the three disables exactly once. Signal handlers and main exit both funnel through one call site that consumes the guard.

Authority state is represented as an ordered chain (Local | SSH | DevContainer) with a `current` pointer and a per-authority `TrustLevel { Trusted, InspectOnly }`. Every authority (including initial Local on first run) starts InspectOnly. `GrantTrust` is an explicit user action that persists the Trusted state for that authority id. Trust decisions are persisted per workspace root. Journal recovery occurs before any authority state is used for execution decisions.

No raw PTY bytes, no child processes, no network, no Git, no LSP servers in this phase.

## Related Code Files

All paths are absolute under `/Users/huynguyen/Personal/Hermito`.

Workspace and crate manifests:
- `/Users/huynguyen/Personal/Hermito/Cargo.toml`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/Cargo.toml`

Core entry and runtime:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/main.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/lib.rs` (public application/harness surface; `main.rs` is a thin binary entry)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/app.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/event_loop.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/action.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/layout.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/persistence/state.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/persistence/journal.rs`

Editor and document model:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/buffer.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/coordinate.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/document.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/edit.rs`

Syntax:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/syntax/mod.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/syntax/tree_sitter.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/syntax/highlight.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/grammars/tree-sitter-rust/Cargo.toml` (vendored pinned grammar; similar for typescript, go, python)

Project and filesystem:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/project/mod.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/project/tree.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/project/fs.rs`

UI shell and widgets (Ratatui):
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/mod.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/workbench.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/toolbar.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/authority_path.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/tool_stripe.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/tool_window.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/editor.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/project_tree.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/status_bar.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/theme.rs`

Input and behavior:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/input/mod.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/input/crossterm_adapter.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/input/handler.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/input/mouse.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/input/keyboard.rs`

Terminal and crossterm ownership:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/terminal/mod.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/terminal/crossterm_host.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/shutdown.rs` (platform signal + console-close handler; owns and consumes TerminalGuard exactly once)

Configuration and first-run:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/config/mod.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/first_run.rs` (initializes journal with InspectOnly authority defaults)
## Implementation Steps

1. Create a virtual workspace at `/Users/huynguyen/Personal/Hermito/Cargo.toml` with `members = ["crates/hermito"]` and resolver 2; do not create an optional root package. Put the reusable application in `crates/hermito/src/lib.rs` and the thin executable in `src/main.rs`, so package-owned integration tests can import the real workbench.

2. Declare both `[lib]` and `[[bin]]` (`hermito`) in `crates/hermito/Cargo.toml`, edition 2021, with the approved direct dependencies. Spike exact versions/features and gate continuation on a library build plus basic event loop on native macOS, Linux, and Windows.

3. Implement `lib.rs::run()` to initialize Tokio, shutdown handlers, TerminalGuard, journal recovery, App, and the event loop inside the owned panic boundary. `main.rs` only calls `hermito::run()` and reports the restored error. Normal exit, signal/console-close, panic, and fatal worker failure all reach the same consuming restore path; no `Drop` restoration.

4. Implement core state with `App` owned only by the event loop: layout, open buffers, current buffer, authority chain/trust, focus, journal handle, and a latest-per-document pending-checkpoint map. `apply_action` mutates state; `retry_pending_checkpoints` uses non-blocking sends on each tick; acknowledgement messages update `last_checkpointed_revision`. Journal recovery populates buffers before `App` becomes drawable.

5. Implement `/Users/huynguyen/Personal/Hermito/crates/hermito/src/event_loop.rs`: spawn a dedicated thread for `crossterm::event::read()` (or async poll), forward `Event` to a bounded channel. Main task does `tokio::select!` over input, background result receivers, and a 16 ms tick for cursor blink and layout animation (if any). Dispatch to `input::handle_event` then `app.apply_action`. After each apply, request a Ratatui draw via a render channel or direct `terminal.draw(|f| ui::workbench::render(f, &snapshot))`.

6. Implement layout system in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/layout.rs`: `WorkbenchLayout` struct with `Rect` (cell units) for every pane, visibility bools, active indices, and split ratios. Provide `resize(width, height)`, `drag_separator(id, delta)`, `toggle_pane(pane_id)`, `set_active_tab(pane, tab)`. All operations keep editor area >= 40 cells wide when possible; collapse context first, then bottom, then primary left.

7. Implement versioned layout state plus a dedicated DirtyJournal worker. Startup recovery is synchronous before the TUI/event loop starts. Runtime edits use `try_checkpoint`; on queue-full, `App` replaces the document's pending payload with its newest revision and retries each tick. The worker may coalesce superseded queued revisions for the same document and acknowledges only after write/fsync/rename/directory-fsync. Surface `Journal lagging` while any pending payload exists. Clean/signal shutdown stops edits and drains both the pending map and worker queue before terminal restoration. Compact only after a matching durable file-save acknowledgement.

8. Implement the Rope-backed document model. `Buffer` carries rope, revision, language, optional path, dirty state, and `last_checkpointed_revision`. `apply_edit` validates the caller revision, mutates the rope, increments revision, and returns an immutable checkpoint payload for `App` to `try_checkpoint` or retain in its pending map. Background workers receive immutable Rope snapshots, never mutable buffer access. Saved and untitled buffers use stable document IDs. Property tests interrupt each journal durability boundary and prove recovery returns the latest acknowledged revision exactly.

9. Implement coordinate conversions in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/coordinate.rs` as pure functions over `&Rope` snapshots and a syntax-metrics cache. `CellPos` maps canonically to grapheme starts; `cell_to_byte` snaps to that grapheme-start byte, while a separate text-position type represents intra-grapheme byte/UTF-16 positions needed by editing and LSP. Provide `byte_to_cell`, `cell_to_byte`, `cell_to_grapheme`, `grapheme_to_cell`, `line_col_to_byte`, and `byte_to_line_col`. Handle CRLF, zero-width code points, combining marks, emoji, and wide CJK text using unicode-segmentation + Ropey. Property tests require inverse round trips only on the declared canonical domain and separately test stable snap behavior for every valid byte boundary.

10. Implement Tree-sitter integration in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/syntax/tree_sitter.rs`: load precompiled (or source) grammars for rust, typescript, go, python from vendored paths under `grammars/`. Maintain one `tree_sitter::Tree` per buffer. On edit, compute `InputEdit` from the rope delta, call `tree.edit(...)`, then `parser.parse(old_tree, ...)` on a worker. Expose `highlight_spans(rev) -> Vec<(ByteRange, HighlightKind)>` using compiled queries. Gate full integration on spike proving < 50 ms incremental parse for 50 kB file on reference hardware.

11. Implement project tree in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/project/tree.rs` and `fs.rs`: `ProjectTree` model is a recursive `Vec<Entry { name, kind: File|Dir, children, is_expanded }>` built by walking from workspace root using `std::fs::read_dir` + `ignore` crate for filtering. Render uses `ui/project_tree.rs` with Ratatui `List` or custom widget. Double-click or Enter on file entry opens or focuses the corresponding buffer. Root is always the Local workspace root for phase 1.

12. Implement Authority Path and Trust Review UI. `Action::ReviewTrust` opens a modal containing exact authority/workspace/capability scope; only the modal's `Grant execution` action emits `GrantTrust`. CURRENT click or `Enter` while Authority Path is focused opens it; command palette exposes the same flow. For trusted authorities the same surfaces emit immediate `RevokeTrust`. `Esc`/Cancel makes no change. Inspect-only execution widgets remain disabled with an exact reason.

13. Implement full input mapping, including F6 landmark cycling, Authority Path Enter, Trust Review Tab/Shift+Tab/Enter/Esc, command-palette trust actions, mouse CURRENT click, editor selection, pane-specific wheel handling, and focus restoration.

14. Implement editor widget in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/editor.rs`: renders gutter (line numbers), text using syntax spans, cursor, selection ranges, scroll viewport. Cursor is drawn as block or bar according to mode stub. Scroll offset is stored per buffer in layout. Use `ratatui::text::Line` / `Span` with style from theme.

15. Wire workbench rendering in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/workbench.rs`: top-to-bottom: toolbar (stub), authority_path, horizontal split (left stripe + primary tool window + editor area + right stripe), optional bottom header. Use `Layout` constraints with cell units. Active pane receives focus ring.

16. Add first-run and config bootstrap in `first_run.rs` and `config/mod.rs`: on absence of state/journal, synthesize the default layout (Project open 28 cells wide, editor filling the remainder, context and bottom collapsed) with Local InspectOnly. Initialize an empty journal or checkpoint the welcome buffer. Write minimal `config.toml` with `theme = "default"`. Open no execution surface.

17. Implement explicit platform shutdown in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/shutdown.rs` and integrate it in main: register Unix SIGINT/TERM/HUP and the Windows console handler, retain one `TerminalGuard`, and funnel clean exit, signal/console-close, top-level event-loop panic, and supervised fatal-task failure through `restore_once(guard)`. The function disables raw mode, leaves the alternate screen, disables mouse, and flushes journal acknowledgements exactly once before exit or panic propagation. No `impl Drop` performs restoration. Add signal, console-close, and render-panic regression tests.

18. Build and run matrix on macOS arm64, Linux x86_64, Windows (via cross or native). Verify no raw bytes ever written except through Crossterm, no blocking calls on the render path.

## Success Criteria

- `cargo run --package hermito` (or equivalent after workspace setup) launches a functional TUI on macOS, Linux, and Windows without panic or terminal corruption.
- On first launch: authority path shows `LOCAL · <host> CURRENT INSPECT ONLY`, Project stripe button active with tool window open, one editor area visible (empty or minimal buffer), right and bottom bodies collapsed. `GrantTrust` action is available and required to enable any execution affordances.
- Recovery restores dirty journal buffers before layout. A recovered dirty buffer whose former backing file is missing remains visible as `Recovered · <filename>` with no writable path until Save As; only non-journal layout tabs with missing files are dropped. Normal quit/SIGTERM flushes all accepted edits before restoration.
- Mouse left-click inside editor moves cursor to nearest valid grapheme cell; drag produces visible blue selection that updates live; wheel over editor scrolls only editor; wheel over project tree scrolls tree independently.
- `[F6]` cycles visible landmarks in the design order: toolbar, authority path, left stripe, primary tool window, editor/work area, contextual pane when open, right stripe, bottom pane/header, status bar. The collapsed-default sequence has eight landmarks; opening context makes nine. `Shift+F6` reverses. Focus is a distinct high-contrast outline, never confused with selection fill.
- `Alt+1` opens/activates Project tool window; `Alt+2` etc. for other left tools (stubs render but do nothing).
- Typing in editor updates the buffer rope, increments revision, marks dirty (asterisk in tab), and re-renders highlighted text within one frame.
- Loading a 5 000-line Rust file produces visible Tree-sitter syntax colors (keywords, strings, comments) within 200 ms of open; subsequent small edits re-highlight incrementally without full reparse.
- From keyboard only, focus Authority Path, press Enter, review scope, activate `Grant execution`, and observe TRUSTED plus enabled controls; invoke `Revoke authority trust` and observe immediate INSPECT ONLY. Cancel/Esc never changes trust. Mouse and command-palette routes produce the same review state.
- Paste 10 kB of mixed Unicode (emoji, CJK, combining, RTL markers) into buffer: every grapheme occupies correct display cells, no corruption, cursor and selection remain accurate after paste.
- Rapidly resize terminal window 30 times (including to 80×24 and 160×45): layout recomputes without panic, no dropped events, editor content reflows correctly, no text truncation at edges.
- Press `Esc` while a modal stub is open: modal closes and focus returns to invoker.
- Application terminates cleanly on Ctrl-C / SIGTERM / window close / console close: terminal is left in cooked mode via the single owned restore path; no residual raw state visible in parent shell. Restoration occurs exactly once regardless of signal vs normal exit path.
- After a reported journal acknowledgement, a full power-cycle simulation recovers that exact revision. The plan makes no false guarantee for edits still queued when an uncatchable SIGKILL/power loss occurs.
- Kill the process after acknowledged checkpoints for saved and untitled buffers: restart restores the exact acknowledged content/revision before UI paint or authority use. Separately kill with a checkpoint still queued: recovery returns the prior acknowledged revision and reports that the later revision was never durable.
- Platform signal + console-close: send SIGTERM (Unix) or close console (Windows) while TUI running; Crossterm is restored to cooked mode exactly once (observable: parent shell has normal input, no raw state); no duplicate restores or panics; journal acks flushed before exit.

## Test and Validation Matrix

| Platform          | Launch + First-Run (InspectOnly + GrantTrust) | Journal Recovery (dirty saved/untitled before layout) | Layout Restore | Mouse/Scroll | Keyboard Focus | Editor + Tree-sitter | Unicode + Large Paste | Rapid Resize + Signal | Clean Exit (exact-once restore) |
|-------------------|--------------------|----------------|--------------|----------------|----------------------|-----------------------|-----------------------|------------|
| macOS 15 arm64    | Pass (INSPECT ONLY default, GrantTrust enables exec) | Pass (recover pre-layout, ack+compact) | Pass (stat validation) | Pass (click/drag/wheel independent) | Pass (F6 cycle, Alt+1-4, Esc) | Pass (rust.ts 10k lines <200 ms) | Pass (emoji+CJK+combining) | Pass (SIGWINCH, 50 resizes) | Pass (single owned restore on SIGTERM/console; no Drop) |
| Linux (Ubuntu 24) x86_64 | Pass            | Pass           | Pass         | Pass           | Pass                 | Pass                  | Pass (SIGWINCH)       | Pass (single owned restore) |
| Windows 11        | Pass (ConPTY not exercised) | Pass      | Pass      | Pass (mouse capture) | Pass       | Pass                 | Pass                  | Pass (resize events)  | Pass (console handler + single restore) |

Failure scenarios exercised and required to produce observable correct behavior:
- Missing non-dirty layout file → tab dropped. Missing file with an acknowledged dirty journal entry → visible `Recovered · <filename>` buffer retains content/revision and requires Save As; no recovered work is hidden.
- Stale revision from background task → ignored, no UI corruption.
- Oversize paste (>1 MiB) → bounded buffer growth with ropey; UI remains responsive.
- Tree-sitter parse failure on malformed input → fallback to plain text, logged at debug level.
- Terminal smaller than minimum (60×20) → graceful clamp, status message, no panic.
- Corrupt state file → fall back to first-run defaults (InspectOnly), backup corrupt file.
- Panic in render path → single restore path restores terminal exactly once before unwind.
- Concurrent edit + highlight apply → revision check discards stale highlight.
- SIGKILL after an acknowledged dirty checkpoint → exact revision/content recovers. SIGKILL while a newer checkpoint is deliberately held before fsync → prior acknowledged revision recovers and the harness proves no partial/corrupt entry is accepted.
- First-run or new authority always starts InspectOnly; `GrantTrust` is the sole transition; execution stubs remain disabled until granted.
- Corrupt journal entry → recovery skips bad entry, logs at debug, continues with good buffers; no crash.
- Duplicate signal delivery or normal + signal exit race → restore executes exactly once (guard consumed); no double-disable or hang.

## Risk Assessment

- Crossterm raw-mode + alternate-screen leaks on early panic or double-init: mitigated by single `TerminalGuard` owned and consumed exactly once by the shutdown path (signal handler or main exit); no Drop performs terminal restore.
- Dirty-buffer journal durability/atomicity: a dedicated writer acknowledges only after temp-file write + file fsync + atomic rename + parent-directory fsync (Windows durable-replace equivalent). UI remains non-blocking; clean shutdown flushes. Uncatchable failure can lose only revisions not yet acknowledged, which the status/test contract states explicitly.
- Tree-sitter incremental edit calculation off-by-one on complex Unicode: mitigated by exhaustive round-trip property tests on coordinate module + grammar-specific fixtures.
- Large project tree walks block UI: mitigated by off-thread walk with result channel + cancellation token; UI shows "loading…" discrete state.
- Persisted layout references absolute paths that become invalid across machines: mitigated by path validation on restore and graceful degradation.
- Ratatui layout constraint drift under repeated resize: mitigated by cell-exact `Rect` arithmetic and golden master snapshot tests at reference sizes.
- Focus model complexity leads to unreachable states: mitigated by exhaustive state machine tests and landmark enumeration.

## Security Considerations

- Phase 1 writes only application state and the dirty journal under the platform config directory. Journal payloads are user buffer content and may themselves contain secrets; files/directories therefore use owner-only permissions/ACLs, are never included in diagnostics, and are deleted after a matching durable save plus compaction.
- App-state contains layout, paths, cursor/scroll metadata, and trust records; the journal separately contains dirty content and revisions. Neither collector reads credential stores or environment variables, but Hermito does not misclassify source text as non-sensitive.
- All workspaces and authorities (Local first-run included) start InspectOnly. `GrantTrust` is the explicit, only transition to Trusted; trust UI gates future execution affordances. Future phases must re-validate trust before any authority-crossing operation.
- Terminal restoration occurs exactly once via owned consuming `restore_once` path (signal or normal exit) and prevents host shell from inheriting raw mode or mouse capture.
- All external crates are pinned via Cargo.lock after verification; no dynamic loading or plugin execution.
- Unicode handling must not allow display spoofing that could mislead authority path rendering (use only ASCII marks + token colors for authority).

## Next Steps

- Phase 2 integrates local PTY via portable-pty, TerminalSurface boundary, system OpenSSH bootstrap (hardened per contract), hermito-remote helper multiplexing, typed authority requests with versioned extensible top-level dispatcher, bounded process supervision, non-transparent PTY Lost/new-session behavior, frame caps, verified TUF helper prerequisite, hostile output containment, and journal-aware reconnect (journal already restored). The editor and layout surfaces from Phase 1 become consumers of `TerminalSurface` for bottom-pane terminals. Trust starts InspectOnly; explicit grant required.
- Subsequent phases add LSP (phase 3), Git (phase 4), containers (phase 5), and ports (phase 6) behind the same authority and trust abstractions.
- After Phase 2, add integration tests that exercise a real local shell session inside the bottom terminal while the editor remains interactive; journal recovery is exercised before any reconnect test.
