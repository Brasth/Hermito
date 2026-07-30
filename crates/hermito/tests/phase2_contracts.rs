//! Phase 2 contract tests (integration, deterministic, no production edits, no network).
//! Run with: cargo test -p hermito --test phase2_contracts
//!
//! Each test fails on plausible regression of Phase 2 public contracts:
//! protocol negotiate/mismatch/dispatch/class, frame caps+aggregate before alloc,
//! authority trust/epoch/path gates, inert VT containment+parse, PTY state/lifecycle/join/input,
//! SSH validation+hardened flags, TUF verify success/fail via fixtures.

use std::path::PathBuf;
use std::task::Poll;
use std::time::Duration;

use hermito::authority::local::LocalAuthority;
use hermito::authority::tuf_verifier::TufVerifier;
use hermito::authority::types::{
    allowlisted_environment, AuthorityRequest, AuthorityTrust, ExecRequest, PtyRequest,
    ReadFileRequest, WriteFileRequest,
};
use hermito::authority::{Authority, AuthorityError};
use hermito::config::known_hosts::{HostKeyCandidate, KnownHostsError, KnownHostsStore};
use hermito::process::SupervisorError;
use hermito::pty::{spawn_local_pty, PtySession, PtySessionError, PtySessionState};
use hermito::remote::ssh_bootstrap::{
    OpenSshInvocation, SshBootstrap, SshBootstrapError, SshTarget,
};
use hermito::remote::ssh_identity::{askpass_client, OneShotAskpass, SshIdentity};
use hermito::remote::tuf::TufPolicy;
use hermito::terminal::{TerminalSurface, VtParser};
use hermito_protocol::frame::{
    read_frame, write_message_version, AggregateBudget, FrameError, FrameLimits,
};
use hermito_protocol::request::{
    CommandSpec, DocumentRevision, EnvironmentEpoch, ExecutionContextV1, WorkspaceEpoch,
};
use hermito_protocol::{
    dispatcher::{
        negotiate, validate_for_dispatch, validate_frame_version, DispatchError, NegotiatedVersion,
    },
    fs, process, pty as proto_pty,
    request::RequestEnvelope,
    response::ResponseEnvelope,
    ExtensionMessage, Message, MessageClass, ProtocolVersion, CURRENT_VERSION,
};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

// ---------- helpers ----------

fn ws0() -> WorkspaceEpoch {
    WorkspaceEpoch(0)
}
fn env0() -> EnvironmentEpoch {
    EnvironmentEpoch(0)
}

fn make_temp_root() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().to_path_buf();
    (tmp, path)
}

fn auth_req<T>(payload: T, ws: WorkspaceEpoch, env: EnvironmentEpoch) -> AuthorityRequest<T> {
    AuthorityRequest::new(payload, ws, env, None)
}
fn auth_doc_req<T>(payload: T, ws: WorkspaceEpoch, env: EnvironmentEpoch) -> AuthorityRequest<T> {
    AuthorityRequest::new(payload, ws, env, Some(DocumentRevision(1)))
}
fn response<T>(payload: T) -> ResponseEnvelope<T> {
    ResponseEnvelope {
        request_id: uuid::Uuid::new_v4(),
        workspace_epoch: ws0(),
        environment_epoch: env0(),
        document_revision: None,
        execution_context: ExecutionContextV1::AuthorityRoot,
        payload: Ok(payload),
    }
}

fn cmd_echo_hello(cwd: &std::path::Path) -> CommandSpec {
    #[cfg(unix)]
    {
        CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'hello\n'".into()],
            cwd: cwd.to_string_lossy().into_owned(),
            env: allowlisted_environment([("TERM".into(), "dumb".into())]),
        }
    }
    #[cfg(not(unix))]
    {
        CommandSpec {
            program: "cmd".into(),
            args: vec!["/c".into(), "echo hello".into()],
            cwd: cwd.to_string_lossy().into_owned(),
            env: allowlisted_environment([("TERM".into(), "dumb".into())]),
        }
    }
}

fn surface_text(s: &TerminalSurface) -> String {
    s.cells().iter().map(|c| c.ch).collect()
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    Runtime::new().unwrap().block_on(f)
}

// ---------- Protocol negotiation / dispatch / mismatch ----------

