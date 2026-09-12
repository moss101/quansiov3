//! Durable protocol state for exact resume (CORE-006, DOMAIN.md §5.7).
//!
//! Recovery must never consult semantic memory (DOSSIER.md §8, D-007). It reads the
//! canonical event stream, the protocol state stored here, checkpoints, evidence and the
//! Effect Ledger, and from those decides the next *safe* action.
//!
//! The decision itself is a pure function of the persisted state
//! ([`next_safe_action`]), so it is testable without a database and cannot depend on
//! anything but what was durably written. An effect whose outcome is unknown is never
//! retried by that function: it is reported as [`NextAction::ReconcileEffect`].

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgConnection;
use sqlx::Row;

use crate::control::schema::{set_tenant_context_conn, SchemaError};

/// Kind of pending wait that parked a run (DOMAIN.md §5.7 `waits[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitKind {
    /// Waiting for an approval receipt.
    Approval,
    /// Waiting for a human answer to a question.
    Question,
    /// Waiting for an external event.
    Event,
    /// Waiting for a timer to fire.
    Timer,
    /// Waiting for a child agent thread.
    Child,
    /// Waiting for a human to take over or hand back the browser/computer.
    Takeover,
}

/// A durable wait entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wait {
    /// What the run is waiting for.
    pub kind: WaitKind,
    /// Correlation key (approval id, question id, timer key, child thread id).
    pub key: String,
    /// RFC 3339 expiry, when the wait is time-bounded.
    pub expires_at: Option<String>,
}

/// A tool call that was dispatched (or reserved) but not yet settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingToolCall {
    /// Tool call id (`tc_…`).
    pub tool_call_id: String,
    /// Tool name.
    pub tool_name: String,
    /// Dispatch token presented to the host.
    pub dispatch_token: String,
    /// Effect record id (`eff_…`) reserved for this call.
    pub effect_id: String,
    /// Effect ledger status recorded at dispatch time (DOMAIN.md §7.2).
    pub effect_status: String,
}

/// A model call that was started but whose completion was not durably recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingModelCall {
    /// Provider-normalized call id.
    pub call_id: String,
    /// Model route id.
    pub route_id: String,
}

/// Who currently drives the managed browser session (DOMAIN.md §8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserControlHolder {
    /// The agent drives input.
    Agent,
    /// A human took over; agent input is fenced.
    User,
    /// Nobody holds control.
    None,
}

/// Browser control state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserControl {
    /// Managed browser session id.
    pub session_id: String,
    /// Current holder of input control.
    pub holder: BrowserControlHolder,
    /// RFC 3339 timestamp of the last control change.
    pub since: Option<String>,
}

/// Reference to a durable terminal session and its byte cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSessionRef {
    /// Terminal session id.
    pub id: String,
    /// Durable byte offset for replay.
    pub cursor: Option<String>,
}

/// The complete durable protocol state of one run (DOMAIN.md §5.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolState {
    /// Run id (`run_…`).
    pub run_id: String,
    /// Generation this state was written under.
    pub generation: i64,
    /// Model call in flight, if any.
    pub pending_model_call: Option<PendingModelCall>,
    /// Tool calls dispatched but not settled.
    pub pending_tool_calls: Vec<PendingToolCall>,
    /// Approval request ids awaiting decision.
    pub pending_approvals: Vec<String>,
    /// Question ids awaiting an answer.
    pub open_questions: Vec<String>,
    /// Logical waits.
    pub waits: Vec<Wait>,
    /// Browser control state, when a session is attached.
    pub browser_control: Option<BrowserControl>,
    /// Terminal sessions attached to the run.
    pub terminal_sessions: Vec<TerminalSessionRef>,
    /// Child agent threads this run is waiting on.
    pub child_agent_threads: Vec<String>,
    /// Whether cancellation was requested.
    pub cancellation_requested: bool,
    /// RFC 3339 timestamp of the cancellation request.
    pub cancellation_at: Option<String>,
    /// Last installed compaction epoch, so recovery does not re-install a stale one.
    pub last_compaction_epoch_id: Option<String>,
}

impl ProtocolState {
    /// Empty state for a run at a given generation.
    #[must_use]
    pub fn new(run_id: impl Into<String>, generation: i64) -> Self {
        Self {
            run_id: run_id.into(),
            generation,
            pending_model_call: None,
            pending_tool_calls: Vec::new(),
            pending_approvals: Vec::new(),
            open_questions: Vec::new(),
            waits: Vec::new(),
            browser_control: None,
            terminal_sessions: Vec::new(),
            child_agent_threads: Vec::new(),
            cancellation_requested: false,
            cancellation_at: None,
            last_compaction_epoch_id: None,
        }
    }
}

