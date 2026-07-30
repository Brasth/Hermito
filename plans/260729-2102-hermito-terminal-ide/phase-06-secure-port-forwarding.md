---
phase: 6
title: "Secure Port Forwarding"
status: pending
priority: P1
dependencies: ["phase-01-core-workbench-and-editor", "phase-02-terminal-and-execution-transport", "phase-05-dev-container-orchestration"]
effort: ""
---

# Phase 6: Secure Port Forwarding

## Overview

Implement a loopback-only host port broker that accepts connections exclusively on 127.0.0.1 (and ::1) and forwards them to container services that are bound only to 127.0.0.1 inside the target environment. The host broker exclusively owns the advertised dual-stack loopback port. Accepted sockets are bridged as multiplexed protocol relay streams over the managed helper (when the container authority is remote) to the remote side; the container relay (launched on demand) then dials the service's 127.0.0.1 address inside the container and pipes bytes. Bind paired IPv4 127.0.0.1 and IPv6 ::1 listeners atomically on one reserved port and reconcile both. Use an explicit lease + versioned app-state model for all forwards. Support both auto-discovered forwards (from devcontainer.json forwardPorts) and manual/explicit user forwards. Handle collisions with clear errors. Reconcile leases on container restart or rebuild using the environment epoch from phase 5. Remote PTYs and LSP sessions do not transparently survive transport loss; they are marked Lost and a new terminal/LSP session is offered.

The Ports tool window (Alt+4), lease table, status bar, and Services tab receive all state. Terminals and language services continue to function; only port operations are added. All behavior is observable through the authority path, lease table binding strings, state dots, and log lines of the exact form `[ports] leased 127.0.0.1:43127 → hermito-dev:3000`.
## Context Links

- Product and architecture contract: /Users/huynguyen/Personal/Hermito/docs/technology-stack.md (Forwarding row, Architecture Rules 9/10, Release-Blocking Gates "Container-localhost forwarding without public listeners", "no public/LAN port binding")
- Interaction and visual contract: /Users/huynguyen/Personal/Hermito/docs/design-guidelines.md (Ports tool window, port table columns and binding strings, lease state, status feedback examples "Forwarding container 3000 → host 127.0.0.1:43127", "Port 9090 could not bind...", authority treatment for forwarded leases)
- Executable reference prototype: /Users/huynguyen/Personal/Hermito/docs/wireframe/index.html (ports-view table with "127.0.0.1:3000 → container:3000", Owner, Source "Dev Container"/"Auto-forward", State dots, "Leased", Services bottom tab PORT entries, terminal log "[ports] leased ...", discover button, explicit lease action)
- Overall plan: /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/plan.md (architecture invariants 5/9, whole-plan acceptance on container-localhost without public listeners, phase ordering)
- Phase 5 contract: /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-05-dev-container-orchestration.md (DevContainerTarget, ContainerRecord, epoch, ContainerEngineAdapter, permission model, local/SSH execution paths, lifecycle reconciliation)
- Phase 1/2 contracts (for authority, transport, app-state, UI panes)

## Requirements

1. A published lease owns paired listeners on `127.0.0.1` and `[::1]` only. Hermito has no API for wildcard/external bind.
2. Chain: paired host broker → canonical `ProtocolV1::Relay` stream → optional outer SSH helper → Phase 5 persistent inner container agent → target `127.0.0.1:<port>`. No `ssh -L`, engine publish, sidecar, shell, or per-connection container exec.
3. Phase 6 adds RelayV1 to the existing inner/outer agent code and regenerates the full-chain signed test TUF target for those exact helper bytes before any relay launch. It adds no relay binary/mode/install path; production publication remains Phase 7.
4. RelayV1 defines `Open/OpenOk/OpenError/Data/WindowUpdate/HalfClose/Close/Cancel`. Every message carries lease capability ID, connection ID, `ExecutionContextV1::DevContainer`, request ID, and epochs. Unknown tags/context/lease/port are rejected before dial.
5. Frames cap payload at 64 KiB. Each direction starts with 256 KiB credit; Data consumes credit and WindowUpdate replenishes only after bytes are written. Fair round-robin scheduling, max 128 connections/lease and 1024 global, 10 s dial timeout, bounded queues, and cancellation prevent memory/stream starvation.
6. Host-read EOF sends HalfClose; the agent drains prior bytes then `shutdown(Write)` on the target. Target EOF mirrors the half-close to the host. Close completes only after both directions drain; Cancel/epoch loss aborts without replay.
7. PortLease is keyed by workspace + authority + logical container target + container port and records stable host port, source, owner, capability ID, and current container epoch. Secret capability material is memory-only. Versioned non-secret records persist.
8. Manual/auto/discovered sources all require an explicit forwarding grant bound to final container config hash, lease port pair, and current trusted target. `forwardPorts` supplies proposals, not implicit grants.
9. Pair reservation is publish-atomic: IPv4 and IPv6-only sockets bind the same port before state is exposed. Automatic allocation retries a bounded 32 candidates; explicit/persisted collision never falls back.
10. Rebuild keeps the broker-owned listeners reserved, cancels old-epoch streams, marks Reconnecting, and atomically switches accepts to the new verified container context. Destroy/revoke/release drops both listeners.
11. TCP only. Listener discovery is inner-agent read-only `/proc/net/tcp{,6}` parsing and advisory; creating a lease remains explicit.
12. UI surfaces every binding, owner, source, state, collision, and reconciliation. All work runs on owned async tasks.