#[test]
fn protocol_negotiate_accepts_same_major_downgrades_minor_rejects_major() {
    let cur = CURRENT_VERSION;
    let ok = negotiate(ProtocolVersion { major: 1, minor: 9 }).unwrap();
    assert_eq!(ok.0.major, 1);
    assert_eq!(ok.0.minor, cur.minor);

    let maj = negotiate(ProtocolVersion { major: 2, minor: 0 }).unwrap_err();
    assert!(matches!(
        maj,
        DispatchError::MajorVersion { local: 1, peer: 2 }
    ));

    let neg = negotiate(cur).unwrap();
    assert!(validate_frame_version(cur, neg).is_ok());
    let bad_ver = ProtocolVersion {
        major: 1,
        minor: 99,
    };
    let fv = validate_frame_version(bad_ver, neg).unwrap_err();
    assert!(matches!(fv, DispatchError::FrameVersion { .. }));
}

#[test]
fn protocol_dispatch_not_negotiated_for_non_control_wrong_major() {
    let hello = Message::Hello {
        version: CURRENT_VERSION,
    };
    let n = negotiate(CURRENT_VERSION).unwrap();
    assert!(validate_for_dispatch(&hello, n).is_ok());

    let pty_spawn = proto_pty::PtySpawn {
        stream_id: 1,
        generation: 0,
        command: CommandSpec {
            program: "x".into(),
            args: vec![],
            cwd: "/".into(),
            env: Default::default(),
        },
        size: proto_pty::PtySize {
            rows: 1,
            cols: 1,
            pixel_width: 0,
            pixel_height: 0,
        },
    };
    let env = RequestEnvelope::authority_root(pty_spawn, ws0(), env0());
    let msg = Message::Pty(proto_pty::PtyMessage::Spawn(env));
    let bad_n = NegotiatedVersion(ProtocolVersion { major: 0, minor: 0 });
    let e = validate_for_dispatch(&msg, bad_n);
    assert!(matches!(e, Err(DispatchError::NotNegotiated)));
    assert!(validate_for_dispatch(&msg, n).is_ok());
}

// ---------- Frame caps / aggregate / class / mismatch / read before alloc ----------

#[test]
fn frame_write_rejects_oversize_class_before_header() {
    let limits = FrameLimits {
        extension: 4,
        ..FrameLimits::default()
    };
    let big = Message::Lsp(ExtensionMessage {
        family_version: 0,
        kind: "x".into(),
        body: vec![0u8; 16],
    });
    let err = block_on(write_message_version(
        &mut Vec::new(),
        &big,
        limits,
        CURRENT_VERSION,
    ));
    assert!(matches!(
        err,
        Err(FrameError::ClassLimit {
            class: MessageClass::Lsp,
            ..
        })
    ));
}

#[test]
fn frame_read_rejects_oversize_badmagic_unknown_before_alloc() {
    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let limits = FrameLimits {
            pty: 8,
            ..FrameLimits::default()
        };
        let budget = AggregateBudget::new(1024);

        // class oversize
        let (mut tx, mut rx) = tokio::io::duplex(128);
        let mut hdr = b"HMT2".to_vec();
        hdr.extend_from_slice(&[1, 0, 1, 0]);
        hdr.extend_from_slice(&(100u32).to_be_bytes());
        tx.write_all(&hdr).await.unwrap();
        drop(tx);
        let e = read_frame(&mut rx, limits, &budget).await.unwrap_err();
        assert!(matches!(
            e,
            FrameError::ClassLimit {
                class: MessageClass::Pty,
                ..
            }
        ));

        // bad magic
        let (mut tx, mut rx) = tokio::io::duplex(128);
        tx.write_all(b"BADMAGIC\x00\x00\x00\x00\x00\x00\x00\x00")
            .await
            .unwrap();
        drop(tx);
        let e = read_frame(&mut rx, limits, &budget).await.unwrap_err();
        assert!(matches!(e, FrameError::BadMagic));

        // unknown class
        let (mut tx, mut rx) = tokio::io::duplex(128);
        let mut h2 = b"HMT2".to_vec();
        h2.extend_from_slice(&[1, 0, 99, 0, 0, 0, 0, 4]);
        tx.write_all(&h2).await.unwrap();
        drop(tx);
        let e = read_frame(&mut rx, limits, &budget).await.unwrap_err();
        assert!(matches!(e, FrameError::UnknownClass(99)));
    });
}

