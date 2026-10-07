//! Event triggers: `trigger {kind: event, spec}` (DOMAIN.md §13.1).
//!
//! `spec` names an event type (`family.name`) or a family (`family.*`). The consumer reads
//! the tenant's RuntimeEvents after its `event_cursors` position and, for every matching
//! event in a routine's workspace committed after the routine was created, claims the fire
//! `routine:<id>:event:<event id>` — in the transaction that advances the cursor. A
//! replayed batch, a restart, or two instances scanning at once therefore claim one fire
//! per event (the ledger's key is unique too).
//!
//! A routine never triggers itself: events of the `routine` and `notification` families,
//! events the scheduler produced, and events of any routine's own thread or runs are not
//! matched.

use std::collections::HashSet;

use quansio_events::{advance_cursor_in_tx, EventError, EventStore, RuntimeEvent, EVENT_FAMILIES};
use serde_json::Value;
use sqlx::Row;

use super::ledger::{claim_in, FireSource};
use super::store::RoutineError;

/// The cursor stream the consumer advances.
pub const TRIGGER_STREAM: &str = "routines:events";
/// The consumer name.
pub const TRIGGER_CONSUMER: &str = "routine-event-triggers";

/// Refuse a spec that names no event family, or a family a routine may not listen to.
///
/// # Errors
/// Returns [`RoutineError::Validation`].
pub fn validate_event_spec(spec: &str) -> Result<(), RoutineError> {
    let invalid = |why: &str| RoutineError::Validation(format!("event trigger {spec:?}: {why}"));
    let (family, name) = spec
        .split_once('.')
        .ok_or_else(|| invalid("spec is family.name or family.*"))?;
    if !EVENT_FAMILIES.contains(&family) {
        return Err(invalid("not an event family"));
    }
    if matches!(family, "routine" | "notification") {
        return Err(invalid(
            "a routine cannot listen to routine or notification events",
        ));
    }
    let name_ok = name == "*"
        || (!name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
    if !name_ok {
        return Err(invalid("the event name is snake_case or *"));
    }
    Ok(())
}

/// Whether an event type matches a spec.
#[must_use]
pub fn event_matches(spec: &str, event_type: &str) -> bool {
    match spec.strip_suffix(".*") {
        Some(family) => event_type
            .split_once('.')
            .is_some_and(|(event_family, _)| event_family == family),
        None => spec == event_type,
    }
}

struct EventRoutine {
    id: String,
    workspace_id: String,
    spec: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// The event-trigger consumer for one tenant.
#[derive(Debug, Clone)]
pub struct EventTriggers {
    events: EventStore,
    tenant_id: String,
}

impl EventTriggers {
    /// A consumer over the pool for one tenant.
    #[must_use]
    pub fn new(pool: sqlx::PgPool, tenant_id: impl Into<String>) -> Self {
        Self {
            events: EventStore::new(pool),
            tenant_id: tenant_id.into(),
        }
    }

    /// Consume up to `limit` events and claim a fire for each match. Returns how many fires
    /// were claimed.
    ///
    /// # Errors
    /// Returns a database or event error; nothing is claimed and the cursor does not move.
    pub async fn scan(&self, limit: i64) -> Result<usize, EventError> {
        let position = self
            .events
            .load_cursor(&self.tenant_id, TRIGGER_STREAM, TRIGGER_CONSUMER)
            .await?
            .map(|cursor| cursor.sequence());
        let batch = self
            .events
            .read_events_after(&self.tenant_id, position, limit)
            .await?;
        let Some(last) = batch.last().map(|event| event.sequence) else {
            return Ok(0);
        };
        let (routines, own_threads, own_runs) = self.context().await?;
        let mut claims: Vec<(String, String, String, String)> = Vec::new();
        for event in &batch {
            if produced_by_routines(event, &own_threads, &own_runs) {
                continue;
            }
            let event_type = event.event_type.to_string();
            for routine in &routines {
                if event.workspace_id.as_deref() == Some(routine.workspace_id.as_str())
                    && event.occurred_at >= routine.created_at
                    && event_matches(&routine.spec, &event_type)
                {
                    claims.push((
                        routine.id.clone(),
                        routine.workspace_id.clone(),
                        format!("routine:{}:event:{}", routine.id, event.event_id),
                        event.event_id.to_string(),
                    ));
                }
            }
        }
        let tenant = self.tenant_id.clone();
        if claims.is_empty() {
            let mut tx = self.events.begin_tenant_transaction(&tenant).await?;
            advance_cursor_in_tx(&mut tx, &tenant, TRIGGER_STREAM, TRIGGER_CONSUMER, last).await?;
            tx.commit().await?;
            return Ok(0);
        }
        let claimed = self
            .events
            .commit_mutation_tx(&self.tenant_id, move |tx, batch| {
                Box::pin(async move {
                    let mut claimed = 0usize;
                    for (routine_id, workspace_id, key, event_id) in &claims {
                        if claim_in(
                            tx,
                            batch,
                            &tenant,
                            workspace_id,
                            routine_id,
                            key,
                            FireSource::Event,
                            None,
                            Some(event_id),
                            None,
                        )
                        .await?
                        .is_some()
                        {
                            claimed += 1;
                        }
                    }
                    advance_cursor_in_tx(tx, &tenant, TRIGGER_STREAM, TRIGGER_CONSUMER, last)
                        .await?;
                    Ok(claimed)
                })
            })
            .await;
        match claimed {
            Ok(claimed) => Ok(claimed),
            // Every claim was already taken (a concurrent scan): move the cursor on alone.
            Err(EventError::NoEventStaged) => {
                let mut tx = self
                    .events
                    .begin_tenant_transaction(&self.tenant_id)
                    .await?;
                advance_cursor_in_tx(
                    &mut tx,
                    &self.tenant_id,
                    TRIGGER_STREAM,
                    TRIGGER_CONSUMER,
                    last,
                )
                .await?;
                tx.commit().await?;
                Ok(0)
            }
            Err(error) => Err(error),
        }
    }

    async fn context(
        &self,
    ) -> Result<(Vec<EventRoutine>, HashSet<String>, HashSet<String>), EventError> {
        let mut tx = self
            .events
            .begin_tenant_transaction(&self.tenant_id)
            .await?;
        let rows = sqlx::query(
            "SELECT id, workspace_id, trigger ->> 'spec' AS spec, created_at FROM routines \
             WHERE tenant_id = $1 AND status = 'active' AND trigger ->> 'kind' = 'event'",
        )
        .bind(&self.tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        let routines = rows
            .iter()
            .map(|row| EventRoutine {
                id: row.get("id"),
                workspace_id: row.get("workspace_id"),
                spec: row.get::<Option<String>, _>("spec").unwrap_or_default(),
                created_at: row.get("created_at"),
            })
            .collect();
        let fires = sqlx::query(
            "SELECT thread_id, run_id FROM routine_fires WHERE tenant_id = $1 \
             AND (thread_id IS NOT NULL OR run_id IS NOT NULL)",
        )
        .bind(&self.tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        // What a fire creates is excluded even before the ledger records it: runs started by
        // a routine and objectives a routine created are found by their own rows.
        let routine_runs: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM runs WHERE tenant_id = $1 AND trigger_kind = 'routine'",
        )
        .bind(&self.tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        let routine_nodes: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM work_nodes WHERE tenant_id = $1 AND created_by ->> 'kind' = 'routine'",
        )
        .bind(&self.tenant_id)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let threads = fires
            .iter()
            .filter_map(|row| row.get::<Option<String>, _>("thread_id"))
            .collect();
        let runs = fires
            .iter()
            .filter_map(|row| row.get::<Option<String>, _>("run_id"))
            .chain(routine_runs)
            .chain(routine_nodes)
            .collect();
        Ok((routines, threads, runs))
    }
}

fn produced_by_routines(
    event: &RuntimeEvent,
    threads: &HashSet<String>,
    runs: &HashSet<String>,
) -> bool {
    let family = event.event_type.family().as_str();
    if matches!(family, "routine" | "notification") || event.aggregate_type == "routine" {
        return true;
    }
    if event.actor.id.starts_with("scheduler") || event.actor.id.starts_with("routine") {
        return true;
    }
    let text = |key: &str| event.payload.get(key).and_then(Value::as_str);
    if text("author_id").is_some_and(|author| author.starts_with("routine:")) {
        return true;
    }
    let thread = if event.aggregate_type == "thread" {
        Some(event.aggregate_id.as_str())
    } else {
        text("thread_id")
    };
    let run = if event.aggregate_type == "run" {
        Some(event.aggregate_id.as_str())
    } else {
        text("run_id")
    };
    let node = if event.aggregate_type == "work_node" {
        Some(event.aggregate_id.as_str())
    } else {
        text("work_node_id")
    };
    thread.is_some_and(|thread| threads.contains(thread))
        || run.is_some_and(|run| runs.contains(run))
        || node.is_some_and(|node| runs.contains(node))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_names_an_event_or_a_family() {
        assert!(validate_event_spec("artifact.created").is_ok());
        assert!(validate_event_spec("thread.*").is_ok());
        for bad in [
            "artifact",
            "nope.created",
            "routine.fired",
            "notification.*",
            "thread.Posted",
            "thread.",
        ] {
            assert!(validate_event_spec(bad).is_err(), "{bad}");
        }
        assert!(event_matches("thread.*", "thread.message_posted"));
        assert!(event_matches("artifact.created", "artifact.created"));
        assert!(!event_matches("artifact.created", "artifact.version_added"));
        assert!(!event_matches("thread.*", "threads.x"));
    }
}