## Architecture

`LeaseManager::ensure_lease` first atomically validates current Authority trust, final config hash, logical target/epoch, requested port pair/source, and forwarding capability grant under its state lock. Failure returns before capability creation, socket bind, persistence, relay install, or dial. Only then `PortBroker` reserves IPv4 + IPv6-only sockets and publishes after both bind.

Each accepted connection receives a random connection ID and enters the capped credit-based RelayV1 state machine. Local routes directly over the existing inner-agent stdio multiplexer. SSH routes the same frames through the authenticated outer helper, which validates and forwards them to the already-running inner agent. The agent validates lease capability/context/epoch/target port and dials only `127.0.0.1:<target>`.

`LeaseManager` owns durable non-secret lease intent plus in-memory capability material and current target context. On rebuild, listeners remain bound while old streams cancel; after Phase 5 verifies agent/lease/epoch, the manager rotates capability material and resumes accepts against the new context. App restart rebinds the exact persisted port only after Phase 5 reconciliation; collision yields Conflict and never selects another port silently.

No layer creates a container listener or engine-published binding.

## Related Code Files

Planned canonical paths under crates/hermito (created or edited in this phase):

- crates/hermito/src/ports/mod.rs
- crates/hermito/src/ports/port_broker.rs (paired IPv4/IPv6-only loopback `TcpSocket` owner; bounded automatic-port retry; atomic publish/rollback; accept loop)
- crates/hermito/src/ports/lease_manager.rs (LeaseManager, ensure/release, collision detection, epoch reconciliation)
- crates/hermito/src/ports/forwarder.rs (RelayV1 host endpoint; direct-inner and outer-helper routes)
- crates/hermito/src/state/port_lease_record.rs (versioned non-secret lease intent)
- crates/hermito/src/ui/ports_pane.rs
- crates/hermito/src/ui/services_tab.rs
- crates/hermito-protocol/src/relay.rs (canonical capped credit/window/half-close state machine messages)
- crates/hermito-remote/src/relay.rs (outer forwarding branch and inner-agent outbound dial handler)
- Fixture and test files:
  - crates/hermito/tests/fixtures/ports/localhost_only_service/Dockerfile
  - crates/hermito/tests/fixtures/ports/localhost_only_service/.devcontainer/devcontainer.json
  - crates/hermito/tests/secure_port_forwarding_test.rs
  - crates/hermito/tests/port_forwarding_matrix.rs
  - crates/hermito/tests/port_relay_fixtures.rs

Integration points:
- crates/hermito/src/containers/dev_container_orchestrator.rs (epoch/container events)
- crates/hermito/src/containers/lifecycle.rs (rebuild notification)
- Phase-2 TUF verifier/cache and managed helper session
- Authority DevContainerTarget/container exec paths from phase 5
## Implementation Steps

