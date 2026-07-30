---
title: Phase 2 Terminal and Execution Transport
phase: 2
date: 2026-07-30T16:45:00+07:00
status: completed
plan: plans/260729-2102-hermito-terminal-ide/plan.md
tags: [phase-2, journal, implementation, review]
---

# Phase 2: Terminal and Execution Transport

## Context
- Vertical slice per `plans/260729-2102-hermito-terminal-ide/phase-02-terminal-and-execution-transport.md` and `plan.md`.
- Contracts: `docs/technology-stack.md` (portable-pty, hermito-protocol framing, hermito-remote stdio service, tough TUF, system OpenSSH only); `docs/design-guidelines.md` (inert TerminalSurface, authority trust starts InspectOnly + explicit GrantTrust, epoch/rev tagging, authority path).
- Scope: local + SSH PTY/file/exec transport surfaces only; bottom terminal pane + capture in Phase 1 workbench; epochs/journal/trust from Phase 1 reused. No LSP, Git, containers, forwarding.
- One Authority trait; TerminalSurface is the sole display surface (raw child bytes never reach Crossterm).

## What Happened
- Core delivered behavior:
  - `TerminalSurface` (bounded 500x500 / 250k cells, scrollback, cursor, title, truncated flag) + `VtParser` (minimal VT subset): `feed` mutates only cells via put/newline/csi/sgr/osc (title/hyperlink); drops all OSC 52, DCS, unknown escapes, controls, binary, long seqs (mark_truncated); surface snapshot-copied via `Arc<RwLock<>>` to UI render only (cells + styles).
  - Local PTY: `portable_pty` (Unix PTY / Windows ConPTY only); `LocalPtySession` owns reader (parser + budget 100MiB -> truncate+kill), writer, `PtyProcessTree`; resize syncs pane rect.
  - Remote: `SshAuthority` activates only after TUF + trust; `RemotePtySession` over `Multiplexer`; concurrent PTY/FS/Process streams.
  - `hermito-protocol`: `CURRENT_VERSION` 1.0; `Message` (Hello + Pty/Fs/Process + reserved Lsp/Git/Container/Relay); fixed `FRAME_MAGIC` + header (ver/class/len) parsed first; `FrameLimits` + `AggregateBudget` (semaphore) enforce class/agg caps before any Vec alloc or dispatch; negotiate rejects major mismatch, validates frames.
  - `hermito-remote`: thin Tokio stdio service; registers streams, dispatches to local pty/exec/fs, bounded queues, EOF shutdown kills children; no TUF/signer inside.
  - Authority: trust gate (`InspectOnly` error before spawn/exec); epoch validate on every op (ws/env match or `StaleEpoch`); `ExecutionContextV1::AuthorityRoot` only (DevContainer reserved); document_revision=None for PTY.
  - SSH hardening: `SshBootstrap` builds `-F none`, `StrictHostKeyChecking=yes`, `IdentitiesOnly=yes`, `IdentityAgent=none`, publickey-only, explicit `-i`/`CertificateFile`, constructed env only; `ssh-keyscan` bounded for fp candidates only; user explicit accept into `KnownHostsStore` (rejects replacement key with `HostKeyChanged`); one-shot `askpass` (authority-bound, expires, zeroize, no persistence).
  - TUF + install: `TufVerifier` (pinned tough, full root/timestamp/snapshot/targets chain + expiry/rollback, offline cache option); `HelperInstaller` does SFTP temp upload + readback size/hash + atomic same-dir rename to digest name + final verify; launch only the absolute verified path; revalidate on reuse.
  - Lifecycle/supervision: `ProcessSupervisor` + `PtyProcessTree` (unix `process_group(0)` + SIGTERM/grace/SIGKILL on pgid; win job + kill_on_close); explicit `CancellationToken` per op; reader/writer always drain + wait + join on cancel/exit/loss; byte budgets + wall timeout + truncate flags.
  - Loss/reconnect: transport loss marks `PtySessionState::Lost`, bumps `environment_epoch` via fetch_max, drains streams; UI shows "Lost · request new session" (never resume); journal (Phase 1) already restored editor state; new terminal only after re-activate + re-grant.
  - Workbench: `TerminalSnapshot` (state + optional surface handle + captured flag); capture mode forwards keys only while Running; Esc -> release to editor; pane resize -> pty resize; authority path shows LOST segment.
  - Input/FS/Exec bounds: 64KiB input chunks, 4MiB default stdout/stderr, 16MiB FS, etc.

