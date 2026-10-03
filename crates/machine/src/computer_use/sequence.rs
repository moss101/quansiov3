//! A sequence of native computer actions and the conditions that end it (ADP-002, DOMAIN.md §8.6.1 B).
//!
//! One ToolCall may carry several actions. The runtime, not the model, walks them: it checks, before it
//! even asks for the next action, whether a person has stopped the run, taken the machine, or whether a
//! limit is spent, and the first of those it finds ends the sequence. An [`ActionFeed`] is the only way
//! actions get in, and once a sequence has ended it is never asked again, so whatever a model would have
//! proposed next is not merely refused: it is never read.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use sqlx::PgConnection;

use super::bridge::{
    ComputerDispatchError, ComputerDispatcher, NativeComputerAction, NativeComputerBridge,
};
use super::config::SequenceLimits;
use super::permission::{AttentionRequest, Permission, PermissionState};
use super::receipt::{ComputerActionResult, LedgerOutcome, ReceiptOutcome};
use super::store::{ComputerControlError, ComputerControlStore};
use super::MachineHolder;

/// Which limit a sequence passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// More actions than `max_actions`.
    Actions,
    /// Longer than `max_duration_ms`.
    Duration,
    /// More text than `max_typed_bytes`.
    TypedBytes,
}

impl LimitKind {
    /// Canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Actions => "actions",
            Self::Duration => "duration",
            Self::TypedBytes => "typed_bytes",
        }
    }
}

/// A condition that ends a sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalCondition {
    /// A person holds the computer, asked to, or held it since the sequence began.
    TakeoverActive,
    /// A permission the sequence had at its start is gone.
    PermissionRevoked {
        /// What the person is guided to; the run parks on it like a missing permission.
        attention: AttentionRequest,
    },
    /// A resource limit is spent.
    LimitExceeded {
        /// Which one.
        limit: LimitKind,
    },
    /// The person stopped the run.
    StoppedByUser,
}

impl TerminalCondition {
    /// The typed error code (DOMAIN.md §15).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TakeoverActive => "COMPUTER_TAKEOVER_ACTIVE",
            Self::PermissionRevoked { .. } => "COMPUTER_PERMISSION_REVOKED",
            Self::LimitExceeded { .. } => "COMPUTER_LIMIT_EXCEEDED",
            Self::StoppedByUser => "COMPUTER_STOPPED_BY_USER",
        }
    }

    /// Whether the same request may be sent again once [`Self::requires`] has happened.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::TakeoverActive | Self::PermissionRevoked { .. })
    }

    /// Whether the model must observe the machine again before acting on it.
    #[must_use]
    pub const fn requires_new_observation(&self) -> bool {
        true
    }

    /// What has to happen first.
    #[must_use]
    pub const fn requires(&self) -> &'static str {
        match self {
            Self::TakeoverActive => "handback",
            Self::PermissionRevoked { .. } => "permission_granted",
            Self::LimitExceeded { .. } => "smaller_sequence",
            Self::StoppedByUser => "user_resume",
        }
    }

    /// The Tool error a model is shown.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut error = json!({
            "code": self.code(),
            "retryable": self.retryable(),
            "requires_new_observation": self.requires_new_observation(),
            "requires": self.requires(),
        });
        match self {
            Self::LimitExceeded { limit } => {
                error["details"] = json!({ "limit": limit.as_str() });
            }
            Self::PermissionRevoked { attention } => {
                error["details"] = json!({ "attention": attention.to_json() });
            }
            Self::TakeoverActive | Self::StoppedByUser => {}
        }
        error
    }
}

/// A flag a person's stop (a `CancelRun`, a stop button) raises from anywhere.
#[derive(Debug, Clone, Default)]
pub struct StopHandle(Arc<AtomicBool>);

impl StopHandle {
    /// A flag that is not raised.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raise the flag. It cannot be lowered: a stopped sequence stays stopped.
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether the flag is raised.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Where a sequence's actions come from.
///
/// A runtime implements this over the model's proposal, one action at a time if it wants to show the
/// model each receipt before the next. The sequence calls it only while it is still running.
pub trait ActionFeed {
    /// The next action, given the actions settled so far; `None` ends the sequence normally.
    fn next_action(&mut self, settled: &[SettledAction]) -> Option<NativeComputerAction>;

