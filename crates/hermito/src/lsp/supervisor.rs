//! Non-blocking language-service lifecycle supervision.
//!
//! The supervisor owns only lifecycle observations and event delivery. Process
//! construction remains behind `Authority::start_lsp`; the app consumes the
//! bounded event stream to project state into snapshots.

use std::{collections::HashMap, time::Duration};

use hermito_protocol::{
    lsp::{AuthorityIdentity, LspContext, LspV1, SessionGeneration},
    request::{EnvironmentEpoch, ExecutionContextV1, WorkspaceEpoch},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    authority::{Authority, AuthorityError},
    config::{lsp_config_digest, EffectiveLspConfig},
    lsp::LspTransport,
};

/// User-configured language identifier used to select a language service.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LanguageId(String);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LanguageIdError {
    #[error("language id must not be empty")]
    Empty,
    #[error("language id contains a control character")]
    ControlCharacter,
}

impl LanguageId {
    pub fn new(value: impl Into<String>) -> Result<Self, LanguageIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(LanguageIdError::Empty);
        }
        if value.chars().any(char::is_control) {
            return Err(LanguageIdError::ControlCharacter);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<crate::document::Language> for LanguageId {
    fn from(language: crate::document::Language) -> Self {
        // Language::as_str is a fixed internal vocabulary and therefore valid.
        Self(language.as_str().to_owned())
    }
}

/// Stable address for one language-service session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SupervisorKey {
    pub workspace_epoch: WorkspaceEpoch,
    pub environment_epoch: EnvironmentEpoch,
    pub authority_identity: AuthorityIdentity,
    pub execution_context: ExecutionContextV1,
    pub language: LanguageId,
}

impl SupervisorKey {
    pub fn new(
        workspace_epoch: WorkspaceEpoch,
        environment_epoch: EnvironmentEpoch,
        authority_identity: AuthorityIdentity,
        execution_context: ExecutionContextV1,
        language: LanguageId,
    ) -> Self {
        Self {
            workspace_epoch,
            environment_epoch,
            authority_identity,
            execution_context,
            language,
        }
    }
}

/// Exact message displayed when LSP execution has not received its independent grant.
pub const LSP_EXECUTION_TRUST_REQUIRED: &str =
    "LSP blocked: execution trust not granted for this authority";

/// Observable language-service lifecycle state. `Ready` is emitted only after
/// the transport completes the LSP `initialize` handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LanguageServiceState {
    Blocked { message: String },
    NotFound { detail: String },
    VersionMismatch { expected: String, actual: String },
    Starting,
    Ready,
    Failed { message: String },
}

impl LanguageServiceState {
    pub fn blocked() -> Self {
        Self::Blocked {
            message: LSP_EXECUTION_TRUST_REQUIRED.to_owned(),
        }
    }

    pub const fn status_label(&self) -> &'static str {
        match self {
            Self::Blocked { .. } => "blocked",
            Self::NotFound { .. } => "not found",
            Self::VersionMismatch { .. } => "version mismatch",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Failed { .. } => "failed",
        }
    }
}

/// Bounded retry accounting retained per service key. The event loop chooses
/// when to try again; the supervisor only records whether that is still allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestartBudget {
    pub attempts: u32,
    pub maximum_attempts: u32,
}

impl RestartBudget {
    pub const fn new(maximum_attempts: u32) -> Self {
        Self {
            attempts: 0,
            maximum_attempts,
        }
    }

    pub const fn remaining(self) -> u32 {
        self.maximum_attempts.saturating_sub(self.attempts)
    }

    pub fn consume(&mut self) -> bool {
        if self.attempts >= self.maximum_attempts {
            return false;
        }
        self.attempts += 1;
        true
    }

    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

impl Default for RestartBudget {
    fn default() -> Self {
        Self::new(3)
    }
}

/// A bounded recovery decision after a server process exits or its transport
/// becomes unusable. The caller performs the delayed restart off the UI loop,
/// then calls [`LspSupervisor::start`] with the returned generation installed
/// in its freshly reset document ledgers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartDecision {
    Restart {
        session_generation: SessionGeneration,
        delay: Duration,
    },
    Exhausted,
    IgnoredStale,
}

