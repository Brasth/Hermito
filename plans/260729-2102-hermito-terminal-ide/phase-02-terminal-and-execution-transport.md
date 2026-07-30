---
phase: 2
title: "Terminal and Execution Transport"
status: pending
priority: P1
dependencies: ["phase-01-core-workbench-and-editor"]
effort: "L"
---

# Phase 2: Terminal and Execution Transport

## Overview

Establish the local and remote execution boundary for Hermito. Define and implement the `TerminalSurface` abstraction that receives parsed PTY output as inert cell grids; raw child bytes never reach the host Crossterm terminal. Deliver a portable local PTY implementation using the `portable-pty` crate (Unix PTYs and Windows ConPTY only). Implement hardened system OpenSSH bootstrap: `-F none`, explicitly constructed environment, Hermito-only known-hosts file, explicit fingerprint acceptance, `StrictHostKeyChecking=yes` (never `accept-new`). Create the `hermito-protocol` library with length-prefixed framing, per-class and aggregate allocation caps, and a versioned top-level `Message` dispatcher that is extensible for later LSP/Git/container/relay variants. The `hermito-remote` helper binary is launched over SSH (or locally) and multiplexes file, PTY, and process operations only after successful TUF verification of the helper (full root/timestamp/snapshot/targets chain, expiry, rollback/freeze checks) using production metadata contract and test fixtures. Introduce the single `Authority` trait that routes operations to the correct local, SSH, or (future) container backend. Remote PTY sessions do not transparently survive transport loss: on loss the session is marked Lost and a new terminal session must be requested. All authorities start InspectOnly; execution requires explicit grant.

## Context Links

- `/Users/huynguyen/Personal/Hermito/docs/technology-stack.md` (portable-pty, TerminalSurface, system OpenSSH, hermito-remote helper, authority rules, release gates)
- `/Users/huynguyen/Personal/Hermito/docs/design-guidelines.md` (authority path rendering, trust, `CURRENT`, inspect-only)
- `/Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/phase-01-core-workbench-and-editor.md` (workbench, editor, layout, and event loop that will consume TerminalSurface)
- `/Users/huynguyen/Personal/Hermito/plans/260729-2102-hermito-terminal-ide/plan.md`

## Requirements

