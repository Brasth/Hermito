#![cfg(unix)]

use std::{collections::BTreeMap, process::Stdio, time::Duration};

use hermito_protocol::{
    frame::{read_frame, write_message, AggregateBudget, FrameError, FrameLimits, ReceivedMessage},
    fs::{FileContent, FsMessage, ReadFile, WriteFile},
    process::{ExecOutput, ExecRequest, ProcessMessage},
    pty::{PtyMessage, PtySize, PtySpawn, PtyStreamContext},
    request::{CommandSpec, DocumentRevision, EnvironmentEpoch, RequestEnvelope, WorkspaceEpoch},
    response::RemoteErrorCode,
    Message, CURRENT_VERSION,
};
use tokio::{io::AsyncWriteExt, process::Command};

async fn read_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    limits: FrameLimits,
    budget: &AggregateBudget,
) -> Result<ReceivedMessage, FrameError> {
    read_frame(reader, limits, budget).await?.into_message()
}

fn envelope<T>(payload: T) -> RequestEnvelope<T> {
    RequestEnvelope::authority_root(payload, WorkspaceEpoch(1), EnvironmentEpoch(2))
}

fn document_envelope<T>(payload: T) -> RequestEnvelope<T> {
    let mut request = envelope(payload);
    request.document_revision = Some(DocumentRevision(1));
    request
}

fn command(cwd: &std::path::Path, script: &str) -> CommandSpec {
    CommandSpec {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: cwd.to_string_lossy().into_owned(),
        env: BTreeMap::from([("TERM".into(), "dumb".into())]),
    }
}

