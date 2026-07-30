---
phase: 4
title: "Advanced Local Git"
status: pending
priority: P1
dependencies: ["phase-01", "phase-02", "phase-03"]
effort: ""
---

# Phase 4: Advanced Local Git

## Overview

Implements the authority-local installed Git service using direct argv execution of the system Git binary with controlled config (system/global/user/repo/executable config disabled for inspect reads via GIT_CONFIG_* + -c flags; credential helpers/hooks isolated). Provides porcelain v2 -z and plumbing parsers producing immutable snapshots, repository mutation lease with operation ID + persisted intent/preconditions + helper result retention/query + ambiguous-disconnect reconciliation, and full support for status, diff, stage (including partial), commit, branches, history, stash, conflict resolution, cherry-pick, rebase, worktrees, and IDE changelists. Git request/response use versioned top-level protocol variants + dispatcher branches. All operations respect the current authority (local direct; SSH via helper), carry revision/epoch/session, and enforce explicit scoped execution trust grant (default InspectOnly) for mutations/execution while allowing inspect for read operations; no secret leakage.

## Context Links

- docs/technology-stack.md (Authority-local system Git, direct argv, porcelain v2/NUL/plumbing contracts, advanced Git features, Git recovery after cancel/crash/SSH loss/conflict, no implicit credential exposure, release-blocking gates)
- docs/design-guidelines.md (Git tool window shows branch divergence, conflicts, staged/unstaged, recent commits, hunk metadata, unified diff; authority and trust visible; Git mutations are execution actions that obey trust; inspect-only blocks mutations; status feedback patterns)
- plans/260729-2102-hermito-terminal-ide/phase-01-core-workbench-and-editor.md (Git tool window, changelists, diff view, editor integration for staging hunks)
- plans/260729-2102-hermito-terminal-ide/phase-02-terminal-and-execution-transport.md (remote transport and process primitives reused for SSH Git via helper)
- plans/260729-2102-hermito-terminal-ide/phase-05-dev-container-orchestration.md (future container Git routing through authority)

## Requirements

1. Git is authority-local and selected by `ExecutionContextV1`: host for Local AuthorityRoot, signed helper for SSH AuthorityRoot, and Phase 5's canonical container dispatcher for DevContainer. Host Git never touches remote/container paths.
2. Before every command, re-read repository config with includes disabled, reject any `include.*`/`includeIf.*` directive, hash the accepted config, then run direct argv with `env_clear`, fixed cwd, system/global config disabled, and an audited per-command override profile. Inspect profiles force empty hooks/fsmonitor/credentials/askpass/pagers/diff/textconv/filter/merge drivers, prohibit aliases/network/submodules, and validate the config hash again under the repository lease before spawn. Malicious-config tests cover every denied execution vector.
3. Parsers consume only porcelain v2 -z (NUL delimited) and plumbing output (NUL or structured); produce immutable value types (StatusSnapshot, DiffHunk, Commit, Branch, StashEntry, etc.). No mutation of parser output.
4. Mutations acquire a per-repository lease and carry a globally unique operation ID. Before dispatch, the host durably persists intent, argv class, repo identity, and preconditions. For SSH execution the helper durably records Pending before spawning Git and Completed (bounded output, exit status, post-state digest) using file fsync + atomic rename + parent-directory fsync before replying. Versioned query/ack messages allow a new helper session to retrieve or acknowledge the record by `(authority installation, repo identity, op_id)`. After disconnect, query first; Completed is never replayed, while Pending/Unknown always requires fresh-state reconciliation and never blind retry.
5. First-release operations: status, unified diff, stage/partial-stage, unstage, commit/amend, branch list/create/switch, log/graph, fetch, pull, push, stash push/pop/list/apply, conflict detection/resolution, single-commit cherry-pick, rebase start/continue/abort/skip, worktree list/add/remove, and IDE-only changelists.
6. Every Git frame/result carries `ExecutionContextV1`, workspace/environment epoch, and session; mismatch is dropped. `ProtocolV1::Git` is the only Git wire family across SSH and Phase 5 containers.
7. Network credentials require a separate per-operation grant bound to authority, remote URL/host, operation, and mode. HTTPS uses an expiring broker that answers only the expected direct Git child PID lineage, prompt class, nonce, and bounded response count; SSH uses an explicit context-local identity with defaults/agent disabled. Network profiles force empty hooks. No credential helper, stdin prompt, agent, persistence, or host-to-remote credential copy.
8. Read and network operations never execute repository hooks. Hook-capable local mutations run only after any credential broker is destroyed and under a distinct scoped grant; Hermito never invokes hook files itself. `pull` is modeled as credentialed hook-free fetch followed by a separate noncredentialed merge/rebase operation.
9. After cancel, crash, SSH loss, or interrupted rebase/cherry-pick, the next status combines durable host intent, helper query state when reachable, and fresh repository state to offer explicit continue/abort/skip/review actions. No ambiguous mutation is automatically repeated.
10. UI never blocks: all Git invocations on Tokio, results posted via bounded channels tagged with epoch/session. Git tool window and editor hunk staging remain responsive.
11. InspectOnly permits only status, diff, log, and branch list through audited no-execution profiles. Mutations, hooks, network, or credentials require a scoped grant matching workspace, authority, accepted repo-config hash, operation class, and effective argv profile before op-ID allocation.
12. No Git library as primary backend (rejected per stack). Always use installed Git for parity.
13. Canonical `ProtocolV1::Git` defines run/output/error/query/result-state/ack variants carrying `ExecutionContextV1`; the Phase 2 dispatcher is extended once. Durable helper mutation records survive session replacement.
14. Fixtures are copied into per-test temporary working repositories from immutable templates under `crates/hermito/tests/fixtures/git`; tests never mutate checked-in fixture state.
15. Authority/trust/frame rules are identical to Phase 3, including typed context and config-hash-bound grants.

