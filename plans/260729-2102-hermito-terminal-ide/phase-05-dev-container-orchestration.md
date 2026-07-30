---
phase: 5
title: "Dev Container Orchestration"
status: pending
priority: P1
dependencies: ["phase-01-core-workbench-and-editor", "phase-02-terminal-and-execution-transport", "phase-03-language-intelligence", "phase-04-advanced-local-git"]
effort: ""
---

# Phase 5: Dev Container Orchestration

## Overview

The pinned external `devcontainer` CLI is the sole creator for build, up, rebuild, and all lifecycle creation operations. Narrow engine adapters (Docker reference; Podman after qualification) perform only inspect, label lookup, stop, remove, logs, and exec against the container ID returned by the CLI. Resolve and read the full CLI-effective configuration (including referenced Compose files, lifecycle commands, run arguments, and exposures). Reject any unknown capability-bearing fields. Permission summaries and trust grants are bound to the exact effective-config hash. Add stable installation+instance identity and engine-visible ownership lease carrying expiry and liveness before orphan reclamation is permitted. Implement DevContainerAuthority with LSP start/send/receive, supervisor epoch rebind/restart, document reopen, and certified language fixtures.

Maintain ownership via labels and leases. Support reconciliation on restart/rebuild. Stream lifecycle logs, support cancellation and cleanup. Surface Compose, Features, mounts, environment variables, credential exposure, and all CLI-resolved run/lifecycle arguments as explicit permission summaries before any executable operation. Support execution of devcontainer terminals and commands under Local authority (container on host) and SSH authority (container on remote via signed helper). All container operations are authority-scoped and epoched.

This phase delivers the Environments tool window integration, authority path DEVCONTAINER segments, DevContainerAuthority, ContainerExecutionTarget for terminals/tasks/LSP, and the container execution surface. No editor core or Git UI behavior changes.

## Context Links

- Product and architecture contract: /Users/huynguyen/Personal/Hermito/docs/technology-stack.md (Dev Containers row, Architecture Rules 1/5/7/8/9, External Tool Policy, Release-Blocking Gates, Docker reference/Podman after matrix, Forwarding rules)
- Interaction and visual contract: /Users/huynguyen/Personal/Hermito/docs/design-guidelines.md (Authority path, Environments inspector, trust states, status feedback patterns, lifecycle actions, port/lease mentions)
- Executable reference prototype: /Users/huynguyen/Personal/Hermito/docs/wireframe/index.html (authority chain DEVCONTAINER cards, environment inspector details, lifecycle state dots, services bottom tab DEV entries, trust controls)
- Overall plan: /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/plan.md (phase ordering, architecture invariants 1/5/7/9, delivery strategy Docker reference first, whole-plan acceptance)
- Prior contracts: phases 1-4 (authority/trust/app-state; remote transport; LSP surface; Git run/query/ack and durable mutation semantics)

