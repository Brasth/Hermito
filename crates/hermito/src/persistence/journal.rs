//! Owner-only atomic dirty-buffer journal.
//! Dedicated worker thread using bounded std sync channel.
//! try_checkpoint returns the payload to App on queue full (never drops newest).
//! Ack sent only after: write to tmp + fsync(tmp) + rename + fsync(parent dir).
//! Worker coalesces to latest per document.
//! Compact triggered by ack_save (after durable main save).
//! Recover parses JSONL, skips corrupt records, returns latest rev per doc exactly.
//! No FS on UI thread except startup recover + shutdown flush.
//! AckSave now has try_ack_save so compactions are retained+retried in App under backpressure (never dropped).
//! Blocking submits only in shutdown paths.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self as std_mpsc, SyncSender, TrySendError};
use std::thread;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::buffer::CheckpointPayload;
use crate::document::{DocumentId, DocumentRevision, Language, WorkspaceEpoch};
use crate::persistence::{durable_atomic_replace, journal_path};

/// Acknowledgement sent by worker after durable write.
#[derive(Clone, Debug)]
pub struct JournalAck {
    pub doc_id: DocumentId,
    pub revision: DocumentRevision,
    pub epoch: WorkspaceEpoch,
}

/// Recovery result: all latest acknowledged dirty buffers. Restored before layout.
#[derive(Debug, Default, Clone)]
pub struct Recovery {
    pub buffers: Vec<RecoveredBuffer>,
}

#[derive(Debug, Clone)]
pub struct RecoveredBuffer {
    pub id: DocumentId,
    pub revision: DocumentRevision,
    pub content: String,
    pub path: Option<PathBuf>,
    pub language: Language,
}

/// Internal command to worker (sync bounded).
enum Command {
    Checkpoint(CheckpointPayload),
    AckSave(DocumentId, DocumentRevision),
    Flush(std_mpsc::Sender<()>),
}

/// Handle owned by App (event-loop thread). Non-blocking try.
#[derive(Clone)]
pub struct JournalHandle {
    tx: SyncSender<Command>,
}

impl JournalHandle {
    /// Try to submit latest checkpoint for a document.
    /// On bounded queue full, returns the payload to caller (App keeps in pending map, retries on tick).
    /// Never drops a newer revision.
    pub fn try_checkpoint(&self, payload: CheckpointPayload) -> Result<(), CheckpointPayload> {
        match self.tx.try_send(Command::Checkpoint(payload)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(cmd)) => {
                if let Command::Checkpoint(p) = cmd {
                    Err(p)
                } else {
                    // unexpected command in error position: synthesize zeroed to satisfy no-silent but allow App to keep caller payload
                    // (App will have original; this branch is defensive)
                    Err(zeroed_checkpoint())
                }
            }
            Err(TrySendError::Disconnected(cmd)) => {
                if let Command::Checkpoint(p) = cmd {
                    Err(p)
                } else {
                    Err(zeroed_checkpoint())
                }
            }
        }
    }

    /// Try to notify worker for compaction after durable file save.
    /// Returns Err((id,rev)) on full/disconnect so App can retain and retry (lossless).
    /// Blocking variants below ONLY for explicit shutdown.
    pub fn try_ack_save(
        &self,
        id: DocumentId,
        rev: DocumentRevision,
    ) -> Result<(), (DocumentId, DocumentRevision)> {
        match self.tx.try_send(Command::AckSave(id, rev)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(Command::AckSave(i, r)))
            | Err(TrySendError::Disconnected(Command::AckSave(i, r))) => Err((i, r)),
            Err(_) => Err((id, rev)),
        }
    }

    /// Notify (best effort; use try + retain for normal path).
    pub fn ack_save(&self, id: DocumentId, rev: DocumentRevision) {
        let _ = self.try_ack_save(id, rev);
    }

    /// Explicit shutdown flush: send and wait for worker to finish current + write.
    pub fn flush(&self) {
        let (tx, rx) = std_mpsc::channel();
        let _ = self.tx.send(Command::Flush(tx)); // blocking send at shutdown is acceptable per contract (explicit flush)
        let _ = rx.recv();
    }

    /// Blocking submit of a retained checkpoint. ONLY called during explicit shutdown paths.
    pub fn submit_checkpoint_blocking(&self, payload: CheckpointPayload) {
        let _ = self.tx.send(Command::Checkpoint(payload));
    }

    /// Blocking submit of a retained ack for compaction. ONLY called during explicit shutdown paths.
    pub fn submit_ack_blocking(&self, id: DocumentId, rev: DocumentRevision) {
        let _ = self.tx.send(Command::AckSave(id, rev));
    }
}
fn zeroed_checkpoint() -> CheckpointPayload {
    // minimal to satisfy type; App always retains its original on error path
    CheckpointPayload {
        doc_id: DocumentId(uuid::Uuid::nil()),
        revision: DocumentRevision(0),
        content: String::new(),
        language: Language::PlainText,
        path: None,
        epoch: WorkspaceEpoch(0),
    }
}

/// Start the dedicated journal worker thread. Seeds the internal dirty map from the supplied
/// Recovery (latest acknowledged checkpoints per doc) *before* entering the command recv loop.
/// This ensures that on any subsequent checkpoint (update) or ack_save (compact/rewrite), all
/// previously recovered docs are preserved in the atomic write (per contract).
/// Returns handle + ack receiver for event loop.
pub fn start_journal_worker(recovery: Recovery) -> (JournalHandle, std_mpsc::Receiver<JournalAck>) {
    start_journal_worker_for_path(recovery, journal_path())
}
/// Synchronous startup-only recovery. Parses records, keeps latest per id, skips corrupt.
pub fn recover_journal() -> Result<Recovery> {
    recover_journal_from(&journal_path())
}