## Architecture

Git access is routed exclusively through the Authority trait (`crates/hermito/src/authority/mod.rs`, shared with LSP phase 3). Authority provides:

async fn run_git(&self, repo: &Path, request: GitRequest) -> Result<GitOutput>;

Every call validates the repository config snapshot and applies a command-specific safe profile. `LocalAuthority` executes AuthorityRoot directly. `SshAuthority` sends canonical `ProtocolV1::Git` with AuthorityRoot.

Phase 5 reuses the same Git request/result types and GitService with `ExecutionContextV1::DevContainer`; only the outer container execution adapter differs.

For mutations, host intent is durable before dispatch. The helper records Pending before Git and Completed before response. Reconnect queries by operation identity; Completed is returned, while Pending/Unknown requires fresh-state reconciliation and never replay.

`GitService` owns the per-authority/repository lease, snapshot cache, and durable host intent store. Local completed results may be retained in the host store; remote authoritative states live in the helper journal and are accessed only through query/ack protocol messages. Reconciliation compares persisted intent/preconditions, queried helper state, and a fresh snapshot. Automatic action is allowed only when it is provably read-only; ambiguous writes require an explicit user choice.

Parsers live in crates/hermito/src/git/parsers.rs : parse_porcelain_v2_z (split on \0, walk records), ... All return owned immutable structs.

Immutable snapshots: ... (in crates/hermito/src/git/snapshot.rs)

Lease/reconciliation and the host intent store live in `git/mutation.rs` and `service.rs`; remote operation retention lives in `hermito-remote/src/git.rs`. Changelists remain IDE-only app state.

Environment epoch + session from Workspace/Environment captured at run_git and attached. Callers drop on mismatch.

Credential policy: every network action shows the canonical remote URL/host and asks for a separate operation grant. Local HTTPS and remote/container HTTPS use an in-memory one-shot askpass responder; the latter sends only that operation's response over the authenticated helper channel. SSH URLs require a user-selected identity already inside the execution context; `IdentitiesOnly=yes`, `IdentityAgent=none`, and no default key search apply. Inspect is non-interactive.

Each test copies an immutable fixture template to a fresh temporary directory before Git initializes or mutates it.