/// The next safe action after a crash or restart, derived from durable state only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextAction {
    /// Honour a cancellation request.
    Cancel,
    /// Reconcile an effect whose outcome is unknown; never blind-retry (D-014/§7.2).
    ReconcileEffect {
        /// Effect record to reconcile.
        effect_id: String,
        /// Tool call that reserved it.
        tool_call_id: String,
    },
    /// Wait for the pending approval.
    WaitApproval {
        /// Approval request id.
        approval_id: String,
    },
    /// Wait for the answer to an open question.
    WaitQuestion {
        /// Question id.
        question_id: String,
    },
    /// Wait for a timer or external event.
    WaitFor {
        /// Wait entry.
        wait: Wait,
    },
    /// Wait for a child agent thread to join.
    WaitChild {
        /// Child agent thread id.
        child_agent_thread_id: String,
    },
    /// The browser is under human control: agent input stays fenced.
    AwaitHandback {
        /// Browser session id.
        session_id: String,
    },
    /// Resume the turn loop.
    Continue,
}

/// Decide the next safe action from durable protocol state alone.
///
/// Precedence follows DOMAIN.md §5.2 and §7.2: cancellation first, then unresolved
/// effects (which must be reconciled, never retried), then human-gated waits, then
/// timers and children, then the normal loop. The function is pure, so recovery cannot
/// accidentally depend on live process state.
#[must_use]
pub fn next_safe_action(state: &ProtocolState) -> NextAction {
    if state.cancellation_requested {
        return NextAction::Cancel;
    }
    if let Some(call) = state
        .pending_tool_calls
        .iter()
        .find(|call| is_unsettled(&call.effect_status))
    {
        return NextAction::ReconcileEffect {
            effect_id: call.effect_id.clone(),
            tool_call_id: call.tool_call_id.clone(),
        };
    }
    if let Some(approval_id) = state.pending_approvals.first() {
        return NextAction::WaitApproval {
            approval_id: approval_id.clone(),
        };
    }
    if let Some(question_id) = state.open_questions.first() {
        return NextAction::WaitQuestion {
            question_id: question_id.clone(),
        };
    }
    if let Some(control) = &state.browser_control {
        if control.holder == BrowserControlHolder::User {
            return NextAction::AwaitHandback {
                session_id: control.session_id.clone(),
            };
        }
    }
    if let Some(child) = state.child_agent_threads.first() {
        return NextAction::WaitChild {
            child_agent_thread_id: child.clone(),
        };
    }
    if let Some(wait) = state.waits.first() {
        return NextAction::WaitFor { wait: wait.clone() };
    }
    NextAction::Continue
}

/// Effect statuses that mean "the external outcome is not settled" (DOMAIN.md §7.2).
#[must_use]
pub fn is_unsettled(effect_status: &str) -> bool {
    matches!(
        effect_status,
        "DISPATCHED" | "OUTCOME_UNKNOWN" | "RECONCILING"
    )
}