1. Define PortLease/record/source/state and the canonical RelayV1 schema. Keep capability secrets out of app state/logs.
2. Implement protocol validation and state machines first: 64 KiB Data cap, 256 KiB directional windows, monotonic sequence, WindowUpdate-after-write, fair scheduling, half-close drain, close/cancel, 10 s dial timeout, 128-per-lease/1024-global caps, and epoch/context rejection.
3. Implement paired broker reservation. Set IPv6-only before bind; bind both loopback sockets before publish. Retry 32 port-0 candidates. Explicit/persisted bind failure drops both and returns the exact collision.
4. Implement `ensure_lease` as the sole creation path. Under one lock validate unrevoked forwarding grant + current trust/effective hash/context/epoch/source/ports before generating capability or binding. Manual, final-config auto, and discovery call it identically. Persist only after paired bind + agent readiness; revalidate grant before publish and roll back on drift.
5. Rebuild the two static helper test targets with RelayV1 and regenerate pinned full-chain test TUF fixtures from committed non-production keys. Verify exact target before launching `session`/`container-agent`; reject the older non-Relay capability manifest.
6. Implement local direct-inner and SSH outer-helper routing through canonical `ProtocolV1::Relay`. Extend codec/host/outer/inner dispatch and version/capability negotiation; no extra process, install path, SSH connection, or engine command.
7. Implement inner Relay dispatch: validate capability/context/epoch/port, dial only `127.0.0.1:<target>`, run bounded flow/half-close. Reject other transports/addresses at DTO decode.
8. Add manual/auto/discovery proposal UI. Final `forwardPorts` and bounded `/proc/net/tcp{,6}` only propose. Grant flows through `ensure_lease`; proposal/discovery never binds/persists/dials.
9. Rebuild retains listeners, cancels streams, rotates target/capability only after verified Phase 5 reconciliation, then resumes. Destroy/revoke/release drops listeners/capability.
10. Restart restores exact pair only after Phase 5 reconciliation. Either bind failure means Conflict; new capability only after target verification.
11. Wire Ports/Services/status models and redacted logs.
12. Add signed-target capability, dispatcher, strict-loopback, and hostile Relay tests for caps/window/sequence/tag/lease/epoch/fairness/half-close/cancel/saturation/loss.
13. Run isolated Docker local + real SSH matrix; Podman inherits Phase 5 gating.

## Success Criteria

- Every published lease has paired loopback listeners only, and a matching current hash/target/port/source forwarding grant was validated before the first side effect and again before publish.
- A service bound only to container `127.0.0.1` is reachable from host IPv4 and IPv6 loopback through the same relay target; bytes and half-closes are correct.
- Evidence contains no engine publish, `ssh -L`, second SSH connection, sidecar, shell, `socat`, relay-specific binary/install/mode, or per-connection exec; normal TUF install verifies current Relay-capable bytes.
- Local and SSH paths use canonical RelayV1; helper diagnostics show context/lease/connection IDs and bounded credit without secrets.
- Manual, final-config auto, and discovered proposals show correct source but create nothing until forwarding grant.
- Rebuild retains the broker port, drops old streams, rejects stale epochs, rotates capability, and restores reachability without releasing listeners.
- App restart rebinds the exact persisted port after target verification; collision becomes Conflict without fallback.
- Explicit collision emits the design-guideline message and leaves no partial/persisted active lease.
- Revoke/destroy/release immediately closes both listeners and cancels all streams.
- Discovery reports only loopback `/proc` listeners; TCP-only DTOs reject every other target.
- Hostile/saturation tests prove frame/window/global limits, fair progress, correct half-close ordering, and no replay after helper loss.

## Test and Validation Matrix

**Engine / Platform / Authority / Security Matrix** (Docker reference required; Podman gated; all local and SSH paths):

| Engine | Host Platform | Authority Path          | Security Posture                                      | Observable Gate |
|--------|---------------|-------------------------|-------------------------------------------------------|-----------------|
| Docker | macOS arm64   | Local → DevContainer    | loopback broker exclusively owns port; relay 127-only; epoch reconciliation; atomic dual-stack | host ss shows 127.0.0.1 + ::1 only; container ss shows 127.0.0.1 service; lease survives restart |
| Docker | Linux x86_64  | Local → DevContainer    | same                                                  | same + explicit manual lease + collision error |
| Docker | Windows       | Local → DevContainer    | same (ConPTY)                                         | same matrix cell |
| Podman | macOS arm64   | Local → DevContainer    | identical after qualification                         | podman adapter cell passes only after docker reference; same loopback invariants |
| Docker | macOS arm64   | SSH → DevContainer      | multiplexed protocol relay streams over managed helper; broker owns port; no ssh -L | multiplexed stream visible in helper diagnostics; host lease reaches remote container 127 service; no host publish; dual-stack reconciled |
| Docker | Linux         | SSH → DevContainer      | same                                                  | full parity |

