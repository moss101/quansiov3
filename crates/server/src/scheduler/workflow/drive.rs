//! The executor (ADP-007, DOMAIN.md §13.8 F to H): one pass over one script run.
//!
//! Everything a pass does is derived from the graph and the journal, and every external step is idempotent by the call's key, so
//! a pass can start anywhere. The order inside a call is the order a restart needs: `call_started` is written before the worker
//! exists (so a call is counted once however often it is resumed), the worker's thread, message and Run are looked up by the
//! call's node before they are created, `call_finished` is written before the node moves, and a node that lags its journal is
//! brought up to it on the next pass.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt;
use quansio_core::{CanonicalId, CorrelationId, Digest, Prefix, UlidGenerator};
use quansio_events::{Actor, EventDraft, EventStore, EventType};
use quansio_graph::WorkNodeStatus;
use quansio_tools::schema::validate_instance;
use serde_json::{json, Value};
use sqlx::{Postgres, Row, Transaction};

use super::gate::{self, GateOutcome};
use super::graph::{NewNode, NodeRef, ScriptGraph};
use super::journal::{Journal, JournalStore};
use super::{AgentCall, CallProgress, ScriptRef, WorkflowError, WorkflowExecutor};
use crate::control::routines::ledger::next_version;
use crate::control::routines::script::{
    render, step_references, Effort, Piece, Step, Substitution, WorkflowScript,
};
use crate::control::schema;
use crate::runtime::state_machine::{RunStatus, RuntimeEngine, RuntimeIdentity};

/// How a pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveStatus {
    /// The run is waiting on a person or on a worker that is still in flight.
    Waiting,
    /// The run ended (this pass or earlier).
    Finished,
    /// Another instance is driving it right now.
    Busy,
    /// Not a script run.
    Idle,
}

/// What a pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveReport {
    /// How it ended.
    pub status: DriveStatus,
    /// Calls started in this pass.
    pub calls_started: u64,
    /// Calls finished in this pass.
    pub calls_finished: u64,
}

impl DriveReport {
    const fn new(status: DriveStatus) -> Self {
        Self {
            status,
            calls_started: 0,
            calls_finished: 0,
        }
    }
}

/// What a call came to.
enum CallOutcome {
    Pending,
    Done,
    Failed(Value),
}

/// Where a step goes next.
enum Flow {
    Next,
    Wait,
    Finish(Ending),
}

enum Ending {
    Succeeded,
    Failed(Value),
    Cancelled(String),
}

/// Whether the call budget allows going on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capacity {
    Cleared,
    Waiting,
    Denied,
    Exceeds { needed: u64, ceiling: u64 },
}

impl WorkflowExecutor {
    /// Start a script run: write its journal head, plan its static nodes, and move its Objective in progress. Idempotent.
    ///
    /// # Errors
    /// A [`WorkflowError`] for a refusal of the journal, the planner or the graph; the fire retries it.
    pub async fn begin(
        &self,
        script: &ScriptRef,
        definition: &WorkflowScript,
        payload: Option<&Value>,
    ) -> Result<(), WorkflowError> {
        let journals = JournalStore::new(self.pool.clone(), script.tenant_id.clone());
        let journal = journals.load(&script.run_id).await?;
        if journal.head().is_none() {
            let summary = definition
                .validate(&self.config.script)
                .map_err(|error| WorkflowError::Script(error.to_string()))?;
            journals
                .append(
                    &script.workspace_id,
                    &script.run_id,
                    "head",
                    json!({
                        "script": definition,
                        "script_ref": script.to_json(),
                        "payload": payload,
                        "payload_digest": payload.map(|payload| Digest::of_canonical_json(&payload.to_string()).to_string()),
                        "threshold": self.config.script.approval_threshold_calls,
                        "static_calls": summary.static_calls,
                    }),
                )
                .await?;
            self.emit(
                script,
                "routine.script_started",
                json!({
                    "routine_id": script.routine_id, "run_id": script.run_id, "fire_key": script.fire_key,
                    "owner_user_id": script.owner_user_id,
                    "max_agent_calls": definition.max_agent_calls,
                    "estimated_agent_calls": definition.estimated_agent_calls,
                }),
            )
            .await?;
        }
        let graph = ScriptGraph::new(self.pool.clone(), script.clone());
        ensure_static_nodes(&graph, script, definition).await?;
        graph
            .advance(&script.objective_id, WorkNodeStatus::InProgress)
            .await
    }

    async fn emit(
        &self,
        script: &ScriptRef,
        event_type: &str,
        payload: Value,
    ) -> Result<(), WorkflowError> {
        let events = EventStore::new(self.pool.clone());
        let tenant = script.tenant_id.clone();
        let (routine, workspace) = (script.routine_id.clone(), script.workspace_id.clone());
        let event_type = EventType::parse(event_type)
            .map_err(|error| WorkflowError::Journal(error.to_string()))?;
        events
            .commit_mutation(&script.tenant_id, move |conn, batch| {
                Box::pin(async move {
                    let version = next_version(conn, &tenant, &routine, batch).await?;
                    batch.emit(
                        EventDraft::new(
                            "routine",
                            routine,
                            version,
                            event_type,
                            CorrelationId::generate(&mut UlidGenerator::new()),
                            Actor::system("workflow"),
                        )
                        .with_workspace(workspace)
                        .with_payload(payload),
                    );
                    Ok(())
                })
            })
            .await
            .map_err(|error| WorkflowError::Journal(error.to_string()))
    }