    /// How many actions are still waiting, for reporting what was skipped.
    fn remaining(&self) -> usize;
}

/// A fixed list of actions.
#[derive(Debug, Clone)]
pub struct ListFeed(VecDeque<NativeComputerAction>);

impl ListFeed {
    /// A feed over `actions`, in order.
    #[must_use]
    pub fn new(actions: impl IntoIterator<Item = NativeComputerAction>) -> Self {
        Self(actions.into_iter().collect())
    }
}

impl ActionFeed for ListFeed {
    fn next_action(&mut self, _settled: &[SettledAction]) -> Option<NativeComputerAction> {
        self.0.pop_front()
    }

    fn remaining(&self) -> usize {
        self.0.len()
    }
}

/// One action that settled, with the result the bridge gave.
#[derive(Debug, Clone, PartialEq)]
pub struct SettledAction {
    /// The action.
    pub action: NativeComputerAction,
    /// Its output and re-derived receipt.
    pub result: ComputerActionResult,
}

/// How a sequence ended.
#[derive(Debug, Clone, PartialEq)]
pub enum SequenceEnd {
    /// The feed had no more actions.
    Completed,
    /// A terminal condition ended it.
    Terminal(TerminalCondition),
    /// A permission was missing at the first action, or the person's input took the foreground back:
    /// the run parks on the attention item.
    Parked(AttentionRequest),
    /// An action's receipt says it did not apply or broke a background invariant: the premise of the
    /// next action is gone, so the sequence stops. A result, not a terminal condition.
    Halted {
        /// The receipt outcome that stopped it.
        outcome: ReceiptOutcome,
    },
}

/// What a sequence did.
#[derive(Debug, Clone, PartialEq)]
pub struct SequenceReport {
    /// The actions that settled, in order.
    pub settled: Vec<SettledAction>,
    /// How many proposed actions were never attempted.
    pub skipped: usize,
    /// How it ended.
    pub end: SequenceEnd,
}

impl SequenceReport {
    /// How the Effect Ledger settles the ToolCall: the least certain of its actions.
    #[must_use]
    pub fn ledger_outcome(&self) -> LedgerOutcome {
        let mut worst = LedgerOutcome::SettledSuccess;
        let mut any = false;
        for settled in &self.settled {
            any = true;
            match settled.result.ledger_outcome() {
                LedgerOutcome::OutcomeUnknown => return LedgerOutcome::OutcomeUnknown,
                LedgerOutcome::SettledFailed => worst = LedgerOutcome::SettledFailed,
                LedgerOutcome::SettledSuccess => {}
            }
        }
        if any {
            worst
        } else {
            LedgerOutcome::SettledFailed
        }
    }

    /// The Tool result: every settled receipt, what was skipped and how it ended.
    ///
    /// # Errors
    /// Returns the receipt error when an output cannot carry its receipt.
    pub fn to_json(&self) -> Result<Value, super::receipt::ReceiptError> {
        let actions = self
            .settled
            .iter()
            .map(|settled| settled.result.tool_result())
            .collect::<Result<Vec<_>, _>>()?;
        let end = match &self.end {
            SequenceEnd::Completed => json!({ "status": "completed" }),
            SequenceEnd::Terminal(condition) => {
                json!({ "status": "terminal", "error": condition.to_json() })
            }
            SequenceEnd::Parked(attention) => {
                json!({ "status": "parked", "attention": attention.to_json() })
            }
            SequenceEnd::Halted { outcome } => {
                json!({ "status": "halted", "outcome": outcome.as_str() })
            }
        };
        Ok(json!({
            "actions": actions,
            "skipped": self.skipped,
            "end": end,
            "ledger_outcome": self.ledger_outcome().as_str(),
        }))
    }
}

/// Runs a sequence through a [`ComputerDispatcher`].
pub struct ComputerSequence<'a, B> {
    dispatcher: &'a ComputerDispatcher<B>,
    limits: SequenceLimits,
    stop: StopHandle,
}

impl<'a, B: NativeComputerBridge> ComputerSequence<'a, B> {
    /// A sequence under the dispatcher's configured limits.
    #[must_use]
    pub fn new(dispatcher: &'a ComputerDispatcher<B>, stop: StopHandle) -> Self {
        Self {
            limits: dispatcher.config().limits,
            dispatcher,
            stop,
        }
    }