/// Test-only variant that seeds worker and directs all persist/recover writes to an explicit
/// journal file path (used by deterministic regression test for multi-doc seed+update without
/// touching real user config dir/journal).
pub fn start_journal_worker_for_path(
    recovery: Recovery,
    path: PathBuf,
) -> (JournalHandle, std_mpsc::Receiver<JournalAck>) {
    let (cmd_tx, cmd_rx) = std_mpsc::sync_channel(16); // bounded per contract
    let (ack_tx, ack_rx) = std_mpsc::channel();
    let p = path;
    thread::spawn(move || {
        journal_worker(cmd_rx, ack_tx, p, recovery);
    });
    (JournalHandle { tx: cmd_tx }, ack_rx)
}

/// Recovers a journal from an explicit path.
///
/// Startup uses [`recover_journal`]; the path-based form keeps recovery deterministic
/// and isolated in tests and recovery tooling.
pub fn recover_journal_from(path: &Path) -> Result<Recovery> {
    if !path.exists() {
        return Ok(Recovery::default());
    }
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(_) => return Ok(Recovery::default()),
    };
    let mut latest: HashMap<DocumentId, RecoveredBuffer> = HashMap::new();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalRecord>(line) {
            Ok(rec) if rec.v == 1 && rec.kind == "checkpoint" => {
                if let (Some(id), Some(rev), Some(content)) = (rec.id, rec.rev, rec.content) {
                    let lang = match rec.lang.as_deref() {
                        Some("rust") => Language::Rust,
                        Some("typescript") => Language::TypeScript,
                        Some("javascript") => Language::JavaScript,
                        Some("go") => Language::Go,
                        Some("python") => Language::Python,
                        _ => Language::PlainText,
                    };
                    let buf = RecoveredBuffer {
                        id,
                        revision: DocumentRevision(rev),
                        content,
                        path: rec.path,
                        language: lang,
                    };
                    latest
                        .entry(buf.id)
                        .and_modify(|e| {
                            if buf.revision.0 > e.revision.0 {
                                *e = buf.clone();
                            }
                        })
                        .or_insert(buf);
                }
            }
            _ => {
                // corrupt record: skip exactly per contract
            }
        }
    }
    let buffers = latest.into_values().collect();
    Ok(Recovery { buffers })
}

/// Internal JSONL record (versioned, allows per-record skip on corrupt).
#[derive(Serialize, Deserialize)]
struct JournalRecord {
    v: u32,
    kind: String,
    id: Option<DocumentId>,
    rev: Option<u64>,
    content: Option<String>,
    path: Option<PathBuf>,
    lang: Option<String>,
    #[serde(default)]
    epoch: Option<u64>,
}

fn journal_worker(
    rx: std_mpsc::Receiver<Command>,
    ack_tx: std_mpsc::Sender<JournalAck>,
    path: PathBuf,
    initial: Recovery,
) {
    let mut dirty: HashMap<DocumentId, CheckpointPayload> = initial
        .buffers
        .into_iter()
        .map(|b| {
            (
                b.id,
                CheckpointPayload {
                    doc_id: b.id,
                    revision: b.revision,
                    content: b.content,
                    language: b.language,
                    path: b.path,
                    epoch: WorkspaceEpoch(0),
                },
            )
        })
        .collect();
    if let Some(parent) = path.parent() {
        let _ = crate::persistence::create_dir_all_owner_only(parent);
    }
    loop {
        match rx.recv() {
            Ok(Command::Checkpoint(p)) => {
                let id = p.doc_id;
                let rev = p.revision;
                let epoch = p.epoch;
                dirty.insert(id, p);
                match persist_current(&dirty, &path) {
                    Ok(()) => {
                        let _ = ack_tx.send(JournalAck {
                            doc_id: id,
                            revision: rev,
                            epoch,
                        });
                    }
                    Err(_) => {
                        // do not ack; App will retain
                    }
                }
            }
            Ok(Command::AckSave(id, rev)) => {
                if let Some(cur) = dirty.get(&id) {
                    if cur.revision.0 <= rev.0 {
                        let removed = dirty.remove(&id);
                        if let Some(p) = removed {
                            if persist_current(&dirty, &path).is_err() {
                                dirty.insert(id, p);
                                // keep checkpoint; shutdown flush can retry compaction
                            }
                            // else: successfully compacted (removed from mem + file)
                        }
                    }
                }
            }
            Ok(Command::Flush(done_tx)) => {
                let _ = persist_current(&dirty, &path);
                let _ = done_tx.send(());
            }
            Err(_) => {
                let _ = persist_current(&dirty, &path);
                break;
            }
        }
    }
}

/// Write full current dirty set as JSONL atomically + fsync + dir + owner.
fn persist_current(
    dirty: &HashMap<DocumentId, CheckpointPayload>,
    target: &Path,
) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
        let _ = crate::persistence::set_owner_only_dir(parent);
    }
    let tmp = target.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        for p in dirty.values() {
            let lang = match p.language {
                Language::Rust => "rust",
                Language::TypeScript => "typescript",
                Language::JavaScript => "javascript",
                Language::Go => "go",
                Language::Python => "python",
                _ => "plain",
            };
            let rec = JournalRecord {
                v: 1,
                kind: "checkpoint".to_string(),
                id: Some(p.doc_id),
                rev: Some(p.revision.0),
                content: Some(p.content.clone()),
                path: p.path.clone(),
                lang: Some(lang.to_string()),
                epoch: Some(p.epoch.0),
            };
            let line = serde_json::to_string(&rec).map_err(io::Error::other)? + "\n";
            f.write_all(line.as_bytes())?;
        }
        f.sync_all()?;
    }
    durable_atomic_replace(&tmp, target)?;
    Ok(())
}
