---
phase: 7
title: "Integration Packaging and Qualification"
status: pending
priority: P0
dependencies: [1, 2, 3, 4, 5, 6]
effort: ""
---

# Phase 7: Integration Packaging and Qualification

## Overview

This phase produces the final integration contract, end-to-end qualification harness, release-blocking matrices, reproducible packaging, and support tooling that together certify the complete system against the contracts in /Users/huynguyen/Personal/Hermito/docs/technology-stack.md and /Users/huynguyen/Personal/Hermito/docs/design-guidelines.md. All prior phases deliver isolated components; this phase wires them through the single Authority abstraction, exercises every cross-boundary path, and gates release on observable system-level behaviors only. No feature internals are re-specified.

## Context Links

- /Users/huynguyen/Personal/Hermito/docs/technology-stack.md (approved stack, architecture rules, release-blocking gates, external tool policy, compatibility baselines)
- /Users/huynguyen/Personal/Hermito/docs/design-guidelines.md (authority model, keyboard model, trust, status feedback, terminal constraints, destructive action patterns)
- /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-01-core-workbench-and-editor.md (workbench, editor, command palette, authority strip, tool windows)
- /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-02-terminal-and-execution-transport.md (crossterm ownership, portable-pty, TerminalSurface, PTY parse isolation)
- /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-03-language-intelligence.md (LSP 3.17 client, document revision + epoch tagging, capability gating)
- /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-04-advanced-local-git.md (authority-local Git argv, porcelain v2/NUL, recovery sequences)
- /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-05-dev-container-orchestration.md (pinned Dev Container CLI, Docker then Podman adapters, environment epoch)
- /Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-06-secure-port-forwarding.md (host loopback broker, SSH hop, container relay, 127.0.0.1-only)

## Requirements

The integrated system must satisfy every rule in the approved contracts plus the following observable integration requirements:

- Authority transitions preserve host-owned buffers/app-state; typed execution context and epochs reject cross-route results. PTY is Lost, LSP reopens, Git queries/reconciles, and broker listeners remain stable through container rebuild.
- Recovery covers host crash, outer/inner helper loss, engine restart, and duplicate app instances. No session/byte/mutation replay or leaked process/listener.
- Hostile VT/LSP/Git/protocol/container/relay/Unicode corpora remain inert, capped, and manifest-complete.
- Performance gates use release binaries on committed runner classes and a fixed sampling protocol; UI paint is separate from controlled cached/cold readiness.
- Keyboard/focus and contrast matrices cover every visible state without mouse.
- Closed TOML is the only extension mechanism. User/workspace source scope, effective config hash, fixed argv/cwd/environment capabilities, and trust behavior are explicit; no code/plugin loading.
- Deterministic support bundles collect allow-listed data only and redact before serialization.
- Reproducible unsigned archives cover five host triples and two static musl helper triples. Each target is built twice in independent clean VMs of one committed native runner class from one sealed source/dependency input; signing follows digest equality.
- Startup/on-demand capability probes consume one committed baseline for Git, OpenSSH, Dev Container CLI 0.88.0, Docker/Compose, gated Podman/provider, configured language servers, release tools, and signing prerequisites. No fallback/download.
- Docker qualification completes before any Podman cell is enabled.
- Production TUF uses embedded root 2-of-3, targets 2-of-3, snapshot 1-of-1, and timestamp 1-of-1. Root keys stay offline; other keys are non-exportable KMS keys reached only from the protected release environment through OIDC. Metadata/target length, hash, version, expiry, rotation, monotonic floors, and atomic install are release gates.
- The same TUF-verified `hermito-remote` targets run as SSH session service and Phase 5 inner `container-agent`; RelayV1 is a protocol mode, not a separately launched target. No helper launches before full verification.
- Protocol framing/JSON/Relay caps reject before allocation. SSH uses explicit identities and hardened host keys. Broker owns paired loopback listeners; no `ssh -L`/publish/per-connection exec.
- Dev Container CLI is sole build/up creator and normal exec launcher. Narrow adapters expose only Phase 5 typed inspect/lifecycle/verified-agent/lease controls. Canonical LSP/Git/Relay reuse `ExecutionContextV1`; Git config and mutations retain Phase 4 guarantees.
- Release aggregation accepts only fresh GitHub OIDC artifact attestations from exact workflow/ref/run and committed runner/tool/fixture/source digests. Missing, stale, replayed, or wrong-architecture cells fail.
## Architecture

