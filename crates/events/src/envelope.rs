//! The RuntimeEvent envelope (DOMAIN.md §9.1).
//!
//! A RuntimeEvent is the durable, ordered record of one committed state transition.
//! It is written in the same PostgreSQL transaction as the mutation it records, plus a
//! transactional outbox row, so the two can never diverge.

use chrono::{DateTime, SecondsFormat, Utc};
use quansio_core::{
    CausationId, CommandId, CorrelationId, EventId, Generation, Sequence, TypedId, UlidGenerator,
};
use serde_json::Value;

use crate::event_type::EventType;

/// Who produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A user action.
    User,
    /// An agent (model-driven) action.
    Agent,
    /// The runtime acting on its own (timers, reconciliation).
    System,
    /// A service principal or connector.
    Service,
}

impl ActorKind {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
            Self::Service => "service",
        }
    }
}

/// The `actor {kind, id}` object carried by every RuntimeEvent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Actor {
    /// Who acted.
    pub kind: ActorKind,
    /// Canonical id of the actor (`usr_…`, `agt_…`, `sp_…`) or a stable system name.
    pub id: String,
}

impl Actor {
    /// Build an actor.
    #[must_use]
    pub fn new(kind: ActorKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    /// A user actor.
    #[must_use]
    pub fn user(id: impl Into<String>) -> Self {
        Self::new(ActorKind::User, id)
    }

    /// An agent actor.
    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self::new(ActorKind::Agent, id)
    }

    /// The runtime acting on its own; the id names the subsystem.
    #[must_use]
    pub fn system(id: impl Into<String>) -> Self {
        Self::new(ActorKind::System, id)
    }

    /// A service principal actor.
    #[must_use]
    pub fn service(id: impl Into<String>) -> Self {
        Self::new(ActorKind::Service, id)
    }
}

/// Default envelope schema version.
pub const SCHEMA_VERSION_V1: &str = "v1";

/// A committed RuntimeEvent (DOMAIN.md §9.1).
///
/// Field names mirror the envelope; `event_type` is the DOMAIN `type` field (Rust
/// reserves `type` as a keyword, and the canonical JSON uses `type`).
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeEvent {
    /// Durable event identity (`evt_…`).
    pub event_id: EventId,
    /// Owning tenant.
    pub tenant_id: String,
    /// Owning workspace, when the aggregate is workspace-scoped.
    pub workspace_id: Option<String>,
    /// Tenant-monotonic ordering, assigned inside the commit transaction.
    pub sequence: Sequence,
    /// Aggregate kind, e.g. `run`.
    pub aggregate_type: String,
    /// Aggregate identity.
    pub aggregate_id: String,
    /// Version of the aggregate after the transition.
    pub aggregate_version: u64,
    /// Event type `<family>.<event>` (DOMAIN `type`).
    pub event_type: EventType,
    /// Envelope schema version.
    pub schema_version: String,
    /// When the transition occurred (UTC).
    pub occurred_at: DateTime<Utc>,
    /// Client command that caused the transition, when there is one.
    pub command_id: Option<CommandId>,
    /// Everything caused by one external input shares this id.
    pub correlation_id: CorrelationId,
    /// The event or command that directly caused this one.
    pub causation_id: Option<CausationId>,
    /// Who acted.
    pub actor: Actor,
    /// Controller generation the mutation was issued under.
    pub generation: Option<Generation>,
    /// Event-specific payload, typed by `type`.
    pub payload: Value,
}

impl RuntimeEvent {
    /// Build a committed event from a staged draft.
    #[must_use]
    pub fn from_draft(draft: EventDraft, tenant_id: impl Into<String>, sequence: Sequence) -> Self {
        Self {
            event_id: draft.event_id,
            tenant_id: tenant_id.into(),
            workspace_id: draft.workspace_id,
            sequence,
            aggregate_type: draft.aggregate_type,
            aggregate_id: draft.aggregate_id,
            aggregate_version: draft.aggregate_version,
            event_type: draft.event_type,
            schema_version: draft.schema_version,
            occurred_at: draft.occurred_at,
            command_id: draft.command_id,
            correlation_id: draft.correlation_id,
            causation_id: draft.causation_id,
            actor: draft.actor,
            generation: draft.generation,
            payload: draft.payload,
        }
    }

    /// The NATS JetStream subject `q.<tenant>.<aggregate_type>.<type>` (DOMAIN.md §9.1).
    #[must_use]
    pub fn subject(&self) -> String {
        format!(
            "q.{}.{}.{}",
            self.tenant_id, self.aggregate_type, self.event_type
        )
    }