#[test]
fn frame_aggregate_budget_reject_and_reclaim() {
    let budget = AggregateBudget::new(10);
    let first = budget.try_reserve(6).unwrap();
    assert_eq!(budget.used(), 6);
    assert!(matches!(
        budget.try_reserve(6),
        Err(FrameError::AggregateLimit { .. })
    ));
    drop(first);
    let second = budget.try_reserve(5).unwrap();
    assert_eq!(budget.used(), 5);
    drop(second);
}

#[test]
fn frame_aggregate_budget_waits_without_losing_the_next_frame() {
    block_on(async {
        let limits = FrameLimits::default();
        let budget = AggregateBudget::new(5);
        let mut bytes = Vec::new();
        for payload in [b"first".as_slice(), b"other".as_slice()] {
            bytes.extend_from_slice(b"HMT2");
            bytes.extend_from_slice(&[1, 0, MessageClass::Control as u8, 0]);
            bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            bytes.extend_from_slice(payload);
        }
        let (mut tx, mut rx) = tokio::io::duplex(64);
        tx.write_all(&bytes).await.unwrap();
        drop(tx);

        let first = read_frame(&mut rx, limits, &budget).await.unwrap();
        assert_eq!(budget.used(), 5);
        let second = read_frame(&mut rx, limits, &budget);
        tokio::pin!(second);
        assert!(matches!(futures_util::poll!(&mut second), Poll::Pending));

        drop(first);
        let second = second.await.unwrap();
        assert_eq!(second.payload(), b"other");
        assert_eq!(budget.used(), 5);
    });
}

#[test]
fn encoded_file_and_process_payloads_fit_their_frame_classes() {
    block_on(async {
        let limits = FrameLimits::default();
        let file = Message::Fs(fs::FsMessage::ReadResult(response(fs::FileContent {
            bytes: vec![0xA5; fs::MAX_WIRE_FILE_BYTES as usize],
        })));
        let encoded_file = serde_json::to_vec(&file).unwrap();
        assert!(encoded_file.len() <= limits.fs);
        write_message_version(&mut tokio::io::sink(), &file, limits, CURRENT_VERSION)
            .await
            .unwrap();

        let process = Message::Process(process::ProcessMessage::Result(response(
            process::ExecOutput {
                exit_code: Some(0),
                stdout: vec![0x5A; process::MAX_WIRE_OUTPUT_BYTES as usize],
                stderr: vec![0xA5; process::MAX_WIRE_OUTPUT_BYTES as usize],
                stdout_truncated: false,
                stderr_truncated: false,
            },
        )));
        let encoded_process = serde_json::to_vec(&process).unwrap();
        assert!(encoded_process.len() <= limits.process);
        write_message_version(&mut tokio::io::sink(), &process, limits, CURRENT_VERSION)
            .await
            .unwrap();
    });
}

#[test]
fn frame_roundtrip_decode_and_class_mismatch() {
    let limits = FrameLimits::default();
    let budget = AggregateBudget::new(limits.aggregate);
    let msg = Message::Hello {
        version: CURRENT_VERSION,
    };
    let mut buf = Vec::new();
    block_on(write_message_version(
        &mut buf,
        &msg,
        limits,
        CURRENT_VERSION,
    ))
    .unwrap();

    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let (mut tx, mut rx) = tokio::io::duplex(128);
        tx.write_all(&buf).await.unwrap();
        drop(tx);
        let rf = read_frame(&mut rx, limits, &budget).await.unwrap();
        assert_eq!(rf.decode_message().unwrap(), msg);
    });

    // mismatch header
    let payload = serde_json::to_vec(&msg).unwrap();
    let mut bad = b"HMT2".to_vec();
    bad.extend_from_slice(&[1, 0, 5, 0]);
    bad.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bad.extend_from_slice(&payload);
    let rt2 = Runtime::new().unwrap();
    rt2.block_on(async {
        let (mut tx, mut rx) = tokio::io::duplex(128);
        tx.write_all(&bad).await.unwrap();
        drop(tx);
        let rf = read_frame(&mut rx, limits, &budget).await.unwrap();
        assert!(matches!(
            rf.decode_message(),
            Err(FrameError::ClassMismatch { .. })
        ));
    });
}

// ---------- Message class ----------

