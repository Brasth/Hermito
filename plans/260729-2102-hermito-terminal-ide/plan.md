---
title: Hermito Terminal IDE Implementation
description: >-
  Build Hermito as a secure editor-first Rust TUI IDE with local, SSH, Dev
  Container, Git, LSP, terminal, and true port-forwarding workflows.
status: in-progress
priority: P1
branch: feat/hermito-terminal-ide
tags:
  - feature
  - frontend
  - infra
  - critical
blockedBy: []
blocks: []
created: '2026-07-29T14:04:27.734Z'
createdBy: 'ck:plan'
source: skill
---

# Hermito Terminal IDE Implementation

## Overview

Implement the approved Hermito product contract as seven release-gated vertical slices. The result is a conventional, mouse-capable terminal IDE for native macOS, Linux, and Windows. It preserves unsaved buffers in a crash-safe host journal while routing execution to explicit Local, SSH, or Dev Container authorities.

**Scope locked by user:** full IDE breadth; conventional editing; local and SSH workspaces; host and container terminals; Docker and Podman; TypeScript/JavaScript, Rust, Go, and Python LSP workflows; advanced local Git; true TCP forwarding; configuration-only extensibility.

**Explicit exclusions:** debugger, executable plugin API, hosting-provider pull-request UI, UDP forwarding, public/LAN port binding, automatic browser opening, bundled Git/SSH/container engines/language servers, and claims of VS Code or JetBrains parity.

## Phases

| Phase | Name | Status |
|-------|------|--------|
| 1 | [Core Workbench and Editor](./phase-01-core-workbench-and-editor.md) | Completed |
| 2 | [Terminal and Execution Transport](./phase-02-terminal-and-execution-transport.md) | Completed |
| 3 | [Language Intelligence](./phase-03-language-intelligence.md) | Pending |
| 4 | [Advanced Local Git](./phase-04-advanced-local-git.md) | Pending |
| 5 | [Dev Container Orchestration](./phase-05-dev-container-orchestration.md) | Pending |
| 6 | [Secure Port Forwarding](./phase-06-secure-port-forwarding.md) | Pending |
| 7 | [Integration Packaging and Qualification](./phase-07-integration-packaging-and-qualification.md) | Pending |

## Dependencies

- Cross-plan dependencies: none.
- Product and architecture contract: [`docs/technology-stack.md`](../../docs/technology-stack.md).
- Interaction and visual contract: [`docs/design-guidelines.md`](../../docs/design-guidelines.md).
- Executable reference prototype: [`docs/wireframe/index.html`](../../docs/wireframe/index.html).
- External tools are capability-probed installed dependencies: Git; OpenSSH `ssh`, `sftp`, and `ssh-keyscan`; Docker or Podman and Compose provider; Dev Container CLI; and configured language servers.
- Phase order is strict: 1 → 2 → 3 → 4 → 5 → 6 → 7. Phase 5 consumes the Phase 3/4 canonical LSP/Git contracts; Phase 6 consumes the persistent verified container-agent session; Phase 7 adds no product behavior.
 
## Architecture Invariants

1. One host Rust modular monolith; only external or untrusted work crosses a process boundary.
2. Crossterm exclusively owns outer-terminal raw mode, input, resize, mouse, paste, and restoration.
3. UI never blocks on filesystem, parser, Git, LSP, PTY, SSH, engine, container, or network work.
4. Unsaved Ropey buffers are authoritative and checkpointed to an atomic, revisioned host journal. All async results carry workspace/environment epoch; document-derived results additionally carry revision. Authority, reconnect, and rebuild transitions advance the applicable epoch rather than pretending old sessions survived.
5. One typed authority abstraction routes local, SSH, and container file/process/Git/LSP/PTY/engine/port operations.
6. Raw PTY, LSP, Git, helper, and repository text is parsed or escaped into inert display state.
7. Repository trust starts inspect/edit only; executable capabilities require explicit, revocable, granular grants.
8. Git runs where the repository filesystem and index live. Hermito never runs local Git over remote paths.
9. Forwarding binds paired host loopback listeners on `127.0.0.1` and `::1` only, with one advertised port owned exclusively by the broker; remote container traffic crosses the verified helper's multiplexed relay and never uses Docker publishing or `ssh -L`.
10. No remote helper executes before full TUF-chain verification, expiry and rollback/freeze checks, and atomic installation; SSH uses only Hermito-managed host keys and an isolated OpenSSH configuration.
11. `hermito-protocol` owns one `ExecutionContextV1` and one top-level family per domain. Container lifecycle uses Container; language, Git, and forwarding use canonical LSP, Git, and Relay families across every authority.
12. Dev Container trust is two-stage and hash-bound. CLI 0.88.0 is sole resolver/creator/normal exec launcher; host-environment interpolation, host publishing, ambient credentials, sockets, and arbitrary adapter calls are rejected.
13. Network credentials, Git hooks, container creation, and forwarding are separate capabilities. Credentialed Git network operations run hook-free; no agent/default key or secret-bearing hook process exists.
14. Release requires two independent native builds per target, deterministic unsigned digest equality, GitHub OIDC provenance, platform signature verification, threshold TUF metadata, and clean published-artifact reruns.

## Delivery Strategy

- Each phase must finish its vertical behavior, targeted tests, recovery behavior, and smoke scenario before the next dependent phase starts.
- Docker is the reference engine. Podman is enabled only for matrix cells that pass the same fixtures.
- Remote Linux is the first helper target; additional OS/architecture targets remain disabled until signed artifact, PTY, Git, LSP, engine, forwarding, update, and recovery gates pass.
- Feature flags may isolate unfinished authority backends during development, but released binaries contain no placeholders or silent fallbacks.

## Whole-Plan Acceptance

- The seven phase contracts contain exact modules, data boundaries, fixtures, failure modes, and observable release gates.
- All selected capabilities work through Local, SSH, and Dev Container authority transitions where applicable.
- Native macOS, Linux, and Windows qualification results are aggregated before release; no platform may leak control sequences, credentials, agents, engine sockets, host mounts, or non-loopback listeners.
- Cancellation, terminal exit, SSH loss, helper crash, container rebuild, Git sequence interruption, and application crash reconcile to explicit recoverable state. A remote PTY lost with its transport is marked `Lost` and replaced only by an explicit new session; it is never presented as resumed.

## Open Questions

- Final `TerminalSurface` emulator dependency after the VT corpus spike.
- Signed remote-helper release targets beyond the remote-Linux reference.
- Qualified Podman/Compose versions per host platform.
- Versioned app-state storage encoding after schema/recovery spike.
