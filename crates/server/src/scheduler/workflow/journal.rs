//! The script journal (ADP-007, DOMAIN.md §13.8 F): append-only Evidence on the script run.
//!
//! Each entry is one insert-only `evidence` row (`kind: log`, captured by `{runtime, workflow}`) whose `inline_summary` is a JSON
//! object `{journal: 1, entry: <kind>, ...}`. The journal is not a store of its own: it is the evidence layer recovery already
//! reads (DOSSIER.md section 8), written through the evidence owner's own `EvidenceStore`, and the executor derives everything it
//! does from it and from the graph. Nothing here is ever updated or deleted.

use async_trait::async_trait;
use chrono::Utc;
use quansio_core::Digest;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};

use super::WorkflowError;
use crate::artifacts::{
    CapturedBy, EvidenceContent, EvidenceKind, EvidenceStore, NewEvidence, ObjectKey, ObjectStore,
    ObjectStoreError,
};
use crate::control::schema;

/// The journal's version, written into every entry.
pub const JOURNAL_VERSION: u32 = 1;
/// `captured_by.kind` of a journal entry.
pub const CAPTURED_BY_KIND: &str = "runtime";
/// `captured_by.id` of a journal entry.
pub const CAPTURED_BY_ID: &str = "workflow";

/// An object store that holds nothing: journal entries are digest-only evidence with their body inline, so the evidence owner
/// never reaches for bytes.
struct NoObjects;

#[async_trait]
impl ObjectStore for NoObjects {
    async fn put(&self, _: &ObjectKey, _: &[u8], _: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::Transport(
            "a journal entry has no object".to_string(),
        ))
    }
    async fn get(&self, key: &ObjectKey) -> Result<Vec<u8>, ObjectStoreError> {
        Err(ObjectStoreError::NotFound(key.as_str().to_string()))
    }
    async fn exists(&self, _: &ObjectKey) -> Result<bool, ObjectStoreError> {
        Ok(false)
    }
    async fn delete(&self, _: &ObjectKey) -> Result<(), ObjectStoreError> {
        Ok(())
    }
}

/// The entries of one script run, oldest first.
#[derive(Debug, Clone, Default)]
pub struct Journal {
    /// Every entry, as written.
    pub entries: Vec<Value>,
}

fn kind(entry: &Value) -> &str {
    entry.get("entry").and_then(Value::as_str).unwrap_or("")
}

fn text<'a>(entry: &'a Value, key: &str) -> Option<&'a str> {
    entry.get(key).and_then(Value::as_str)
}

impl Journal {
    /// The head: the script as compiled, its caps and its payload.
    #[must_use]
    pub fn head(&self) -> Option<&Value> {
        self.entries.iter().find(|entry| kind(entry) == "head")
    }

    /// The `end` entry, once the run finished.
    #[must_use]
    pub fn ended(&self) -> Option<&Value> {
        self.entries.iter().find(|entry| kind(entry) == "end")
    }

    /// Calls made so far: a call counts when `call_started` is written.
    #[must_use]
    pub fn calls_used(&self) -> u64 {
        self.entries
            .iter()
            .filter(|entry| kind(entry) == "call_started")
            .count() as u64
    }

    /// Whether the call named `key` has been started.
    #[must_use]
    pub fn started(&self, key: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| kind(entry) == "call_started" && text(entry, "key") == Some(key))
    }

    /// The `call_finished` entry of `key`.
    #[must_use]
    pub fn finished(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|entry| kind(entry) == "call_finished" && text(entry, "key") == Some(key))
    }

    /// The recorded result of `key`, when it finished `done`.
    #[must_use]
    pub fn result_of(&self, key: &str) -> Option<&Value> {
        self.finished(key)
            .filter(|entry| text(entry, "status") == Some("done"))
            .and_then(|entry| entry.get("result"))
    }

    /// The gate entry named `gate_key`.
    #[must_use]
    pub fn gate(&self, gate_key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|entry| kind(entry) == "gate" && text(entry, "gate_key") == Some(gate_key))
    }

    /// How a gate was decided, once it was: `granted` or `denied`.
    #[must_use]
    pub fn gate_decision(&self, gate_key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| {
                kind(entry) == "gate_resolved" && text(entry, "gate_key") == Some(gate_key)
            })
            .and_then(|entry| text(entry, "status"))
    }

    /// The gate that is waiting on a person: requested and not yet resolved.
    #[must_use]
    pub fn open_gate(&self) -> Option<&Value> {
        self.entries
            .iter()
            .filter(|entry| kind(entry) == "gate")
            .find(|entry| {
                text(entry, "gate_key").is_some_and(|key| self.gate_decision(key).is_none())
            })
    }

    /// The most calls a granted `fan_out` gate approved.
    #[must_use]
    pub fn approved_calls(&self) -> u64 {
        self.granted_calls("fan_out")
    }

    /// The call cap now in force: the script's own, or the most a granted `extend_budget` gate raised it to.
    #[must_use]
    pub fn max_calls(&self) -> u64 {
        let declared = self
            .head()
            .and_then(|head| head.get("script"))
            .and_then(|script| script.get("max_agent_calls"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        declared.max(self.granted_calls("extend_budget"))
    }

    fn granted_calls(&self, gate_kind: &str) -> u64 {
        self.entries
            .iter()
            .filter(|entry| kind(entry) == "gate" && text(entry, "gate") == Some(gate_kind))
            .filter(|entry| {
                text(entry, "gate_key")
                    .is_some_and(|key| self.gate_decision(key) == Some("granted"))
            })
            .filter_map(|entry| entry.get("calls").and_then(Value::as_u64))
            .max()
            .unwrap_or(0)
    }

    /// Whether the `parallel` step fanned out.
    #[must_use]
    pub fn fanned_out(&self, step: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| kind(entry) == "fanout" && text(entry, "step") == Some(step))
    }

    /// Whether the `parallel` step's items are all accounted for.
    #[must_use]
    pub fn group_done(&self, step: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| kind(entry) == "group_done" && text(entry, "step") == Some(step))
    }

    /// Whether a `log` step was written.
    #[must_use]
    pub fn logged(&self, step: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| kind(entry) == "log" && text(entry, "step") == Some(step))
    }
}