    /// The same sequence under other limits (a run's budget may be tighter than the default).
    #[must_use]
    pub const fn with_limits(mut self, limits: SequenceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Run `feed` as the actions of one ToolCall.
    ///
    /// Before every action, and before the feed is asked for it, the sequence checks in order: a user
    /// stop; the durable control row (a holder other than the agent, a pending takeover or a changed
    /// generation is a takeover, even one already handed back, because what the model saw is stale);
    /// then, once the action is known, the limits; then the permissions it needs (the dispatcher's
    /// gate). The first thing found ends the sequence and the feed is never asked again.
    ///
    /// # Errors
    /// Returns a dispatch error that is not a terminal condition: a stale foreground, a tier refusal, a
    /// database failure, or an unknown outcome (the ToolCall stays active for reconciliation).
    pub async fn run<F: ActionFeed>(
        &self,
        conn: &mut PgConnection,
        tenant_id: &str,
        target_id: &str,
        tool_call_id: &str,
        generation: i64,
        mut feed: F,
    ) -> Result<SequenceReport, ComputerDispatchError> {
        let started = Instant::now();
        let mut settled: Vec<SettledAction> = Vec::new();
        let mut typed = 0_usize;
        // An action the feed gave that was not run: it counts as skipped with whatever is still waiting.
        let mut unrun = 0_usize;
        let end = loop {
            if self.stop.is_stopped() {
                break SequenceEnd::Terminal(TerminalCondition::StoppedByUser);
            }
            let control = ComputerControlStore::load(conn, tenant_id, target_id).await?;
            if control.holder != MachineHolder::Agent
                || control.takeover_pending
                || control.generation != generation
            {
                break SequenceEnd::Terminal(TerminalCondition::TakeoverActive);
            }
            let Some(action) = feed.next_action(&settled) else {
                break SequenceEnd::Completed;
            };
            if let Some(limit) = self.limit_passed(started, settled.len(), typed, &action) {
                unrun = 1;
                break SequenceEnd::Terminal(TerminalCondition::LimitExceeded { limit });
            }
            match self
                .dispatcher
                .execute(
                    conn,
                    tenant_id,
                    target_id,
                    tool_call_id,
                    generation,
                    &action,
                )
                .await
            {
                Ok(result) => {
                    typed += action.typed_bytes();
                    let outcome = result.receipt.outcome;
                    settled.push(SettledAction { action, result });
                    if matches!(
                        outcome,
                        ReceiptOutcome::NotApplied | ReceiptOutcome::InvariantViolated
                    ) {
                        break SequenceEnd::Halted { outcome };
                    }
                }
                Err(ComputerDispatchError::PermissionRequired(attention)) => {
                    // A permission the sequence already used is withdrawn: terminal. One missing at the
                    // first action, or a lapsed foreground grant, is the person's to resolve: parked.
                    let permission_gone = attention.permission.is_some()
                        && attention.status != Some(PermissionState::Restricted)
                        && !settled.is_empty();
                    unrun = 1;
                    break if permission_gone {
                        SequenceEnd::Terminal(TerminalCondition::PermissionRevoked { attention })
                    } else {
                        SequenceEnd::Parked(attention)
                    };
                }
                Err(ComputerDispatchError::Control(
                    ComputerControlError::Held(_)
                    | ComputerControlError::TakeoverPending
                    | ComputerControlError::StaleGeneration { .. },
                )) => {
                    // A takeover landed between the check above and the action's own begin.
                    unrun = 1;
                    break SequenceEnd::Terminal(TerminalCondition::TakeoverActive);
                }
                Err(error) => return Err(error),
            }
        };
        let skipped = feed.remaining() + unrun;
        Ok(SequenceReport {
            settled,
            skipped,
            end,
        })
    }

    fn limit_passed(
        &self,
        started: Instant,
        done: usize,
        typed: usize,
        next: &NativeComputerAction,
    ) -> Option<LimitKind> {
        if done >= self.limits.max_actions {
            Some(LimitKind::Actions)
        } else if started.elapsed() >= self.limits.max_duration {
            Some(LimitKind::Duration)
        } else if typed.saturating_add(next.typed_bytes()) > self.limits.max_typed_bytes {
            Some(LimitKind::TypedBytes)
        } else {
            None
        }
    }
}

/// The permission a revoked-permission terminal names, for callers that only have the condition.
#[must_use]
pub const fn revoked_permission(condition: &TerminalCondition) -> Option<Permission> {
    match condition {
        TerminalCondition::PermissionRevoked { attention } => attention.permission,
        _ => None,
    }
}