Integration uses one Authority surface, `ExecutionContextV1`, and canonical protocol families. The host owns buffers, journal, app state, trust, TUF verification, capability probes, broker listeners, and Crossterm. The signed outer/inner `hermito-remote` binary owns remote/container execution but no updater/signer. Release qualification is a separate supply-chain boundary: immutable runner baselines → two clean builds → digest compare → artifact attestation → platform signing and threshold TUF metadata → published-artifact rerun.

## Related Code Files

Future files and modules that must exist after phases 1-6 and are exercised or extended only in this phase (absolute paths):

- /Users/huynguyen/Personal/Hermito/Cargo.toml (workspace root with members for hermito, hermito-protocol, hermito-remote)
- /Users/huynguyen/Personal/Hermito/crates/hermito-protocol/Cargo.toml
- /Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/lib.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/types.rs (`ExecutionContextV1`, identities, revision/epoch envelopes)
- /Users/huynguyen/Personal/Hermito/crates/hermito-remote/src/main.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito-remote/src/protocol.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/Cargo.toml
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/main.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/app.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/mod.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/local.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/ssh.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/devcontainer_authority.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/config/mod.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/config/schema.rs (extension TOML schema + validation)
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/config/loader.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/diagnostics/mod.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/diagnostics/support_bundle.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/diagnostics/redactor.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/probes/tool_capability.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/probes/startup.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/tuf_verifier.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/terminal/mod.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/terminal/surface.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/git/mod.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/lsp/client.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/src/ports/port_broker.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration/authority_transitions.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration/recovery.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration/hostile_corpus.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration/performance.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration/keyboard.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration/config_extensions.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration/diagnostics_redaction.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/integration/packaging.rs
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/fixtures/hostile/
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/fixtures/hostile/manifest.toml
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/fixtures/devcontainer/hermito-test/
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/fixtures/git-repo/
- /Users/huynguyen/Personal/Hermito/crates/hermito/tests/fixtures/ssh-test/ (test-only keys/config)
- /Users/huynguyen/Personal/Hermito/scripts/build-reproducible.sh
- /Users/huynguyen/Personal/Hermito/scripts/prepare-tuf-signing-request.sh
- /Users/huynguyen/Personal/Hermito/scripts/verify-signing-prerequisites.sh
- /Users/huynguyen/Personal/Hermito/scripts/probe-capabilities.sh
- /Users/huynguyen/Personal/Hermito/scripts/qualify-docker.sh
- /Users/huynguyen/Personal/Hermito/scripts/qualify-podman.sh
- /Users/huynguyen/Personal/Hermito/scripts/generate-support-bundle.sh
- /Users/huynguyen/Personal/Hermito/scripts/release-matrix.sh
- /Users/huynguyen/Personal/Hermito/packaging/hermito.toml.template
- /Users/huynguyen/Personal/Hermito/packaging/remote-helper-manifest.json
- /Users/huynguyen/Personal/Hermito/packaging/tool-baselines.toml
- /Users/huynguyen/Personal/Hermito/packaging/runner-baselines.toml
- /Users/huynguyen/Personal/Hermito/packaging/performance-baselines.toml
- /Users/huynguyen/Personal/Hermito/packaging/signing-policy.toml
- /Users/huynguyen/Personal/Hermito/packaging/ci-actions.lock
- /Users/huynguyen/Personal/Hermito/.github/workflows/qualification.yml
- /Users/huynguyen/Personal/Hermito/.github/workflows/release.yml
- /Users/huynguyen/Personal/Hermito/config/example.toml
- /Users/huynguyen/Personal/Hermito/LICENSE (Apache-2.0)
- /Users/huynguyen/Personal/Hermito/README.md (packaging and qualification instructions section)

## Implementation Steps