    /// The advisory lock of one script run, held in an open transaction so it is released if the holder is dropped or dies.
    async fn lock(
        &self,
        tenant_id: &str,
        run_id: &str,
    ) -> Result<Option<Transaction<'static, Postgres>>, WorkflowError> {
        let mut tx = self.pool.begin().await?;
        let got: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!("workflow:{tenant_id}:{run_id}"))
                .fetch_one(&mut *tx)
                .await?;
        Ok(got.then_some(tx))
    }

    /// The script runs of a tenant that have no `end` in their journal, oldest first: running, waiting, and ended by someone
    /// else (cancelled or failed from outside) and not yet wound up.
    ///
    /// # Errors
    /// A database error.
    pub async fn active_runs(
        &self,
        tenant_id: &str,
        limit: i64,
    ) -> Result<Vec<String>, WorkflowError> {
        let mut tx = self.pool.begin().await?;
        schema::set_tenant_context(&mut tx, tenant_id).await?;
        let runs = sqlx::query_scalar(
            "SELECT r.id FROM runs r WHERE r.tenant_id = $1 AND r.trigger_kind = 'routine' \
               AND EXISTS (SELECT 1 FROM evidence e WHERE e.tenant_id = r.tenant_id AND e.run_id = r.id \
                           AND e.captured_by_kind = 'runtime' AND e.captured_by_id = 'workflow') \
               AND NOT EXISTS (SELECT 1 FROM evidence e WHERE e.tenant_id = r.tenant_id AND e.run_id = r.id \
                           AND e.captured_by_kind = 'runtime' AND e.captured_by_id = 'workflow' \
                           AND e.inline_summary LIKE '%\"entry\":\"end\"%') \
             ORDER BY r.created_at, r.id LIMIT $2",
        )
        .bind(tenant_id)
        .bind(limit)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(runs)
    }

    /// One pass over every unfinished script run of a tenant.
    ///
    /// # Errors
    /// A database error listing the runs; a run that fails is reported with its error and the others go on.
    pub async fn drive_tenant(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<(String, Result<DriveReport, WorkflowError>)>, WorkflowError> {
        let mut reports = Vec::new();
        for run_id in self.active_runs(tenant_id, 64).await? {
            let report = self.drive_run(tenant_id, &run_id).await;
            reports.push((run_id, report));
        }
        Ok(reports)
    }

    /// One pass over one script run.
    ///
    /// # Errors
    /// A [`WorkflowError`] for a refusal the next pass may not hit again; the run is left as it was.
    pub async fn drive_run(
        &self,
        tenant_id: &str,
        run_id: &str,
    ) -> Result<DriveReport, WorkflowError> {
        let Some(_lock) = self.lock(tenant_id, run_id).await? else {
            return Ok(DriveReport::new(DriveStatus::Busy));
        };
        let journals = JournalStore::new(self.pool.clone(), tenant_id);
        let journal = journals.load(run_id).await?;
        let Some(head) = journal.head().cloned() else {
            return Ok(DriveReport::new(DriveStatus::Idle));
        };
        let script =
            ScriptRef::from_json(head.get("script_ref").ok_or_else(|| {
                WorkflowError::Journal("the head has no script_ref".to_string())
            })?)?;
        let definition = WorkflowScript::parse(
            head.get("script")
                .ok_or_else(|| WorkflowError::Journal("the head has no script".to_string()))?,
        )
        .map_err(|error| WorkflowError::Script(error.to_string()))?;
        let identity = RuntimeIdentity::system(
            tenant_id,
            format!("workflow:{run_id}"),
            CorrelationId::generate(&mut UlidGenerator::new()),
        );
        let engine = RuntimeEngine::new(self.pool.clone(), identity)
            .map_err(|error| WorkflowError::Runtime(error.to_string()))?;
        let mut pass = Pass {
            ex: self,
            graph: ScriptGraph::new(self.pool.clone(), script.clone()),
            script,
            definition,
            payload: head
                .get("payload")
                .filter(|payload| !payload.is_null())
                .cloned(),
            journals,
            journal,
            engine,
            nodes: HashMap::new(),
            started: AtomicU64::new(0),
            finished: AtomicU64::new(0),
        };
        let status = pass.run().await?;
        Ok(DriveReport {
            status,
            calls_started: pass.started.load(Ordering::SeqCst),
            calls_finished: pass.finished.load(Ordering::SeqCst),
        })
    }

    /// What the projection shows of a script run, derived from its journal.
    ///
    /// # Errors
    /// A journal error.
    pub async fn overview(
        &self,
        tenant_id: &str,
        run_id: &str,
    ) -> Result<Option<Value>, WorkflowError> {
        let journal = JournalStore::new(self.pool.clone(), tenant_id)
            .load(run_id)
            .await?;
        Ok(overview_of(&journal))
    }
}