#[test]
fn message_class_variants_tryfrom() {
    assert_eq!(
        Message::Hello {
            version: CURRENT_VERSION
        }
        .class(),
        MessageClass::Control
    );
    let fsmsg = Message::Fs(fs::FsMessage::Read(RequestEnvelope::authority_root(
        fs::ReadFile {
            path: "/x".into(),
            max_bytes: 10,
        },
        ws0(),
        env0(),
    )));
    assert_eq!(fsmsg.class(), MessageClass::Fs);
    assert_eq!(MessageClass::try_from(2).unwrap(), MessageClass::Fs);
    assert!(MessageClass::try_from(200).is_err());
}

// ---------- Authority trust/epoch/path gates + FS via local ----------

#[test]
fn authority_trust_grant_revoke_and_gates() {
    let (_tmp, root) = make_temp_root();
    let la = LocalAuthority::new("l", root.clone(), ws0()).unwrap();
    assert_eq!(la.trust(), AuthorityTrust::InspectOnly);
    la.grant_execution();
    assert_eq!(la.trust(), AuthorityTrust::ExecutionGranted);
    la.revoke_execution();
    assert_eq!(la.trust(), AuthorityTrust::InspectOnly);
}

#[test]
fn authority_trust_epoch_path_gates_reject() {
    let (_tmp, root) = make_temp_root();
    let la = LocalAuthority::new("l", root, ws0()).unwrap();
    let rp = ReadFileRequest {
        path: PathBuf::from("m"),
        max_bytes: 1,
    };
    let denied_exec = auth_req(
        ExecRequest {
            command: cmd_echo_hello(la.root()),
            stdout_limit: 1,
            stderr_limit: 1,
            timeout: Duration::from_secs(1),
        },
        ws0(),
        env0(),
    );
    assert!(matches!(
        block_on(la.exec(denied_exec, CancellationToken::new())),
        Err(AuthorityError::InspectOnly)
    ));

    la.grant_execution();
    let rs = auth_doc_req(rp.clone(), WorkspaceEpoch(99), env0());
    assert!(matches!(
        block_on(la.read_file(rs)),
        Err(AuthorityError::StaleEpoch { .. })
    ));
    let re = auth_doc_req(rp.clone(), ws0(), EnvironmentEpoch(99));
    assert!(matches!(
        block_on(la.read_file(re)),
        Err(AuthorityError::StaleEpoch { .. })
    ));
    let missing_revision = auth_req(rp.clone(), ws0(), env0());
    assert!(matches!(
        block_on(la.read_file(missing_revision)),
        Err(AuthorityError::MissingDocumentRevision)
    ));

    let esc = auth_doc_req(
        ReadFileRequest {
            path: PathBuf::from("../e"),
            max_bytes: 1,
        },
        ws0(),
        env0(),
    );
    assert!(matches!(
        block_on(la.read_file(esc)),
        Err(AuthorityError::PathEscapesRoot(_))
    ));
}

#[test]
fn authority_read_write_roundtrip_epochs_match() {
    let (_tmp, root) = make_temp_root();
    let la = LocalAuthority::new("l", root, ws0()).unwrap();
    la.grant_execution();
    let bytes = b"phase2\n".to_vec();
    let w = auth_doc_req(
        WriteFileRequest {
            path: PathBuf::from("f"),
            bytes: bytes.clone(),
            create: true,
        },
        ws0(),
        env0(),
    );
    let wr = block_on(la.write_file(w)).unwrap();
    assert_eq!(wr.payload, bytes.len());

    let r = auth_doc_req(
        ReadFileRequest {
            path: PathBuf::from("f"),
            max_bytes: 100,
        },
        ws0(),
        env0(),
    );
    let rr = block_on(la.read_file(r)).unwrap();
    assert_eq!(rr.payload, bytes);
    assert_eq!(rr.workspace_epoch, ws0());
    assert_eq!(rr.document_revision, Some(DocumentRevision(1)));
}

// ---------- PTY lifecycle / state / join / input / lost (no sleep, join waits) ----------

