use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Result};
use hermito_protocol::{
    lsp::{
        LspContext, LspJsonRpcError, LspJsonRpcNotification, LspJsonRpcRequest,
        LspJsonRpcResponse, LspServerConfig, LspV1, TransactionalWorkspaceEdit,
    },
    request::ExecutionContextV1,
    Message,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdout, Command},
    sync::{mpsc, Mutex as AsyncMutex},
};
use tokio_util::sync::CancellationToken;

use super::OutboundSender;

const LSP_STDIN_LIMIT: usize = 1024 * 1024;
const LSP_FRAME_LIMIT: usize = 8 * 1024 * 1024;
const LSP_STDERR_LIMIT: usize = 64 * 1024;
const MAX_LSP_SESSIONS: usize = 16;
const MAX_PENDING_SERVER_REQUESTS: usize = 256;

pub(crate) struct RemoteLsp {
    context: LspContext,
    child: Arc<Mutex<Child>>,
    write_tx: mpsc::Sender<Vec<u8>>,
    cancellation: CancellationToken,
    pending_server_requests: Arc<AsyncMutex<HashSet<hermito_protocol::lsp::LspRequestId>>>,
}

impl RemoteLsp {
    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
        if let Ok(mut ch) = self.child.lock() {
            let _ = ch.start_kill();
        }
    }
}

pub(crate) async fn handle_lsp(
    message: LspV1,
    lsps: Arc<AsyncMutex<HashMap<LspContext, Arc<RemoteLsp>>>>,
    tx: OutboundSender,
    shutdown: CancellationToken,
) -> Result<()> {
    match message {
        LspV1::Start { context, config } => {
            handle_start(context, config, lsps, tx, shutdown).await
        }
        LspV1::Shutdown { context } => handle_shutdown(context, lsps, tx).await,
        LspV1::JsonRpcRequest { context, payload } => {
            forward_request(context, lsps, tx, payload).await
        }
        LspV1::JsonRpcNotification { context, payload } => {
            forward_notification(context, lsps, tx, payload).await
        }
        LspV1::WorkspaceEditResult {
            context,
            request_id,
            applied,
            reason,
        } => handle_workspace_result(context, lsps, request_id, applied, reason).await,
        _ => Ok(()),
    }
}

async fn handle_start(
    context: LspContext,
    config: LspServerConfig,
    lsps: Arc<AsyncMutex<HashMap<LspContext, Arc<RemoteLsp>>>>,
    tx: OutboundSender,
    shutdown: CancellationToken,
) -> Result<()> {
    match &context.execution_context {
        ExecutionContextV1::AuthorityRoot => {}
        ExecutionContextV1::DevContainer { .. } => {
            let _ = tx
                .send(Message::Lsp(LspV1::Exited {
                    context: context.clone(),
                    exit_code: None,
                }))
                .await;
            return Ok(());
        }
    }
    if let Err(_) = config.validate() {
        let _ = tx
            .send(Message::Lsp(LspV1::Exited {
                context: context.clone(),
                exit_code: None,
            }))
            .await;
        return Ok(());
    }
    let mut guard = lsps.lock().await;
    if guard.len() >= MAX_LSP_SESSIONS {
        drop(guard);
        let _ = tx
            .send(Message::Lsp(LspV1::Exited {
                context: context.clone(),
                exit_code: None,
            }))
            .await;
        return Ok(());
    }
    if guard.contains_key(&context) {
        drop(guard);
        return Ok(());
    }
    let mut cmd = Command::new(&config.program);
    cmd.args(&config.args)
        .current_dir(&config.cwd)
        .env_clear()
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => {
            drop(guard);
            let _ = tx
                .send(Message::Lsp(LspV1::Exited {
                    context: context.clone(),
                    exit_code: None,
                }))
                .await;
            return Ok(());
        }
    };
    let stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            drop(guard);
            let _ = tx
                .send(Message::Lsp(LspV1::Exited {
                    context: context.clone(),
                    exit_code: None,
                }))
                .await;
            return Ok(());
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            drop(guard);
            let _ = tx
                .send(Message::Lsp(LspV1::Exited {
                    context: context.clone(),
                    exit_code: None,
                }))
                .await;
            return Ok(());
        }
    };
    let stderr = child.stderr.take();
    let cancellation = CancellationToken::new();
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(4);
    let write_canc = cancellation.clone();
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(bytes) = write_rx.recv().await {
            if write_canc.is_cancelled() {
                break;
            }
            if stdin.write_all(&bytes).await.is_err() {
                break;
            }
            let _ = stdin.flush().await;
        }
    });
    if let Some(mut err) = stderr {
        let cap = LSP_STDERR_LIMIT;
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let mut used = 0usize;
            loop {
                match err.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        used += n;
                        if used > cap {
                            break;
                        }
                    }
                }
            }
        });
    }
    let session = Arc::new(RemoteLsp {
        context: context.clone(),
        child: Arc::new(Mutex::new(child)),
        write_tx: write_tx.clone(),
        cancellation: cancellation.clone(),
        pending_server_requests: Arc::new(AsyncMutex::new(HashSet::new())),
    });
    guard.insert(context.clone(), Arc::clone(&session));
    drop(guard);
    let _ = tx
        .send(Message::Lsp(LspV1::Started {
            context: context.clone(),
            capabilities: serde_json::json!({}),
        }))
        .await;
    let tx2 = tx.clone();
    let lsps2 = Arc::clone(&lsps);
    let sess2 = Arc::clone(&session);
    let ctx2 = context.clone();
    tokio::spawn(async move {
        run_reader(
            stdout, ctx2, sess2, write_tx, tx2, lsps2, cancellation, shutdown,
        )
        .await;
    });
    Ok(())
}