1. Verify the existing three-member workspace; extend only pinned phase-qualified dependencies. Generate and gate `cargo deny` license/advisory/source results plus CycloneDX SBOM. `hermito-remote` dependency graph must prove absence of TUF, HTTP, TLS, updater, and signing crates.
2. Finalize wire ownership: `ExecutionContextV1` and canonical LSP/Git/Container/Relay families live only in hermito-protocol. The host-only TUF wrapper owns update policy/floors/install. Add compile-time feature/dependency checks against forbidden helper links.
3. Expose one crate-library Authority harness entrypoint; the binary remains thin. Integration tests live under `crates/hermito/tests` so `cargo test -p hermito --test integration` is a real target.
4. Implement integration modules for authority transitions, crash/reconnect, hostile corpus, keyboard, config, redaction, performance, and packaging. Drive public workbench/Authority APIs and real child/helper/engine boundaries; no source-text assertions or bypass dispatch.
5. Implement the canonical Rust capability probe from `packaging/tool-baselines.toml`. Fixed argv, bounded stdout/stderr/time, strict version parser, and named behavior checks cover Git `>=2.39,<3.0`, OpenSSH identity/host-key flags, Dev Container CLI exactly `0.88.0`, Docker/Compose enabled cells, gated Podman/provider pairs, configured language servers, Rust/linkers/archive tools, `tuftool 0.17.0`, cargo-deny, cargo-cyclonedx, and platform signers.
6. Implement deterministic support bundles. Collector takes an explicit allow-list object—not filesystem roots—redacts secrets/paths before any serializer sees them, normalizes order/time/mode, and emits a deterministic archive. The wrapper only invokes `hermito --support-bundle`.
7. Implement closed TOML with source provenance. User config and workspace config are parsed separately with `deny_unknown_fields`; executable task/command/LSP entries are fixed argv + cwd enum + literal allow-listed env + capability class. Workspace executable entries require review and a grant for source/effective hash; all execution still requires current Authority trust. No shell strings, PATH mutation, secret fields, or dynamic loading.
8. Populate immutable fixtures. Every hostile file is listed once with category, bytes, and SHA-256; test enumerates manifest and directory both ways. Git/devcontainer tests copy templates to unique temp roots. Test SSH/TUF keys are marked test-only and rejected by production key IDs.
9. Create runner/tool/performance/signing baselines. Each release runner class records required labels, OS build/image digest, native architecture, CPU/RAM floor, Rust target/toolchain, SDK/linker/archive versions and deterministic flags. CI fails before build when host architecture or any identity differs. Pin third-party Actions by full commit SHA in `ci-actions.lock`.
10. Implement `build-reproducible.sh`. Accept only seven manifest targets; verify sealed source revision and Cargo.lock/vendor checksums; run `cargo build --release --frozen --offline --target`; disable incrementality; apply path remap and target-specific deterministic linker flags (`-no_uuid`, stable Linux build-id policy, `/Brepro`); normalize archive order/path/mode/uid/gid/mtime to `SOURCE_DATE_EPOCH`; emit unsigned archive, binary digest, SBOM, license notice, and provenance JSON. Two builds use independent clean VMs and caches of the same runner class.
11. Implement performance harness and baseline protocol. A runner must match `performance-baselines.toml`, be AC-powered/idle with no concurrent job, and record CPU/RAM/OS/power/tool data. UI/render/LSP/Git/PTY/palette: 5 warmups + 30 samples (render: 500 warmups + 5,000 samples); cached readiness: 5 + 30; cold readiness: 1 + 10 with app metadata/container/helper state reset and preloaded artifacts. Use monotonic clocks, nearest-rank p95 `sorted[ceil(.95*n)-1]`, store every raw sample, and fail on missing/reset-contaminated samples. Public-network samples are diagnostic only.
12. Implement Docker qualification and enable Podman only from explicit passed cells in the baseline. Each cell invokes `cargo test -p hermito --test integration` filters plus real engine/SSH fixtures and emits machine-readable evidence; unsupported cells are `DISABLED(reason)`, never PASS.
13. Build remote-helper manifest for exactly two musl targets with protocol range, modes `session` and `container-agent`, length, SHA-256, source/tool/runner provenance, and reproducibility attestation IDs. RelayV1 is a protocol capability in both modes, not `relay-stdio`.
14. Establish production TUF prerequisites before release automation: root 2-of-3 offline Ed25519 public keys/ceremony and recovery document; targets 2-of-3 non-exportable KMS keys; snapshot and timestamp distinct 1-of-1 KMS keys; GitHub OIDC role restricted to protected release environment/workflow/ref; key IDs/thresholds/expiry policy committed in `signing-policy.toml`; private material absent from repository/runners. Timestamp max age/expiry is 24 h/7 d, snapshot 7 d/30 d, targets 30 d/180 d, root 180 d/730 d.
15. Implement `prepare-tuf-signing-request.sh` around pinned `tuftool 0.17.0`. It accepts only twice-reproduced attested helper digests, increments versions monotonically, requests 2 targets KMS signatures plus snapshot/timestamp signatures, validates the resulting full chain with the embedded production root and host verifier, and emits an immutable publication bundle. Root rotation is a separate 2-of-3 offline ceremony; CI cannot sign root.
16. Implement signing prerequisite preflight. macOS requires protected Developer ID identity + notarization credentials and verifies ticket/staple; Windows requires protected Authenticode service/certificate and verifies chain/timestamp; TUF requires OIDC/KMS key access and thresholds. Missing prerequisite fails before release fan-out. Secrets use protected environments, ephemeral keychains/tokens, log masking, and cleanup.
17. Implement release matrix. Dispatch two independent clean builds for each exact native runner class, compare unsigned digests, run native/engine/performance qualifications, create GitHub OIDC build-provenance attestations, and aggregate only attestations from the same source commit, protected workflow ref/run attempt, runner-baseline digest, tool-baseline digest, fixture digest, and outputs produced within 24 h. Verify with `gh attestation verify`; reject reuse across releases.
18. Only after aggregation: codesign/notarize/staple macOS, Authenticode-sign/timestamp Windows, publish Linux checksums/SBOM/provenance, create and verify threshold TUF bundle, then publish. Download published artifacts/metadata into clean native cells and rerun version, signature/TUF, helper launch, Docker-before-Podman, Relay, PTY Lost, and cold-readiness smoke gates.
19. Keep probe/support/qualification shell scripts as thin argument allow-lists over Rust binaries. No duplicate version parser, baseline, signing policy, or matrix logic.