1. Discover supported devcontainer paths through Authority reads; multiple configs are selectable. Hash raw config plus every referenced Dockerfile, Compose file, and lockfile.
2. Selection adds a DEVCONTAINER segment showing name/image and starts InspectOnly.
3. Trust is two-stage. `Resolve configuration` grants only the pinned CLI's read-configuration command against the raw-input hash. Hermito then shows normalized merged/features/Compose capabilities. `Create and execute` is a separate grant bound to workspace, authority, CLI identity/version, engine/provider identity, raw-input hash, and exact effective-config hash. Drift revokes executable actions.
4. User-installed `@devcontainers/cli 0.88.0` is the sole build/up creator and normal exec entrypoint. Hermito accepts only fixed argv for `read-configuration --include-merged-configuration --include-features-configuration`, `build`, `up`, and `exec`; exact JSON schemas are fixture-pinned. No bundling/download.
5. Docker is the reference. Podman remains disabled per engine/platform until its identical matrix passes.
6. Containers receive immutable managed/workspace/installation/config labels. A TUF-verified static `hermito-remote` container agent at a fixed digest-addressed path owns command/LSP/Git/relay dispatch and atomic lease records; it is installed by fixed copy/read-back/hash operations after CLI up. No shell command constructs control state.
7. Ownership uses a renewable lease `{installation_id, owner_instance_id, container_id, generation, sequence, issued_at, expires_at, effective_hash}`. Host sends renew every 5 s with 30 s expiry; the agent atomically replaces and fsyncs the record. `ContainerRecordStore::compare_and_swap_lease` durably mirrors only monotonic sequence/generation changes. Cleanup requires identity match, expiry, two unchanged samples spanning at least 10 s, no matching live app-state owner, and a successful atomic claim by the fixed agent control command.
8. Lifecycle operations are cancelable and emit bounded structured events. Partial cleanup uses the same claim protocol; uncertainty leaves the container and reports manual cleanup.
9. Before resolution, reject `${localEnv:*}` and any host-environment/command interpolation in raw config and referenced inputs. Run CLI/Compose/engine tools with `env_clear`, absolute executable, fixed cwd, and a minimal tool-only allow-list (engine endpoint only where required; never credentials/agents); tool env is not container env. Normalize merged/features/Compose output, lifecycle commands, run args, mounts, env names/redacted literal values, and port metadata. Frozen locks required; unknown capability fields and publish sources rejected. `forwardPorts` is broker metadata only; post-up inspect shows zero host bindings.
10. General terminal/process execution enters through the persistent container agent launched by fixed `devcontainer exec`; the narrow engine adapter may execute only enumerated agent install/hash/lease-claim controls against the CLI-returned ID.
11. Local management runs on host. SSH management is delegated to the signed outer helper, which runs the same pinned CLI/adapter and connects to the inner container agent. Host never accesses remote engine state.
12. Phase 5 consumes canonical `ExecutionContextV1::DevContainer` and the Phase 3/4 LSP/Git variants. The outer container dispatcher validates ID/epoch/lease then forwards to the inner agent's existing canonical handlers. No duplicate LSP/Git schema, parser, client, service, or journal.
13. Environment epoch advances on successful up/rebuild/replacement. Every event/result carries request ID, execution context, workspace/environment epochs, and document revision where applicable.
14. No engine socket, SSH agent, credential file, host-env interpolation/value, unintended mount, or published port reaches the container. InspectOnly blocks resolve unless separately granted, and blocks all later execution/network/forwarding.
15. Authority path, Environments, Services, and status update only from bounded event channels.
## Architecture

Container orchestration extends the existing Authority adapters; it does not own editor, LSP, or Git state. `DevContainerCli` alone resolves/builds/creates and is the normal exec launcher. Docker/Podman adapters are enumerated post-creation controls only: inspect/list/stop/remove/logs, fixed agent copy/read-back/hash, and fixed agent lease-claim command. They expose no arbitrary argv, create, build, run, or publish method.

The verified inner `hermito-remote` agent is the single container execution endpoint. Its stdio connection is established by fixed `devcontainer exec`; on SSH, the signed outer helper owns that process and multiplexes frames back to the host. Terminal/process, canonical LSP/Git, lease renewal, and Phase 6 relay all share this bounded protocol.

Trust binds both a pre-resolution raw-input envelope and a post-resolution effective envelope. Creation cannot begin until the final envelope is approved. Immutable labels identify candidates; the renewable agent-written lease plus durable app-state CAS establishes current ownership.

Local data flow: Authority read → dependency hash/static interpolation/exposure rejection → resolve-only grant → pinned CLI under constructed non-secret tool env → normalized review/final grant → CLI up → inspect ID/no ports/env leakage → verified agent → CAS lease → ContainerRecord.

SSH performs the identical flow inside the outer signed helper. Engine state, lease state, durable app state, and epoch are authoritative; TUI caches are projections only.


## Related Code Files

Planned canonical paths under crates/hermito (created or edited in this phase only; no changes to editor, git, or lsp modules except for DevContainerAuthority integration):