- Exactly one `Authority` abstraction in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/mod.rs` that dispatches file read/write, PTY spawn, process exec, and (future) Git/LSP operations. Local and remote implementations must be interchangeable behind the trait.
- `TerminalSurface` (defined in `/Users/huynguyen/Personal/Hermito/crates/hermito/src/terminal/surface.rs`) is an inert grid of cells (char + style + hyperlink metadata) produced by a replaceable VT parser. No escape sequences, OSC, or control bytes ever propagate to the host terminal.
- Local PTY uses `portable_pty::CommandBuilder` + `PtyPair` on Unix and ConPTY on Windows. No WinPTY fallback. PTY size is kept in sync with the bottom terminal pane rect.
- Remote transport uses installed OpenSSH `ssh`, `sftp`, and bounded `ssh-keyscan`. The user must select an identity file (and optional certificate) for the authority; both `ssh` and `sftp` receive `-F none`, Hermito known-hosts, `StrictHostKeyChecking=yes`, `IdentitiesOnly=yes`, `IdentityAgent=none`, `PreferredAuthentications=publickey`, and explicit `-i`/`CertificateFile` argv. Default `~/.ssh/id_*`, config, agents, forwarding, proxy/local-command, and ControlMaster paths are prohibited. An encrypted key uses a local one-shot `hermito --ssh-askpass <nonce-endpoint>` channel after explicit UI prompt; no password-auth fallback or passphrase persistence exists.
- The `hermito-remote` helper is versioned and signed. Phase 2 uses a pinned, audited TUF client in the host crate to verify the complete root → timestamp → snapshot → targets chain and the selected target's length/hash. Only after authority trust is granted does hardened OpenSSH SFTP place those exact bytes at a digest-addressed remote path using upload-to-temp, read-back hash verification, permission setting, and same-directory atomic rename. SSH launches that absolute verified path—never `PATH` lookup or an arbitrary pre-placed binary. Helper multiplexing carries concurrent PTY, file, and process streams over one length-prefixed `hermito-protocol` channel.
- `hermito-protocol` top-level dispatcher (`Message` enum or tagged union) is versioned (major/minor) and extensible: variants for PTY/FS/Process plus reserved extension points for future LSP, Git, Container, Relay messages. Unknown high-level tags are rejected early. Every frame is size-capped from its fixed-width header before payload allocation, with per-class limits plus a connection aggregate in-flight budget.
- Every envelope carries request ID plus workspace/environment epochs and `ExecutionContextV1::{AuthorityRoot, DevContainer { container_id, environment_epoch }}`. Phase 2 emits only AuthorityRoot but reserves the typed container context for phases 3-5 so canonical LSP/Git variants are extended rather than duplicated. Document-derived messages additionally require document revision; other messages use `None`.
- Process supervision is bounded: every child (local PTY or remote exec) has an explicit `CancellationToken`, a stdout/stderr byte budget (e.g. 4 MiB ring or hard cap before truncation), and a wall-time watchdog. On cancellation the token is cancelled, PTY is closed, and the helper is instructed to SIGTERM + wait + SIGKILL.
- Reconnect: loss of SSH transport must not corrupt open editor buffers (Phase 1 journal recovery has already run). Local unsaved state survives via journal. Remote PTY sessions are marked Lost on transport loss; no transparent resume or reattach promise is made. User must request a new terminal session. "Lost" state is shown in authority path and terminal header. Re-establishing the helper allows new PTY sessions only.
- Hostile output containment: the VT parser must treat all input as untrusted. Unknown sequences, long lines, binary garbage, and OSC 52 clipboard must be dropped or rendered as replacement characters inside `TerminalSurface`. The parser must never allocate unboundedly on malicious input.
- The bottom terminal pane (added to the Phase 1 workbench layout) renders a `TerminalSurface` snapshot using Ratatui. Input capture mode forwards keystrokes to the active PTY via the authority; `Esc` releases capture.
- Authority path (Phase 1) is extended to show remote segments once an SSH authority is selected and connected. Trust is per-authority and persisted; every authority (including SSH) starts InspectOnly. Explicit `GrantTrust` is required before any PTY spawn or exec on that authority.
- All PTY bytes flow: child → parser → TerminalSurface → Ratatui render. Crossterm is used only for host keyboard/mouse → authority PTY input and for final host screen paint.
- On Windows the implementation must use the native ConPTY surfaces provided by `portable-pty`; pseudoconsole handles are owned and cleaned up.
- Helper binaries are static `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` artifacts. The same verified binary later runs persistent `container-agent` mode whose multiplexed protocol includes RelayV1; no separate relay mode/binary. Other remote targets remain disabled until Phase 7 full qualification.
- No ambient environment variables, current working directory, or file descriptors are inherited by remote helpers except those explicitly constructed by the authority request.

## Architecture

The execution stack is layered:

1. `Authority` trait (one instance per reachable execution context). Methods: `spawn_pty(cmd, size, token) -> PtySession`, `exec(cmd, token) -> ExecResult`, `read_file(path, revision) -> FileContent`, `write_file(...)`, etc. Every result carries request ID plus workspace/environment epochs; document operations additionally carry document revision.
2. Local authority implementation (`LocalAuthority`) uses `portable_pty` directly on the host.
3. Remote authority implementation (`SshAuthority`) uses a hardened OpenSSH transport and one `hermito-remote` process. Before activation, the host's pinned TUF client verifies the complete metadata chain and target bytes, installs or revalidates those bytes at a digest-addressed remote path through hardened SFTP, reads the installed file back and verifies its target hash, then launches that exact absolute path. Requests use the versioned extensible `hermito_protocol::Message` dispatcher; the fixed-width header is parsed into bounded lengths before payload allocation.
4. `TerminalSurface` owns a 2D grid of `Cell { ch: char, fg: Color, bg: Color, attrs: Attrs, ... }` plus cursor and scrollback. A `VtParser` (initially a minimal vt100 subset, later swappable) consumes bytes from the PTY reader and mutates the surface. The surface is snapshot-copied to the UI thread at most once per frame.
5. `ProcessSupervisor` owns the cancellation token, byte counters, and watchdog for each PTY or exec. It is responsible for draining the PTY reader task and enforcing budgets.
6. `SshBootstrap` + host-only TUF verifier + helper installer: acquire bounded host-key candidates with the installed OpenSSH `ssh-keyscan`, calculate fingerprints in-process, require explicit user comparison/acceptance, and write the exact accepted key to Hermito's known-hosts store. Then verify the TUF chain with the pinned audited client, install/revalidate the target via SFTP temp upload + read-back hash + same-directory rename, and return a digest-bound absolute launch path. Any mismatch quarantines the candidate and prevents SSH exec.
7. `hermito-protocol` defines wire types using a versioned top-level `Message` enum that is forward-extensible for LSP/Git/Container/Relay. A single codec is selected and pinned by the implementation spike. Fixed-width headers carry class and length; per-class plus aggregate budgets are enforced before payload allocation. Round-trip and hostile-frame tests are release gates.
8. `hermito-remote` is a small Tokio stdio service dispatching local PTY, filesystem, process, and later versioned extension requests. It never reads shell startup files. Host-side TUF verification and digest-bound remote installation/selection happen before its absolute path can be launched.

The Phase 1 event loop and workbench are extended with a bottom terminal tab that holds `Option<TerminalHandle>`. When a terminal is opened on an authority (after explicit GrantTrust for InspectOnly start), the handle wires PTY output into a `TerminalSurface` and input from the captured keyboard into the authority's PTY writer. On transport loss a PTY handle transitions to Lost state; the UI offers "New terminal session" rather than resume.

All heavy I/O (PTY reads, SSH, helper I/O) occurs on Tokio tasks. The UI thread only ever sees immutable snapshots carrying revision/epoch. Journal recovery from Phase 1 has already completed before any reconnect attempt.

## Related Code Files

All paths absolute under `/Users/huynguyen/Personal/Hermito`.

New crates:
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/Cargo.toml`
- `/Users/huynguyen/Personal/Hermito/crates/hermito-remote/Cargo.toml`
- `/Users/huynguyen/Personal/Hermito/crates/hermito-remote/src/main.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/tests/fixtures/tuf/` (generated test repository, committed test-only root/keys/config, success and rejection metadata; no production key)
- `/Users/huynguyen/Personal/Hermito/scripts/generate-tuf-test-fixtures.sh` (fixed wrapper over pinned `tuftool 0.17.0`; builds the native static helper test target and emits root/targets/snapshot/timestamp fixtures)

