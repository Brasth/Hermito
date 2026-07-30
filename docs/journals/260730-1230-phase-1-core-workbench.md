---
title: Phase 1 Core Workbench and Editor
phase: 1
date: 2026-07-30T12:30:00+07:00
status: completed
plan: plans/260729-2102-hermito-terminal-ide/plan.md
tags: [phase-1, journal, implementation, review]
---

# Phase 1: Core Workbench and Editor

## Context
- Vertical slice per `plans/260729-2102-hermito-terminal-ide/phase-01-core-workbench-and-editor.md` and `plan.md`.
- Contracts: `docs/technology-stack.md` (Ropey, Ratatui+Crossterm sole terminal owner, Tree-sitter pinned grammars, epoch/rev tagging), `docs/design-guidelines.md` (cell-grid layout, authority path, InspectOnly default, landmarks, token colors).
- Scope: host monolith only; no PTY, LSP, Git, remote, containers, forwarding.

## Outcomes
- Cargo workspace (`Cargo.toml` members + `crates/hermito` lib+bin); Tokio runtime; `App` exclusively owned by event loop.
- `buffer`: `ropey::Rope` + monotonic `u64` revision; `apply_edit` transactional (stale rev reject); dirty → `try_checkpoint`.
- `coordinate`: pure fns over `&Rope` + syntax metrics; `cell_to_byte`/`byte_to_cell`/`cell_to_grapheme`/`grapheme_to_cell`/`line_col_*`; canonical grapheme starts for edits; snaps deterministic; CRLF/zero-width/combining/wide/CJK/tabs handled.
- `syntax`: incremental `tree_sitter::Parser` pool + `InputEdit`; highlight queries for rust/ts/js/go/py (vendored pinned); worker task, rev-checked apply; plaintext fallback.
- `layout`: `WorkbenchLayout` (pane `Rect`s in cells, vis flags, active tabs, per-buffer cursor/scroll); `resize`/`drag`/`toggle`/`set_active`; responsive collapse (context, bottom, left); persist/restore with stat validation.
- `journal`: bounded dedicated worker; `try_checkpoint` (UI retains/replaces latest pending on full, retries on tick); ack only after durable replace (temp+fsync+rename+dirsync); `compact` post-save match; startup `recover` (latest per-doc, skip corrupt); clean shutdown drains pending+queue.
- Trust: authority chain starts InspectOnly (incl. first-run Local); `AuthorityPath` + modal + palette; `GrantTrust` only on focused "Grant execution"+Enter (never open/Esc/Cancel); `Revoke` immediate; persisted per workspace root; exec stubs gated.
- `project`: host fs walk (`std::fs::read_dir` + `ignore`); collapsible tree; open/focus buffer by path.
- Input/UI: crossterm dedicated thread → bounded events; `F6`/`Shift+F6` landmarks (8/9), `Alt+1-4`, `Ctrl/Cmd+K` palette, authority Enter, Esc, mouse (click/drag/wheel independent); Ratatui widgets (Workbench, Editor gutter/syntax/cursor/sel, ProjectTree, AuthorityPath, ToolStripe, StatusBar); exact token theme.
- Shutdown: Unix signals + Windows console registered; single consuming `TerminalGuard` (raw/alt/mouse disable exactly once); no `Drop` restore; journal flush + state save before exit or panic propagation.
- First-run: project stripe+window (28 cells), editor (welcome buffer), context/bottom collapsed; Local InspectOnly; minimal config; journal seeded.

## Review-Driven Corrections
- **Durability**: pending map per doc (backpressure, retain newest); worker coalesces superseded; ack post-durable only; shutdown drains map+queue before restore; compaction only on matching save ack; SIGKILL semantics explicit (prior ack only).
- **Unicode byte/char domains**: coordinate pure over Rope snapshots; cell positions = grapheme-start canonical for edits (no display cells in model); separate intra-grapheme byte/UTF-16; property + contract tests for roundtrips/snaps on emoji/CJK/combining/tabs/CRLF/header/border/gutter cases; buffer apply tests confirm.
- **Bounded state machines**: coalesce tests (syntax/project/save): at most one in-flight, retain latest pending, stale result/epoch ignored, no wedge; event_loop `select!` + rev/epoch guards on every handler and result path.
- **Render/hit-test geometry**: layout rects canonical; `render_split_rects` align with hit-test rects at key sizes; bottom/required/restore roundtrips preserve fixed fields; stripe width formula single; 80x24/120x36/160x45 + ref sizes exercised.
- **Trust/save/shutdown**: trust paths never grant on open/Esc/Cancel (only focused grant); save never concurrent in-flight; shutdown reports state error only after cleanup/flush attempted; single restore across panic/signal/quit/input-disconnect; retained work submitted on disconnect.

Iterated via PhaseOneContractFix, Terminal/App/EventStructureRepair, BoundedPanicFix, Approval*Fix cycles, PostEdit* sweeps, PhaseOne*Reviewer/Approval/BoundedVerdict passes.

## Decisions
- Event loop sole mutable owner of `App`; workers receive immutable snapshots only; all results epoch/rev tagged.
- Crossterm exclusive terminal state; restoration via owned consuming guard exactly once (signal/normal/panic converge).
- Journal: worker owns all fs durability; UI non-blocking; pending replace on queue full (never drop newest rev).
- Coordinates: document model stays in byte/grapheme; display cells only in render/hit-test.
- Trust default + restore: always InspectOnly; explicit grant only; no ambient exec.
- Recovery order: journal buffers before layout restore; dirty-missing → visible Recovered buffer (forces Save As); clean-missing tabs dropped silently.
- Testing: property + contract tests for invariants; no behavioral claims beyond exercised paths.

## Verification Evidence
- artifact://298: 92 tests passed (43 lib + 49 phase1_contracts, 0.18s). Includes coordinate roundtrips/snaps/unicode, buffer checkpoint/rev/journal ack/compaction, layout rects/hit-tests/landmarks/responsive/restore, syntax incremental/unicode, trust modal (grant/revoke paths), project load/focus, save coalesce, shutdown flush/panic, mouse/keyboard, first-run, owner perms, epoch/rev rejection.
- artifact://300: release build succeeded (`Finished `release` profile [optimized] target(s) in 10.42s`).
- agent://PhaseOneBoundedVerdict: APPROVE (overall_correctness: correct, confidence 0.98). "The five prior defects are closed. Tick retries both retained journal work classes; focused-landmark state round-trips with a legacy default; restored fixed layout heights and renderer regions agree with WorkbenchLayout; and every orderly quit, shutdown, and input-worker-disconnect path submits retained work, saves state, flushes the journal, and propagates a state-save error only after flush."

## Next
Phase 2 (terminal and execution transport) as specified in phase-01 next steps and plan phases table. Phase 1 complete per contracts and evidence.