- crates/hermito/src/containers/mod.rs
- crates/hermito/src/containers/dev_container_orchestrator.rs
- crates/hermito/src/containers/devcontainer_config.rs (DevContainerJson struct + from_path via authority read + serde; effective config resolution)
- crates/hermito/src/containers/devcontainer_cli.rs (exact 0.88.0 probe and fixed read/build/up/exec schemas)
- crates/hermito/src/containers/adapter.rs (enumerated inspect/list/stop/remove/logs, verified-agent install/hash, and lease-claim controls only)
- crates/hermito/src/containers/adapters/docker_adapter.rs
- crates/hermito/src/containers/adapters/podman_adapter.rs
- crates/hermito/src/containers/lease.rs (5 s renew, 30 s expiry, 10 s grace sampling, agent claim protocol)
- crates/hermito/src/containers/lifecycle.rs (CLI supervisor, inner-agent session, cancellation, epoch rebind)
- crates/hermito/src/containers/permission_summary.rs (effective-config/hash review, unknown-capability and published-port rejection)
- crates/hermito/src/authority/devcontainer_target.rs
- crates/hermito/src/authority/devcontainer_authority.rs (implements terminal/process plus phase-3 LSP and phase-4 Git contracts)
- crates/hermito/src/persistence/container_record.rs (`ContainerRecordStore` with mutex-serialized, file-fsync + atomic-rename + parent-fsync `compare_and_swap_lease`)
- crates/hermito/src/ui/environments_pane.rs (updates only; reads from orchestrator events)
- crates/hermito/src/ui/services_tab.rs (lifecycle log append only)
- crates/hermito-protocol/src/container.rs (versioned container lifecycle, adapter, lease, and resolved-target messages only; it does not duplicate LSP/Git variants)
- crates/hermito-remote/src/container.rs (outer lifecycle dispatcher and inner-agent session router; delegates canonical LSP/Git/Relay)
- scripts/generate-tuf-test-fixtures.sh (Phase 2 pipeline rerun with `--mode container` after inner-agent code lands)
- Fixture and test files:
  - crates/hermito/tests/fixtures/devcontainer/minimal-rust/.devcontainer/devcontainer.json
  - crates/hermito/tests/fixtures/devcontainer/with-compose/.devcontainer/devcontainer.json
  - crates/hermito/tests/fixtures/devcontainer/with-features/.devcontainer/devcontainer.json
  - crates/hermito/tests/container_orchestration_test.rs
  - crates/hermito/tests/container_matrix.rs
  - crates/hermito/tests/devcontainer_lifecycle_fixtures.rs

Phase 5 owns `ProtocolV1::Container` only for resolve/lifecycle/adapter/lease/agent-session messages. Canonical LSP/Git messages already carry `ExecutionContextV1::DevContainer`; the outer dispatcher validates the context and routes them unchanged to the inner agent.

## Implementation Steps

1. Extend execution-target routing with `DevContainerTarget`; keep terminal/process/LSP/Git APIs canonical. Target identity includes authority, raw/effective hashes, canonical container ID, owner instance, and epoch.
2. Implement Authority-only discovery and dependency hashing. Parse JSONC/referenced Compose/Dockerfile/lock inputs without execution; reject publish/socket/agent paths plus `${localEnv:*}` and host-env/command interpolation before any CLI call.
3. Implement two-stage trust. Resolve permits only exact read-configuration argv, inputs, and constructed non-secret tool env. Final scope binds normalized effective config/tool/provider identities.
4. Implement CLI 0.88.0 adapter using absolute tool path, `env_clear`, fixed cwd, minimal baseline-owned env, fixed workspace/config/labels/JSON flags, frozen locks, and capped DTOs. Engine endpoint may exist only in tool process env and must be rejected from resolved container/build/run env. Spike exact argv/schema/env first.
5. Normalize merged/features + fixed Compose JSON, lifecycle commands, mounts, literal env names/redacted values, run args, interpolation records, and ports. Reject unknown executable/mount/network fields, every host-env interpolation, secret/agent/socket transfer, and publish form before final review.
6. Define narrow Docker/Podman adapters with identical process isolation. Validate ID/labels every call; post-up inspect rejects bindings and any forbidden env/mount. No arbitrary exec/create/publish.
7. Extend the Phase 2 TUF fixture/release pipeline for the current inner-agent bits. Copy only verified bytes to a unique temp path, read-back hash, atomically rename to a digest-addressed fixed path, re-hash, then launch `container-agent` via fixed `devcontainer exec`. Fail if the target filesystem is noexec; do not fall back to shell/PATH.
8. Implement agent-owned lease write/renew/relinquish/claim. Use 5 s renew, 30 s expiry, 10 s two-sample grace, monotonic generation/sequence, atomic record replace, and atomic claim directory. Implement durable `ContainerRecordStore::compare_and_swap_lease(expected_owner, expected_generation, expected_sequence, next)` with serialized access and full file/directory durability.
9. Implement lifecycle supervision and reconciliation. Rebuild is verified stop/remove followed by CLI up. App crash leaves renewal to expire; a new instance waits/samples/claims before attach or cleanup. Advancing, future, foreign, corrupt, missing, or contended leases are never auto-claimed/deleted.
10. Implement outer/inner agent routing for `ProtocolV1::Container` plus canonical terminal/process/LSP/Git. Local host or outer SSH helper owns the single inner stdio session. Every dispatch validates container identity, lease ownership, execution context, and epoch.
11. Reuse Phase 3 LspSupervisor/client/ledger and Phase 4 GitService/schema/parsers/lease/recovery. Container HTTPS credentials use the one-shot inner-agent askpass response; container SSH Git requires an explicitly approved identity already inside the container. No host credential file crosses.
12. Wire authority/Environments/Services/status projections and persist ContainerRecord solely through the store API.
13. Add real-engine tests for both trust stages, CLI/schema/env drift, `${localEnv:SSH_AUTH_SOCK}` and synthetic-secret rejection, frozen features, publish/mount/env rejections, verified agent/noexec, cancellation, lease races, canonical LSP/Git, and fixture ownership.