## Success Criteria

- `cargo test -p hermito --test integration -- --quiet` discovers the crate-owned target, exits 0, and emits every applicable local cell; engine/release cells are separate explicit commands.
- Authority/context/epoch tests prove no cross-route result; PTY Lost, LSP reopen, Git no-replay, stable rebuild listeners, and lease CAS behavior match phase contracts.
- Hostile manifest and directory enumerate each other exactly; all bytes are consumed with zero escape/process/panic and every cap enforced.
- Performance evidence comes only from matching baseline runners and contains environment attestation, warmups, all raw samples, reset logs, calculation, and p95. Uncontrolled network data never satisfies a gate.
- Keyboard/contrast coverage passes every default/context-open state without mouse.
- Config tests prove source provenance, unknown-field rejection, hash review, fixed argv/cwd/env capability, trust, and no shell/dynamic loading.
- Support bundle is byte-reproducible from fixed input and contains no synthetic prohibited value; collector-open audit shows no credential/environment/root walk.
- Each of five host and two helper targets has matching unsigned archive + binary digests from two independent native baseline VMs, plus SBOM/license/provenance and OIDC attestations.
- Production TUF metadata meets configured key IDs/thresholds/expiry, contains both helper digests/modes, and passes root rotation, rollback/replay/freeze, monotonic-floor, and atomic-install tests. Test keys cannot validate production.
- Docker is 100% before enabled Podman cells; disabled cells retain explicit reasons.
- Capability output and all baseline/runner/action/signing/fixture digests match committed files; no wildcard/placeholder.
- Published hosts have valid platform signatures/timestamps/notarization where applicable. Published helpers report compatible protocol/modes and launch only through verified TUF.
- Aggregation verifies same release run/source/ref, native architecture, all baseline/evidence digests, GitHub OIDC provenance, and <=24 h freshness; replay from another run/release fails.
- No test/script/manifest/baseline/evidence contains TODO, FIXME, wildcard version, placeholder, private key, or secret.

