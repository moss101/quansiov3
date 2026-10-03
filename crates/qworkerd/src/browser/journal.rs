//! The browser action journal: idempotency for side-effecting actions, and the in-doubt outcome
//! (ADP-001, DOMAIN.md §7.2, §7.4).
//!
//! The Effect Ledger owns what happened; this is the worker-side half of the same record, kept so a
//! redelivered action and a restarted worker both behave. It is deliberately not a ledger of its own:
//! it holds no policy, no approval and no outcome the server has not been told, and the runtime still
//! reconciles an unknown outcome through `EffectRecord` (`OUTCOME_UNKNOWN → RECONCILING`). What it adds
//! is the part only the worker can know — whether an input event was dispatched at all.
//!
//! A side-effecting action is keyed by `(call_id, action_id)` and carries a **canonical-argument
//! digest** computed from the arguments in their placeholder form, so a secret value can never reach it.
//!
//! * a key never seen is `Fresh`: the action runs, and a `start` line is made durable **before** the
//!   first input event is dispatched (refusals that happen earlier — a stale revision, a sensitive
//!   field — write nothing, because nothing could have happened);
//! * the same key with the same digest after a completion returns the stored result without touching the
//!   page (`Replay`);
//! * the same key with a different digest is a conflict ([`FaultCode::ActionReplayConflict`]): one
//!   action id is one action;
//! * a `start` with no completion — the worker was restarted, the connection was lost, the future was
//!   dropped — is **in doubt**. It is never run again. The result is explicit (`retryable = false`) and
//!   the model reconciles it with a fresh observation, because the click may have happened.
//!
//! The file is append-only JSON lines. A torn final line (a crash mid-write) is ignored: the `start`
//! before it was already durable, which is the line that matters.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::{json, Value};

use super::fault::{BrowserFault, FaultCode};

/// The identity of one side-effecting browser action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActionKey {
    /// The ToolCall the action belongs to (`tc_…`).
    pub call_id: String,
    /// The runtime-derived action id (the effect's idempotency key).
    pub action_id: String,
}

impl ActionKey {
    /// A key from its two parts.
    #[must_use]
    pub fn new(call_id: impl Into<String>, action_id: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            action_id: action_id.into(),
        }
    }
}

/// Where a journaled action stands.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionState {
    /// Dispatch began and no outcome was recorded in this process.
    Started,
    /// The action finished; the stored result is what a replay returns.
    Completed(Value),
    /// The outcome could not be established. Never retried.
    InDoubt {
        /// Why it is in doubt, in words safe to show a model.
        reason: String,
    },
    /// The model observed the page again and the runtime reconciled the doubt.
    Reconciled {
        /// What the observation established.
        outcome: String,
    },
}

#[derive(Debug, Clone)]
struct Entry {
    digest: String,
    state: ActionState,
}

/// What [`ActionJournal::begin`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Begin {
    /// Nothing is recorded for the key: run the action.
    Fresh,
    /// The key completed with this digest: return the stored result.
    Replay(Value),
    /// The key is in doubt: return the in-doubt result, run nothing.
    InDoubt {
        /// Why.
        reason: String,
    },
    /// The key was reconciled: the doubt is closed, and the action is not to run again either.
    Reconciled {
        /// What reconciliation established.
        outcome: String,
    },
}