/// Errors from the protocol-state store.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolStateError {
    /// The database rejected the operation.
    #[error("protocol state store: {0}")]
    Database(#[from] sqlx::Error),
    /// The tenant context could not be established.
    #[error("protocol state store: {0}")]
    Schema(#[from] SchemaError),
    /// The requested run has no durable protocol state.
    #[error("no protocol state for run {0}")]
    NotFound(String),
    /// A JSON column could not be decoded.
    #[error("protocol state decode: {0}")]
    Decode(String),
}

/// Durable protocol-state store (one row per run, CORE-006).
pub struct ProtocolStateStore;

impl ProtocolStateStore {
    /// Load the protocol state for a run, or `None` when nothing was persisted yet.
    ///
    /// # Errors
    /// Returns an error when the query fails or a JSON column cannot be decoded.
    pub async fn load(
        conn: &mut PgConnection,
        tenant_id: &str,
        run_id: &str,
    ) -> Result<Option<ProtocolState>, ProtocolStateError> {
        set_tenant_context_conn(conn, tenant_id).await?;
        let row = sqlx::query(
            "SELECT run_id, generation, pending_model_call, pending_tool_calls, pending_approvals, \
             open_questions, waits, browser_control, terminal_sessions, child_agent_threads, \
             cancellation_requested, cancellation_at, last_compaction_epoch_id \
             FROM protocol_states WHERE run_id = $1",
        )
        .bind(run_id)
        .fetch_optional(&mut *conn)
        .await?;
        row.map(|row| decode_state(&row)).transpose()
    }

    /// Persist the protocol state for a run, replacing any previous value.
    ///
    /// # Errors
    /// Returns an error when the run does not exist for this tenant or the write fails.
    pub async fn store(
        conn: &mut PgConnection,
        tenant_id: &str,
        state: &ProtocolState,
    ) -> Result<(), ProtocolStateError> {
        let row_id = format!("pst_{}", state.run_id.trim_start_matches("run_"));
        set_tenant_context_conn(conn, tenant_id).await?;
        sqlx::query(
            "INSERT INTO protocol_states (id, tenant_id, run_id, generation, pending_model_call, \
             pending_tool_calls, pending_approvals, open_questions, waits, browser_control, \
             terminal_sessions, child_agent_threads, cancellation_requested, cancellation_at, \
             last_compaction_epoch_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14::timestamptz, $15) \
             ON CONFLICT (run_id) DO UPDATE SET generation = EXCLUDED.generation, \
             pending_model_call = EXCLUDED.pending_model_call, \
             pending_tool_calls = EXCLUDED.pending_tool_calls, \
             pending_approvals = EXCLUDED.pending_approvals, \
             open_questions = EXCLUDED.open_questions, waits = EXCLUDED.waits, \
             browser_control = EXCLUDED.browser_control, \
             terminal_sessions = EXCLUDED.terminal_sessions, \
             child_agent_threads = EXCLUDED.child_agent_threads, \
             cancellation_requested = EXCLUDED.cancellation_requested, \
             cancellation_at = EXCLUDED.cancellation_at, \
             last_compaction_epoch_id = EXCLUDED.last_compaction_epoch_id",
        )
        .bind(row_id)
        .bind(tenant_id)
        .bind(&state.run_id)
        .bind(state.generation)
        .bind(json(&state.pending_model_call)?)
        .bind(json(&state.pending_tool_calls)?)
        .bind(json(&state.pending_approvals)?)
        .bind(json(&state.open_questions)?)
        .bind(json(&state.waits)?)
        .bind(json(&state.browser_control)?)
        .bind(json(&state.terminal_sessions)?)
        .bind(json(&state.child_agent_threads)?)
        .bind(state.cancellation_requested)
        .bind(&state.cancellation_at)
        .bind(&state.last_compaction_epoch_id)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// Load the state and immediately decide the next safe action (crash recovery entry).
    ///
    /// # Errors
    /// Returns [`ProtocolStateError::NotFound`] when the run has no durable state.
    pub async fn resume(
        conn: &mut PgConnection,
        tenant_id: &str,
        run_id: &str,
    ) -> Result<NextAction, ProtocolStateError> {
        let state = Self::load(conn, tenant_id, run_id)
            .await?
            .ok_or_else(|| ProtocolStateError::NotFound(run_id.to_string()))?;
        Ok(next_safe_action(&state))
    }
}

fn json<T: Serialize>(value: &T) -> Result<Option<serde_json::Value>, ProtocolStateError> {
    serde_json::to_value(value)
        .map(Some)
        .map_err(|error| ProtocolStateError::Decode(error.to_string()))
}

fn decode_json<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, ProtocolStateError> {
    serde_json::from_value(value).map_err(|error| ProtocolStateError::Decode(error.to_string()))
}

fn column(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<serde_json::Value, ProtocolStateError> {
    row.try_get::<serde_json::Value, _>(name)
        .map_err(|error| ProtocolStateError::Decode(error.to_string()))
}

fn optional_column(
    row: &sqlx::postgres::PgRow,
    name: &str,
) -> Result<Option<serde_json::Value>, ProtocolStateError> {
    let raw = row
        .try_get::<Option<serde_json::Value>, _>(name)
        .map_err(|error| ProtocolStateError::Decode(error.to_string()))?;
    Ok(raw.filter(|value| !value.is_null()))
}

fn decode_error(error: sqlx::Error) -> ProtocolStateError {
    ProtocolStateError::Decode(error.to_string())
}

fn decode_state(row: &sqlx::postgres::PgRow) -> Result<ProtocolState, ProtocolStateError> {
    Ok(ProtocolState {
        run_id: row.try_get("run_id").map_err(decode_error)?,
        generation: row.try_get("generation").map_err(decode_error)?,
        pending_model_call: optional_column(row, "pending_model_call")?
            .map(decode_json)
            .transpose()?,
        pending_tool_calls: decode_json(column(row, "pending_tool_calls")?)?,
        pending_approvals: decode_json(column(row, "pending_approvals")?)?,
        open_questions: decode_json(column(row, "open_questions")?)?,
        waits: decode_json(column(row, "waits")?)?,
        browser_control: optional_column(row, "browser_control")?
            .map(decode_json)
            .transpose()?,
        terminal_sessions: decode_json(column(row, "terminal_sessions")?)?,
        child_agent_threads: decode_json(column(row, "child_agent_threads")?)?,
        cancellation_requested: row
            .try_get("cancellation_requested")
            .map_err(decode_error)?,
        cancellation_at: row.try_get("cancellation_at").map_err(decode_error)?,
        last_compaction_epoch_id: row
            .try_get("last_compaction_epoch_id")
            .map_err(decode_error)?,
    })
}