async fn handle_shutdown(
    context: LspContext,
    lsps: Arc<AsyncMutex<HashMap<LspContext, Arc<RemoteLsp>>>>,
    tx: OutboundSender,
) -> Result<()> {
    if let Some(sess) = lsps.lock().await.remove(&context) {
        sess.cancel();
        let _ = tx
            .send(Message::Lsp(LspV1::Exited {
                context,
                exit_code: None,
            }))
            .await;
    }
    Ok(())
}

async fn forward_request(
    context: LspContext,
    lsps: Arc<AsyncMutex<HashMap<LspContext, Arc<RemoteLsp>>>>,
    tx: OutboundSender,
    payload: LspJsonRpcRequest,
) -> Result<()> {
    let sess = lsps.lock().await.get(&context).cloned();
    if let Some(sess) = sess {
        if sess.cancellation.is_cancelled() {
            return Ok(());
        }
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": payload.id,
            "method": payload.method,
            "params": payload.params,
        });
        if send_frame(&sess.write_tx, &frame, LSP_STDIN_LIMIT)
            .await
            .is_err()
        {
            drop_session(&lsps, &tx, sess, context).await;
        }
    }
    Ok(())
}

async fn forward_notification(
    context: LspContext,
    lsps: Arc<AsyncMutex<HashMap<LspContext, Arc<RemoteLsp>>>>,
    _tx: OutboundSender,
    payload: LspJsonRpcNotification,
) -> Result<()> {
    let sess = lsps.lock().await.get(&context).cloned();
    if let Some(sess) = sess {
        if sess.cancellation.is_cancelled() {
            return Ok(());
        }
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "method": payload.method,
            "params": payload.params,
        });
        let _ = send_frame(&sess.write_tx, &frame, LSP_STDIN_LIMIT).await;
    }
    Ok(())
}

async fn handle_workspace_result(
    context: LspContext,
    lsps: Arc<AsyncMutex<HashMap<LspContext, Arc<RemoteLsp>>>>,
    request_id: hermito_protocol::lsp::LspRequestId,
    applied: bool,
    reason: Option<String>,
) -> Result<()> {
    let sess = lsps.lock().await.get(&context).cloned();
    if let Some(sess) = sess {
        let is_pending = {
            let mut pending = sess.pending_server_requests.lock().await;
            pending.remove(&request_id)
        };
        if is_pending {
            let result = if applied {
                serde_json::json!({"applied": true})
            } else {
                serde_json::json!({"applied": false, "failureReason": reason})
            };
            let resp = serde_json::json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": result,
            });
            let _ = send_frame(&sess.write_tx, &resp, LSP_STDIN_LIMIT).await;
        }
    }
    Ok(())
}

async fn send_frame(
    tx: &mpsc::Sender<Vec<u8>>,
    val: &serde_json::Value,
    limit: usize,
) -> Result<()> {
    let body = serde_json::to_vec(val)?;
    if body.len() > limit {
        return Err(anyhow!("LSP JSON-RPC exceeds cap"));
    }
    let hdr = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut w = Vec::with_capacity(hdr.len() + body.len());
    w.extend_from_slice(hdr.as_bytes());
    w.extend_from_slice(&body);
    tx.send(w).await.map_err(|_| anyhow!("lsp writer stopped"))
}

async fn drop_session(
    lsps: &Arc<AsyncMutex<HashMap<LspContext, Arc<RemoteLsp>>>>,
    tx: &OutboundSender,
    sess: Arc<RemoteLsp>,
    ctx: LspContext,
) {
    lsps.lock().await.remove(&ctx);
    sess.cancel();
    let _ = tx
        .send(Message::Lsp(LspV1::Exited {
            context: ctx,
            exit_code: None,
        }))
        .await;
}