fn restart_delay(attempts: u32) -> Duration {
    const BASE_DELAY: Duration = Duration::from_millis(200);
    BASE_DELAY.saturating_mul(1u32 << attempts.saturating_sub(1).min(2))
}

/// Events delivered to the host event loop. Delivery always uses `try_send`;
/// no supervisor method awaits receiver capacity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupervisorEvent {
    StateChanged {
        key: SupervisorKey,
        state: LanguageServiceState,
    },
    DiagnosticsCountChanged {
        key: SupervisorKey,
        count: usize,
    },
}

/// Non-blocking lifecycle supervisor. It never creates a process itself; an
/// authority owns that operation through its existing `start_lsp` contract.
pub struct LspSupervisor {
    states: HashMap<SupervisorKey, LanguageServiceState>,
    restart_budgets: HashMap<SupervisorKey, RestartBudget>,
    /// The sole accepted generation for a running service. Every exit advances
    /// it before a replacement transport can be started, invalidating old
    /// client pending work and incoming results by their LspContext tag.
    session_generations: HashMap<SupervisorKey, SessionGeneration>,
    events: mpsc::Sender<SupervisorEvent>,
}

impl LspSupervisor {
    /// Construct a supervisor and its bounded event receiver for the host loop.
    /// `event_capacity` must be non-zero, as required by Tokio's bounded channel.
    pub fn channel(event_capacity: usize) -> (Self, mpsc::Receiver<SupervisorEvent>) {
        let (events, receiver) = mpsc::channel(event_capacity);
        (Self::with_event_sender(events), receiver)
    }

    pub fn with_event_sender(events: mpsc::Sender<SupervisorEvent>) -> Self {
        Self {
            states: HashMap::new(),
            restart_budgets: HashMap::new(),
            session_generations: HashMap::new(),
            events,
        }
    }

    pub fn state(&self, key: &SupervisorKey) -> Option<&LanguageServiceState> {
        self.states.get(key)
    }

    pub fn restart_budget(&self, key: &SupervisorKey) -> RestartBudget {
        self.restart_budgets.get(key).copied().unwrap_or_default()
    }

    pub fn set_restart_budget(&mut self, key: SupervisorKey, budget: RestartBudget) {
        self.restart_budgets.insert(key, budget);
    }

    pub fn consume_restart_budget(&mut self, key: &SupervisorKey) -> bool {
        self.restart_budgets.entry(key.clone()).or_default().consume()
    }

    pub fn reset_restart_budget(&mut self, key: &SupervisorKey) {
        self.restart_budgets.entry(key.clone()).or_default().reset();
    }

    /// Current server-session generation. Absence means no transport was
    /// successfully associated with this key yet.
    pub fn session_generation(&self, key: &SupervisorKey) -> Option<SessionGeneration> {
        self.session_generations.get(key).copied()
    }

    /// True only for messages from the currently active server session. The
    /// receive loop must call this before accepting a result or diagnostic.
    pub fn is_current_session(&self, key: &SupervisorKey, context: &LspContext) -> bool {
        context.workspace_epoch == key.workspace_epoch
            && context.environment_epoch == key.environment_epoch
            && context.execution_context == key.execution_context
            && context.authority_identity == key.authority_identity
            && self.session_generation(key) == Some(context.session_generation)
    }

