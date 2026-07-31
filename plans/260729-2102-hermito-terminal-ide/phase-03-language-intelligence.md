---
phase: 3
title: "Language Intelligence"
status: pending
priority: P1
dependencies: ["phase-01", "phase-02"]
effort: ""
---

# Phase 3: Language Intelligence

## Overview

Implements the minimal capability-gated LSP 3.17 client in the host modular monolith. LSP spawn/probe/restart is disabled until the authority grants explicit execution trust; inspect-only authorities expose static editor intelligence (local buffer syntax/state features) plus explicit blocked reason for external language server capabilities. Routes requests through the Authority abstraction (local direct, SSH via signed helper), enforces per-document sent-version/session-generation + epoch stale-result rejection on every response and diagnostic, performs exact UTF-8 / UTF-16 / grapheme / display-cell coordinate conversions, and certifies end-to-end workflows for diagnostics, completion, hover, definition, and rename against TypeScript/JavaScript, Rust, Go, and Python fixtures using only detected installed servers on trusted authorities.

## Context Links

- docs/technology-stack.md (LSP 3.17 baseline, capability-gated client, certified languages, exact coordinate conversions, authority routing, no bundling of language servers, release-blocking gates)
- docs/design-guidelines.md (Problems pane for diagnostics, Services for language service state, authority path and trust, inspect-only constraints, editor-centered diagnostics and language state, non-blocking UI)
- plans/260729-2102-hermito-terminal-ide/phase-01-core-workbench-and-editor.md (editor buffer model, diagnostics routing, rename UI flow, Services/Problems models)
- plans/260729-2102-hermito-terminal-ide/phase-02-terminal-and-execution-transport.md (process supervision and stdio transport patterns, remote channel primitives reused for LSP proxy)
- plans/260729-2102-hermito-terminal-ide/phase-05-dev-container-orchestration.md (future extension of Authority for container LSP routing)

## Requirements