#[test]
#[cfg(unix)]
fn pty_spawn_join_exited_surface_has_output() {
    let (_tmp, root) = make_temp_root();
    let la = LocalAuthority::new("l", root.clone(), ws0()).unwrap();
    la.grant_execution();
    let preq = PtyRequest {
        command: cmd_echo_hello(&root),
        size: portable_pty::PtySize {
            rows: 5,
            cols: 20,
            pixel_width: 0,
            pixel_height: 0,
        },
    };
    let tok = CancellationToken::new();
    let pres = block_on(la.spawn_pty(auth_req(preq, ws0(), env0()), tok)).unwrap();
    let ps = pres.payload;
    ps.join_reader().unwrap();
    assert_eq!(ps.state(), PtySessionState::Exited);
    assert!(surface_text(&ps.snapshot()).contains('h'));
}

#[test]
#[cfg(unix)]
fn pty_input_size_limit_notrunning_after_cancel() {
    let (_tmp, root) = make_temp_root();
    let la = LocalAuthority::new("l", root.clone(), ws0()).unwrap();
    la.grant_execution();
    let preq = PtyRequest {
        command: cmd_echo_hello(&root),
        size: portable_pty::PtySize {
            rows: 3,
            cols: 10,
            pixel_width: 0,
            pixel_height: 0,
        },
    };
    let tok = CancellationToken::new();
    let pres = block_on(la.spawn_pty(auth_req(preq, ws0(), env0()), tok)).unwrap();
    let ps = pres.payload;
    assert!(matches!(
        ps.write_input(&vec![0; 99999]),
        Err(PtySessionError::InputTooLarge(_))
    ));
    ps.cancel();
    assert!(matches!(
        ps.write_input(b"x"),
        Err(PtySessionError::NotRunning(_))
    ));
}

#[test]
#[cfg(unix)]
fn pty_mark_lost_on_local_rejects_input() {
    let (_tmp, root) = make_temp_root();
    let cmd = cmd_echo_hello(&root);
    let tok = CancellationToken::new();
    let lps = spawn_local_pty(7, &cmd, 3, 10, ws0(), env0(), tok).unwrap();
    lps.mark_lost();
    assert_eq!(lps.state(), PtySessionState::Lost);
    assert!(matches!(
        lps.write_input(b"1"),
        Err(PtySessionError::NotRunning(PtySessionState::Lost))
    ));
}

// ---------- Exec results / truncate / cancel ----------

#[test]
#[cfg(unix)]
fn exec_stdout_exit_truncate_on_limit() {
    let (_tmp, root) = make_temp_root();
    let la = LocalAuthority::new("l", root.clone(), ws0()).unwrap();
    la.grant_execution();
    let ereq = ExecRequest {
        command: cmd_echo_hello(&root),
        stdout_limit: 100,
        stderr_limit: 10,
        timeout: Duration::from_secs(2),
    };
    let er = block_on(la.exec(auth_req(ereq, ws0(), env0()), CancellationToken::new())).unwrap();
    assert!(String::from_utf8(er.payload.stdout)
        .unwrap()
        .contains("hello"));
    assert_eq!(er.payload.exit_code, Some(0));

    let ereq2 = ExecRequest {
        command: cmd_echo_hello(&root),
        stdout_limit: 1,
        stderr_limit: 1,
        timeout: Duration::from_secs(2),
    };
    let er2 = block_on(la.exec(auth_req(ereq2, ws0(), env0()), CancellationToken::new())).unwrap();
    assert!(er2.payload.stdout_truncated);
}

#[test]
fn exec_cancelled_token_errors_cancelled() {
    let (_tmp, root) = make_temp_root();
    let la = LocalAuthority::new("l", root.clone(), ws0()).unwrap();
    la.grant_execution();
    let ereq = ExecRequest {
        command: cmd_echo_hello(&root),
        stdout_limit: 10,
        stderr_limit: 10,
        timeout: Duration::from_secs(5),
    };
    let tok = CancellationToken::new();
    tok.cancel();
    let e = block_on(la.exec(auth_req(ereq, ws0(), env0()), tok));
    assert!(matches!(
        e,
        Err(AuthorityError::Process(SupervisorError::Cancelled))
    ));
}

// ---------- Inert VT + bounds ----------

#[test]
fn vt_drops_osc52_binary_controls_overflow_sets_safe() {
    let mut surf = TerminalSurface::new(40, 3, 10);
    let mut p = VtParser::default();
    let bad = b"hi\x1b]52;;sec\x07\x00\xff\x1b]0;TTTTTTTTTTTTTTTTTTTTTTTT\x07\x1b[99C\x1bPdcslong\x1b\\ok";
    p.feed(bad, &mut surf);
    let t = surface_text(&surf);
    assert!(t.contains('h') && t.contains('i') && t.contains('o') && t.contains('k'));
    assert!(!t.contains("sec"));
    assert!(surf.truncated() || surf.title().len() <= 256);
}

