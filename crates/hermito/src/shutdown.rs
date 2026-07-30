use crate::persistence::journal::JournalHandle;
use crate::terminal::TerminalGuard;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::LazyLock;
use std::sync::Mutex;

static SHUTDOWN_TX: LazyLock<Mutex<Option<Sender<ShutdownReason>>>> =
    LazyLock::new(|| Mutex::new(None));

#[derive(Clone, Copy, Debug)]
pub enum ShutdownReason {
    Normal,
    Signal,
    FatalWorker,
    Panic,
}

pub fn register_shutdown(
    _guard_owner: &mut Option<TerminalGuard>,
    _journal: &JournalHandle,
) -> Receiver<ShutdownReason> {
    let (tx, rx) = mpsc::channel::<ShutdownReason>();
    {
        let mut g = SHUTDOWN_TX.lock().unwrap();
        *g = Some(tx.clone());
    }

    // Platform signal registration (cfg gated, using only approved tokio signal + windows-sys)
    #[cfg(unix)]
    {
        let tx2 = tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigint = signal(SignalKind::interrupt()).expect("sigint");
            let mut sigterm = signal(SignalKind::terminate()).expect("sigterm");
            let mut sighup = signal(SignalKind::hangup()).expect("sighup");
            tokio::select! {
                _ = sigint.recv() => {},
                _ = sigterm.recv() => {},
                _ = sighup.recv() => {},
            }
            let _ = tx2.send(ShutdownReason::Signal);
        });
    }
    #[cfg(windows)]
    {
        unsafe extern "system" fn handler(ctrl_type: u32) -> windows_sys::core::BOOL {
            if ctrl_type == windows_sys::Win32::System::Console::CTRL_C_EVENT
                || ctrl_type == windows_sys::Win32::System::Console::CTRL_CLOSE_EVENT
            {
                if let Ok(g) = SHUTDOWN_TX.lock() {
                    if let Some(tx) = g.as_ref() {
                        let _ = tx.send(ShutdownReason::Signal);
                    }
                }
                windows_sys::Win32::Foundation::TRUE
            } else {
                0
            }
        }
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(
                Some(handler),
                windows_sys::Win32::Foundation::TRUE,
            );
        }
    }

    rx
}

/// Single consuming restore path. Flush journal then restore terminal exactly once.
/// Called for normal quit, signal, fatal, caught panic.
pub fn restore_once(
    guard: TerminalGuard,
    journal: &JournalHandle,
    reason: ShutdownReason,
) -> std::io::Result<()> {
    journal.flush();
    let res = guard.restore();
    if matches!(reason, ShutdownReason::Panic) {
        // terminal restored before any error report or re-panic
    }
    res
}

pub fn take_shutdown_sender() -> Option<Sender<ShutdownReason>> {
    SHUTDOWN_TX.lock().ok().and_then(|g| g.clone())
}