**Fixture Tests** (exact named files):

- crates/hermito/tests/secure_port_forwarding_test.rs: InspectOnly manual/auto/discovery produce zero socket/capability/record/agent side effect; grant-drift race rolls back; paired loopback, strict-127, no-publish, rebuild, collision, revoke, TCP-only.
- crates/hermito/tests/port_forwarding_matrix.rs: isolated real engine/platform/authority cells and recorded argv/socket ownership.
- crates/hermito/tests/port_relay_fixtures.rs: relay protocol caps, credit starvation, fairness, half-close drain, cancel, unknown/stale context, outer/inner loss, and byte-integrity corpus.
- crates/hermito/tests/fixtures/ports/localhost_only_service/.devcontainer/devcontainer.json (contains "forwardPorts": [8080] and a server that binds 127.0.0.1 only).
- crates/hermito/tests/fixtures/ports/localhost_only_service/server.js or equivalent (the process inside that does the 127-only bind).

**Observable Gates** (smoke verifiable from fixture run):

- Final-config forward proposal remains Pending until grant; then Ports shows paired binding and Leased state.
- Services emits redacted `[ports] leased 127.0.0.1:<hp> → hermito-dev:8080`.
- Host IPv4/IPv6 clients succeed; inner agent reports target peer at 127.0.0.1.
- Host socket inspection shows only paired loopback listeners owned by Hermito; container inspection shows only the application's 127 listener.
- Rebuild preserves the host port/listeners, cancels live connections, then resumes against new epoch.
- Explicit/persisted collision yields Conflict and no half-bound state.
- SSH diagnostics show RelayV1 on existing outer/inner sessions; no `ssh -L` or extra SSH/container exec.
- Half-close test drains queued bytes in both directions before final Close; helper loss aborts without replay.

All tests clean up listeners, relays, and containers. Matrix cells are isolated.

## Risk Assessment

- Relay lifetime: many TCP connections multiplex through existing agent sessions; per-lease/global caps and fair credit scheduling bound resources.
- Helper loss: established connections fail closed. New connections wait for freshly verified sessions/context; no byte replay/resume.
- Port exhaustion/privilege: 32 paired retries for automatic; explicit/persisted fails without fallback.
- Rebuild race: listener ownership is stable; context/capability rotation is serialized and stale opens/data fail.
- IPv6 portability: paired IPv6-only bind is tested per platform; unsupported pairing blocks the platform cell rather than weakening to IPv4-only.
- Backpressure/half-close bugs: model tests cover credit conservation, sequence, drain-before-shutdown, cancellation, and fair progress.

## Security Considerations

- Broker binds only paired loopback sockets and never publishes state before both succeed.
- Relay DTO contains only validated `u16` target port; inner agent hardcodes target address `127.0.0.1`.
- Lease capability secrets are random, memory-only, rotated on restart/rebuild, compared in constant time, and never logged/persisted.
- SSH uses the existing authenticated outer session and verified inner agent. No port-forward option, extra connection, credential, or engine socket exists.
- Context + lease + epoch scope prevents cross-container/workspace routing; stale frames are rejected.
- Revocation/release destroys capabilities, listeners, queued frames, and active streams.
- Discovery parses `/proc` read-only and cannot create a lease.
- Credit windows and caps bound memory independently of connection lifetime; TCP only.
- Stable paired listener ownership prevents rebuild hijacking; restart collision fails closed.

## Next Steps

- Phase 7 consumes the port lease table and broker for qualification matrices and packaging (no new behavior).
- Future work can add UI for bulk lease management or per-lease policy, always behind the same loopback + multiplexed relay + epoch + broker-exclusive model.
- Any additional container engine adds only a narrow adapter implementation; the broker, lease manager, and relay launcher remain unchanged.
- The strict "container service bound only to 127.0.0.1 is reachable via broker-owned dual-stack lease with multiplexed relay when remote" test remains the permanent regression gate for all forwarding changes.