## Test and Validation Matrix

### End-to-End Authority Transition Matrix (release blocking)

| Transition Sequence                  | Live Artifacts Exercised                  | Observable Success (exact)                                      | Platforms          | Blocking |
|--------------------------------------|-------------------------------------------|-----------------------------------------------------------------|--------------------|----------|
| LOCAL → SSH → DEVCONTAINER → LOCAL | host buffer/revision, terminal, Git, broker lease | bytes/revision unchanged; contexts/epochs monotonic; stale results rejected; PTY replacement is user-started; broker port remains owned until explicit route release | macOS, Linux, Windows hosts → Linux SSH target | YES |
| LOCAL → DEVCONTAINER (direct) | same + LSP + lease | rebuild increments epoch; LSP reopens current host text; diagnostics match; paired listeners remain bound while relay target rotates | all host platforms | YES |
| SSH → DEVCONTAINER (drop/reconnect) | terminal, uncommitted hunk, 2 forwards | PTY Lost; Git query/reconcile no replay; hunk remains; listeners stay bound but streams fail closed until verified contexts return | certified Linux SSH target | YES |

### Crash / Restart / Reconnect Recovery Matrix (release blocking)

| Failure Mode                  | Recovery Action                          | Observable Success                                                                 | Blocking |
|-------------------------------|------------------------------------------|------------------------------------------------------------------------------------|----------|
| host SIGKILL after remote Git mutation dispatch | recover journal, reconnect, query op ID, read fresh repo state | host buffer restored; Completed result returned without rerun, Pending/Unknown enters explicit reconciliation; no claim that `git commit` can resume | YES |
| SSH drop during terminal exec | reconnect helper | old PTY is `Lost`; exact status instructs user to start a new terminal; no transparent resume | YES |
| Dev Container stop while LSP active | trusted restart/rebuild, new environment epoch, reopen documents | server reinitializes from current host text; stale diagnostics rejected; no duplicates | YES |
| host restart with active port forwards | reconcile lease records and rebind paired listeners | same advertised port restored only if both loopback addresses bind; otherwise explicit conflict; relay carries current epoch | YES |

### Hostile Input Corpus Matrix (release blocking)

| Corpus Category | Fixture Count Source | Success Criteria |
|---|---|---|
| VT/terminal bytes | hostile manifest | zero child bytes/control effects reach Crossterm; bounded inert cells; no panic |
| LSP/JSON-RPC | hostile manifest | malformed/oversized messages rejected within quotas; no state mutation or UI block |
| Git porcelain/diff/paths | hostile manifest | exact bounded parse; no command, terminal, or path injection |
| Unicode/grapheme/coordinate | hostile manifest | canonical-domain round trips and deterministic snapping pass on every host |
| SSH/protocol/container output | hostile manifest | frame caps hold and every displayable byte remains inert |

### Performance Budget Matrix (release blocking)

All gates use a matching committed runner class, release binary, monotonic clock, nearest-rank p95, raw-sample artifact, and no concurrent job. Standard samples are 5 warmups + 30 recorded; render is 500 + 5,000; cold readiness is 1 + 10 with reset evidence.

| Operation | Budget p95 | Controlled input | Samples | Blocking |
|---|---:|---|---:|---|
| Authority selection + paint | <=80 ms | event injection to completed frame; excludes readiness | 5+30 | YES |
| Cached SSH/container readiness | <=2 s | local SSH fixture + running pinned container/verified agents | 5+30 | YES |
| Cold readiness | SSH install <=15 s; preloaded container up <=180 s | reset app/helper/container state; no public network | 1+10 | YES |
| Full 160×45 render | <=16 ms | fixed snapshot/update corpus | 500+5,000 | YES |
| LSP diagnostic apply | <=50 ms | predecoded capped server frame to painted Problems state | 5+30 | YES |
| Git 10k status parse | <=30 ms | fixed porcelain-v2 byte fixture; parse only | 5+30 | YES |
| PTY 1 KiB surface update | <=5 ms | fixed hostile-safe byte burst | 5+30 | YES |
| Palette open + filter | <=12 ms | fixed 1,000-command model to paint | 5+30 | YES |

