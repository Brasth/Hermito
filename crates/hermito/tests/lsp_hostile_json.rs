//! Deterministic ingress and authorization contracts for LSP.
//! Run with: cargo test -p hermito --test lsp_hostile_json

use std::path::PathBuf;

use hermito::{
    authority::{local::LocalAuthority, Authority, AuthorityError},
    config::{lsp_config_digest, EffectiveLspConfig},
    lsp::{
        parse_and_validate_frame, position_from_json, read_lsp_frame, write_lsp_frame,
        JsonRpcFrameError, MAX_CONTENT_LENGTH,
    },
};
use hermito_protocol::{
    lsp::{
        AuthorityIdentity, LspContext, LspJsonRpcRequest, LspProtocolError, LspRequestId,
        LspV1, SentVersion, SessionGeneration,
    },
    request::{DocumentRevision, EnvironmentEpoch, ExecutionContextV1, WorkspaceEpoch},
};
use serde_json::json;
use tokio::{
    io::{duplex, AsyncWriteExt},
    runtime::Runtime,
};
use tokio_util::sync::CancellationToken;

fn lsp_context() -> LspContext {
    LspContext {
        workspace_epoch: WorkspaceEpoch(1),
        environment_epoch: EnvironmentEpoch(0),
        document_revision: Some(DocumentRevision(3)),
        sent_version: SentVersion(5),
        session_generation: SessionGeneration(7),
        execution_context: ExecutionContextV1::AuthorityRoot,
        authority_identity: AuthorityIdentity("local".into()),
    }
}

fn lsp_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lsp/rust")
}

#[test]
fn canonical_jsonrpc_frame_and_typed_lsp_roundtrip_validate() {
    let payload = br#"{"jsonrpc":"2.0","id":9,"method":"textDocument/hover","params":{"line":0}}"#;
    let runtime = Runtime::new().unwrap();
    let observed = runtime.block_on(async {
        let (mut writer, mut reader) = duplex(1024);
        write_lsp_frame(&mut writer, payload).await.unwrap();
        read_lsp_frame(&mut reader, MAX_CONTENT_LENGTH).await.unwrap()
    });
    assert_eq!(observed, payload);
    assert_eq!(parse_and_validate_frame(&observed).unwrap()["method"], "textDocument/hover");

    let message = LspV1::JsonRpcRequest {
        context: lsp_context(),
        payload: LspJsonRpcRequest {
            id: LspRequestId::Number(9),
            method: "textDocument/hover".into(),
            params: Some(json!({"line": 0, "character": 0})),
        },
    };
    assert_eq!(message.validate(), Ok(()));
    let roundtrip: LspV1 = serde_json::from_slice(&serde_json::to_vec(&message).unwrap()).unwrap();
    assert_eq!(roundtrip, message);

    let invalid = LspV1::JsonRpcRequest {
        context: lsp_context(),
        payload: LspJsonRpcRequest {
            id: LspRequestId::String(String::new()),
            method: String::new(),
            params: None,
        },
    };
    assert!(matches!(invalid.validate(), Err(LspProtocolError::EmptyMethod)));
}

#[test]
fn content_length_and_hostile_numeric_values_are_rejected_before_conversion() {
    let runtime = Runtime::new().unwrap();
    let declared = MAX_CONTENT_LENGTH + 1;
    let frame_error = runtime.block_on(async {
        let (mut reader, mut writer) = duplex(128);
        writer
            .write_all(format!("Content-Length: {declared}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        drop(writer);
        read_lsp_frame(&mut reader, MAX_CONTENT_LENGTH).await.unwrap_err()
    });
    assert!(matches!(
        frame_error,
        JsonRpcFrameError::ContentLengthExceeded {
            requested,
            cap: MAX_CONTENT_LENGTH,
        } if requested == declared
    ));

    for raw in [
        br#"{"jsonrpc":"2.0","id":-1,"method":"x"}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1.5,"method":"x"}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":9223372036854775808,"method":"x"}"#.as_slice(),
    ] {
        assert!(matches!(
            parse_and_validate_frame(raw),
            Err(JsonRpcFrameError::NumericViolation { field }) if field == "id"
        ));
    }

    for raw_position in [
        json!({"line": -1, "character": 0}),
        json!({"line": 0, "character": 1.5}),
        json!({"line": u32::MAX as u64 + 1, "character": 0}),
    ] {
        assert!(matches!(
            position_from_json(&raw_position),
            Err(JsonRpcFrameError::NumericViolation { .. })
        ));
    }
}

#[test]
fn lsp_execution_requires_an_exact_config_digest_grant() {
    let root = lsp_fixture_root();
    assert!(root.is_dir());
    let authority = LocalAuthority::new("host", root, WorkspaceEpoch(1)).unwrap();
    let effective = EffectiveLspConfig {
        executable: PathBuf::from("intentionally-not-spawned-language-server"),
        args: vec!["--stdio".into()],
        initialization_options: None,
        version_probe_args: None,
        expected_version: None,
        expected_digest: None,
    };
    let digest = lsp_config_digest(&effective);
    let different_digest = lsp_config_digest(&EffectiveLspConfig {
        executable: effective.executable.clone(),
        args: vec!["--different".into()],
        initialization_options: None,
        version_probe_args: None,
        expected_version: None,
        expected_digest: None,
    });

    authority.grant_execution();
    assert!(!authority.is_lsp_execution_granted(&digest));
    let runtime = Runtime::new().unwrap();
    assert!(matches!(
        runtime.block_on(authority.start_lsp(lsp_context(), effective.clone(), CancellationToken::new())),
        Err(AuthorityError::InspectOnly)
    ));

    authority.grant_lsp_execution(&digest);
    assert!(authority.is_lsp_execution_granted(&digest));
    assert!(!authority.is_lsp_execution_granted(&different_digest));
    authority.revoke_lsp_execution(&digest);
    assert!(!authority.is_lsp_execution_granted(&digest));
}
