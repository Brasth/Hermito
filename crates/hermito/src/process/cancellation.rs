use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessLimits {
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub wall_time: Duration,
    pub graceful_shutdown: Duration,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            stdout_bytes: 4 * 1024 * 1024,
            stderr_bytes: 4 * 1024 * 1024,
            wall_time: Duration::from_secs(30),
            graceful_shutdown: Duration::from_millis(500),
        }
    }
}