    /// Bind a freshly reset document ledger to the live transport generation.
    /// App-owned spawning uses this after `inspect` and before the authority
    /// creates a transport, so later exit and I/O-loss events can be accepted
    /// by [`Self::observe_transport_loss`].
    pub fn activate_session(
        &mut self,
        key: &SupervisorKey,
        context: &LspContext,
    ) -> Result<(), AuthorityError> {
        if context.workspace_epoch != key.workspace_epoch
            || context.environment_epoch != key.environment_epoch
            || context.execution_context != key.execution_context
            || context.authority_identity != key.authority_identity
        {
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                session_generation = context.session_generation.0,
                reason = "context_key_mismatch",
                "rejected LSP session activation"
            );
            return Err(AuthorityError::Protocol(
                "LSP context does not match supervisor key".into(),
            ));
        }
        if let Some(expected) = self.session_generation(key) {
            if expected != context.session_generation {
                tracing::debug!(
                    authority_identity = %context.authority_identity.0,
                    execution_context = ?context.execution_context,
                    session_generation = context.session_generation.0,
                    reason = "session_generation_mismatch",
                    "rejected LSP session activation"
                );
                return Err(AuthorityError::Protocol(
                    "LSP context session generation does not match supervisor".into(),
                ));
            }
        }
        self.session_generations
            .insert(key.clone(), context.session_generation);
        tracing::debug!(
            authority_identity = %context.authority_identity.0,
            execution_context = ?context.execution_context,
            session_generation = context.session_generation.0,
            state = "active",
            "activated LSP session"
        );
        Ok(())
    }

    /// Return a context with the current replacement generation. Callers use
    /// it only after resetting authoritative document ledgers, then re-run
    /// initialize and didOpen for every current authoritative document.
    pub fn restart_context(
        &self,
        key: &SupervisorKey,
        previous: &LspContext,
    ) -> Option<LspContext> {
        let generation = self.session_generation(key)?;
        let mut context = previous.clone();
        context.session_generation = generation;
        Some(context)
    }

    /// Record a process exit or transport I/O loss. The transition is
    /// synchronous and uses `try_send`, so no UI path waits for channel
    /// capacity. A replacement generation is minted before the restart
    /// directive is returned; stale pending/results cannot match it.
    pub fn observe_transport_loss(
        &mut self,
        key: &SupervisorKey,
        context: &LspContext,
        detail: impl Into<String>,
    ) -> RestartDecision {
        if !self.is_current_session(key, context) {
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                session_generation = context.session_generation.0,
                reason = "stale_session",
                "ignored LSP transport loss"
            );
            return RestartDecision::IgnoredStale;
        }
        let detail = detail.into();
        let (attempts, maximum_attempts) = {
            let budget = self.restart_budgets.entry(key.clone()).or_default();
            if !budget.consume() {
                (None, budget.maximum_attempts)
            } else {
                (Some(budget.attempts), budget.maximum_attempts)
            }
        };
        let Some(attempts) = attempts else {
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                session_generation = context.session_generation.0,
                reason = "restart_budget_exhausted",
                state = "failed",
                "rejected LSP restart"
            );
            self.record_state(
                key.clone(),
                LanguageServiceState::Failed {
                    message: format!(
                        "LSP restart budget exhausted after {maximum_attempts} attempts: {detail}",
                    ),
                },
            );
            return RestartDecision::Exhausted;
        };
        let Some(session_generation) = context
            .session_generation
            .0
            .checked_add(1)
            .map(SessionGeneration)
        else {
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                session_generation = context.session_generation.0,
                reason = "session_generation_exhausted",
                state = "failed",
                "rejected LSP restart"
            );
            self.record_state(
                key.clone(),
                LanguageServiceState::Failed {
                    message: "LSP session generation exhausted".into(),
                },
            );
            return RestartDecision::Exhausted;
        };
        tracing::debug!(
            authority_identity = %context.authority_identity.0,
            execution_context = ?context.execution_context,
            session_generation = session_generation.0,
            state = "starting",
            "scheduled LSP restart"
        );
        self.session_generations.insert(key.clone(), session_generation);
        self.record_state(key.clone(), LanguageServiceState::Starting);
        RestartDecision::Restart {
            session_generation,
            delay: restart_delay(attempts),
        }
    }

    /// Observe a typed transport message. Remote helper and local process
    /// transports surface terminal sessions as `LspV1::Exited`; callers use
    /// [`Self::observe_transport_loss`] for send/receive I/O errors.
    pub fn observe_transport_message(
        &mut self,
        key: &SupervisorKey,
        message: &LspV1,
    ) -> Option<RestartDecision> {
        match message {
            LspV1::Exited { context, exit_code } => Some(self.observe_transport_loss(
                key,
                context,
                match exit_code {
                    Some(code) => format!("language server exited with status {code}"),
                    None => "language server session exited".into(),
                },
            )),
            _ => None,
        }
    }

    /// Mark a transport ready only after its LSP `initialize` handshake has
    /// completed. A stale replacement may never publish readiness.
    pub fn mark_ready(
        &mut self,
        key: &SupervisorKey,
        context: &LspContext,
    ) -> Result<(), AuthorityError> {
        if !self.is_current_session(key, context) {
            return Err(AuthorityError::Protocol(
                "cannot mark a stale LSP session ready".into(),
            ));
        }
        self.record_state(key.clone(), LanguageServiceState::Ready);
        Ok(())
    }

    /// Explicit user restart clears an exhausted automatic budget, mints a
    /// fresh generation, and transitions to Starting. The caller still must
    /// reset ledgers, wait for no delay, and invoke `start`; that call repeats
    /// authority identity, epoch, execution-context, and exact-digest trust
    /// checks before it can spawn anything.
    pub fn request_restart(
        &mut self,
        key: &SupervisorKey,
        context: &LspContext,
    ) -> Result<SessionGeneration, AuthorityError> {
        if !self.is_current_session(key, context) {
            tracing::debug!(
                authority_identity = %context.authority_identity.0,
                execution_context = ?context.execution_context,
                session_generation = context.session_generation.0,
                reason = "stale_session",
                "rejected explicit LSP restart"
            );
            return Err(AuthorityError::Protocol(
                "cannot restart a stale LSP session".into(),
            ));
        }
        let generation = context
            .session_generation
            .0
            .checked_add(1)
            .map(SessionGeneration)
            .ok_or_else(|| AuthorityError::Protocol("LSP session generation exhausted".into()))?;
        self.restart_budgets.entry(key.clone()).or_default().reset();
        self.session_generations.insert(key.clone(), generation);
        self.record_state(key.clone(), LanguageServiceState::Starting);
        tracing::debug!(
            authority_identity = %context.authority_identity.0,
            execution_context = ?context.execution_context,
            session_generation = generation.0,
            state = "starting",
            "requested LSP restart"
        );
        Ok(generation)
    }

    /// Delay only the background recovery task. Cancellation is an intentional
    /// shutdown/reconfiguration and must not launch a replacement process.
    pub async fn wait_for_restart(
        delay: Duration,
        cancellation: &CancellationToken,
    ) -> bool {
        tokio::select! {
            _ = cancellation.cancelled() => false,
            _ = tokio::time::sleep(delay) => true,
        }
    }

    fn record_state(&mut self, key: SupervisorKey, state: LanguageServiceState) {
        self.states.insert(key.clone(), state.clone());
        let _ = self
            .events
            .try_send(SupervisorEvent::StateChanged { key, state });
    }

    /// Enter an externally observed service state and attempt bounded delivery
    /// to the event loop without awaiting it.
    pub fn enter_state(
        &mut self,
        key: SupervisorKey,
        state: LanguageServiceState,
    ) -> Result<(), mpsc::error::TrySendError<SupervisorEvent>> {
        self.states.insert(key.clone(), state.clone());
        self.events
            .try_send(SupervisorEvent::StateChanged { key, state })
    }

    /// Record an already-normalized diagnostic count without blocking the
    /// supervisor or performing document inspection.
    pub fn enter_diagnostics_count(
        &self,
        key: SupervisorKey,
        count: usize,
    ) -> Result<(), mpsc::error::TrySendError<SupervisorEvent>> {
        self.events
            .try_send(SupervisorEvent::DiagnosticsCountChanged { key, count })
    }

    /// Inspect only synchronous authority identity, epoch, and exact-digest
    /// trust. This intentionally performs no PATH lookup, version probe, or
    /// process spawn.
    pub fn inspect<A: Authority + ?Sized>(
        &mut self,
        authority: &A,
        key: SupervisorKey,
        config_digest: &str,
    ) -> Result<LanguageServiceState, mpsc::error::TrySendError<SupervisorEvent>> {
        let state = if authority.workspace_epoch() != key.workspace_epoch
            || authority.environment_epoch() != key.environment_epoch
        {
            LanguageServiceState::Failed {
                message: "LSP supervisor key epoch does not match authority".into(),
            }
        } else if authority.host_authority_id() != key.authority_identity.0 {
            LanguageServiceState::Failed {
                message: "LSP supervisor key authority identity does not match authority".into(),
            }
        } else if !authority.is_lsp_execution_granted(config_digest) {
            LanguageServiceState::blocked()
        } else {
            LanguageServiceState::Starting
        };
        tracing::debug!(
            authority_identity = %key.authority_identity.0,
            execution_context = ?key.execution_context,
            config_digest = %config_digest,
            state = state.status_label(),
            "inspected LSP start gate"
        );
        self.enter_state(key, state.clone())?;
        Ok(state)
    }

    /// Start through the authority after the no-block inspection gate. The
    /// returned transport remains owned by the caller that drives `LspClient`.
    pub async fn start<A: Authority + ?Sized>(
        &mut self,
        authority: &A,
        key: SupervisorKey,
        context: LspContext,
        effective_config: EffectiveLspConfig,
        cancellation: CancellationToken,
    ) -> Result<Box<dyn LspTransport>, AuthorityError> {
        let digest = lsp_config_digest(&effective_config);
        let inspected = self.inspect(authority, key.clone(), &digest);
        let state = match inspected {
            Ok(state) => state,
            Err(_) => self
                .state(&key)
                .cloned()
                .unwrap_or_else(LanguageServiceState::blocked),
        };

        match state {
            LanguageServiceState::Starting => {}
            LanguageServiceState::Blocked { .. } => {
                tracing::debug!(
                    authority_identity = %key.authority_identity.0,
                    execution_context = ?key.execution_context,
                    config_digest = %digest,
                    state = "blocked",
                    reason = "lsp_execution_trust_not_granted",
                    "blocked LSP start"
                );
                return Err(AuthorityError::InspectOnly);
            }
            LanguageServiceState::Failed { message } => {
                tracing::debug!(
                    authority_identity = %key.authority_identity.0,
                    execution_context = ?key.execution_context,
                    config_digest = %digest,
                    state = "failed",
                    reason = "inspection_failed",
                    "blocked LSP start"
                );
                return Err(AuthorityError::Protocol(message));
            }
            _ => {
                return Err(AuthorityError::Protocol(
                    "LSP supervisor inspection produced an invalid start state".into(),
                ));
            }
        }
        if context.workspace_epoch != key.workspace_epoch
            || context.environment_epoch != key.environment_epoch
            || context.execution_context != key.execution_context
            || context.authority_identity != key.authority_identity
        {
            let state = LanguageServiceState::Failed {
                message: "LSP context does not match supervisor key".into(),
            };
            let _ = self.enter_state(key, state);
            return Err(AuthorityError::Protocol(
                "LSP context does not match supervisor key".into(),
            ));
        }
        match self.session_generations.get(&key).copied() {
            Some(expected) if expected != context.session_generation => {
                let state = LanguageServiceState::Failed {
                    message: "LSP context session generation does not match supervisor".into(),
                };
                let _ = self.enter_state(key, state);
                return Err(AuthorityError::Protocol(
                    "LSP context session generation does not match supervisor".into(),
                ));
            }
            Some(_) => {}
            None => {
                self.session_generations
                    .insert(key.clone(), context.session_generation);
            }
        }

        tracing::debug!(
            authority_identity = %context.authority_identity.0,
            execution_context = ?context.execution_context,
            config_digest = %digest,
            session_generation = context.session_generation.0,
            sent_version = context.sent_version.0,
            revision = ?context.document_revision,
            state = "starting",
            "starting LSP transport"
        );
        match authority
            .start_lsp(context, effective_config, cancellation)
            .await
        {
            Ok(transport) => {
                if authority.workspace_epoch() != key.workspace_epoch
                    || authority.environment_epoch() != key.environment_epoch
                {
                    let error = AuthorityError::StaleEpoch {
                        expected_workspace: authority.workspace_epoch(),
                        actual_workspace: key.workspace_epoch,
                        expected_environment: authority.environment_epoch(),
                        actual_environment: key.environment_epoch,
                    };
                    let _ = self.enter_state(
                        key,
                        LanguageServiceState::Failed {
                            message: error.to_string(),
                        },
                    );
                    return Err(error);
                }
                tracing::debug!(
                    authority_identity = %key.authority_identity.0,
                    execution_context = ?key.execution_context,
                    config_digest = %digest,
                    state = "started",
                    "started LSP transport"
                );
                Ok(transport)
            }
            Err(error) => {
                let state = if matches!(&error, AuthorityError::InspectOnly) {
                    LanguageServiceState::blocked()
                } else {
                    LanguageServiceState::Failed {
                        message: error.to_string(),
                    }
                };
                let _ = self.enter_state(key, state);
                Err(error)
            }
        }
    }
}