#[test]
fn surface_clips_dim_and_budget() {
    let s = TerminalSurface::new(0, 0, 0);
    assert!(s.width() >= 1 && s.height() >= 1);
    let big = TerminalSurface::new(2000, 2000, 0);
    assert!((big.width() as usize * big.height() as usize) <= 250000);
}

// ---------- SSH validation + hardened flags ----------

#[test]
fn ssh_target_bootstrap_produces_hardened_flags() {
    let bad = SshTarget {
        host: String::new(),
        port: 0,
        user: String::new(),
    };
    assert!(bad.validate().is_err());

    let tmp = TempDir::new().unwrap();
    let key = tmp.path().join("k");
    std::fs::write(&key, "k").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let id = SshIdentity {
        private_key: key.clone(),
        certificate: None,
    };
    assert!(id.validate().is_ok());

    let kh = tmp.path().join("kh");
    std::fs::write(&kh, "").unwrap();
    let t = SshTarget {
        host: "h".into(),
        port: 22,
        user: "u".into(),
    };
    let bs = SshBootstrap::new(t, id, kh).unwrap();
    let inv = bs.ssh_invocation(&["true".into()], None).unwrap();
    let a = &inv.args;
    assert!(a.contains(&"-F".to_string()) && a.contains(&"none".to_string()));
    assert!(a.iter().any(|s| s.contains("StrictHostKeyChecking=yes")));
    assert!(a.iter().any(|s| s.contains("IdentitiesOnly=yes")));
    assert!(a.iter().any(|s| s.contains("IdentityAgent=none")));
    assert!(a.iter().any(|s| s == "UpdateHostKeys=no"));
}

#[test]
#[cfg(unix)]
fn ssh_bootstrap_aborts_oversized_output() {
    let tmp = TempDir::new().unwrap();
    let key = tmp.path().join("key");
    std::fs::write(&key, "key").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let known_hosts = tmp.path().join("known_hosts");
    std::fs::write(&known_hosts, "").unwrap();
    let bootstrap = SshBootstrap::new(
        SshTarget {
            host: "host".into(),
            port: 22,
            user: "user".into(),
        },
        SshIdentity {
            private_key: key,
            certificate: None,
        },
        known_hosts,
    )
    .unwrap();
    let invocation = OpenSshInvocation {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "while :; do printf 0123456789; done".into()],
        env: Default::default(),
        cwd: PathBuf::from("/"),
        stdin_bytes: None,
    };
    let result = block_on(bootstrap.run(&invocation, 1024, Duration::from_secs(5)));
    assert!(matches!(result, Err(SshBootstrapError::OutputTooLarge)));
}

#[test]
fn ssh_identity_validate_nonabs_insecure() {
    let rel = SshIdentity {
        private_key: PathBuf::from("rel"),
        certificate: None,
    };
    assert!(rel.validate().is_err());
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("k2");
    std::fs::write(&p, "k").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SshIdentity {
            private_key: p,
            certificate: None
        }
        .validate()
        .is_err());
    }
}

#[test]
fn known_host_acceptance_is_idempotent_and_rejects_replacement() {
    let tmp = TempDir::new().unwrap();
    let store = KnownHostsStore::new(tmp.path().join("known_hosts"));
    let first = HostKeyCandidate::parse("host ssh-ed25519 AQID").unwrap();
    store
        .accept("host", 22, &first, &first.fingerprint)
        .unwrap();
    store
        .accept("host", 22, &first, &first.fingerprint)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(store.path())
            .unwrap()
            .lines()
            .count(),
        1
    );

    let replacement = HostKeyCandidate::parse("host ssh-ed25519 BAUG").unwrap();
    assert!(matches!(
        store.accept("host", 22, &replacement, &replacement.fingerprint),
        Err(KnownHostsError::HostKeyChanged(_))
    ));
}