- Cargo.toml (root workspace; crates/hermito member: only std/tokio process, no git lib)
- crates/hermito-protocol/src/git.rs (`GitV1Run`, output/error, query/result-state/ack variants, repo/installation/op identities, integrated under `ProtocolV1::Git`)
- crates/hermito-remote/src/git.rs (controlled Git dispatcher plus durable Pending/Completed operation journal; query/ack handling survives helper restart and new SSH session)
- crates/hermito/src/authority/mod.rs (canonical Git surface taking `ExecutionContextV1`; Local/Ssh now, DevContainer adapter in Phase 5; config-hash/operation-scoped trust guard)
- crates/hermito/src/git/mod.rs (GitService, GitError, public API surface, protocol variant adapters)
- crates/hermito/src/git/service.rs (operation ID allocation, durable host intent/precondition store, lease, safe execution profiles, helper query/ack, reconciliation entrypoint)
- crates/hermito/src/git/parsers.rs (parse_porcelain_v2_z, parse_unified_diff, parse_log, parse_stash, parse_worktree, parse_conflict_state, parse_cherry_pick_state, etc.; all return immutable snapshots)
- crates/hermito/src/git/snapshot.rs (immutable types: StatusSnapshot, StatusEntry { path, index, worktree, ... }, DiffHunk, HunkLine, Commit, Branch, StashEntry, Worktree, ConflictFile, RebaseState)
- crates/hermito/src/git/mutation.rs (mutation lease; persist intent before dispatch; query-before-retry state machine; explicit Completed/Pending/Unknown reconciliation)
- crates/hermito/src/git/changelist.rs (ChangelistStore persisted in app-state; Changelist { id, name, selections: Vec<HunkRef> }, apply_to_index via git plumbing)
- crates/hermito/src/git/credential.rs (credential handling, askpass wrapper, trust-gated + isolated helper injection; no leakage)
- crates/hermito/src/git/hook.rs (Git-native hook policy and safe-profile selection only; Hermito never executes hook files directly)
- crates/hermito/src/git/recovery.rs (detect_interrupted_state from status, build recovery actions using retained op results: ContinueRebase, AbortCherryPick, etc.; ambiguous reconnect handling)
- crates/hermito/tests/fixtures/git/basic-repo/ (immutable source tree plus deterministic initialization manifest)
- crates/hermito/tests/fixtures/git/bare-remote/ (deterministic bare-remote manifest)
- crates/hermito/tests/fixtures/git/partial-stage/
- crates/hermito/tests/fixtures/git/rebase-conflict/
- crates/hermito/tests/fixtures/git/cherry-pick/
- crates/hermito/tests/fixtures/git/worktree/
- crates/hermito/tests/git_integration.rs
- crates/hermito/tests/git_parsers.rs
- crates/hermito/tests/git_recovery.rs
- crates/hermito/tests/git_remote_idempotency.rs

## Implementation Steps