- Observed verification (per main agent): cargo fmt --all -- --check PASS; cargo clippy --workspace --all-targets -- -D warnings PASS; cargo test --workspace --all-targets PASS (122 tests, 7 suites); focused hermito-remote protocol_service PASS.
- Hard lifecycle/backpressure/security issues found during review and fixed:
  - Stale launch during rapid authority/terminal switch or loss: launch_id + epoch guard in `attach_terminal`/`fail_terminal_start`; mismatch path does explicit cancel + thread join_reader (prevents attach of dead session).
  - Aggregate budget backpressure could drop next frame or wedge: semaphore permit held until decode/drop reclaims; `frame_aggregate_budget_waits_without_losing_the_next_frame` test; pending request cap + try_send returns `Backpressure` (UI surfaces error, no hang).
  - PTY reader budget hit or transport loss left zombies/FDs: reader always cancels + terminate + kill + wait even on budget; `PtyProcessTree` RAII Drop + explicit paths on all platforms; join threads on close/cancel.
  - Remote PTY streams crosstalk or loss not propagated: per-stream mpsc channels in mux; `mark_lost` drains pty_streams with synthetic Lost, sets alive=false; remote reader task sets Lost state on Lost message.
  - TUF/helper substitution or pre-placed binary: full chain verify is hard gate before SFTP; installer always temp+readback+hash+rename+reverify; absolute digest path only; no PATH, no manual override; tampered fixture test asserts rejection naming step.
  - SSH ambient config/agent/default key or weak host checking: every invocation constructs identical -o flags + fixed PATH + no agent; keyscan output capped + explicit fp match required; known_hosts accept is idempotent but rejects changed key; askpass never inherited.
  - VT parser escape leakage or unbounded on malicious output: state machine + MAX_CSI/OSC/DCS + ignore OSC 52 + put only non-control + title truncate 256; surface clips dims/budget and marks truncated; test `vt_drops_osc52_binary_controls_overflow_sets_safe`.
  - Epoch drift after reconnect: every request validates current env_epoch; loss path does fetch_max before clearing mux; stale always errors at authority layer (no silent reuse).
  - Single in-flight terminal start violated: channel capacity 1 + launch_in_flight flag + supersede on new OpenTerminal; old result drains before new dispatch.
  - Per-stream PTY overload isolation: outbound queue/budget refusal truncates and cancels only the producing PTY; the shared transport remains live, while actual writer failure still cancels it.
  - Automatic host-key updates: `UpdateHostKeys=no` is part of the shared `ssh`/`sftp` options, so OpenSSH cannot add unapproved server keys to Hermito's isolated known-hosts file.
  - Required document revisions across local/SSH/helper FS: read/write now reject `document_revision: None` (`MissingDocumentRevision` locally or `InvalidRequest` remotely); PTY messages remain revisionless by contract.
  - Helper SSH stderr lifecycle: `spawn_ssh` sends helper child stderr to `Stdio::null()`, preventing an unread pipe from stalling the multiplexed transport; bounded remote process execution retains explicit stdout/stderr capture.
  - Qualified helper allowlist: configured activation accepts only `hermito-remote-{x86_64,aarch64}-unknown-linux-musl`; other signed target names are rejected before TUF verification or launch.

- Iterated via Phase2*Reviewer/Closure/Definitive/FinalCodeReview + FinalBlockerTester PASS + FinalBlockerReviewer PASS.

## Reflection
- Inert surface + snapshot sharing + dedicated reader thread achieved the "raw bytes never reach host terminal" contract cleanly; RwLock acceptable because mutation is one-writer.
- Versioned dispatcher + class caps + early header parse is the right foundation for Phase 3+ extensions without duplicating framing.
- Explicit "no resume" + Lost + epoch bump + journal precedence is the correct failure model; review forced it to be observable and non-silent everywhere.
- Backpressure everywhere (semaphore, bounded mpsc, try_send errors) protects memory but callers must treat NotRunning/Backpressure as terminal for that session.
- Process tree + cross-platform kill paths were the highest-risk area; unix pgid + win job + always-join pattern survived the contract matrix.
- TUF + hardened installer + readback is heavy but necessary; fixture pipeline keeps test keys visibly TEST_ONLY and non-production.

## Decisions
- `TerminalSurface` is inert by construction: public API exposes only cells/scrollback/cursor/title/truncated; parser is the only mutator and drops everything unsafe; Crossterm used solely for host keyboard -> PTY bytes and final paint.
- Protocol is versioned + bounded: top-level `Message` enum extensible; every frame starts with fixed 12-byte header (magic+ver+class+len); limits checked and aggregate reserved before payload allocation or serde; negotiate at hello only; unknown class rejected in TryFrom.
- Context/epochs: every `RequestEnvelope` carries `workspace_epoch` + `environment_epoch` (doc rev only for doc-derived); `AuthorityRoot` vs reserved `DevContainer`; loss/transport failure always advances env epoch and forces new sessions.
- Hardened SSH/TUF helper: TUF verification (complete chain via pinned client) is prerequisite to any remote activation or SFTP; helper bytes placed only via upload-temp + readback-hash + atomic digest rename + final verify; `ssh`/`sftp`/`ssh-keyscan` invocations use fixed argv, `-F none`, `StrictHostKeyChecking=yes`, `IdentitiesOnly=yes`, no agent, and explicit identity only; key acceptance requires explicit fingerprint match; encrypted keys use one-shot local askpass endpoint only.
- Cancellation and process-group cleanup: every PTY/exec owns a `CancellationToken`; supervisor/reader always do graceful SIGTERM + bounded wait + SIGKILL + wait + join; unix uses `process_group(0)` + negative-pgid kill; Windows uses job object + kill_on_close; Drop and panic paths converge on same cleanup; no ambient FDs/cwd/env inherited.
- One terminal launch in flight; stale results explicitly cancelled+joined; capture is boolean on Running state only; Esc always releases to editor focus.

## Next
Phase 3 (language intelligence) per plan phases table and phase-03 spec. Phase 2 is complete per its contract, phase-02 md, and observed validation. The seven-phase plan remains in-progress.