## Success Criteria

- Discovery works over Local and SSH Authority reads; selection produces stable raw-input identity and a DEVCONTAINER segment.
- Resolve-only uses exact CLI argv plus constructed non-secret environment and cannot create/exec. Raw host-env interpolation fails before spawn. Final grant binds effective/tool/provider hashes; drift revokes.
- Environments shows lifecycle/health/workspace/image/route/tool identities/hashes/lease owner+expiry and typed actions.
- CLI is sole build/up creator and normal exec launcher. Narrow adapters never call create/run/build/publish or arbitrary exec.
- Docker reference and qualified Podman cells show immutable labels, zero host bindings, advancing lease, and verified digest-addressed inner agent.
- Cancellation removes a partial container only after claim predicates; otherwise it remains with manual-cleanup state.
- Unknown/floating capabilities, `${localEnv:*}`, host secret interpolation, socket/agent mounts, runArgs/Compose/appPort publishing, forbidden resolved env, or post-up HostPort fail before ownership.
- Local/SSH container terminal and process calls execute through the inner agent; identity output proves container context.
- Certified LSP and Git pass through canonical variants with `ExecutionContextV1::DevContainer`; rebuild bumps epoch, LSP explicitly reopens, and Git query/reconcile never replays.
- Crash/restart tests prove lease record and ContainerRecordStore remain monotonic across every write boundary. One of two racers claims; advancing/future/foreign/corrupt/missing/contended leases remain untouched.
- Every operation/event validates request ID, context, epochs, lease owner, and document ledger where applicable.
- Inspection and synthetic sentinels prove no engine socket, agent, host credential/secret/environment value, unintended mount, or published port enters the container or review/log output.

## Test and Validation Matrix

**Engine / Platform / Security Matrix** (all cells must pass before phase complete; Docker reference cells required, Podman cells gate the adapter):

| Engine   | Host Platform     | Container Arch | Security Posture                              | Observable Gate |
|----------|-------------------|----------------|-----------------------------------------------|-----------------|
| Docker | macOS arm64 | linux/arm64 | renewable lease; no agent/socket/published port | owned container has immutable labels + advancing lease record; inspect reports no HostPort; trusted exec succeeds |
| Docker | Linux x86_64 | linux/amd64 | same | restart reconciliation restores or safely claims expired record; Git/LSP contract passes |
| Docker | Windows | linux/amd64 | same via Docker Desktop | fixed adapter argv, lifecycle/lease events, ConPTY terminal pass |
| Podman | macOS arm64 | linux/arm64 | identical contract | enabled only after provider/version-specific matrix passes |
| Podman | Linux x86_64 | linux/amd64 | identical contract | full Docker parity including exposure and lease-race tests |