Host crate additions and changes:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/Cargo.toml` (add hermito-protocol as workspace dependency, portable-pty, OpenSSH `ssh`/`sftp` process invocation support, tokio-util, bytes, and TUF/hash dependencies)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/mod.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/local.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/ssh.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/tuf_verifier.rs` (host-only wrapper around the pinned audited TUF client; hard prerequisite gate for SshAuthority activation)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/remote/helper_installer.rs` (hardened SFTP upload/revalidation, digest-addressed path selection, read-back hash, same-directory atomic rename)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/authority/types.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/terminal/surface.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/terminal/vt_parser.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/terminal/mod.rs` (extend from phase 1)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/pty/mod.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/pty/local.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/pty/session.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/remote/ssh_bootstrap.rs` (hardened launcher contract: -F none, StrictHostKeyChecking=yes, explicit fingerprint, constructed env, Hermito-only known hosts)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/remote/ssh_identity.rs` (explicit identity/certificate validation and one-shot askpass capability)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/remote/helper_launcher.rs` (TUF gate before any exec)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/remote/multiplexer.rs` (versioned dispatcher + frame cap enforcement before routing)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/process/supervisor.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/process/cancellation.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/terminal_pane.rs` (new widget consuming TerminalSurface)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/ui/workbench.rs` (extend bottom area)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/input/terminal_capture.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/config/known_hosts.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/config/trust.rs` (extend from phase 1; InspectOnly + GrantTrust)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/src/remote/tuf.rs` (host-only update policy, trusted-root/cache paths, and monotonic metadata state)