1. Extend the Phase 2 dispatcher with the sole canonical `GitV1` family. Every run/query/ack includes `ExecutionContextV1`, installation/repository/op identities, epochs/session, intent/preconditions, and bounded result states. Round-trip AuthorityRoot and DevContainer contexts.
2. Implement the helper Git handler and durable journal. Validate trust/context/repository/config hash, persist Pending before spawn and Completed before response with file + parent-directory durability, and retain query/ack records across helper sessions. DevContainer dispatch is added only in Phase 5.
3. Extend Authority with canonical run/query/ack operations. Local and SSH select different adapters, not different Git schemas/services. Guard config-hash/operation trust before op-ID or argv.
4. Define immutable snapshots in `git/snapshot.rs`.
5. Implement pure porcelain/plumbing parsers and golden tests in `git/parsers.rs`.
6. Implement `GitService` with a durable host intent/precondition record written before dispatch, snapshot cache, and query/ack workflow. Safe inspect calls use per-command profiles. On Completed, persist the result before ack; on Pending/Unknown, obtain a fresh snapshot and enter explicit reconciliation.
7. Implement `MutationLease` and the mutation state machine in `git/mutation.rs`. Never infer “request lost” means “operation did not run.” A reconnect always queries by identity/op ID before any action; no mutation auto-replays.
8. Implement recovery/reconciliation from durable host intent + helper state + fresh repository state. Completed returns the stored result; Pending/Unknown exposes review/continue/abort as applicable. Every recovery mutation gets a new op ID.
9. Implement IDE-only changelists in `git/changelist.rs`.
10. Implement interrupted-operation detection/actions in `git/recovery.rs`.
11. Implement per-command profiles, split network/local integration, and credential flow. Inspect rejects repo includes and disables all execution/network vectors. Network operations force empty hooks and use a one-operation credential broker restricted to direct Git child lineage + prompt class + nonce + response count; destroy it before any separate hook-capable local mutation. UI shows remote/mode and separate grants. No ambient/default credentials.
12. Create immutable Git fixture templates under `crates/hermito/tests/fixtures/git/` plus deterministic builders. Every test copies/builds into a unique temporary directory; no test mutates the source template.
13. Add crate-owned parser golden tests.
14. Add crate-owned integration tests for Local + SSH operations, bare remote flows, malicious config/include/hash-change rejection, credentials, hooks, and context isolation.
15. Add crate-owned recovery/idempotency tests for every interruption boundary, helper/session replacement, Completed/Pending/Unknown query, and no replay.
16. Wire GitService into Git tool model (phase 1). ...
17. Enforce trust before op-ID/argv for mutations and network/credential/hook-capable operations. Inspect reads use only the audited command profiles and return a precise blocked reason for anything else.
18. Trace redacted argv, safe-profile ID/overrides, op ID, lease, durable state transitions, query/ack, reconciliation, trust, hook, and credential decisions.
19. Implement cancellation/disconnect safety: cancellation stops supervision where possible but does not erase operation state; reconnect follows query-before-reconcile and never retry-on-timeout.
20. Qualify natively on macOS/Linux/Windows against `packaging/tool-baselines.toml` (`git >=2.39,<3.0` initially). Emit machine-readable version, porcelain, safe-profile, malicious-config, and journal evidence; reject out-of-range Git rather than silently degrading.

## Success Criteria

- [ ] status on basic (non-bare) fixture returns exact StatusSnapshot matching porcelain v2 -z output (staged, unstaged, untracked, renamed, deleted).
- [ ] unified diff for a modified file matches git diff --no-color -U3 output exactly (hunks, headers, +/- lines).
- [ ] stage of a path updates index; subsequent status shows the file as staged; partial stage of specific hunk via plumbing updates only that hunk.
- [ ] commit with message succeeds; log shows the new commit at HEAD; amend updates the previous commit message/subject.
- [ ] branch list shows current + others with upstream divergence; create_branch + switch_branch updates HEAD and status.
- [ ] Trusted fetch updates remote refs and push updates only intended ref with hooks disabled. Pull is hook-free fetch then separately journaled merge/rebase after credential destruction. InspectOnly blocks before argv/credential.
- [ ] history (log) returns ordered commits with graph markers when requested; stash push creates entry, pop restores working tree and removes from list.
- [ ] conflict fixture: status shows conflicted files with UU; after manual edit + stage + commit the conflict state clears.
- [ ] cherry-pick of conflicting commit leaves repo in cherry-pick state; recovery offers continue/abort; abort cleans to pre-pick state.
- [ ] rebase start on conflicting branch leaves rebase in progress; status + recovery detect it; continue after resolution or abort succeeds and returns to clean state.
- [ ] worktree list shows main + added worktree; add worktree creates linked checkout; remove cleans up.
- [ ] changelist create + add two hunks from different files; "stage changelist" results in exactly those hunks staged (verified by status + diff --cached); changelist persists in app-state after index reset.
- [ ] InspectOnly blocks mutation/network/credential/hook paths before op-ID, secret access, or argv. Safe reads succeed only when repo config has no include directives and its hash matches the validated snapshot.
- [ ] Disconnect after remote mutation dispatch: a new session queries by installation/repo/op ID. Completed returns retained output without rerun; Pending/Unknown enters explicit reconciliation.
- [ ] Every result carries execution context, environment epoch, and session; cross-context/stale results are dropped.
- [ ] lease + op_id ensures no two mutations run concurrently on same (authority, repo); second blocks or fails fast with lease held.
- [ ] recovery after kill -9 of host process: next launch + open repo detects any interrupted op from porcelain + special files + uses prior retained if applicable and offers actions.
- [ ] Credential prompt occurs only for scoped network trust; broker rejects hook/unknown lineage, wrong prompt/nonce, extra response, and expired request; no secret persists/leaks.
- [ ] Hook-capable local mutations run only under separate trust after no credential channel exists. Malicious pre-push/post-merge fixtures cannot access or prompt the prior secret.
- [ ] Every call disables system/global config and applies its profile. Inspect and network profiles disable hooks; malicious fsmonitor/hook/diff/textconv/pager/credential/include config causes no side effect.
- [ ] Git request/response/query/ack travel as versioned `ProtocolV1::Git(...)` variants, and the remote mutation journal survives helper restart and SSH-session replacement.
- [ ] Every test starts from a fresh temporary copy/builder output; checked-in fixture templates remain byte-identical after the full suite.
## Test and Validation Matrix