#[test]
fn askpass_capability_is_authority_bound_and_one_shot() {
    block_on(async {
        assert!(OneShotAskpass::start(
            zeroize::Zeroizing::new(b"bad\npass".to_vec()),
            "ssh-a".into(),
            Duration::from_secs(1),
        )
        .await
        .is_err());

        let (endpoint, server) = OneShotAskpass::start(
            zeroize::Zeroizing::new(b"secret".to_vec()),
            "ssh-a".into(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(
            askpass_client(&endpoint.endpoint).await.unwrap().as_slice(),
            b"secret"
        );
        server.await.unwrap().unwrap();
        assert!(askpass_client(&endpoint.endpoint).await.is_err());
    });
}

// ---------- TUF policy + fixture verify ----------

#[test]
fn tuf_policy_bad_and_fixture_success() {
    let bad = TufPolicy {
        trusted_root: "/no".into(),
        metadata_base_url: "file:///n".parse().unwrap(),
        targets_base_url: "file:///n".parse().unwrap(),
        datastore: "/t".into(),
        target_cache: "/t".into(),
        offline_metadata_url: None,
        offline_targets_url: None,
        allow_offline_cache: false,
        max_target_bytes: 0,
    };
    assert!(bad.validate().is_err());

    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tuf/generated/x86_64-unknown-linux-musl/repository");
    let datastore = TempDir::new().unwrap();
    let target_cache = TempDir::new().unwrap();
    let pol = TufPolicy {
        trusted_root: base.join("root.json"),
        metadata_base_url: url::Url::from_file_path(base.join("metadata")).unwrap(),
        targets_base_url: url::Url::from_file_path(base.join("targets")).unwrap(),
        datastore: datastore.path().join("d"),
        target_cache: target_cache.path().join("c"),
        offline_metadata_url: None,
        offline_targets_url: None,
        allow_offline_cache: false,
        max_target_bytes: 20 * 1024 * 1024,
    };
    let v = TufVerifier::new(pol).unwrap();
    let vt = block_on(v.verify_target("hermito-remote-x86_64-unknown-linux-musl")).unwrap();
    assert_eq!(vt.name, "hermito-remote-x86_64-unknown-linux-musl");
    assert!(vt.sha256_hex.len() == 64);
}

#[test]
fn tuf_fixture_rejects_tampered_target() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tuf/generated/x86_64-unknown-linux-musl/repository");
    let fixture = TempDir::new().unwrap();
    for directory in ["metadata", "targets"] {
        let destination = fixture.path().join(directory);
        std::fs::create_dir(&destination).unwrap();
        for entry in std::fs::read_dir(source.join(directory)).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), destination.join(entry.file_name())).unwrap();
        }
    }
    std::fs::copy(source.join("root.json"), fixture.path().join("root.json")).unwrap();
    let target = std::fs::read_dir(fixture.path().join("targets"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&target).unwrap();
    bytes[0] ^= 0xFF;
    std::fs::write(target, bytes).unwrap();

    let datastore = TempDir::new().unwrap();
    let target_cache = TempDir::new().unwrap();
    let policy = TufPolicy {
        trusted_root: fixture.path().join("root.json"),
        metadata_base_url: url::Url::from_file_path(fixture.path().join("metadata")).unwrap(),
        targets_base_url: url::Url::from_file_path(fixture.path().join("targets")).unwrap(),
        datastore: datastore.path().join("d"),
        target_cache: target_cache.path().join("c"),
        offline_metadata_url: None,
        offline_targets_url: None,
        allow_offline_cache: false,
        max_target_bytes: 20 * 1024 * 1024,
    };
    let verifier = TufVerifier::new(policy).unwrap();
    assert!(block_on(verifier.verify_target("hermito-remote-x86_64-unknown-linux-musl")).is_err());
}

// ---------- Loss via pty ----------

#[test]
#[cfg(unix)]
fn loss_sets_lost_state() {
    let (_tmp, root) = make_temp_root();
    let la = LocalAuthority::new("l", root.clone(), ws0()).unwrap();
    la.grant_execution();
    let preq = PtyRequest {
        command: cmd_echo_hello(&root),
        size: portable_pty::PtySize {
            rows: 2,
            cols: 8,
            pixel_width: 0,
            pixel_height: 0,
        },
    };
    let tok = CancellationToken::new();
    let pres = block_on(la.spawn_pty(auth_req(preq, ws0(), env0()), tok)).unwrap();
    let ps = pres.payload;
    if let PtySession::Local(l) = &ps {
        l.mark_lost();
    } else {
        ps.cancel();
    }
    assert_eq!(ps.state(), PtySessionState::Lost);
}