async fn run_reader(
    stdout: ChildStdout,
    context: LspContext,
    session: Arc<RemoteLsp>,
    write_tx: mpsc::Sender<Vec<u8>>,
    tx: OutboundSender,
    lsps: Arc<AsyncMutex<HashMap<LspContext, Arc<RemoteLsp>>>>,
    cancellation: CancellationToken,
    shutdown: CancellationToken,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        if cancellation.is_cancelled() || shutdown.is_cancelled() {
            break;
        }
        let frame_val = match read_lsp_frame(&mut reader, LSP_FRAME_LIMIT).await {
            Ok(v) => v,
            Err(_) => {
                let _ = tx
                    .send(Message::Lsp(LspV1::Exited {
                        context: context.clone(),
                        exit_code: None,
                    }))
                    .await;
                break;
            }
        };
        let ctx = context.clone();
        let id_val = frame_val.get("id").cloned();
        let method = frame_val
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_owned();
        let params = frame_val.get("params").cloned();
        let has_result_or_error =
            frame_val.get("result").is_some() || frame_val.get("error").is_some();
        if id_val.is_some() && has_result_or_error {
            if let Ok(id) = parse_request_id(id_val.as_ref().unwrap()) {
                let result = frame_val.get("result").cloned();
                let error = frame_val.get("error").and_then(|e| {
                    serde_json::from_value::<LspJsonRpcError>(e.clone()).ok()
                });
                let resp = LspJsonRpcResponse { id, result, error };
                let _ = tx
                    .send(Message::Lsp(LspV1::JsonRpcResponse {
                        context: ctx,
                        payload: resp,
                    }))
                    .await;
            }
        } else if id_val.is_some() && !method.is_empty() {
            if let Ok(id) = parse_request_id(id_val.as_ref().unwrap()) {
                if method == "workspace/applyEdit" {
                    let edit = params
                        .clone()
                        .and_then(|params| {
                            serde_json::from_value::<lsp_types::ApplyWorkspaceEditParams>(params).ok()
                        })
                        .and_then(|params| TransactionalWorkspaceEdit::try_from(params.edit).ok());
                    if let Some(edit) = edit {
                        let registered = {
                            let mut pending = session.pending_server_requests.lock().await;
                            pending.len() < MAX_PENDING_SERVER_REQUESTS && pending.insert(id.clone())
                        };
                        if !registered {
                            let err_frame = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": { "code": -32603, "message": "workspace edit capacity exceeded" }
                            });
                            let _ = send_frame(&write_tx, &err_frame, LSP_STDIN_LIMIT).await;
                            continue;
                        }
                        if tx
                            .send(Message::Lsp(LspV1::WorkspaceEdit {
                                context: ctx,
                                request_id: id.clone(),
                                edit,
                            }))
                            .await
                            .is_err()
                        {
                            session.pending_server_requests.lock().await.remove(&id);
                            let err_frame = serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": { "code": -32603, "message": "workspace edit handler unavailable" }
                            });
                            let _ = send_frame(&write_tx, &err_frame, LSP_STDIN_LIMIT).await;
                        }
                        continue;
                    }
                    let err_frame = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32602, "message": "invalid workspace edit" }
                    });
                    let _ = send_frame(&write_tx, &err_frame, LSP_STDIN_LIMIT).await;
                } else {
                    let err_frame = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": "method not supported" }
                    });
                    let _ = send_frame(&write_tx, &err_frame, LSP_STDIN_LIMIT).await;
                }
            }
        } else if !method.is_empty() {
            if method == "textDocument/publishDiagnostics" {
                if let Some(p) = params.clone() {
                    #[derive(serde::Deserialize)]
                    struct Diags {
                        uri: String,
                        #[serde(default)]
                        version: Option<i32>,
                        #[serde(default)]
                        diagnostics: Vec<hermito_protocol::lsp::LspDiagnostic>,
                    }
                    if let Ok(d) = serde_json::from_value::<Diags>(p) {
                        let _ = tx
                            .send(Message::Lsp(LspV1::PublishDiagnostics {
                                context: ctx,
                                uri: d.uri,
                                version: d.version,
                                diagnostics: d.diagnostics,
                            }))
                            .await;
                        continue;
                    }
                }
            }
            let notif = LspJsonRpcNotification { method, params };
            let _ = tx
                .send(Message::Lsp(LspV1::JsonRpcNotification {
                    context: ctx,
                    payload: notif,
                }))
                .await;
        }
    }
    let _ = tx
        .send(Message::Lsp(LspV1::Exited {
            context,
            exit_code: None,
        }))
        .await;
}

async fn read_lsp_frame<R>(r: &mut BufReader<R>, limit: usize) -> Result<serde_json::Value>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let bytes = r.read_line(&mut line).await?;
        if bytes == 0 {
            return Err(anyhow!("eof reading lsp header"));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            if let Ok(n) = v.trim().parse::<usize>() {
                content_length = Some(n);
            }
        }
    }
    let len = content_length.ok_or_else(|| anyhow!("missing Content-Length"))?;
    if len > limit {
        return Err(anyhow!("lsp frame exceeds pre-allocation cap"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    let val: serde_json::Value = serde_json::from_slice(&body)?;
    Ok(val)
}

fn parse_request_id(v: &serde_json::Value) -> Result<hermito_protocol::lsp::LspRequestId> {
    if let Some(n) = v.as_i64() {
        Ok(hermito_protocol::lsp::LspRequestId::Number(n))
    } else if let Some(s) = v.as_str() {
        if s.is_empty() {
            Err(anyhow!("empty request id"))
        } else {
            Ok(hermito_protocol::lsp::LspRequestId::String(s.to_string()))
        }
    } else {
        Err(anyhow!("invalid request id type"))
    }
}