### Accessibility / Keyboard Completeness Matrix (release blocking)

| Requirement                        | Verification Method                              | Observable Success                                      | Blocking |
|------------------------------------|--------------------------------------------------|---------------------------------------------------------|----------|
| Full landmark cycles | crates/hermito/tests/integration/keyboard.rs | default: F6 x8; context-open: F6 x9; each visible landmark focused | YES |
| Primary tool windows               | Alt+1..4                                         | each opens correct pane; no mouse event recorded       | YES     |
| Command search + execute           | Ctrl/Cmd+K then arrows+Enter                     | executes without pointer                               | YES     |
| Esc releases capture + dialogs     | capture terminal then Esc                        | returns focus to invoker; never changes authority      | YES     |
| High-contrast + color AA | automated token contrast calculation + terminal-profile visual fixture | >=4.5:1 text and >=3:1 focus/borders for every state token | YES |
| Status announcements               | status bar text after every major op             | live text matches design patterns; no emoji            | YES     |

### Config-Only Extension Schema Matrix

| Extension Kind       | Declared In                  | Success Observable                                      | Blocking |
|----------------------|------------------------------|---------------------------------------------------------|----------|
| tasks | `[tasks]` user/workspace | fixed argv/cwd/env capability; workspace hash review; CURRENT authority + grant | YES |
| keybindings | `[keybindings]` | valid override; collision/unknown key rejects whole source | YES |
| lsp.servers | `[lsp.servers]` user-owned executable spec | config hash in trust; context-local identity/version/capability probe | YES |
| layouts | `[layouts]` | only declared structural IDs/ratios; unknown rejects | YES |
| commands | `[commands]` user/workspace | fixed argv capability; source shown; no shell/dynamic load | YES |

### Diagnostics / Support Bundle Redaction Matrix (release blocking)

| Redaction Target       | Input Source             | Observable in Bundle          | Blocking |
|------------------------|--------------------------|-------------------------------|----------|
| SSH/private-key-shaped data | synthetic redaction fixtures only | absent or `[REDACTED]`; collector never opens credential directories | YES |
| Git/API credential tokens | synthetic config/env fixture | absent | YES |
| Engine socket path | synthetic probe result | redacted basename only; no socket access | YES |
| Environment | explicit allow-list fixture | only approved `HERMITO_*` diagnostics fields; no full `PATH` dump | YES |
| Workspace absolute path | synthetic buffers + Git result | replaced by `$WORKSPACE` | YES |

### Packaging & Reproducibility Matrix (release blocking)

Qualification accepts two independent clean native baseline VMs per target, not two directories in one job. Both consume one sealed source/dependency input but separate caches. Unsigned archive/binary digests must match before OIDC attestation and any platform/TUF signature.

| Target | Host artifact | Remote-helper target | Unsigned SHA-256 match | Post-repro release treatment | License | Blocking |
|---|---|---|---|---|---|---|
| aarch64-apple-darwin | `hermito` | — | two clean native builds | codesign + notarize host archive | Apache-2.0 | YES |
| x86_64-apple-darwin | `hermito` | — | two clean native builds | codesign + notarize host archive | Apache-2.0 | YES |
| x86_64-unknown-linux-gnu | `hermito` | `hermito-remote` (`x86_64-unknown-linux-musl`) | both artifacts, two clean native-architecture builds | host checksums/provenance; helper digest in threshold-signed TUF metadata | Apache-2.0 | YES |
| aarch64-unknown-linux-gnu | `hermito` | `hermito-remote` (`aarch64-unknown-linux-musl`) | both artifacts, two clean native-architecture builds | host checksums/provenance; helper digest in threshold-signed TUF metadata | Apache-2.0 | YES |
| x86_64-pc-windows-msvc | `hermito.exe` | — | two clean native builds | Authenticode-sign host archive | Apache-2.0 | YES |
### Dependency / Tool Capability Probe Matrix (release blocking)

`packaging/tool-baselines.toml` stores fixed probe argv, parser, accepted range, and named capability. `runner-baselines.toml` owns OS/architecture/SDK/linker/archive identity; `ci-actions.lock` owns full action SHAs.