/// Reads and appends the journal of script runs of one tenant.
#[derive(Debug, Clone)]
pub struct JournalStore {
    pool: PgPool,
    tenant_id: String,
}

impl JournalStore {
    /// A store over the pool for one tenant.
    #[must_use]
    pub fn new(pool: PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            pool,
            tenant_id: tenant_id.into(),
        }
    }

    /// The journal of a script run.
    ///
    /// # Errors
    /// A database error, or an entry that is not JSON (nothing but this module writes them).
    pub async fn load(&self, run_id: &str) -> Result<Journal, WorkflowError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.tenant_id).await?;
        let rows = sqlx::query(
            "SELECT inline_summary FROM evidence WHERE tenant_id = $1 AND run_id = $2 \
             AND captured_by_kind = $3 AND captured_by_id = $4 ORDER BY captured_at, id",
        )
        .bind(&self.tenant_id)
        .bind(run_id)
        .bind(CAPTURED_BY_KIND)
        .bind(CAPTURED_BY_ID)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let body: Option<String> = row.try_get("inline_summary")?;
            let value: Value =
                serde_json::from_str(body.as_deref().unwrap_or("{}")).map_err(|error| {
                    WorkflowError::Journal(format!("an entry is not JSON: {error}"))
                })?;
            entries.push(value);
        }
        Ok(Journal { entries })
    }

    /// Append one entry. `entry` names its kind; the rest of `fields` is the entry's body.
    ///
    /// # Errors
    /// A database or evidence error; nothing is written.
    pub async fn append(
        &self,
        workspace_id: &str,
        run_id: &str,
        entry: &str,
        mut fields: Value,
    ) -> Result<(), WorkflowError> {
        let object = fields
            .as_object_mut()
            .ok_or_else(|| WorkflowError::Journal("an entry body is an object".to_string()))?;
        object.insert("journal".to_string(), json!(JOURNAL_VERSION));
        object.insert("entry".to_string(), json!(entry));
        object.insert("at".to_string(), json!(Utc::now().to_rfc3339()));
        let body = fields.to_string();
        let objects = NoObjects;
        let store = EvidenceStore::new(&objects);
        let mut tx = self.pool.begin().await?;
        store
            .record(
                &mut tx,
                &self.tenant_id,
                &NewEvidence {
                    workspace_id: workspace_id.to_string(),
                    run_id: Some(run_id.to_string()),
                    step_id: None,
                    effect_id: None,
                    kind: EvidenceKind::Log,
                    content: EvidenceContent::DigestOnly {
                        digest: Digest::of_canonical_json(&body),
                    },
                    inline_summary: Some(body),
                    captured_at: None,
                    captured_by: CapturedBy {
                        kind: CAPTURED_BY_KIND.to_string(),
                        id: CAPTURED_BY_ID.to_string(),
                    },
                    redaction_applied: false,
                },
            )
            .await
            .map_err(|error| WorkflowError::Journal(error.to_string()))?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(entries: Vec<Value>) -> Journal {
        Journal { entries }
    }

    #[test]
    fn a_journal_answers_what_was_called_what_it_returned_and_what_was_approved() {
        let journal = journal(vec![
            json!({ "entry": "head", "script": { "max_agent_calls": 10 } }),
            json!({ "entry": "call_started", "key": "collect" }),
            json!({ "entry": "call_finished", "key": "collect", "status": "done", "result": { "items": ["a"] } }),
            json!({ "entry": "call_started", "key": "each#0" }),
            json!({ "entry": "call_finished", "key": "each#0", "status": "failed", "error": { "code": "WORKFLOW_RESULT_INVALID" } }),
            json!({ "entry": "gate", "gate_key": "g1", "gate": "fan_out", "calls": 30 }),
            json!({ "entry": "gate_resolved", "gate_key": "g1", "status": "granted" }),
            json!({ "entry": "gate", "gate_key": "g2", "gate": "extend_budget", "calls": 40 }),
            json!({ "entry": "fanout", "step": "each", "count": 1 }),
        ]);
        assert_eq!(journal.calls_used(), 2);
        assert!(journal.started("collect") && !journal.started("summary"));
        assert_eq!(
            journal.result_of("collect"),
            Some(&json!({ "items": ["a"] }))
        );
        assert_eq!(
            journal.result_of("each#0"),
            None,
            "a failed call has no result"
        );
        assert_eq!(
            journal
                .finished("each#0")
                .and_then(|e| e["status"].as_str()),
            Some("failed")
        );
        assert_eq!(journal.approved_calls(), 30);
        assert_eq!(
            journal.max_calls(),
            10,
            "an extension that was not granted raises nothing"
        );
        assert_eq!(
            journal.open_gate().and_then(|g| g["gate_key"].as_str()),
            Some("g2")
        );
        assert!(journal.fanned_out("each") && !journal.fanned_out("other"));
        assert!(journal.ended().is_none());
    }
}