/// The journal.
#[derive(Debug)]
pub struct ActionJournal {
    path: Option<PathBuf>,
    entries: Mutex<BTreeMap<ActionKey, Entry>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ActionJournal {
    /// A journal that lives only as long as the process.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// Open (or create) the journal at `path`, recovering what a previous process left.
    ///
    /// Every `start` without a terminal line becomes `InDoubt`, and that is written back, so the doubt
    /// survives a second restart rather than being re-derived.
    ///
    /// # Errors
    /// Returns [`BrowserFault`] when the file cannot be read or written.
    pub fn open(path: &Path) -> Result<Self, BrowserFault> {
        let io = |error: std::io::Error| {
            BrowserFault::new(
                FaultCode::PageFailure,
                format!("the action journal {} is unusable: {error}", path.display()),
            )
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let mut entries: BTreeMap<ActionKey, Entry> = BTreeMap::new();
        if path.exists() {
            let text = std::fs::read_to_string(path).map_err(io)?;
            for line in text.lines() {
                // A torn final line is a crash mid-write: the `start` before it is what mattered.
                let Ok(record) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                apply(&mut entries, &record);
            }
        }
        let journal = Self {
            path: Some(path.to_path_buf()),
            entries: Mutex::new(entries),
        };
        let doubtful: Vec<ActionKey> = lock(&journal.entries)
            .iter()
            .filter(|(_, entry)| matches!(entry.state, ActionState::Started))
            .map(|(key, _)| key.clone())
            .collect();
        for key in doubtful {
            journal.mark_in_doubt(
                &key,
                "the worker restarted before the outcome of this action was recorded",
            )?;
        }
        Ok(journal)
    }

    /// Decide what to do with an action. Nothing is written for a fresh key: the caller first makes the
    /// refusals it can make without touching the page, then calls [`Self::start`].
    ///
    /// # Errors
    /// Returns [`FaultCode::ActionReplayConflict`] when the key was seen with a different digest, and
    /// [`FaultCode::PageFailure`] when an interrupted earlier attempt's doubt cannot be recorded.
    pub fn check(&self, key: &ActionKey, digest: &str) -> Result<Begin, BrowserFault> {
        let mut entries = lock(&self.entries);
        if let Some(entry) = entries.get(key) {
            if entry.digest != digest {
                return Err(BrowserFault::new(
                    FaultCode::ActionReplayConflict,
                    format!(
                        "action {} of call {} was already used with different arguments",
                        key.action_id, key.call_id
                    ),
                )
                .with_details(json!({
                    "call_id": key.call_id,
                    "action_id": key.action_id,
                    "recorded_digest": entry.digest,
                    "presented_digest": digest,
                })));
            }
            let state = entry.state.clone();
            return Ok(match state {
                ActionState::Completed(result) => Begin::Replay(result),
                ActionState::InDoubt { reason } => Begin::InDoubt { reason },
                ActionState::Reconciled { outcome } => Begin::Reconciled { outcome },
                // A start with no outcome while this process is alive means the earlier attempt's
                // future was dropped. It may have dispatched, so it is doubt, not a fresh action.
                ActionState::Started => {
                    let reason = "an earlier attempt of this action was interrupted before its outcome was recorded";
                    let entry = Entry {
                        digest: digest.to_string(),
                        state: ActionState::InDoubt {
                            reason: reason.to_string(),
                        },
                    };
                    self.append(&json!({
                        "op": "in_doubt",
                        "call_id": key.call_id,
                        "action_id": key.action_id,
                        "reason": reason,
                    }))?;
                    entries.insert(key.clone(), entry);
                    Begin::InDoubt {
                        reason: reason.to_string(),
                    }
                }
            });
        }
        Ok(Begin::Fresh)
    }

    /// Make the start of an action durable. Called after every refusal that happens before the page is
    /// touched, and immediately before the first input event: from this line on, an action with no
    /// recorded outcome is in doubt.
    ///
    /// # Errors
    /// Returns [`FaultCode::PageFailure`] when the line cannot be made durable — an action whose start
    /// is not recorded must not run.
    pub fn start(&self, key: &ActionKey, digest: &str) -> Result<(), BrowserFault> {
        let mut entries = lock(&self.entries);
        self.append(&json!({
            "op": "start",
            "call_id": key.call_id,
            "action_id": key.action_id,
            "digest": digest,
        }))?;
        entries.insert(
            key.clone(),
            Entry {
                digest: digest.to_string(),
                state: ActionState::Started,
            },
        );
        Ok(())
    }

    /// Record that the action finished, with the result a replay returns.
    ///
    /// # Errors
    /// Returns [`FaultCode::PageFailure`] when the line cannot be written.
    pub fn complete(&self, key: &ActionKey, result: &Value) -> Result<(), BrowserFault> {
        self.append(&json!({
            "op": "complete",
            "call_id": key.call_id,
            "action_id": key.action_id,
            "result": result,
        }))?;
        if let Some(entry) = lock(&self.entries).get_mut(key) {
            entry.state = ActionState::Completed(result.clone());
        }
        Ok(())
    }

    /// Record that the outcome could not be established.
    ///
    /// # Errors
    /// Returns [`FaultCode::PageFailure`] when the line cannot be written.
    pub fn mark_in_doubt(&self, key: &ActionKey, reason: &str) -> Result<(), BrowserFault> {
        self.append(&json!({
            "op": "in_doubt",
            "call_id": key.call_id,
            "action_id": key.action_id,
            "reason": reason,
        }))?;
        if let Some(entry) = lock(&self.entries).get_mut(key) {
            entry.state = ActionState::InDoubt {
                reason: reason.to_string(),
            };
        }
        Ok(())
    }

    /// Close an in-doubt action after the runtime reconciled it from a fresh observation.
    ///
    /// The key stays spent: a reconciled action is never re-dispatched under the same id. Repeating the
    /// work is a new action with a new id, decided by the model from what it saw.
    ///
    /// # Errors
    /// Returns [`FaultCode::InvalidRequest`] when the action is not in doubt, and
    /// [`FaultCode::PageFailure`] when the line cannot be written.
    pub fn reconcile(&self, key: &ActionKey, outcome: &str) -> Result<(), BrowserFault> {
        {
            let entries = lock(&self.entries);
            match entries.get(key).map(|entry| &entry.state) {
                Some(ActionState::InDoubt { .. }) => {}
                _ => {
                    return Err(BrowserFault::new(
                        FaultCode::InvalidRequest,
                        "only an action that is in doubt can be reconciled",
                    ))
                }
            }
        }
        self.append(&json!({
            "op": "reconciled",
            "call_id": key.call_id,
            "action_id": key.action_id,
            "outcome": outcome,
        }))?;
        if let Some(entry) = lock(&self.entries).get_mut(key) {
            entry.state = ActionState::Reconciled {
                outcome: outcome.to_string(),
            };
        }
        Ok(())
    }

    /// The state of one action, if the journal has seen it.
    #[must_use]
    pub fn state(&self, key: &ActionKey) -> Option<ActionState> {
        lock(&self.entries)
            .get(key)
            .map(|entry| entry.state.clone())
    }

    /// Every action in doubt, for the surface that shows them.
    #[must_use]
    pub fn in_doubt(&self) -> Vec<(ActionKey, String)> {
        lock(&self.entries)
            .iter()
            .filter_map(|(key, entry)| match &entry.state {
                ActionState::InDoubt { reason } => Some((key.clone(), reason.clone())),
                _ => None,
            })
            .collect()
    }

    fn append(&self, record: &Value) -> Result<(), BrowserFault> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let io = |error: std::io::Error| {
            BrowserFault::new(
                FaultCode::PageFailure,
                format!("the action journal could not be written: {error}"),
            )
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(io)?;
        let mut line = record.to_string();
        line.push('\n');
        file.write_all(line.as_bytes()).map_err(io)?;
        // Durable before the action is dispatched: a `start` that is only in the page cache is a doubt
        // nobody can recover.
        file.sync_data().map_err(io)
    }
}

fn apply(entries: &mut BTreeMap<ActionKey, Entry>, record: &Value) {
    let text = |field: &str| record.get(field).and_then(Value::as_str);
    let (Some(op), Some(call_id), Some(action_id)) =
        (text("op"), text("call_id"), text("action_id"))
    else {
        return;
    };
    let key = ActionKey::new(call_id, action_id);
    match op {
        "start" => {
            if let Some(digest) = text("digest") {
                entries.insert(
                    key,
                    Entry {
                        digest: digest.to_string(),
                        state: ActionState::Started,
                    },
                );
            }
        }
        "complete" => {
            if let Some(entry) = entries.get_mut(&key) {
                entry.state =
                    ActionState::Completed(record.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        "in_doubt" => {
            if let Some(entry) = entries.get_mut(&key) {
                entry.state = ActionState::InDoubt {
                    reason: text("reason").unwrap_or("outcome unknown").to_string(),
                };
            }
        }
        "reconciled" => {
            if let Some(entry) = entries.get_mut(&key) {
                entry.state = ActionState::Reconciled {
                    outcome: text("outcome").unwrap_or("reconciled").to_string(),
                };
            }
        }
        _ => {}
    }
}