| Tool/capability | Fixed probe | Observable | Blocking |
|---|---|---|---|
| Git | `git --version` + behavior probes | `>=2.39,<3.0`; safe profiles pass | YES |
| Docker/Compose | version JSON + Compose version/config fixture | exact enabled provider cell | YES |
| Podman/provider | version JSON + provider fixture | exact explicitly enabled post-Docker cell | YES |
| Dev Container CLI | `devcontainer --version` + fixed schema fixtures | exactly 0.88.0 | YES |
| OpenSSH | `ssh -V` + bounded ssh/sftp/keyscan contracts | explicit identity/host-key flags pass | YES |
| Language servers | configured fixed version argv + initialize | version/digest/capability baseline | YES |
| Release tools | Rust/linker/archive, tuftool, deny, cyclonedx, platform verifiers | exact runner/tool baseline | YES |
| Signing services | non-secret identity/preflight only | protected env, certificate/KMS key IDs and thresholds available | YES |

### Staged Docker then Podman Qualification Matrix (release blocking)

| Stage   | Engine   | Fixture Used                              | Cells Exercised                          | Gate Condition                  | Blocking |
|---------|----------|-------------------------------------------|------------------------------------------|---------------------------------|----------|
| 1 | Docker | crates/hermito/tests/fixtures/devcontainer/hermito-test | enabled authority/terminal/Git/LSP/Relay/lease cells | all PASS before Podman | YES |
| 2 | Podman | same fixture + qualified provider config | only explicitly enabled identical cells | all enabled cells PASS; disabled named | YES |

## Risk Assessment

- Reproducibility drift: immutable native runner identities, independent VMs/caches, sealed inputs, deterministic linker/archive flags, raw provenance, and digest comparison before signing. Container builds never substitute for macOS/Windows.
- Signing/TUF compromise: protected environment, GitHub OIDC least-privilege KMS policy, role-separated non-exportable keys, 2-of-3 targets threshold, offline 2-of-3 root, preflight, full-chain self-verification, and published-artifact rerun.
- Podman parity is never inferred; cells remain disabled until identical evidence passes after Docker.
- Fixture growth requires bidirectional manifest update; changed/unlisted/missing bytes fail.
- Benchmark noise/gaming: exact runner/power/idle/reset/sample/p95 protocol and raw samples; mismatched machines are diagnostic only.
- Terminal keyboard/color claims combine event model tests with native PTY/terminal-profile smoke cells.

## Security Considerations

- Test harness grants only exact scoped capabilities through the same UI/action boundary; InspectOnly remains default. Resolve/create/execute/network/forward grants are distinct where specified.
- Redactor receives only allow-listed records and runs before serialization.
- Helper launch always follows production/test-root-separated full-chain verification and exact target hash. Inner agent is reverified on copy.
- PTY/LSP/Relay loss never resumes bytes/sessions; Git never replays an ambiguous mutation.
- Frame/JSON/Relay caps and context/epoch/lease validation precede allocation/dial/model mutation.
- SSH uses accepted host keys plus explicit identity/one-shot askpass; no default key, agent, config, proxy, or forwarding.
- Dev Container CLI 0.88.0 alone resolves/builds/creates and normally execs; narrow adapters expose only enumerated controls. Persistent verified inner agent handles canonical LSP/Git/Relay.
- Fixtures mount only test-workspace paths; no engine socket, agent, host credential, or published port.
- Capability/signing probes use fixed non-secret argv/API identities only.
- Hostile corpus is parser/surface input only.
- Release provenance is OIDC-attested and run-bound; KMS/platform signing occurs after reproducibility. Private keys never enter artifacts/logs/runners except ephemeral protected platform credentials where unavoidable.

## Next Steps

- Update the README packaging section with the exact local, Docker, Podman, and release-aggregation commands after the matrices pass.
- Tag and publish five native `hermito` artifacts plus two versioned Linux `hermito-remote` TUF targets through the gated release workflow.
- Freeze the two-target remote-helper allow-list in `packaging/remote-helper-manifest.json`; additions require the full remote qualification gates.
- Start post-1.0 work only after published-artifact reruns pass Docker-then-Podman, full TUF, controlled cold readiness, PTY Lost, and native-runner aggregation checks.
