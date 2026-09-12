//! Event families and typed `type` values (DOMAIN.md §9.2).
//!
//! `schemas/catalog/event-families.yaml` is generated from DOMAIN.md and is the naming
//! authority; [`EVENT_FAMILIES`] mirrors it and a test asserts the two cannot drift.
//! A `type` is always `<family>.<event>` and the family must be canonical, so a
//! misspelled or unknown family is rejected with a typed error instead of being
//! written into the stream.

use core::fmt;
use core::str::FromStr;

use crate::error::EventError;

/// The canonical event families from DOMAIN.md §9.2.
pub const EVENT_FAMILIES: &[&str] = &[
    "tenant",
    "workspace",
    "member",
    "thread",
    "work",
    "agent",
    "run",
    "turn",
    "step",
    "model",
    "tool",
    "effect",
    "approval",
    "question",
    "target",
    "lease",
    "browser",
    "terminal",
    "checkpoint",
    "artifact",
    "evidence",
    "knowledge",
    "memory",
    "skill",
    "pack",
    "routine",
    "notification",
    "connector",
    "policy",
    "audit",
    "usage",
    "capability",
    "compaction",
    "ops",
];

/// One canonical event family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EventFamily {
    /// `tenant.*`
    Tenant,
    /// `workspace.*`
    Workspace,
    /// `member.*`
    Member,
    /// `thread.*`
    Thread,
    /// `work.*`
    Work,
    /// `agent.*`
    Agent,
    /// `run.*`
    Run,
    /// `turn.*`
    Turn,
    /// `step.*`
    Step,
    /// `model.*`
    Model,
    /// `tool.*`
    Tool,
    /// `effect.*`
    Effect,
    /// `approval.*`
    Approval,
    /// `question.*`
    Question,
    /// `target.*`
    Target,
    /// `lease.*`
    Lease,
    /// `browser.*`
    Browser,
    /// `terminal.*`
    Terminal,
    /// `checkpoint.*`
    Checkpoint,
    /// `artifact.*`
    Artifact,
    /// `evidence.*`
    Evidence,
    /// `knowledge.*`
    Knowledge,
    /// `memory.*`
    Memory,
    /// `skill.*`
    Skill,
    /// `pack.*`
    Pack,
    /// `routine.*`
    Routine,
    /// `notification.*`
    Notification,
    /// `connector.*`
    Connector,
    /// `policy.*`
    Policy,
    /// `audit.*`
    Audit,
    /// `usage.*`
    Usage,
    /// `capability.*`
    Capability,
    /// `compaction.*`
    Compaction,
    /// `ops.*`
    Ops,
}

impl EventFamily {
    /// Every canonical family, in DOMAIN.md §9.2 order.
    #[must_use]
    pub const fn all() -> &'static [EventFamily] {
        &[
            Self::Tenant,
            Self::Workspace,
            Self::Member,
            Self::Thread,
            Self::Work,
            Self::Agent,
            Self::Run,
            Self::Turn,
            Self::Step,
            Self::Model,
            Self::Tool,
            Self::Effect,
            Self::Approval,
            Self::Question,
            Self::Target,
            Self::Lease,
            Self::Browser,
            Self::Terminal,
            Self::Checkpoint,
            Self::Artifact,
            Self::Evidence,
            Self::Knowledge,
            Self::Memory,
            Self::Skill,
            Self::Pack,
            Self::Routine,
            Self::Notification,
            Self::Connector,
            Self::Policy,
            Self::Audit,
            Self::Usage,
            Self::Capability,
            Self::Compaction,
            Self::Ops,
        ]
    }

    /// The canonical family prefix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::Workspace => "workspace",
            Self::Member => "member",
            Self::Thread => "thread",
            Self::Work => "work",
            Self::Agent => "agent",
            Self::Run => "run",
            Self::Turn => "turn",
            Self::Step => "step",
            Self::Model => "model",
            Self::Tool => "tool",
            Self::Effect => "effect",
            Self::Approval => "approval",
            Self::Question => "question",
            Self::Target => "target",
            Self::Lease => "lease",
            Self::Browser => "browser",
            Self::Terminal => "terminal",
            Self::Checkpoint => "checkpoint",
            Self::Artifact => "artifact",
            Self::Evidence => "evidence",
            Self::Knowledge => "knowledge",
            Self::Memory => "memory",
            Self::Skill => "skill",
            Self::Pack => "pack",
            Self::Routine => "routine",
            Self::Notification => "notification",
            Self::Connector => "connector",
            Self::Policy => "policy",
            Self::Audit => "audit",
            Self::Usage => "usage",
            Self::Capability => "capability",
            Self::Compaction => "compaction",
            Self::Ops => "ops",
        }
    }
}

impl fmt::Display for EventFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parse a family name against the canonical list.
///
/// # Errors
/// Returns [`EventError::UnknownEventFamily`] when the name is not canonical.
pub fn parse_family(value: &str) -> Result<EventFamily, EventError> {
    EventFamily::all()
        .iter()
        .copied()
        .find(|family| family.as_str() == value)
        .ok_or_else(|| EventError::UnknownEventFamily {
            family: value.to_string(),
        })
}

/// A validated `<family>.<event>` RuntimeEvent type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventType {
    family: EventFamily,
    name: String,
}

impl EventType {
    /// Parse and validate a `<family>.<event>` value.
    ///
    /// # Errors
    /// Returns [`EventError::MalformedEventType`] when the value is not
    /// `<family>.<event>` with a lowercase snake_case event, and
    /// [`EventError::UnknownEventFamily`] when the family is not canonical.
    pub fn parse(value: &str) -> Result<Self, EventError> {
        let malformed = || EventError::MalformedEventType {
            value: value.to_string(),
        };
        let (family, name) = value.split_once('.').ok_or_else(malformed)?;
        if family.is_empty()
            || name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(malformed());
        }
        Ok(Self {
            family: parse_family(family)?,
            name: name.to_string(),
        })
    }

    /// The canonical family.
    #[must_use]
    pub const fn family(&self) -> EventFamily {
        self.family
    }

    /// The event name within the family.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for EventType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.family, self.name)
    }
}

impl FromStr for EventType {
    type Err = EventError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl serde::Serialize for EventType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for EventType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}