1. LSP client is strictly capability-gated: client requests only supported features in initialize; server response capabilities are inspected before any textDocument/* request; unsupported features are never invoked.
2. Supported minimal feature set for first release: textDocument/publishDiagnostics, textDocument/completion (including trigger characters and context), textDocument/hover, textDocument/definition (and declaration), textDocument/rename (prepareRename + rename producing WorkspaceEdit).
3. Server resolution is authority-local and configuration-only. Trusted Local resolves the configured executable against a controlled PATH; trusted SSH resolves it through the signed helper; Phase 5 container uses `devcontainer exec`. Each records resolved path, executable SHA-256 where readable, version probe output, argv, cwd, capabilities, and the pinned certification baseline. InspectOnly performs no PATH/version probe or spawn and reports the exact block reason.
4. All LSP operations use the Phase 2 `ExecutionContextV1` envelope and canonical `ProtocolV1::Lsp` variants. AuthorityRoot routes to Local/SSH; DevContainer routes the same variants through the canonical container dispatcher added in Phase 5. No container-specific LSP variant, client, or ledger exists.
5. Every async result/diagnostic carries originating document sent-version, session-generation, revision, workspace/environment epoch, and execution context; stale or cross-context results are rejected. Prefer server diagnostic version; apply the declared discard/refresh policy to versionless diagnostics.
6. CoordinateMapper distinguishes arbitrary UTF-8/UTF-16 text positions from canonical grapheme-start/display-cell positions. Byte ↔ UTF-16 and Rope char mappings are lossless where the source position is a valid code-point boundary; display-cell mappings snap deterministically to grapheme starts and are invertible only on that canonical domain. Exhaustive Unicode fixtures and snap invariants are release-blocking.
7. Certified fixtures exist for each language with self-contained projects exercising syntax errors (diagnostics), member access (completion), symbol info (hover), cross-symbol navigation (definition), and identifier rename (rename) including multi-file edits.
8. Document synchronization uses incremental didChange with contentChanges carrying full text or deltas; version = current revision; didOpen/didClose bracket open buffers. Per-document sent-version and session-generation ledger is maintained and reflected in frames.
9. UI thread (Ratatui event loop) never blocks; all LSP I/O, parsing, and conversion occurs on Tokio tasks; results are sent over bounded mpsc channels with sent-version + session + revision + epoch tags; editor applies only after validation.
10. Cap `Content-Length` and buffered JSON-RPC bytes before allocation/deserialization, then decode through validated DTOs before `lsp-types`. IDs, positions, versions, strings, arrays, edits, and diagnostics have explicit numeric/count/byte limits; invalid data returns a typed protocol error before editor state.
11. Unsaved buffers remain authoritative on host; language servers receive content from host memory even for remote workspaces.
12. No server is bundled, auto-downloaded, or discovered from repository config. User-owned TOML lists executable, fixed args, optional version-probe args, expected version range/digest, and language associations per execution context; Hermito reports precise missing/skewed capability details.
13. External servers use direct argv, fixed authority workspace cwd, minimal constructed environment, bounded stdin/stdout/stderr/JSON-RPC queues, and owned cancellation. Spawn requires trust for the exact workspace + authority + effective language-server config hash.
14. Phase 3 extends the Phase 2 dispatcher once with canonical `ProtocolV1::Lsp` request/response variants carrying `ExecutionContextV1`; host and helper route on that context. There is no second container LSP schema.
15. Phase 5 implements only the DevContainer execution adapter for this existing surface using `devcontainer exec`; document ledgers, DTOs, capabilities, trust, and LSP variants remain unchanged.

## Architecture

The LSP subsystem lives behind the single Authority trait. Spawn/probe/restart requires a grant scoped to workspace, authority identity, and effective server-config hash; inspect-only exposes static editor intelligence plus a blocked reason.

`LspSupervisor` owns one client per `(workspace, authority identity, ExecutionContextV1, language_id)` and manages capability intersection, document ledgers, bounded restart, and service state. `ExecutionContextV1::AuthorityRoot` remains the canonical wire context for both Local and SSH; authority identity is a host-side isolation key.

AuthorityRoot Local spawns a configured server directly. AuthorityRoot SSH sends canonical `Message::Lsp(lsp::LspV1)` frames to the signed helper. Phase 5 DevContainer routing sends those same frames and `ExecutionContextV1::DevContainer` through the helper's canonical container dispatcher, which performs fixed `devcontainer exec`; it does not add an LSP protocol.

LspClient performs JSON-RPC over the transport using lsp_types structs. 

The existing Rope-backed `Buffer` is the sole authoritative text and revision owner. It holds an authority-keyed sent-version/session-generation ledger and emits accepted-edit snapshots for bounded didChange delivery; no parallel document registry is introduced.

On every incoming publishDiagnostics/response: validate against document's sent-version + session-generation + revision + environment epoch (use server-provided diagnostic.version when present for matching; safe discard + optional refresh for versionless). Frames (local + remote) always carry epoch + session. 

CoordinateMapper (crates/hermito/src/lsp/coordinate.rs) is single source for all position math. 

App-owned diagnostic and language-service stores project filtered `NormalizedDiagnostic` and supervisor state (including InspectOnlyBlocked) into Problems and Services snapshots; the existing status count and service string remain derived summaries. Rename (and any write) applies a transactional `WorkspaceEdit` batch through Authority mutation, validating all paths, revisions, and trust before commit and never silently committing a partial multi-file edit.

### Approved Review Corrections

- Persisted execution trust is scoped to the canonical effective language-server configuration digest. A restored grant with no digest or a digest mismatch is InspectOnly; no Local or SSH probe, argv construction, or spawn may occur before the digest matches.
- `Message::Lsp(lsp::LspV1)` replaces the opaque `ExtensionMessage` LSP slot. The protocol exports one typed LSP family; the remote dispatcher and host multiplexer are updated atomically. The multiplexer correlates bounded LSP responses and independently streams bounded LSP notifications without treating them as unsupported protocol traffic.
- The authority exposes one transactional workspace-edit mutation carrying all relative paths, expected revisions, and replacements. Local, SSH protocol, and the helper validate the entire batch then stage/commit or compensate as one operation before rename is enabled.
- Integration gates live in `crates/hermito/tests` and are invoked with `cargo test -p hermito --test <name>`; fixtures are resolved from `CARGO_MANIFEST_DIR`.

- Cargo.toml (root workspace; member crates/hermito depends on ropey, lsp-types, serde_json, tokio, futures-util, unicode-segmentation)
- crates/hermito-protocol/src/lsp.rs (the sole `LspV1` request/response schema; every message carries `ExecutionContextV1`)
- crates/hermito-remote/src/lsp.rs (canonical LSP handler for AuthorityRoot; Phase 5 reuses it after the outer container dispatcher establishes the execution context)
- crates/hermito/src/authority/mod.rs (single LSP surface; Local/Ssh implementations now, DevContainer adapter contract later; trust is scoped to effective server-config hash)
- crates/hermito/src/lsp/mod.rs (LanguageId enum, re-exports, LanguageServerSpec, protocol variant adapters)
- crates/hermito/src/lsp/client.rs (LspClient: initialize, guarded requests, per-document sent-version + session-generation ledger, incoming filter by sent-version+session+revision+epoch using server diag version when present + safe policy, position conversion)
- crates/hermito/src/lsp/supervisor.rs (LspSupervisor: probe+spawn ONLY under explicit execution trust via authority; no-op stub + blocked reason for inspect-only; handshake, exit watch, bounded restart, shutdown, state for Services)
- crates/hermito/src/lsp/coordinate.rs (CoordinateMapper with exact byte/UTF-16 mappings plus canonical grapheme/display-cell snap semantics and exhaustive tests)
- crates/hermito/src/lsp/diagnostics.rs (NormalizedDiagnostic conversion from lsp_types::Diagnostic, severity mapping, routing to Problems store; ledger-aware using server version or safe discard)
- crates/hermito/src/lsp/requests.rs (CompletionProvider, HoverProvider, DefinitionProvider, RenameProvider; behind capability guard + execution trust for server calls)
- crates/hermito/src/buffer.rs (`Buffer` remains the authoritative Rope/revision owner and adds authority-keyed LSP ledger plus accepted-edit snapshots)
- crates/hermito/src/workspace.rs (Workspace, Environment { id, epoch: u64 })
- crates/hermito/src/config/language.rs (user-owned `LanguageServerConfig`: executable, fixed args, version-probe args, expected version range/digest, associations, per-context overrides; repository files cannot define executable/init options)
- tests/fixtures/lsp/typescript/package.json
- tests/fixtures/lsp/typescript/tsconfig.json
- tests/fixtures/lsp/typescript/src/app.ts
- tests/fixtures/lsp/rust/Cargo.toml
- tests/fixtures/lsp/rust/src/main.rs
- crates/hermito/tests/lsp_hostile_json.rs (negative, fractional, >u32 position, >i32 version, and oversized JSON-RPC field rejection)
- tests/fixtures/lsp/go/go.mod
- tests/fixtures/lsp/go/main.go
- tests/fixtures/lsp/python/pyproject.toml
- tests/fixtures/lsp/python/main.py
- crates/hermito/tests/lsp_integration.rs (per-language per-feature + rename verification + stale/ledger drop tests using versioned protocol)
- crates/hermito/tests/lsp_coordinate.rs (unicode matrix release gate tests)

## Implementation Steps

1. Update Cargo.toml (root workspace and crates/hermito) to add direct dependencies: ropey, lsp-types (pin after spike to version exposing full 3.17 structs), serde_json, tokio (process + io + sync), futures-util, unicode-segmentation. Do not enable proposed features until after fixture qualification. Record exact versions in Cargo.lock only after passing local + cross-platform build and test gates.
2. Replace the opaque LSP slot with one canonical `Message::Lsp(lsp::LspV1)` request/response family. Include `ExecutionContextV1`, epochs, authority identity, session generation, and document ledger fields; round-trip both AuthorityRoot and reserved DevContainer contexts. Update the remote dispatcher and host multiplexer together; do not create local/SSH/container-specific LSP wire types or a parallel protocol wrapper.
3. Implement the helper's canonical LSP handler. For AuthorityRoot, resolve/probe the user-configured server remotely only after scoped trust, record identity/version, spawn with fixed argv/cwd/minimal environment, and bridge capped JSON-RPC. Keep the handler context-neutral so Phase 5 can invoke it after container dispatch.
4. Extend Authority with `start_lsp`/send/receive using the canonical envelope. Local executes directly; SSH dispatches over Phase 2. Guard against missing trust or config-hash drift before any probe or argv construction. Declare, but do not implement here, the Phase 5 DevContainer execution adapter.
5. Extend the Phase 1 Rope-backed `Buffer`/document model with sent-version and session-generation ledgers; do not recreate or fork document ownership. Host memory remains authoritative.
6. Implement `crates/hermito/src/lsp/coordinate.rs` CoordinateMapper. Keep exact byte/Rope-char/UTF-16 conversions separate from grapheme/display-cell conversions; the latter expose deterministic snap-to-grapheme-start APIs. Test inverse laws only on valid domains plus snap idempotence for interior bytes/cells, including tabs, CRLF, combining sequences, emoji, and CJK.
7. Implement crates/hermito/src/lsp/client.rs LspClient. pending keyed by (sent_version, session, rev, epoch). Guard requests. On incoming: match sent-version/session/rev/epoch (prefer server diag version for publishDiagnostics); apply safe discard + request-refresh policy on versionless; drop mismatch. Convert positions.
7a. Implement capped JSON-RPC ingress: reject oversized `Content-Length` before buffer allocation, cap aggregate pending bytes, then validate IDs, numeric ranges, strings, arrays, diagnostics, and workspace edits in DTOs before model conversion.
8. Implement LspSupervisor. InspectOnly returns a no-execution service state. Trusted execution resolves the user-configured binary inside the selected execution context, verifies configured digest/version constraints, records identity and advertised capabilities, starts the canonical session, watches exit, and permits at most three bounded restarts.
9. Wire editor change from the existing `Buffer` mutation boundary: on accepted mutation bump its authority-keyed sent_version+revision ledger and queue `authority.lsp_did_change` (skipped if !trusted). didOpen only on trusted.
10. Implement crates/hermito/src/lsp/requests.rs providers guarded by capability + execution trust (any server call). rename etc use authority writes (blocked on inspect).
11. Implement crates/hermito/src/lsp/diagnostics.rs : convert, on publish (filtered by ledger using server version when present or safe policy), update store. Versionless -> discard or refresh.
12. Implement user-owned language TOML. Reject repository-provided executable/init-option configuration, hash the effective config into the trust scope, and require an explicit new grant after executable/argv/init-option changes.
13. Create four certified fixture trees (listed in Related): minimal self-contained for the ops.
14. Add `crates/hermito/tests/lsp_integration.rs` exercising on trusted Local: full lifecycle + rename + post verify. Separate matrix: stale sent-version, versionless diags policy, session-gen bump, typed `Message::Lsp` roundtrip on wire, epoch reject, and SSH path.
15. Add `crates/hermito/tests/lsp_coordinate.rs` for exact-domain and canonical-snap Unicode laws, plus `crates/hermito/tests/lsp_hostile_json.rs` for numeric-range and allocation-limit rejection; both are release gates.
16. Connect states (incl. blocked) to Services/Problems (phase1). Inspect-only shows static+blocked.
17. Graceful paths: no trust or no binary -> no LS session; static intelligence + clear blocked message in UI/Services.
18. Tracing on all: trust decision at spawn, ledger decisions, variant dispatch, blocked reasons.
19. Qualification spike: pin each certified server version/toolchain in `packaging/tool-baselines.toml`; store fixture-local observed path/digest/version/capability output as generated qualification evidence, not an editable `SERVER.md` contract.
20. Non-block validation + rapid edit test.

## Success Criteria

- [ ] For every certified fixture, opening the primary source file produces >=1 diagnostic visible in Problems bottom pane within 2s when using trusted Local authority.
- [ ] Completion on member-access trigger in each fixture returns >=1 item with label and detail that match fixture source; only results whose sent-version + session-generation + revision + epoch matched (or server version) are surfaced.
- [ ] Hover on identifier in each fixture returns non-empty contents (markdown/plain) whose range exactly covers the symbol in the fixture.
- [ ] Definition request on symbol in fixture resolves to the definition location (same file or different) and editor can navigate to it.
- [ ] Rename of identifier (local + cross-file) in fixture succeeds: prepareRename then rename; WorkspaceEdit applied via authority; affected buffers/disk updated; no unrelated changes.
- [ ] Edit after request sent but before response: response dropped; log "discarding stale LSP result" with old sent-version/session/rev/epoch.
- [ ] External SIGKILL of LS child (under trusted): LspSupervisor restarts, re-inits, re-delivers diags; UI responsive.
- [ ] Coordinate tests pass on every OS target: exact byte/UTF-16 round trips on valid code-point boundaries; grapheme/display-cell round trips on canonical grapheme starts; deterministic idempotent snapping for interior cells/bytes.
- [ ] SSH remote (trusted): canonical `ProtocolV1::Lsp` with AuthorityRoot starts the configured remote server and returns identical fixture outcomes.
- [ ] No server probe/spawn occurs without both execution trust and a matching effective config hash; repository server config cannot alter executable, argv, environment, or init options.
- [ ] InspectOnly reports `LSP blocked: execution trust not granted for this authority`; rename rejects before mutation.
- [ ] Services records execution context, resolved path, digest (where available), version, capability set, config hash, and Ready/Blocked/NotFound/VersionMismatch state.
- [ ] Every frame/result validates context + sent-version + session-generation + revision + epochs; stale container or authority results cannot cross into the current buffer.
- [ ] Phase 5 proves DevContainer execution by routing the same canonical LSP variants and client through `ExecutionContextV1::DevContainer`; no duplicate schema/client/ledger is added.

## Test and Validation Matrix

### Observable Gates (release blocking)

- Coordinate gate: `cargo test --test lsp_coordinate` passes the exact-domain and canonical-snap laws for emoji, combining marks, CJK, astral code points, tabs, and CRLF on all target OSes; `cargo test --test lsp_hostile_json` rejects negative, fractional, overflowed, and oversized wire values before model conversion.
- Fixture workflow gate: `cargo test --test lsp_integration -- --test-threads=1 local::typescript::full_lifecycle` (and rust/go/python equivalents) pass: diagnostics, completion, hover, definition, rename, post-rename verification using trusted authority.
- Ledger + version gate: synthetic + integration tests assert sent-version/session-generation/revision/epoch filtering; server diag version used when present; versionless diagnostics safely discarded or trigger refresh; no application of mismatched.
- Versioned protocol gate: frames on wire use ProtocolV1::Lsp top-level variant + dispatcher branches; local and remote paths consistent.
- Supervision recovery gate: test kills LS pid (trusted); asserts restart, new session ready, diagnostics restored within 5s, UI never blocked.
- Capability gate: test forces a server advertising subset (e.g. no rename); asserts rename provider disabled, no request emitted.
- Trust/inspect gate: inspect-only authority creates no LS sessions, never probes/spawns; surfaces static intelligence + blocked reason; LS ops blocked early.
- Non-block gate: edit storm test shows ratatui draw loop continues at target rate.

### Failure / Recovery Matrix

| Failure | Recovery | Observable / Gate |
|---------|----------|-------------------|
| LS binary absent from PATH and config (trusted) | No session created; exact "not found" + install hint emitted | Services entry: "typescript-language-server: not found (install via npm -g ...)" ; first open of .ts buffer shows same in status |
| LS exits during initialize (code 1) | Up to 3 restart attempts with backoff; then Failed with code + last stderr | Services: "rust-analyzer: failed (exit status: 1, stderr tail: ...)" ; manual "Restart language server" action succeeds |
| LS crashes mid-request (after open) | Restart, re-init, re-didOpen current docs at latest revision/sent/session; in-flight for old ledger dropped | Log "supervisor restart for rust on LOCAL"; diagnostics reappear; no crash in client |
| Document sent-version/revision advanced while request in flight | Response arrives with old sent-version/session/rev/epoch tag; handler drops it | trace log: "discarding stale LSP result sent=5 session=3 rev=42 (current sent=6)" ; no UI change or edit applied |
| Versionless diagnostic received | Apply safe discard policy (or request-refresh by re-didOpen); never blindly apply | Log "versionless diagnostic discarded for <doc>; policy=safe" ; optional re-sync |
| WorkspaceEdit from rename spans files at inconsistent revisions | Authority aborts the batch; partial writes (if any) rolled back or left for user review | User message: "Rename could not complete cleanly due to concurrent changes. Review affected files." ; no silent partial state |
| Remote helper connection lost mid-LSP session | On reconnect, authority bumps epoch + session-generation, supervisor re-starts LS sessions for still-open docs, re-syncs content using ledger | Host buffers untouched; intelligence resumes after reconnect banner; epoch/session change visible in logs |
| Server advertises fewer capabilities after restart | Re-intersect; features that vanished become unavailable | Completion still works, rename grayed in UI if no longer supported; no request sent |
| Extremely large completion list | Client receives, bounds display to first page; protocol limit respected | No memory blowup; UI shows results + "(more)" if applicable |
| Negative/fractional/overflowed `Position`, out-of-range document version, or oversized JSON-RPC field | Validated ingress rejects frame with typed protocol error before `lsp-types`/ledger/model conversion | `lsp_hostile_json` asserts no editor, diagnostic, pending-request, or allocation state changed |
| Attempt to start LSP on inspect-only authority | Supervisor returns stub immediately; no probe, no argv, no spawn | Services: "LSP blocked: execution trust not granted for this authority"; static intelligence available; no child process |

## Risk Assessment

- Language servers report incomplete or varying capabilities: strict client intersection + per-fixture certification against concrete server versions (spike gate records exact advertised set).
- Unicode coordinate errors cause wrong rename ranges or definition jumps: coordinate module + exhaustive matrix is hard release gate; any failing case blocks.
- Restart loops on flaky servers: bounded attempts (3), explicit Failed state, manual restart command, per-supervisor budget. Only under trusted execution.
- Slow or hanging LS starves TUI: initialize/request have timeouts; all I/O on dedicated tasks + bounded channels; pure coordinate math is <1 ms.
- Server skew across host/SSH/container: effective spec is user-owned, hash-bound to trust, resolved inside the execution context, and checked against pinned version/digest baselines before initialize.
- Unsaved content vs LS view drift: host always pushes current revisioned text; LS never owns disk truth.
- Ledger version/session mismatch on reconnect or authority switch: epoch + session bump forces full re-open and refresh.

## Security Considerations

- Server binaries start only under a grant for the exact workspace/authority/effective-config hash. Record resolved path, argv, cwd, identity digest when readable, version, and capabilities.
- Minimal constructed environment excludes credentials, agents, engine sockets, and host paths. Repository files cannot inject executable, args, environment, or initialization options.
- For SSH/container, only workspace document content crosses the canonical helper protocol; host paths outside the workspace never do.
- WorkspaceEdit mutations route through Authority and are validated/capped before writes.
- Capped JSON-RPC framing and DTO validation precede `lsp-types`; server payload never causes shell execution.
- Registry credentials are not part of LSP configuration in this release.
- Clean shutdown + owned cancellation ensures no zombie LS processes on host or remote after close or disconnect.
- Tracing records every trust decision, start (binary, authority, workspace), ledger decisions, and every file mutation originating from an LS WorkspaceEdit for audit.
- Versioned top-level protocol + dispatcher branches ensure no ambiguous framing across local/remote.
- Authority trait + explicit scoped trust grant (InspectOnly default; execution only on grant) is identical to phase-4 Git rules; no secret leakage on untrusted.

## Next Steps

- Phase 1 must deliver the core editor buffer events, RevisionedDocument (with ledger), Services model subscription, and Problems store before LSP can surface results.
- Phase 2 must deliver the remote transport channel and process supervision primitives that SshAuthority and supervisor reuse for LSP proxy (versioned protocol dispatch).
- Phase 4 and phase 3 share the canonical Authority path (`crates/hermito/src/authority/mod.rs`), protocol paths (`crates/hermito-protocol`), trust model (inspect-only default, explicit execution grant only, no secret leak), and frame-tagging rules.
- Phase 5 must implement DevContainerAuthority satisfying the explicit LSP contract stated in Requirements/Architecture (container exec path, full ledger/version/session/epoch support, trust gating, no host spawn, same variant dispatch); Git surface also required.
- Phase 7 runs the full per-platform, per-language, local+SSH fixture matrix plus restart stress + ledger cases + trust boundary cases as release qualification.
- Post-release: additional LSP methods (signatureHelp, codeAction, documentSymbol) added only after new fixtures are certified and only via configuration; never in first release.