### Observable Gates (release blocking)

- Parser gate: `cargo test --test git_parsers` passes against golden outputs captured from real git on the fixture repos.
- Full workflow gate: `git_integration` covers local/SSH status through worktree, plus trusted fetch/pull/push against the separate bare remote.
- Remote idempotency gate: `git_remote_idempotency` cuts transport after dispatch and after completion, restarts the helper and SSH session, then verifies query-before-reconcile and zero duplicate mutations.
- Recovery gate: `git_recovery` covers interrupted sequences and Completed/Pending/Unknown outcomes without blind retry.
- Lease/op-ID gate: concurrent mutations serialize; host intent and remote state identities match.
- Epoch/session gate: stale results are dropped without discarding the durable operation record.
- Controlled-config gate: malicious local config proves every inspect profile neutralizes executable/credential/network side effects while preserving required repository semantics.
- Versioned protocol gate: Run/Output/Error/QueryResult/ResultState/AcknowledgeResult use `ProtocolV1::Git` dispatcher branches on host and helper; protocol tests cover identity mismatch and unknown op IDs.
- Non-block gate: rapid status refreshes + background mutation; UI draw loop unaffected.

### Failure / Recovery Matrix

| Failure | Recovery | Observable / Gate |
|---------|----------|-------------------|
| Git binary missing on authority | run_git returns precise "git not found in PATH on <authority>"; no crash | Git tool: "Git unavailable on LOCAL: install git" ; other authorities may still work |
| External modification during staged commit | Lease + op_id detects post-status drift; reconcile using retained + intent offers "commit what is now staged" or "re-stage" | Log "reconciliation (op=abc123): 2 files changed externally"; user chooses; no lost work |
| Rebase interrupted by SSH loss (ambiguous) | On reconnect, recovery + retained op result detects rebase-apply; offers continue/abort using preconditions | Status shows "rebase in progress (3/7)"; banner with actions; retained used |
| Partial cherry-pick conflict left after cancel | Next status + recovery detects CHERRY_PICK_HEAD + conflicts using op retained; abort restores | Repo returns to pre-cherry state after user chooses abort |
| Concurrent mutation from outside Hermito (other terminal) | Lease acquire sees dirty index or special files; reconcile surfaces diff of intent vs reality (retained pre) | "External changes detected since last snapshot (op_id=...). Review before continuing." |
| Hook exits nonzero or mutates files before failing | Git mutation reports failure; Hermito captures bounded output and immediately refreshes status under the lease | UI shows hook failure plus actual post-hook diff; never claims rollback or unchanged files |
| Credential required but trust inspect-only | No askpass injected (isolated); git fails cleanly with "could not read Username"; no prompt | "Credential operation blocked: authority is inspect only" |
| Interrupted rebase + host crash | Restart: detect rebase state from files; recovery actions available without data loss (use prior retained if op_id matches) | On reopen: "Rebase in progress on feature-x. Continue / Abort" |
| Large history log | Parser streams or bounds; UI shows first page + "fetch more" | No OOM; results tagged with epoch + session |
| Malicious repository config requests fsmonitor/hook/diff/textconv/pager/credential/include execution during inspect | Command profile overrides the relevant keys/flags and runs non-interactively | Sentinel executables and network listener observe zero calls; expected status/diff/log output still parses |