#[tokio::test]
async fn stdio_service_negotiates_and_serves_fs_process_and_pty() {
    let root =
        std::env::temp_dir().join(format!("hermito-remote-contract-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("remote.txt");

    let mut child = Command::new(env!("CARGO_BIN_EXE_hermito-remote"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let limits = FrameLimits::default();
    let budget = AggregateBudget::new(limits.aggregate);

    write_message(
        &mut stdin,
        &Message::Hello {
            version: CURRENT_VERSION,
        },
        limits,
    )
    .await
    .unwrap();
    let hello = read_message(&mut stdout, limits, &budget).await.unwrap();
    assert!(matches!(
        hello.message,
        Message::HelloAck { version } if version == CURRENT_VERSION
    ));
    drop(hello);

    let missing_revision = envelope(ReadFile {
        path: file.to_string_lossy().into_owned(),
        max_bytes: 64,
    });
    let missing_revision_id = missing_revision.request_id;
    write_message(
        &mut stdin,
        &Message::Fs(FsMessage::Read(missing_revision)),
        limits,
    )
    .await
    .unwrap();
    let missing_revision_response = read_message(&mut stdout, limits, &budget).await.unwrap();
    let (missing_revision_message, missing_revision_frame) = missing_revision_response.into_parts();
    match missing_revision_message {
        Message::Fs(FsMessage::ReadResult(response)) => {
            assert_eq!(response.request_id, missing_revision_id);
            assert_eq!(
                response.payload.unwrap_err().code,
                RemoteErrorCode::InvalidRequest
            );
        }
        message => panic!("unexpected missing-revision response: {message:?}"),
    }
    drop(missing_revision_frame);

    let write_request = document_envelope(WriteFile {
        path: file.to_string_lossy().into_owned(),
        bytes: b"remote-data".to_vec(),
        create: true,
    });
    let write_id = write_request.request_id;
    write_message(
        &mut stdin,
        &Message::Fs(FsMessage::Write(write_request)),
        limits,
    )
    .await
    .unwrap();
    let write_response = read_message(&mut stdout, limits, &budget).await.unwrap();
    let (write_message_result, write_frame) = write_response.into_parts();
    match write_message_result {
        Message::Fs(FsMessage::WriteResult(response)) => {
            assert_eq!(response.request_id, write_id);
            assert_eq!(response.payload.unwrap().bytes_written, 11);
        }
        message => panic!("unexpected write response: {message:?}"),
    }
    drop(write_frame);

    let read_request = document_envelope(ReadFile {
        path: file.to_string_lossy().into_owned(),
        max_bytes: 64,
    });
    write_message(
        &mut stdin,
        &Message::Fs(FsMessage::Read(read_request)),
        limits,
    )
    .await
    .unwrap();
    let read_response = read_message(&mut stdout, limits, &budget).await.unwrap();
    let (read_message_result, read_frame) = read_response.into_parts();
    match read_message_result {
        Message::Fs(FsMessage::ReadResult(response)) => {
            assert_eq!(
                response.payload.unwrap(),
                FileContent {
                    bytes: b"remote-data".to_vec()
                }
            );
        }
        message => panic!("unexpected read response: {message:?}"),
    }
    drop(read_frame);

    let exec_request = envelope(ExecRequest {
        command: command(&root, "printf process-ok"),
        timeout_ms: 2_000,
        stdout_limit: 64,
        stderr_limit: 64,
    });
    write_message(
        &mut stdin,
        &Message::Process(ProcessMessage::Exec(exec_request)),
        limits,
    )
    .await
    .unwrap();
    let exec_response = read_message(&mut stdout, limits, &budget).await.unwrap();
    let (exec_message_result, exec_frame) = exec_response.into_parts();
    match exec_message_result {
        Message::Process(ProcessMessage::Result(response)) => {
            assert_eq!(
                response.payload.unwrap(),
                ExecOutput {
                    exit_code: Some(0),
                    stdout: b"process-ok".to_vec(),
                    stderr: Vec::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                }
            );
        }
        message => panic!("unexpected process response: {message:?}"),
    }
    drop(exec_frame);

    let stream_id = 41;
    let pty_request = envelope(PtySpawn {
        stream_id,
        generation: 2,
        command: command(&root, "printf pty-ok"),
        size: PtySize {
            rows: 3,
            cols: 20,
            pixel_width: 0,
            pixel_height: 0,
        },
    });
    let pty_context = PtyStreamContext::from_spawn(&pty_request);
    write_message(
        &mut stdin,
        &Message::Pty(PtyMessage::Spawn(pty_request)),
        limits,
    )
    .await
    .unwrap();

    let mut pty_output = Vec::new();
    let mut started = false;
    let mut exited = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !exited {
            let message = read_message(&mut stdout, limits, &budget).await.unwrap();
            match message.message {
                Message::Pty(PtyMessage::Started { context, .. }) if context == pty_context => {
                    started = true;
                }
                Message::Pty(PtyMessage::Output { context, bytes }) if context == pty_context => {
                    pty_output.extend(bytes);
                }
                Message::Pty(PtyMessage::Exited {
                    context,
                    truncated: false,
                    ..
                }) if context == pty_context => exited = true,
                other => panic!("unexpected PTY response: {other:?}"),
            }
        }
    })
    .await
    .unwrap();
    assert!(started);
    assert!(String::from_utf8_lossy(&pty_output).contains("pty-ok"));
    let lingering_request = envelope(PtySpawn {
        stream_id: 42,
        generation: 3,
        command: command(&root, "sleep 30"),
        size: PtySize {
            rows: 3,
            cols: 20,
            pixel_width: 0,
            pixel_height: 0,
        },
    });
    let lingering_context = PtyStreamContext::from_spawn(&lingering_request);
    write_message(
        &mut stdin,
        &Message::Pty(PtyMessage::Spawn(lingering_request)),
        limits,
    )
    .await
    .unwrap();
    let lingering_pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let message = read_message(&mut stdout, limits, &budget).await.unwrap();
            if let Message::Pty(PtyMessage::Started {
                context,
                process_id: Some(process_id),
            }) = message.message
            {
                if context == lingering_context {
                    break process_id;
                }
            }
        }
    })
    .await
    .unwrap();

    stdin.shutdown().await.unwrap();
    drop(stdin);
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    let process_exists = unsafe { libc::kill(lingering_pid as i32, 0) };
    assert_eq!(process_exists, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );

    std::fs::remove_dir_all(root).unwrap();
}