- `container_orchestration_test.rs`: trust stages, CLI schema, effective hash, agent verification, canonical terminal/LSP/Git, epoch, cancellation, and every reject path.
- `container_matrix.rs`: isolated Docker then gated Podman engines; local and real SSH route; zero bindings/socket/agent; CLI creator; typed adapter argv; no-replay Git.
- `devcontainer_lifecycle_fixtures.rs`: CAS crash boundaries, 5 s renew/30 s expiry/10 s samples, relinquish, host crash, engine restart, duplicate instances, atomic claim race, and safe refusal.
**Observable Gates** (must be demonstrable in smoke run of the fixture):

- After open workspace containing .devcontainer: Environments lists "hermito-dev" with green dot.
- Select DEVCONTAINER in authority: path shows CURRENT amber; inspector shows "Running · health checks passed", "/workspaces/hermito", image, effective config hash, "Trusted".
- Resolve-only then final hash-bound grant; CLI up emits logs, labels match, lease advances, inner agent hash matches TUF target, and inspect reports zero bindings.
- Cancellation cleans only after claim; otherwise manual cleanup.
- Graceful restart relinquishes/claims immediately; crash restart waits for expiry + samples. Exactly one duplicate instance can claim.
- Real SSH devcontainer shows identical outer/inner dispatch and observables.
- Permission dialog reflects raw/effective hashes, tool identities, normalized capabilities, and redacted secrets.
- Container LSP/Git use canonical variants; rebuild changes context epoch; loss yields explicit LSP reopen and Git query/reconcile.
- Every prohibited/unknown/floating capability and post-up binding fails closed.
- Foreign, future, corrupt, missing, advancing, or contended lease is never auto-deleted.
All tests use direct argv to engines via CLI for creation, narrow adapter for post-creation ops only, never rely on ambient config, and clean fixtures between runs. No project-wide cargo test; each matrix cell is an isolated binary invocation against the fixture.

## Risk Assessment

- CLI/schema skew: accept exactly 0.88.0 in first-release baseline; DTO fixtures and fixed argv fail closed on drift.
- Podman/provider differences: remain disabled per engine/platform until the full reference matrix passes.
- Lifecycle/cancel races: owned tokens plus post-cancel identity/claim checks.
- Lease/store races: monotonic CAS, durable writes, explicit timings, dual-instance/engine-restart tests; uncertainty never deletes.
- Inner-agent failure/substitution/noexec: TUF target + copy/read-back/final hash + absolute path; context becomes Lost and no shell fallback exists.
- Effective-config gaps: two-stage review, raw dependency hash, merged/features output, Compose resolution, frozen lock, unknown-field reject, and post-up inspection.
- SSH failure: outer and inner sessions have separate epochs/Lost states; buffers stay host-owned and Git never replays.
## Security Considerations

- Resolve/create grants are separate and bind inputs, constructed tool env identity, tools/providers, effective config, and target.
- Host-env interpolation is rejected before CLI; CLI/Compose/engine children use cleared minimal environments. Engine endpoints never become build/container env.
- Engine sockets, agents, credentials, secrets, mounts, and publishing are rejected; network secrets are one-shot protocol responses only.
- CLI 0.88.0 alone creates; adapters expose only enumerated controls.
- Effective review covers merged/features/Compose/lifecycle/mount/literal-env/run/port/interpolation; unknown/floating capability fails closed.
- Verified inner agent owns execution, lease, Relay; no shell/PATH control.
- Remote management stays in signed outer helper.
- Logs/events/bundles/hash reviews redact values without hiding capability names/source.
- LSP loss requires reopen; Git uses durable query-before-reconcile.
## Next Steps

- Phase 6 reuses the already verified inner-agent session and adds only Relay messages plus broker leases; it does not add copy/install/exec or SSH `-L`.
- Phase 7 aggregates CLI/tool/agent/lease/engine evidence and release packaging.
- New engines implement the same narrow typed adapter and full qualification; no creator or arbitrary-exec API is added.
- The CLI argv/schema spike and tool baseline must land before orchestration implementation.