/// The projection of a journal (also used where there is no executor at hand).
#[must_use]
pub fn overview_of(journal: &Journal) -> Option<Value> {
    let head = journal.head()?;
    let definition = WorkflowScript::parse(head.get("script")?).ok()?;
    let status = match journal.ended() {
        Some(end) => end
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("finished")
            .to_string(),
        None if journal.open_gate().is_some() => "waiting".to_string(),
        None => "running".to_string(),
    };
    let steps: Vec<Value> = definition
        .steps
        .iter()
        .map(|step| {
            let id = step.id();
            let node_id = journal
                .entries
                .iter()
                .find(|entry| {
                    entry.get("entry").and_then(Value::as_str) == Some("call_started")
                        && entry.get("key").and_then(Value::as_str) == Some(id)
                })
                .and_then(|entry| entry.get("node_id").cloned())
                .unwrap_or(Value::Null);
            match step {
                Step::Log { .. } => json!({
                    "id": id, "kind": "log", "status": if journal.logged(id) { "done" } else { "pending" },
                }),
                Step::Parallel { .. } => {
                    let prefix = format!("{id}#");
                    let finished: Vec<&Value> = journal
                        .entries
                        .iter()
                        .filter(|entry| {
                            entry.get("entry").and_then(Value::as_str) == Some("call_finished")
                                && entry
                                    .get("key")
                                    .and_then(Value::as_str)
                                    .is_some_and(|key| key.starts_with(&prefix))
                        })
                        .collect();
                    let failed = finished
                        .iter()
                        .filter(|entry| entry.get("status").and_then(Value::as_str) == Some("failed"))
                        .count();
                    let total = journal
                        .entries
                        .iter()
                        .find(|entry| {
                            entry.get("entry").and_then(Value::as_str) == Some("fanout")
                                && entry.get("step").and_then(Value::as_str) == Some(id)
                        })
                        .and_then(|entry| entry.get("count").and_then(Value::as_u64));
                    json!({
                        "id": id, "kind": "parallel",
                        "status": if journal.group_done(id) { "done" } else if journal.fanned_out(id) { "running" } else { "pending" },
                        "items": { "total": total, "finished": finished.len(), "failed": failed },
                    })
                }
                _ => {
                    let state = match journal.finished(id).and_then(|entry| entry.get("status").and_then(Value::as_str)) {
                        Some("done") => "done",
                        Some(_) => "failed",
                        None if journal.started(id) => "running",
                        None => "pending",
                    };
                    json!({ "id": id, "kind": step.kind(), "status": state, "node_id": node_id })
                }
            }
        })
        .collect();
    Some(json!({
        "status": status,
        "calls_used": journal.calls_used(),
        "max_agent_calls": journal.max_calls(),
        "approved_calls": journal.approved_calls(),
        "estimated_agent_calls": definition.estimated_agent_calls,
        "open_gate": journal.open_gate().map(|gate| json!({
            "gate": gate.get("gate"), "calls": gate.get("calls"), "approval_id": gate.get("approval_id"),
        })),
        "error": journal.ended().and_then(|end| end.get("error").cloned()),
        "script_ref": head.get("script_ref"),
        "steps": steps,
    }))
}

/// Plan the nodes of the script's static steps and the edges between them. Idempotent.
async fn ensure_static_nodes(
    graph: &ScriptGraph,
    script: &ScriptRef,
    definition: &WorkflowScript,
) -> Result<HashMap<String, NodeRef>, WorkflowError> {
    let existing = graph.children(&script.objective_id).await?;
    let mut nodes = Vec::new();
    let mut edges: Vec<(String, String)> = Vec::new();
    for step in &definition.steps {
        let (kind, mut depends): (&'static str, Vec<String>) = match step {
            Step::Agent { prompt, .. } => ("agent", step_references(prompt)),
            Step::Parallel { over, agent, .. } => {
                let mut reads = step_references(&agent.prompt);
                reads.push(over.step.clone());
                ("parallel", reads)
            }
            Step::Reduce { of, prompt, .. } => {
                let mut reads = step_references(prompt);
                reads.push(of.clone());
                ("reduce", reads)
            }
            Step::Log { .. } => continue,
        };
        nodes.push(NewNode {
            wf_key: step.id().to_string(),
            kind: "task",
            title: format!("{} ({kind})", step.id()),
            parent_id: script.objective_id.clone(),
            step_kind: kind,
        });
        depends.sort();
        depends.dedup();
        edges.extend(depends.into_iter().map(|to| (step.id().to_string(), to)));
    }
    graph
        .plan(
            "static steps",
            &nodes,
            &edges,
            &existing,
            std::slice::from_ref(&script.objective_id),
        )
        .await
}

struct Pass<'a> {
    ex: &'a WorkflowExecutor,
    script: ScriptRef,
    definition: WorkflowScript,
    payload: Option<Value>,
    journals: JournalStore,
    journal: Journal,
    graph: ScriptGraph,
    engine: RuntimeEngine,
    nodes: HashMap<String, NodeRef>,
    started: AtomicU64,
    finished: AtomicU64,
}

fn failure(code: &str, message: impl Into<String>) -> Value {
    json!({ "code": code, "message": message.into() })
}

/// A worker's reply as a JSON value: the whole text, or one fenced block.
fn parse_reply(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        // A fence that is opened is closed, and the closing fence ends the reply.
        let (inside, after) = rest
            .trim_start_matches(['\n', '\r'])
            .rsplit_once("```")
            .ok_or_else(|| "the reply opens a code fence and never closes it".to_string())?;
        if !after.trim().is_empty() {
            return Err("the reply has text after its code fence".to_string());
        }
        inside.trim()
    } else {
        trimmed
    };
    serde_json::from_str(body).map_err(|error| format!("the reply is not JSON: {error}"))
}