## Risk Assessment

- Git version/format skew: first-release baseline is `>=2.39,<3.0`, capability-probed and pinned in `packaging/tool-baselines.toml`; an unsupported version fails before repository mutation.
- Interrupted state detection fragile (race between files and index): always use combination of porcelain status + existence of rebase-apply/MERGE_HEAD/CHERRY_PICK_HEAD + ls-files --unmerged + retained op state; never assume single indicator.
- Lease deadlocks or leaks under panic/cancel + op ID: use RAII guard + timeout on acquire; on drop force release + status refresh + retained cleanup.
- Reconciliation complexity leading to data loss: immutable pre-snapshots + intent + retained helper results + user-visible diff of intent vs reality; never auto-commit on drift without explicit confirmation for destructive cases. Ambiguous disconnect uses op retention.
- Hook side effects (e.g. formatting on commit): run under lease + grant, capture full output, surface before/after status; user can abort. Isolated.
- Platform Git differences (Windows line endings, path separators): parsers normalize; fixtures include CRLF cases; tests run on all three OSes.
- Config TOCTOU or new execution key: reject includes, hash the accepted local config, revalidate under the lease immediately before spawn, and maintain deny/sentinel coverage per command. Hash drift fails closed and requires review/regrant.

## Security Considerations

- Direct argv, cleared environment, disabled system/global config, rejected repo includes, config hashing, and audited profiles isolate Git. Inspect disables hooks/fsmonitor/external diff/textconv/filter/merge/pager/credential/network vectors.
- Credentials use separate grants and a PID-lineage/prompt/nonce/count-bound one-shot broker. Network profiles disable hooks; broker is destroyed before local hook-capable mutations. Explicit SSH identities only; no agents/defaults/helpers.
- Git-native hooks run only for separately granted noncredentialed local mutations with bounded output. `pull` is split at the credential boundary; Hermito never invokes hook files.
- Durable host intent + helper Pending/Completed journal + query/ack protocol prevents blind replay after crash or disconnect. Pending/Unknown is never treated as “not run.”
- Remote Git runs under the signed helper. Only validated request data and explicit credential responses cross; output is parsed, never executed.
- Inspect-only blocks every mutation/network/credential/hook-capable path before op-ID or argv; audited local reads remain available.
- Audit logs redact sensitive argv/data while recording profile ID, authority, op ID, lease, journal/query/ack, hook/credential decisions, and reconciliation.
- SSH agents are never forwarded; there is no `forward credentials` exception.
- Scoped grant + no-secret-leakage + `ExecutionContextV1` + canonical Authority/protocol rules match Phase 3.

## Next Steps

- Phase 2 supplies the versioned dispatcher and reliable framing; Phase 4 adds Git run/query/ack variants plus the durable helper journal. Long-running operations use ambiguous-disconnect signals, never transport-level retries.
- Phase 3 LSP rename may produce file changes that Git subsequently sees; status must reconcile cleanly using op/retained where applicable.
- Phase 4 and phase 3 share the canonical Authority path (`crates/hermito/src/authority/mod.rs`), protocol paths (`crates/hermito-protocol/src/*` with top-level versioned variants/dispatchers for Git and LSP), trust model (InspectOnly start, explicit scoped execution grant only, no secret leakage), controlled execution, and epoch/session tagging.
- Phase 5 adds only the DevContainer execution adapter/dispatcher for canonical Git. It reuses GitService, schema, parsers, lease, journal, credentials, and recovery unchanged.
- Phase 7 qualification executes the full operation + recovery + multi-platform + SSH + bare-remote + controlled-config + op/reconcil matrix using the seeded fixtures (non-bare basic + bare-remote).
- Changelists and IDE virtual state live only in app-state records; never leak into .git beyond what user explicitly stages.
