# Hermito Technology Stack

**Status:** Approved
**Date:** 2026-07-29
**License:** Apache-2.0

## Product Contract

Hermito: conventional terminal IDE for native macOS, Linux, Windows.

First-release scope:
- Mouse click, drag selection, wheel scroll, keyboard-complete UX
- Host and Dev Container terminals
- Local and SSH-remote workspaces
- Managed versioned remote helper
- Docker and Podman Dev Containers
- True container-localhost TCP forwarding
- Certified TypeScript/JavaScript, Rust, Go, Python LSP workflows
- Advanced Git: partial staging, graph, stash, conflicts, cherry-pick, rebase, worktrees, changelists
- Configuration-only extensibility
- No debugger or hosting-provider PR integration

## Approved Stack

| Area | Choice | Boundary |
|---|---|---|
| Language | Stable Rust | Host executable, remote helper, container relay |
| TUI | Ratatui | IDE layout and cell rendering only |
| Outer terminal | Crossterm | Sole owner of raw mode, host input, resize, mouse, paste |
| Async | Tokio | Bounded channels, child streams, timers, sockets |
| Document storage | Ropey | UTF-8 text storage; no UI/LSP coordinate leakage |
| Syntax | Tree-sitter | Curated pinned grammars; incremental highlighting |
| Language intelligence | LSP 3.17 baseline | Capability-gated client; servers remain external |
| Local PTY | portable-pty | Unix PTY and Windows ConPTY adapter |
| Terminal emulation | Replaceable `TerminalSurface`; evaluate vt100 | Parse child bytes into inert cell state |
| Remote transport | Installed OpenSSH `ssh`/`sftp`/`ssh-keyscan` | Strict host-key enrollment/pinning; no ambient executable SSH config |
| Remote services | TUF-verified static `hermito-remote` | `session` and `container-agent` modes; canonical files/PTY/Git/LSP/Container/Relay multiplexing; Linux musl x86_64/aarch64 only |
| Helper updates | Full-chain TUF targets | Threshold roles, length/hash/version/expiry, durable rollback/freeze floors, atomic digest-addressed install |
| Dev Containers | Pinned external Dev Container CLI | Spec resolution, Features, lifecycle, build/up/exec |
| Engine lifecycle | Narrow Hermito Docker/Podman adapters | Inspect/list/stop/remove/log plus fixed verified-agent copy/hash/lease controls only; never create/build/publish/general exec |
| Git | Authority-local system Git | Direct argv; porcelain v2/NUL/plumbing contracts |
| Forwarding | Host loopback broker + remote hop + container relay | TCP only; no publishing substitution |
| Configuration | TOML | Themes, keybindings, tasks, language servers, layouts |
| Persistent metadata | Versioned app-state records | Trust, sessions, port leases, IDE changelists |

## Architecture Rules

1. One host modular monolith. External/untrusted work crosses process boundaries.
2. UI thread never blocks on files, parsers, Git, LSP, PTY, SSH, engines, or network.
3. Unsaved buffers are authoritative on the client and survive environment failure.
4. All async results carry workspace/environment epoch; document-derived results additionally carry document revision. Authority/reconnect/rebuild transitions advance the applicable epoch, while host buffer revisions remain authoritative.
5. Remote/local/container execution uses one typed authority abstraction; never local Git on remote paths.
6. Raw PTY bytes never reach the outer host terminal.
7. Subprocesses use direct argument vectors, fixed working directory, bounded output, and owned cancellation.
8. Docker is the reference engine path. Podman ships only after its full matrix passes.
9. Untrusted workspaces are inspect/edit only; executable capabilities require explicit grants.
10. Forwarded ports bind only `127.0.0.1` and `::1`.

## External Tool Policy

Hermito detects and validates installed tools; it does not bundle them:
- Git
- Docker or Podman and Compose provider
- Dev Container CLI
- OpenSSH (`ssh`, `sftp`, and bounded `ssh-keyscan` for explicit host-key enrollment)
- Language servers

Hermito reports exact missing or incompatible capabilities. It never silently downloads or executes repository-selected tools.

## Compatibility Baselines

- Native macOS and Linux
- Native Windows with ConPTY; no WinPTY fallback
- Local Docker Engine/Desktop
- Local Podman, Podman machine/Desktop only after qualification
- First-release helper targets: `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`; unsupported remote/container architectures fail closed
- xterm-256color terminal profile; unsupported VT extensions remain inert

## Release-Blocking Gates

- Terminal restoration and process-tree cleanup on all platforms
- Exact Unicode/grapheme/display-cell/LSP position conversions
- Managed helper verification, host-key pinning, version negotiation, rollback protection
- Docker and Podman Dev Container fixture matrices
- Container-localhost forwarding without public listeners
- Git recovery after cancel, crash, SSH loss, conflict, and interrupted sequences
- No implicit credential, agent, host environment, engine socket, or host mount exposure
- No raw PTY/LSP/Git escape sequence can affect the host terminal

## Rejected Foundations

- Helix/Neovim embedding: inherited editing model and unstable product boundary
- Python/Textual: unsuitable durable systems/process foundation for selected breadth
- TypeScript/OpenTUI: promising but younger runtime/native packaging surface
- Git library as primary backend: poorer parity than installed Git
- Custom Dev Container spec implementation: non-differentiating compatibility risk
- Docker port publishing presented as forwarding: incorrect localhost semantics

## Version Policy

Pin every direct Rust dependency and grammar set through the lockfile. Pin and capability-probe external protocol/tool baselines. Record tool versions in diagnostics. Upgrade only after platform and fixture matrices pass.

## Unresolved Questions

- Final terminal emulator after VT compatibility spike
- Exact remote helper OS/architecture release matrix beyond remote Linux
- Required Podman Compose provider/version per platform