impl Pass<'_> {
    fn limits(&self) -> crate::control::routines::config::ScriptConfig {
        self.ex.config.script
    }

    async fn reload(&mut self) -> Result<(), WorkflowError> {
        self.journal = self.journals.load(&self.script.run_id).await?;
        Ok(())
    }

    async fn run(&mut self) -> Result<DriveStatus, WorkflowError> {
        // A script that already ended only needs its run brought to rest (a crash may have stopped between the two).
        if let Some(end) = self.journal.ended().cloned() {
            self.settle_run(
                end.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed"),
                end.get("error"),
            )
            .await?;
            return Ok(DriveStatus::Finished);
        }
        let run_id = CanonicalId::parse_typed(&self.script.run_id, Prefix::Run)
            .map_err(|error| WorkflowError::Runtime(error.to_string()))?;
        let run = self
            .engine
            .store()
            .load_run(&run_id)
            .await
            .map_err(|error| WorkflowError::Runtime(error.to_string()))?;
        if run.status.is_terminal() {
            // Cancelled (or failed) from outside: nothing more is called, and what is unfinished is cancelled.
            return self
                .finish(Ending::Cancelled(format!(
                    "the script run is {}",
                    run.status.as_db_str()
                )))
                .await;
        }
        self.nodes = ensure_static_nodes(&self.graph, &self.script, &self.definition).await?;
        if self.journal.calls_used() == 0 {
            let estimated = self.definition.estimated_agent_calls;
            match self.capacity(estimated, estimated).await? {
                Capacity::Cleared => {}
                Capacity::Waiting => return Ok(DriveStatus::Waiting),
                Capacity::Denied => {
                    return self
                        .finish(Ending::Cancelled("the approval was denied".to_string()))
                        .await
                }
                Capacity::Exceeds { needed, ceiling } => {
                    return self.finish(Ending::Failed(exceeds(needed, ceiling))).await
                }
            }
        }
        let steps = self.definition.steps.clone();
        for step in &steps {
            match self.step(step).await? {
                Flow::Next => {}
                Flow::Wait => return Ok(DriveStatus::Waiting),
                Flow::Finish(ending) => return self.finish(ending).await,
            }
        }
        self.finish(Ending::Succeeded).await
    }

    // ------------------------------------------------------------------ the call budget and its gates

    /// Whether `needed_total` calls (and the projection `projected` for the approval threshold) may go on. Takes the cap first and
    /// the threshold second; either may park the run on an approval.
    async fn capacity(
        &mut self,
        needed_total: u64,
        projected: u64,
    ) -> Result<Capacity, WorkflowError> {
        let config = self.limits();
        let max = self.journal.max_calls();
        if needed_total > max {
            let target = needed_total.min(config.max_agent_calls_ceiling);
            if target <= max {
                return Ok(Capacity::Exceeds {
                    needed: needed_total,
                    ceiling: config.max_agent_calls_ceiling,
                });
            }
            match self.gate("extend_budget", target).await? {
                GateOutcome::Cleared => {}
                GateOutcome::Waiting => return Ok(Capacity::Waiting),
                GateOutcome::Denied => return Ok(Capacity::Denied),
            }
        }
        let needed = projected.max(self.definition.estimated_agent_calls);
        if needed > config.approval_threshold_calls && needed > self.journal.approved_calls() {
            match self.gate("fan_out", needed).await? {
                GateOutcome::Cleared => {}
                GateOutcome::Waiting => return Ok(Capacity::Waiting),
                GateOutcome::Denied => return Ok(Capacity::Denied),
            }
        }
        Ok(Capacity::Cleared)
    }

    async fn gate(&mut self, kind: &str, calls: u64) -> Result<GateOutcome, WorkflowError> {
        let key = format!("{kind}:{calls}");
        let fresh = self.journal.gate(&key).is_none();
        let was_open = !fresh && self.journal.gate_decision(&key).is_none();
        let untrusted = self.payload.is_some();
        let (config, script) = (self.limits(), self.script.clone());
        let outcome = gate::ensure(
            &self.ex.pool,
            &self.journals,
            &mut self.journal,
            &script,
            config.gate_ttl_seconds,
            untrusted,
            kind,
            calls,
        )
        .await?;
        if outcome == GateOutcome::Waiting && fresh {
            let reason = if kind == "extend_budget" {
                format!(
                    "it reached its call budget of {} and needs approval for {calls}",
                    self.journal.max_calls()
                )
            } else {
                format!("it needs approval for up to {calls} agent calls")
            };
            self.ex
                .emit(
                    &self.script,
                    "routine.script_paused",
                    json!({
                        "routine_id": script.routine_id, "run_id": script.run_id, "owner_user_id": script.owner_user_id,
                        "reason": reason, "gate": kind, "calls": calls,
                        "calls_used": self.journal.calls_used(), "max_agent_calls": self.journal.max_calls(),
                    }),
                )
                .await?;
        }
        if outcome == GateOutcome::Cleared && was_open {
            self.ex
                .emit(
                    &self.script,
                    "routine.script_resumed",
                    json!({ "routine_id": script.routine_id, "run_id": script.run_id, "gate": kind, "calls": calls }),
                )
                .await?;
        }
        Ok(outcome)
    }

    // ------------------------------------------------------------------ steps

    async fn step(&mut self, step: &Step) -> Result<Flow, WorkflowError> {
        match step {
            Step::Log { id, message } => {
                if !self.journal.logged(id) {
                    self.journals
                        .append(
                            &self.script.workspace_id,
                            &self.script.run_id,
                            "log",
                            json!({ "step": id, "message": message }),
                        )
                        .await?;
                    self.reload().await?;
                }
                Ok(Flow::Next)
            }
            Step::Agent {
                id,
                prompt,
                output_schema,
                effort,
            } => {
                self.single_call(id, prompt, output_schema, *effort, None)
                    .await
            }
            Step::Reduce {
                id,
                of,
                prompt,
                output_schema,
                effort,
            } => {
                self.single_call(id, prompt, output_schema, *effort, Some(of))
                    .await
            }
            Step::Parallel { id, .. } => self.parallel(id, step).await,
        }
    }

    fn substitution(
        &self,
        name: &str,
        results: Option<&Value>,
        item: Option<(usize, &Value)>,
    ) -> Option<Substitution> {
        let data = |source: String, text: String| Some(Substitution { source, text });
        match name {
            "payload" => data(
                "inbound_hook".to_string(),
                self.payload
                    .as_ref()
                    .map_or_else(|| "null".to_string(), Value::to_string),
            ),
            "results" => results.map(|results| Substitution {
                source: "results".to_string(),
                text: results.to_string(),
            }),
            "index" => item.map(|(index, _)| Substitution {
                source: "item_index".to_string(),
                text: index.to_string(),
            }),
            "item" => item.map(|(_, value)| Substitution {
                source: "item".to_string(),
                text: match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                },
            }),
            other => {
                let id = other.strip_prefix("steps.")?;
                let result = self.journal.result_of(id)?;
                data(format!("step:{id}"), result.to_string())
            }
        }
    }

    fn blocks(&self, key: &str, pieces: &[Piece], output_schema: &Value) -> Vec<Value> {
        let mut blocks = vec![json!({
            "type": "system_notice",
            "text": format!("Workflow step {key} of \u{201c}{}\u{201d}.", self.script.title),
            "notice": { "workflow_call": key, "script_run": self.script.run_id, "routine_id": self.script.routine_id },
        })];
        blocks.extend(pieces.iter().map(Piece::to_block));
        blocks.push(json!({
            "type": "text",
            "text": format!(
                "\n\nReply with one JSON object and nothing else. It must satisfy this JSON Schema:\n{output_schema}"
            ),
        }));
        blocks
    }

    async fn single_call(
        &mut self,
        id: &str,
        prompt: &str,
        output_schema: &Value,
        effort: Effort,
        reduces: Option<&String>,
    ) -> Result<Flow, WorkflowError> {
        let node = self
            .nodes
            .get(id)
            .cloned()
            .ok_or_else(|| WorkflowError::Graph(format!("step {id} has no node")))?;
        if let Some(finished) = self.journal.finished(id) {
            let ok = finished.get("status").and_then(Value::as_str) == Some("done");
            self.graph
                .advance(
                    &node.id,
                    if ok {
                        WorkNodeStatus::Done
                    } else {
                        WorkNodeStatus::Failed
                    },
                )
                .await?;
            return Ok(if ok {
                Flow::Next
            } else {
                Flow::Finish(Ending::Failed(
                    finished
                        .get("error")
                        .cloned()
                        .unwrap_or_else(|| failure("CALL_FAILED", "the step failed")),
                ))
            });
        }
        let results = reduces.map(|of| self.collected_results(of));
        let pieces = match render(prompt, &|name| {
            self.substitution(name, results.as_ref(), None)
        }) {
            Ok(pieces) => pieces,
            Err(error) => {
                return Ok(Flow::Finish(Ending::Failed(failure(
                    "VALIDATION_SCHEMA",
                    error.to_string(),
                ))))
            }
        };
        if !self.journal.started(id) {
            match self.capacity(self.journal.calls_used() + 1, 0).await? {
                Capacity::Cleared => {}
                Capacity::Waiting => return Ok(Flow::Wait),
                Capacity::Denied => {
                    return Ok(Flow::Finish(Ending::Cancelled(
                        "the approval was denied".to_string(),
                    )))
                }
                Capacity::Exceeds { needed, ceiling } => {
                    return Ok(Flow::Finish(Ending::Failed(exceeds(needed, ceiling))))
                }
            }
        }
        let blocks = self.blocks(id, &pieces, output_schema);
        let outcome = self
            .execute_call(id, &node.id, blocks, effort, output_schema)
            .await?;
        self.reload().await?;
        Ok(match outcome {
            CallOutcome::Pending => Flow::Wait,
            CallOutcome::Done => Flow::Next,
            CallOutcome::Failed(error) => Flow::Finish(Ending::Failed(error)),
        })
    }

    /// The outcomes of a `parallel` step's items, in item order, failures included: what its reducer is given.
    fn collected_results(&self, parallel_id: &str) -> Value {
        let count = self
            .journal
            .entries
            .iter()
            .find(|entry| {
                entry.get("entry").and_then(Value::as_str) == Some("fanout")
                    && entry.get("step").and_then(Value::as_str) == Some(parallel_id)
            })
            .and_then(|entry| entry.get("count").and_then(Value::as_u64))
            .unwrap_or(0);
        Value::Array(
            (0..count)
                .map(|index| {
                    let key = format!("{parallel_id}#{index}");
                    match self.journal.finished(&key) {
                        Some(entry) if entry.get("status").and_then(Value::as_str) == Some("done") => json!({
                            "index": index, "status": "done", "result": entry.get("result"),
                        }),
                        Some(entry) => json!({
                            "index": index, "status": "failed", "error": entry.get("error"),
                        }),
                        None => json!({ "index": index, "status": "failed", "error": failure("CALL_MISSING", "no outcome was recorded") }),
                    }
                })
                .collect(),
        )
    }

    async fn parallel(&mut self, id: &str, step: &Step) -> Result<Flow, WorkflowError> {
        let Step::Parallel {
            over,
            max_items,
            agent,
            ..
        } = step
        else {
            return Ok(Flow::Next);
        };
        let group = self
            .nodes
            .get(id)
            .cloned()
            .ok_or_else(|| WorkflowError::Graph(format!("step {id} has no node")))?;
        if self.journal.group_done(id) {
            self.graph.advance(&group.id, WorkNodeStatus::Done).await?;
            return Ok(Flow::Next);
        }
        let Some(source) = self.journal.result_of(&over.step).cloned() else {
            return Ok(Flow::Finish(Ending::Failed(failure(
                "WORKFLOW_SOURCE_MISSING",
                format!("step {:?} has no result to fan out over", over.step),
            ))));
        };
        let Some(items) = source
            .pointer(&over.path)
            .and_then(Value::as_array)
            .cloned()
        else {
            return Ok(Flow::Finish(Ending::Failed(failure(
                "WORKFLOW_SOURCE_MISSING",
                format!(
                    "step {:?}'s result has no array at {:?}",
                    over.step, over.path
                ),
            ))));
        };
        if items.len() > *max_items {
            return Ok(Flow::Finish(Ending::Failed(failure(
                "VALIDATION_BOUNDS",
                format!("{} items are more than max_items {max_items}", items.len()),
            ))));
        }

        // The fan-out: the budget and the gate first, then the nodes.
        if !self.journal.fanned_out(id) {
            let remaining_static = self
                .definition
                .steps
                .iter()
                .filter(|other| {
                    matches!(other, Step::Agent { .. } | Step::Reduce { .. })
                        && !self.journal.started(other.id())
                })
                .count() as u64;
            let needed = self.journal.calls_used() + remaining_static + items.len() as u64;
            match self.capacity(needed, needed).await? {
                Capacity::Cleared => {}
                Capacity::Waiting => return Ok(Flow::Wait),
                Capacity::Denied => {
                    return Ok(Flow::Finish(Ending::Cancelled(
                        "the approval was denied".to_string(),
                    )))
                }
                Capacity::Exceeds { needed, ceiling } => {
                    return Ok(Flow::Finish(Ending::Failed(exceeds(needed, ceiling))))
                }
            }
            let existing = self.graph.children(&group.id).await?;
            let nodes: Vec<NewNode> = (0..items.len())
                .map(|index| NewNode {
                    wf_key: format!("{id}#{index}"),
                    kind: "subtask",
                    title: format!("{id} [{index}]"),
                    parent_id: group.id.clone(),
                    step_kind: "item",
                })
                .collect();
            self.graph
                .plan(
                    &format!("fan out {id}"),
                    &nodes,
                    &[],
                    &existing,
                    std::slice::from_ref(&group.id),
                )
                .await?;
            self.graph
                .advance(&group.id, WorkNodeStatus::InProgress)
                .await?;
            self.journals
                .append(
                    &self.script.workspace_id,
                    &self.script.run_id,
                    "fanout",
                    json!({ "step": id, "count": items.len() }),
                )
                .await?;
            self.reload().await?;
        }

        // The items: one worker each, a bounded number at a time. An item that is already finished is not called again.
        let children = self.graph.children(&group.id).await?;
        // Every round settles at least one item (or asks for the budget it lacks, once); a round count past that means the
        // journal and the graph disagree about what is finished, and the loop must end rather than spin.
        let mut rounds = 0usize;
        loop {
            rounds += 1;
            if rounds > items.len() + 4 {
                return Ok(Flow::Finish(Ending::Failed(failure(
                    "STALLED",
                    format!("step {id} made no progress over {} rounds", rounds - 1),
                ))));
            }
            let mut in_flight: Vec<(usize, String)> = Vec::new();
            let mut fresh: Vec<(usize, String)> = Vec::new();
            for index in 0..items.len() {
                let key = format!("{id}#{index}");
                let node = children
                    .get(&key)
                    .ok_or_else(|| WorkflowError::Graph(format!("item {key} has no node")))?;
                if let Some(finished) = self.journal.finished(&key) {
                    let ok = finished.get("status").and_then(Value::as_str) == Some("done");
                    self.graph
                        .advance(
                            &node.id,
                            if ok {
                                WorkNodeStatus::Done
                            } else {
                                WorkNodeStatus::Failed
                            },
                        )
                        .await?;
                } else if self.journal.started(&key) {
                    in_flight.push((index, node.id.clone()));
                } else {
                    fresh.push((index, node.id.clone()));
                }
            }
            if in_flight.is_empty() && fresh.is_empty() {
                break;
            }
            let room = usize::try_from(
                self.journal
                    .max_calls()
                    .saturating_sub(self.journal.calls_used()),
            )
            .unwrap_or(usize::MAX);
            let held_back = fresh.len().saturating_sub(room);
            fresh.truncate(room);
            let batch: Vec<(usize, String)> = in_flight.into_iter().chain(fresh).collect();
            if batch.is_empty() {
                // Every remaining item needs a call the budget does not have: ask for it.
                let needed = self.journal.calls_used() + held_back as u64;
                match self.capacity(needed, 0).await? {
                    Capacity::Cleared => continue,
                    Capacity::Waiting => return Ok(Flow::Wait),
                    Capacity::Denied => {
                        return Ok(Flow::Finish(Ending::Cancelled(
                            "the approval was denied".to_string(),
                        )))
                    }
                    Capacity::Exceeds { needed, ceiling } => {
                        return Ok(Flow::Finish(Ending::Failed(exceeds(needed, ceiling))))
                    }
                }
            }
            let this = &*self;
            let outcomes: Vec<Result<CallOutcome, WorkflowError>> =
                futures_util::stream::iter(batch)
                    .map(|(index, node_id)| {
                        let key = format!("{id}#{index}");
                        let pieces = render(&agent.prompt, &|name| {
                            this.substitution(name, None, Some((index, &items[index])))
                        });
                        async move {
                            let pieces =
                                pieces.map_err(|error| WorkflowError::Script(error.to_string()))?;
                            let blocks = this.blocks(&key, &pieces, &agent.output_schema);
                            this.execute_call(
                                &key,
                                &node_id,
                                blocks,
                                agent.effort,
                                &agent.output_schema,
                            )
                            .await
                        }
                    })
                    .buffer_unordered(this.limits().max_parallel_calls.max(1))
                    .collect()
                    .await;
            let mut pending = false;
            for outcome in outcomes {
                if matches!(outcome?, CallOutcome::Pending) {
                    pending = true;
                }
            }
            self.reload().await?;
            if pending {
                return Ok(Flow::Wait);
            }
        }
        self.graph.advance(&group.id, WorkNodeStatus::Done).await?;
        self.journals
            .append(
                &self.script.workspace_id,
                &self.script.run_id,
                "group_done",
                json!({ "step": id }),
            )
            .await?;
        self.reload().await?;
        Ok(Flow::Next)
    }

    // ------------------------------------------------------------------ one call

    /// Run one call: journal it as started (once), run the worker, check its reply against the schema, record the outcome, and
    /// move the node. Safe to resume at any point.
    async fn execute_call(
        &self,
        key: &str,
        node_id: &str,
        blocks: Vec<Value>,
        effort: Effort,
        output_schema: &Value,
    ) -> Result<CallOutcome, WorkflowError> {
        if !self.journal.started(key) {
            self.journals
                .append(
                    &self.script.workspace_id,
                    &self.script.run_id,
                    "call_started",
                    json!({ "key": key, "node_id": node_id }),
                )
                .await?;
            self.started.fetch_add(1, Ordering::SeqCst);
        }
        self.graph
            .advance(node_id, WorkNodeStatus::InProgress)
            .await?;
        let progress = self
            .ex
            .port
            .run_call(AgentCall {
                script: self.script.clone(),
                node_id: node_id.to_string(),
                key: key.to_string(),
                blocks,
                effort,
            })
            .await?;
        match progress {
            CallProgress::Pending { .. } => Ok(CallOutcome::Pending),
            CallProgress::Failed {
                run_id,
                code,
                message,
            } => {
                let error = failure(&code, message);
                self.record(key, node_id, run_id.as_deref(), Err(error.clone()))
                    .await?;
                Ok(CallOutcome::Failed(error))
            }
            CallProgress::Replied { run_id, text } => match self.check(&text, output_schema, key) {
                Ok(result) => {
                    self.record(key, node_id, Some(&run_id), Ok(result)).await?;
                    Ok(CallOutcome::Done)
                }
                Err(error) => {
                    self.record(key, node_id, Some(&run_id), Err(error.clone()))
                        .await?;
                    Ok(CallOutcome::Failed(error))
                }
            },
        }
    }

    /// A reply against its contract: JSON, within the size bound, satisfying the step's schema. A failure is typed and carries
    /// every violation; nothing outside the schema survives.
    fn check(&self, text: &str, output_schema: &Value, key: &str) -> Result<Value, Value> {
        let invalid = |message: String, violations: Vec<Value>| json!({ "code": "WORKFLOW_RESULT_INVALID", "message": message, "details": { "step": key, "violations": violations } });
        if text.len() > self.limits().max_result_bytes {
            return Err(invalid(
                format!("the reply is over {} bytes", self.limits().max_result_bytes),
                Vec::new(),
            ));
        }
        let value = parse_reply(text).map_err(|message| invalid(message, Vec::new()))?;
        let violations = validate_instance(output_schema, &value);
        if violations.is_empty() {
            Ok(value)
        } else {
            Err(invalid(
                "the reply does not satisfy the step's output schema".to_string(),
                violations
                    .iter()
                    .map(|violation| json!({ "path": violation.path, "detail": violation.detail }))
                    .collect(),
            ))
        }
    }

    async fn record(
        &self,
        key: &str,
        node_id: &str,
        run_id: Option<&str>,
        outcome: Result<Value, Value>,
    ) -> Result<(), WorkflowError> {
        let entry = match &outcome {
            Ok(result) => json!({
                "key": key, "node_id": node_id, "run_id": run_id, "status": "done",
                "result": result, "result_digest": Digest::of_canonical_json(&result.to_string()).to_string(),
            }),
            Err(error) => json!({
                "key": key, "node_id": node_id, "run_id": run_id, "status": "failed", "error": error,
            }),
        };
        // The journal is the record; the node follows it, and a node that lags is brought up on the next pass.
        self.journals
            .append(
                &self.script.workspace_id,
                &self.script.run_id,
                "call_finished",
                entry,
            )
            .await?;
        self.finished.fetch_add(1, Ordering::SeqCst);
        self.graph
            .advance(
                node_id,
                if outcome.is_ok() {
                    WorkNodeStatus::Done
                } else {
                    WorkNodeStatus::Failed
                },
            )
            .await
    }

    // ------------------------------------------------------------------ the end

    async fn finish(&mut self, ending: Ending) -> Result<DriveStatus, WorkflowError> {
        let (status, error) = match &ending {
            Ending::Succeeded => ("succeeded", None),
            Ending::Failed(error) => ("failed", Some(error.clone())),
            Ending::Cancelled(reason) => ("cancelled", Some(failure("CANCELLED", reason.clone()))),
        };
        if !matches!(ending, Ending::Succeeded) {
            self.cancel_unfinished().await?;
        }
        // The Objective follows the outcome; the script run is brought to rest after it.
        let goal = match ending {
            Ending::Succeeded => WorkNodeStatus::Done,
            Ending::Failed(_) => WorkNodeStatus::Failed,
            Ending::Cancelled(_) => WorkNodeStatus::Cancelled,
        };
        self.graph.advance(&self.script.objective_id, goal).await?;
        if !self.settle_run(status, error.as_ref()).await? {
            // The run is parked (a granted approval is still being applied): the end waits one pass.
            return Ok(DriveStatus::Waiting);
        }
        let script = self.script.clone();
        let event = if status == "succeeded" {
            "routine.script_completed"
        } else {
            "routine.script_failed"
        };
        self.ex
            .emit(
                &script,
                event,
                json!({
                    "routine_id": script.routine_id, "run_id": script.run_id, "owner_user_id": script.owner_user_id,
                    "status": status, "calls_used": self.journal.calls_used(), "error": error,
                }),
            )
            .await?;
        self.journals
            .append(
                &script.workspace_id,
                &script.run_id,
                "end",
                json!({ "status": status, "calls_used": self.journal.calls_used(), "error": error }),
            )
            .await?;
        self.reload().await?;
        Ok(DriveStatus::Finished)
    }

    /// Bring the script run to the state its end names. Returns whether it is at rest.
    async fn settle_run(&self, status: &str, error: Option<&Value>) -> Result<bool, WorkflowError> {
        let runtime = |error: crate::runtime::state_machine::RuntimeError| {
            WorkflowError::Runtime(error.to_string())
        };
        let run_id = CanonicalId::parse_typed(&self.script.run_id, Prefix::Run)
            .map_err(|error| WorkflowError::Runtime(error.to_string()))?;
        let run = self
            .engine
            .store()
            .load_run(&run_id)
            .await
            .map_err(runtime)?;
        if run.status.is_terminal() {
            return Ok(true);
        }
        match status {
            "succeeded" => {
                if run.status != RunStatus::Running {
                    return Ok(false);
                }
                self.engine
                    .transition_run(&run_id, run.generation, RunStatus::Verifying, None)
                    .await
                    .map_err(runtime)?;
                self.engine
                    .transition_run(&run_id, run.generation, RunStatus::Succeeded, None)
                    .await
                    .map_err(runtime)?;
            }
            "cancelled" => {
                self.engine
                    .cancel(&run_id, run.generation)
                    .await
                    .map_err(runtime)?;
            }
            _ => {
                let reason = error
                    .and_then(|error| error.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("WORKFLOW_FAILED")
                    .to_string();
                self.engine
                    .transition_run(&run_id, run.generation, RunStatus::Failed, Some(reason))
                    .await
                    .map_err(runtime)?;
            }
        }
        Ok(true)
    }

    /// On a failure or a cancellation: the nodes that did not finish are cancelled and their live worker Runs stopped.
    async fn cancel_unfinished(&self) -> Result<(), WorkflowError> {
        let mut parents = vec![self.script.objective_id.clone()];
        let mut nodes: Vec<NodeRef> = Vec::new();
        let mut index = 0;
        while index < parents.len() {
            let children = self.graph.children(&parents[index]).await?;
            for node in children.into_values() {
                parents.push(node.id.clone());
                nodes.push(node);
            }
            index += 1;
        }
        for node in nodes {
            self.stop_workers(&node.id).await?;
            self.graph
                .advance(&node.id, WorkNodeStatus::Cancelled)
                .await?;
        }
        Ok(())
    }

    async fn stop_workers(&self, node_id: &str) -> Result<(), WorkflowError> {
        let mut tx = self.ex.pool.begin().await?;
        schema::set_tenant_context(&mut tx, &self.script.tenant_id).await?;
        let rows = sqlx::query(
            "SELECT id, generation FROM runs WHERE tenant_id = $1 AND work_node_id = $2 \
             AND status NOT IN ('SUCCEEDED', 'FAILED', 'CANCELLED', 'BLOCKED_UNRECOVERABLE')",
        )
        .bind(&self.script.tenant_id)
        .bind(node_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        for row in rows {
            let id: String = row.try_get("id")?;
            let generation: i64 = row.try_get("generation")?;
            let (Ok(run_id), Ok(generation)) = (
                CanonicalId::parse_typed(&id, Prefix::Run),
                quansio_core::Generation::new(u64::try_from(generation).unwrap_or(1)),
            ) else {
                continue;
            };
            // A run another path already ended is not an error here.
            let _ = self.engine.cancel(&run_id, generation).await;
        }
        Ok(())
    }
}

fn exceeds(needed: u64, ceiling: u64) -> Value {
    failure(
        "BUDGET_EXHAUSTED",
        format!("the script would need {needed} agent calls, above the ceiling of {ceiling} that an approval may raise it to"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_one_json_value_bare_or_in_one_fence_and_nothing_else() {
        assert_eq!(parse_reply(r#"{"a":1}"#), Ok(json!({ "a": 1 })));
        assert_eq!(parse_reply("  \n{\"a\": 1}\n "), Ok(json!({ "a": 1 })));
        assert_eq!(
            parse_reply("```json\n{\"a\": 1}\n```"),
            Ok(json!({ "a": 1 }))
        );
        assert_eq!(parse_reply("```\n{\"a\": 1}\n```\n"), Ok(json!({ "a": 1 })));
        for bad in [
            "",
            "Sure! Here is the answer: {\"a\": 1}",
            "{\"a\": 1} and then some prose",
            "```json\n{\"a\": 1}",
            "{\"a\": 1}{\"b\": 2}",
            "not json at all",
        ] {
            assert!(parse_reply(bad).is_err(), "{bad:?} is not a reply");
        }
    }

    #[test]
    fn a_journal_is_shown_as_its_steps_with_their_states_and_the_budget() {
        let script = json!({
            "version": 1, "max_agent_calls": 9, "estimated_agent_calls": 4,
            "steps": [
                { "id": "list", "kind": "agent", "prompt": "list", "output_schema":
                    { "type": "object", "additionalProperties": false, "properties": { "items": { "type": "array", "maxItems": 3, "items": { "type": "string" } } } } },
                { "id": "each", "kind": "parallel", "over": { "step": "list", "path": "/items" }, "max_items": 3,
                  "agent": { "prompt": "{{item}}", "output_schema": { "type": "object", "additionalProperties": false, "properties": {} } } },
                { "id": "sum", "kind": "reduce", "of": "each", "prompt": "{{results}}", "output_schema": { "type": "object", "additionalProperties": false, "properties": {} } }
            ]
        });
        let journal = Journal {
            entries: vec![
                json!({ "entry": "head", "script": script, "script_ref": { "run_id": "run_x" } }),
                json!({ "entry": "call_started", "key": "list", "node_id": "wn_1" }),
                json!({ "entry": "call_finished", "key": "list", "status": "done", "result": { "items": ["a", "b"] } }),
                json!({ "entry": "fanout", "step": "each", "count": 2 }),
                json!({ "entry": "call_started", "key": "each#0", "node_id": "wn_2" }),
                json!({ "entry": "call_finished", "key": "each#0", "status": "failed", "error": { "code": "WORKFLOW_RESULT_INVALID" } }),
                json!({ "entry": "call_started", "key": "each#1", "node_id": "wn_3" }),
                json!({ "entry": "gate", "gate_key": "extend_budget:12", "gate": "extend_budget", "calls": 12, "approval_id": "apr_1" }),
            ],
        };
        let overview = overview_of(&journal).expect("overview");
        assert_eq!(overview["status"], "waiting");
        assert_eq!(overview["calls_used"], 3);
        assert_eq!(
            overview["max_agent_calls"], 9,
            "an extension nobody granted raises nothing"
        );
        assert_eq!(overview["open_gate"]["calls"], 12);
        assert_eq!(overview["steps"][0]["status"], "done");
        assert_eq!(overview["steps"][0]["node_id"], "wn_1");
        assert_eq!(overview["steps"][1]["status"], "running");
        assert_eq!(
            overview["steps"][1]["items"],
            json!({ "total": 2, "finished": 1, "failed": 1 })
        );
        assert_eq!(overview["steps"][2]["status"], "pending");
        assert!(
            overview_of(&Journal::default()).is_none(),
            "no head, not a script"
        );
    }
}