Protocol and wire:
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/lib.rs` (versioned Message top-level + dispatcher entrypoint)
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/dispatcher.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/request.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/response.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/pty.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/fs.rs`
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/src/frame.rs` (size caps + aggregate quota enforcement; early reject before alloc)

Tests and fixtures:
- `/Users/huynguyen/Personal/Hermito/crates/hermito/tests/remote/tuf_verification.rs` (production contract + fixtures)
- `/Users/huynguyen/Personal/Hermito/crates/hermito-protocol/tests/frame_caps.rs` (hostile oversized per-class + aggregate)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/tests/ssh_launcher_contract.rs` (hardened flags, no ambient config, fingerprint flow)
- `/Users/huynguyen/Personal/Hermito/crates/hermito/tests/remote/helper_installation.rs` (stale/pre-placed/substituted helper rejection and exact target launch path)
## Implementation Steps

1. Add `crates/hermito-protocol` and `crates/hermito-remote` to the workspace. Implement capped framing plus the versioned dispatcher and define `ExecutionContextV1` in shared request envelopes; PTY/FS/process use AuthorityRoot, while the reserved DevContainer variant is round-trip tested before later LSP/Git use. Select one pinned codec only after hostile allocation tests.
2. Implement `hermito-remote` as a Tokio stdio session service. It negotiates protocol version, supports concurrent PTY/file/process streams, uses bounded channels, and shuts down children on EOF/authenticated shutdown. It contains no TUF client or signer.
3. Implement `TerminalSurface`, the replaceable VT parser, and `LocalPtySession`. The parser mutates only a bounded inert cell grid/scrollback, drops OSC 52, constrains OSC 8 URI/text lengths, rejects DCS/unknown sequences, and never writes child bytes to Crossterm. Local PTY spawn uses direct argv, fixed cwd, an allow-listed environment, portable-pty/ConPTY only, resize, cancellation, and owned process-tree cleanup.
4. Implement `ProcessSupervisor`: a cancellation token per child, bounded stdout/stderr and PTY queues, a wall-time policy for noninteractive exec, graceful termination followed by a bounded hard-kill, and drain/join acknowledgement. A terminal session may be long-lived but still has byte/scrollback/backpressure limits and explicit user cancellation.
5. Pin an audited Rust TUF client in the host `hermito` crate after an API/fixture spike. Wrap it in `authority/tuf_verifier.rs` and `remote/tuf.rs`; do not implement signature or metadata-chain verification in `hermito-protocol`. Embed/provision the trusted root, persist verified metadata and monotonic version floors atomically, enforce root rotation thresholds, timestamp/snapshot/targets length/hash/version/expiry, freeze/rollback/offline policy, and stream the selected target into a bounded temporary file whose length/hash are verified before use.
6. Implement SSH enrollment and identity flow. `ssh-keyscan` only gathers bounded untrusted host-key candidates for explicit fingerprint acceptance. Separately require a user-selected identity file/optional certificate, validate owner permissions where supported, and construct identical authentication flags for `ssh` and `sftp`: `IdentitiesOnly=yes`, `IdentityAgent=none`, public-key only, explicit identity paths. Encrypted-key passphrases use an expiring one-shot local askpass nonce bound to the pending authority; no ambient/default key, password fallback, logging, or persistence.
7. Implement `helper_installer.rs` and `helper_launcher.rs`. After TUF verification and execution trust, use the hardened SFTP client to upload a unique temp file in the final directory, read it back and verify target length/hash, set executable permissions, rename to a digest-addressed name, then read/hash the final file immediately before launch. Reuse an existing target only after the same final verification. Launch only the returned absolute path and require protocol-version negotiation before authority activation.
7a. Add the test-only TUF fixture pipeline before any remote-helper smoke test. Provision `tuftool 0.17.0` explicitly, build the matching static helper, and run `scripts/generate-tuf-test-fixtures.sh --target <musl-triple> --mode session`; the wrapper reads fixed role versions/expiries/thresholds and committed test keys from `crates/hermito/tests/fixtures/tuf/config/`. The test root is visibly marked non-production. Production roots/private keys are never stored in the repository.
8. Implement the multiplexed helper transport with bounded per-stream queues, fair dispatch, stream cancellation, generation IDs, and ingress caps enforced before routing. Implement `SshAuthority` over it for file, PTY, and process operations; all envelopes carry request ID and epochs, with document revision required only for document-derived messages.
9. Wire the bottom terminal pane and capture model into the Phase 1 workbench. Surface snapshots—not raw bytes—cross into rendering. `Esc` releases capture, pane resize updates the PTY, and closing a pane cancels and joins its supervised session.
10. Wire SSH authority selection into the authority path. Selection may inspect capabilities and enroll a host key, but helper installation/launch, PTY, and process execution remain blocked until explicit authority trust. Surface bootstrap and negotiation states without blocking the event loop.
11. Implement loss/reconnect behavior. SSH EOF/error increments the environment generation, marks every live PTY `Lost`, cancels pending non-durable requests, and leaves host buffers untouched. Reconnection starts a newly verified helper session; no old PTY stream is resumed or replayed.
12. Add frame/VT/PTY/SSH/TUF/loss tests, including explicit-identity and encrypted-key askpass cases, sentinel default identities, identical ssh/sftp flags, and the generated signed helper fixture. Run host qualification natively; helper fixture/release targets remain the two static musl Linux triples.