    /// Canonical JSON of the envelope, matching `schemas/json/runtime-event.schema.json`.
    ///
    /// Optional fields are omitted rather than emitted as `null`.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("event_id".into(), Value::String(self.event_id.to_string()));
        object.insert("tenant_id".into(), Value::String(self.tenant_id.clone()));
        if let Some(workspace_id) = &self.workspace_id {
            object.insert("workspace_id".into(), Value::String(workspace_id.clone()));
        }
        object.insert("sequence".into(), Value::Number(self.sequence.get().into()));
        object.insert(
            "aggregate_type".into(),
            Value::String(self.aggregate_type.clone()),
        );
        object.insert(
            "aggregate_id".into(),
            Value::String(self.aggregate_id.clone()),
        );
        object.insert(
            "aggregate_version".into(),
            Value::Number(self.aggregate_version.into()),
        );
        object.insert("type".into(), Value::String(self.event_type.to_string()));
        object.insert(
            "schema_version".into(),
            Value::String(self.schema_version.clone()),
        );
        object.insert(
            "occurred_at".into(),
            Value::String(
                self.occurred_at
                    .to_rfc3339_opts(SecondsFormat::Millis, true),
            ),
        );
        if let Some(command_id) = &self.command_id {
            object.insert("command_id".into(), Value::String(command_id.to_string()));
        }
        object.insert(
            "correlation_id".into(),
            Value::String(self.correlation_id.to_string()),
        );
        if let Some(causation_id) = &self.causation_id {
            object.insert(
                "causation_id".into(),
                Value::String(causation_id.to_string()),
            );
        }
        object.insert(
            "actor".into(),
            serde_json::to_value(&self.actor).expect("actor serializes"),
        );
        if let Some(generation) = self.generation {
            object.insert("generation".into(), Value::Number(generation.get().into()));
        }
        object.insert("payload".into(), self.payload.clone());
        Value::Object(object)
    }
}

impl serde::Serialize for RuntimeEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json_value().serialize(serializer)
    }
}

/// A RuntimeEvent staged inside a transaction, before its sequence is assigned.
///
/// The caller supplies identity and metadata; the store owns `tenant_id` (from the
/// commit) and `sequence` (from the tenant counter).
#[derive(Debug, Clone, PartialEq)]
pub struct EventDraft {
    /// Durable event identity (`evt_…`).
    pub event_id: EventId,
    /// Owning workspace, when the aggregate is workspace-scoped.
    pub workspace_id: Option<String>,
    /// Aggregate kind, e.g. `run`.
    pub aggregate_type: String,
    /// Aggregate identity.
    pub aggregate_id: String,
    /// Version of the aggregate after the transition.
    pub aggregate_version: u64,
    /// Event type `<family>.<event>`.
    pub event_type: EventType,
    /// Envelope schema version.
    pub schema_version: String,
    /// When the transition occurred (UTC).
    pub occurred_at: DateTime<Utc>,
    /// Client command that caused the transition, when there is one.
    pub command_id: Option<CommandId>,
    /// Everything caused by one external input shares this id.
    pub correlation_id: CorrelationId,
    /// The event or command that directly caused this one.
    pub causation_id: Option<CausationId>,
    /// Who acted.
    pub actor: Actor,
    /// Controller generation the mutation was issued under.
    pub generation: Option<Generation>,
    /// Event-specific payload.
    pub payload: Value,
}

impl EventDraft {
    /// Stage a new event with a freshly generated `event_id` and current timestamp.
    #[must_use]
    pub fn new(
        aggregate_type: impl Into<String>,
        aggregate_id: impl Into<String>,
        aggregate_version: u64,
        event_type: EventType,
        correlation_id: CorrelationId,
        actor: Actor,
    ) -> Self {
        let mut generator = UlidGenerator::new();
        Self {
            event_id: EventId::generate(&mut generator),
            workspace_id: None,
            aggregate_type: aggregate_type.into(),
            aggregate_id: aggregate_id.into(),
            aggregate_version,
            event_type,
            schema_version: SCHEMA_VERSION_V1.to_string(),
            occurred_at: Utc::now(),
            command_id: None,
            correlation_id,
            causation_id: None,
            actor,
            generation: None,
            payload: Value::Object(serde_json::Map::new()),
        }
    }

    /// Set the owning workspace.
    #[must_use]
    pub fn with_workspace(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    /// Set the event payload.
    #[must_use]
    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }

    /// Set the causing command.
    #[must_use]
    pub fn with_command_id(mut self, command_id: CommandId) -> Self {
        self.command_id = Some(command_id);
        self
    }

    /// Set the direct causation.
    #[must_use]
    pub fn with_causation_id(mut self, causation_id: CausationId) -> Self {
        self.causation_id = Some(causation_id);
        self
    }

    /// Set the controller generation.
    #[must_use]
    pub fn with_generation(mut self, generation: Generation) -> Self {
        self.generation = Some(generation);
        self
    }

    /// Override the generated event id (used by deterministic fixtures and replays).
    #[must_use]
    pub fn with_event_id(mut self, event_id: EventId) -> Self {
        self.event_id = event_id;
        self
    }

    /// Override the occurrence time.
    #[must_use]
    pub fn with_occurred_at(mut self, occurred_at: DateTime<Utc>) -> Self {
        self.occurred_at = occurred_at;
        self
    }
}