## Success Criteria

- `cargo run` (after phase 1) can open a local shell in the bottom terminal pane (`Ctrl+`` or menu stub). The pane shows live shell output (prompt, typing, command results) using only `TerminalSurface` cells. Pressing keys inside the captured pane sends input; `Esc` releases capture.
- Local PTY resize (pane height change) is reflected immediately in the child (e.g. `stty size` reports correct rows/cols).
- On macOS and Linux: local PTY uses real Unix PTY; on Windows: ConPTY. No other PTY backends are compiled in.
- SSH from any supported host to a certified remote Linux target succeeds only after `scripts/generate-tuf-test-fixtures.sh --target <triple> --mode session` has produced a full-chain signed test repository and the user has accepted the host key, selected an explicit client identity, and granted execution trust. No default identity, agent, password fallback, pre-placed helper, or manually selected helper is accepted.
- Helper multiplexing: open two remote terminals and perform a file read concurrently; all three operations complete without deadlock or cross-talk.
- Kill a running remote command (via supervisor or user interrupt in terminal): the process is terminated on the remote side within 2 s; no zombie remains.
- Disconnect the SSH network (kill ssh or pull cable): open editor buffers remain intact (Phase 1 journal recovery already ran before reconnect logic). Terminal panes show "Lost – request new session". No transparent resume. Re-establish network + re-grant if needed; new terminal session can be created. No data loss for editor content.
- Paste 50 kB of mixed output containing OSC sequences, long lines, and binary into a remote PTY: the terminal surface renders only safe text; no escape sequences reach the host terminal; surface memory usage remains bounded.
- On all platforms, after any PTY session (local or remote) and any forced termination, the host terminal is left in the exact state it was in before the session; `tput` or `reset` is not required.
- Switch authority from Local to SSH while an editor buffer is dirty: Phase 1 journal recovery populates the buffer before any authority use; buffer state survives; no execution attempted until explicit `GrantTrust` for the SSH authority.
- Every PTY message carries request/stream ID plus workspace/environment epochs and `document_revision: None`; stale generation/epoch chunks are dropped.
- Version mismatch or TUF verification failure between host and any candidate helper produces a clear error; session does not proceed. Protocol dispatcher version is exchanged and enforced.
- Frame caps: send oversized frame or exceed aggregate quota from a malicious or buggy helper simulation → frame layer rejects before allocation; connection isolated; no OOM or unbounded growth.
- Hardened launcher contract: both `ssh` and `sftp` use isolated config/known-hosts plus `IdentitiesOnly=yes`, `IdentityAgent=none`, public-key-only auth, and explicit identity/certificate paths. Sentinel default keys and agents are never opened; encrypted-key askpass is one-shot, local, redacted, and expires.
- TUF prerequisite: missing/corrupt/expired/rolled-back fixture metadata or a target not produced by the pinned test fixture pipeline fails before SFTP/helper launch, naming the failed role/check.
- PTY Lost behavior: transport loss on live PTY → handle marked Lost; UI offers only new session, never resume of old PTY bytes.
| Platform          | Local PTY (shell live, resize, kill, input) | Remote SSH + Helper (TUF gate + hardened launcher + multiplex) | Lost PTY (no resume) | Frame Caps + Hostile | Clean Process Tree | Terminal Capture + GrantTrust |
|-------------------|---------------------------------------------|-----------------------------------------------------------|----------------------|----------------------------|----------------------------|------------------------|
| macOS 15 arm64    | Pass (real PTY, stty size correct, kill within 2 s) | Pass (TUF verify using fixtures, -F none + Strict=yes, live shell + concurrent fs, no ambient leak) | Pass (Lost state, new session only) | Pass (oversized + aggregate reject before alloc in frame_caps.rs) | Pass (no zombies) | Pass (capture keys, Esc; GrantTrust required for exec) |
| Linux x86_64      | Pass                                        | Pass (TUF + hardened contract)                            | Pass                 | Pass (frame_caps.rs + tuf_verification.rs) | Pass               | Pass                   |
| Windows 11        | Pass (ConPTY only)                          | Pass (native OpenSSH client to certified remote Linux; TUF + hardened contract) | Pass | Pass | Pass (no conhost leaks) | Pass |

Failure scenarios exercised and required to produce observable correct behavior:
- SSH host key changed → explicit rejection (fingerprint mismatch), no connection, clear status. Tests assert `StrictHostKeyChecking=yes` and no accept-new.
- TUF verification failure (bad root/signature/expiry/rollback in `tests/fixtures/tuf/`) → no helper installation or launch; error names the exact failed step (`tests/remote/tuf_verification.rs`).
- Helper binary missing or unverified → actionable error from tuf_verifier + launcher; never falls back to unverified.
- Oversized frame or aggregate quota breach (protocol/tests/frame_caps.rs) → early reject before Vec alloc or surface mutation; session isolated.
- PTY reader receives 100 MB → byte budget aborts; surface shows truncated; frame layer also caps ingress.
- Rapid authority switch during PTY spawn → only final authority active; previous cancelled; trust grant checked on each.
- Child exits non-zero while surface rendering → final output + "exited"; no further input.
- Transport loss on live PTY → handle becomes Lost; "New terminal session" offered; no resume of prior PTY stream.
- Corrupt frame on wire → multiplexer (dispatcher) drops stream; surfaces show Lost; no panic/desync.
- Terminal pane closed while live → supervisor cancels/drains; no reader leaks.
- First use of SSH authority with InspectOnly → PTY spawn refused at Authority layer until explicit GrantTrust; error reaches UI.

## Risk Assessment
- OpenSSH behavior differs by platform: capability-probe fixed argv for `ssh`, `sftp`, and `ssh-keyscan`; reject the authority if the hardened flags or key-scan format are unsupported. No ControlMaster, per-connection fallback branch, or ambient config path exists in the first release.
- portable-pty ConPTY leaks on panic: mitigated by RAII + explicit close in supervisor.
- Helper left after host crash: mitigated by stdin-EOF watchdog on remote + short lifetime.
- VT parser / frame parser explosion on malicious input: bounded buffers + explicit per-class + aggregate caps in `frame.rs`; early rejection in dispatcher and hostile tests (`frame_caps.rs`).
- Reconnect / Lost races: PTY handles carry generation; input after Lost is rejected with "new session required"; journal (phase 1) already restored.
- TUF bypass or helper substitution: mitigated by the complete metadata chain, durable rollback floors, verified target bytes, hardened SFTP temp install + read-back hash + atomic digest-addressed rename, a final read-back hash before absolute-path launch, and no `PATH`/manual-unverified route. Corruption and substitution tests are release-blocking. A remote account already compromised during the final verify/exec interval is outside the client trust boundary and must be stated explicitly.
- Versioned dispatcher extensibility drift: unknown high-level tags rejected early; reserved slots documented; round-trip + version tests required.
## Security Considerations
- OpenSSH commands use direct argv, fixed cwd, constructed environment, isolated known-hosts, explicit identity/certificate, `IdentitiesOnly=yes`, `IdentityAgent=none`, and public-key-only auth. `ssh-keyscan` remains untrusted until fingerprint acceptance. The one-shot askpass capability handles only the selected encrypted key and is never inherited by remote commands.
- Remote helper runs with SSH login user's privileges only; never requests elevation.
- All PTY input treated as untrusted bytes passed verbatim; child responsible for its parsing.
- Hostile output fully contained in `TerminalSurface`; no sequence reaches host terminal or clipboard.
- Trust model: every authority (SSH included) starts InspectOnly. `GrantTrust` (explicit) is required at `Authority` layer before `spawn_pty` or `exec`. `config/trust.rs` + authority_path enforce.
- TUF metadata and exact helper bytes are a hard prerequisite to SSH activation. The host verifies production metadata, installs/revalidates the target through hardened SFTP, hashes the final remote file, and launches only the returned digest-addressed absolute path. No arbitrary pre-placed or manually unverified helper path exists.
- Cancellation/kill paths must not leak FDs or PTY masters on any platform.
- Frame layer + dispatcher enforce size/aggregate caps before any allocation or dispatch; oversized frames never reach PTY surface or remote helper.

- Phase 3 (Language Intelligence) will route LSP requests through the same `Authority` trait (behind the versioned extensible dispatcher) so language servers execute on the selected remote/container with identical trust (InspectOnly start + explicit GrantTrust), epoch, and journal rules.
- Phase 4 (Advanced Local Git) will add Git operations behind `Authority` using local Git binary or remote helper (direct argv, controlled config, idempotent mutation ids).
- Phase 5 (Dev Container Orchestration) will extend the authority chain; DevContainer CLI is the sole lifecycle creator; engine adapters only inspect/stop/remove/log/exec. TUF verification and hardened transport rules apply to container helpers.
- Phase 6 (Secure Port Forwarding) will add TCP forwarding using the host broker + multiplexed helper stream (never `ssh -L`); authority + protocol already carry the extension points.
- After phase 2, add qualification that a remote Rust project can be opened, terminal runs `cargo check`, editor browses files, all while host terminal is isolated from escape sequences. Journal recovery precedes any reconnect test; PTY sessions are explicitly new after loss; TUF + frame caps + hardened launcher are exercised in the matrix.
