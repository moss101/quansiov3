//! Canonical typed identifiers (DOMAIN.md §1.1).
//!
//! Every canonical entity identity is `<prefix>_<ULID>`: a stable, time-sortable,
//! typed string. The prefix table here mirrors DOMAIN.md §1.1 and is cross-checked in
//! tests against the generated `quansio.v1.core.EntityPrefix` enum, so the two cannot
//! drift apart.

use core::fmt;
use core::str::FromStr;

use crate::error::CoreError;
use crate::ulid::{Ulid, UlidGenerator};

/// Entity prefixes from DOMAIN.md §1.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Prefix {
    /// `tn_` tenant.
    Tenant,
    /// `ws_` workspace.
    Workspace,
    /// `usr_` user.
    User,
    /// `sp_` service principal.
    ServicePrincipal,
    /// `agt_` teammate (agent definition).
    Teammate,
    /// `ath_` agent thread.
    AgentThread,
    /// `thr_` thread.
    Thread,
    /// `msg_` message.
    Message,
    /// `wn_` work node.
    WorkNode,
    /// `we_` work edge.
    WorkEdge,
    /// `run_` run.
    Run,
    /// `trn_` turn.
    Turn,
    /// `stp_` step.
    Step,
    /// `att_` attempt.
    Attempt,
    /// `cmd_` command.
    Command,
    /// `evt_` runtime event.
    Event,
    /// `q_` question.
    Question,
    /// `skl_` skill.
    Skill,
    /// `sklv_` skill version.
    SkillVersion,
    /// `pck_` capability pack.
    CapabilityPack,
    /// `pckv_` capability pack version.
    CapabilityPackVersion,
    /// `cnx_` connector.
    Connector,
    /// `mr_` model route.
    ModelRoute,
    /// `aud_` audit entry.
    AuditEntry,
    /// `tc_` tool call.
    ToolCall,
    /// `eff_` effect record.
    EffectRecord,
    /// `apr_` approval request.
    ApprovalRequest,
    /// `rcp_` approval receipt.
    ApprovalReceipt,
    /// `rule_` user rule.
    UserRule,
    /// `cap_` capability projection.
    CapabilityProjection,
    /// `pol_` policy.
    Policy,
    /// `tgt_` execution target.
    ExecutionTarget,
    /// `lse_` lease.
    Lease,
    /// `ckp_` checkpoint.
    Checkpoint,
    /// `cep_` compaction epoch.
    CompactionEpoch,
    /// `art_` artifact.
    Artifact,
    /// `artv_` artifact version.
    ArtifactVersion,
    /// `evd_` evidence.
    Evidence,
    /// `evb_` evidence bundle.
    EvidenceBundle,
    /// `kn_` knowledge entry.
    KnowledgeEntry,
    /// `mem_` memory entry.
    MemoryEntry,
    /// `rtn_` routine.
    Routine,
    /// `ntf_` notification.
    Notification,
    /// `sec_` secret handle.
    SecretHandle,
    /// `use_` usage record.
    UsageRecord,
    /// `whk_` webhook subscription.
    WebhookSubscription,
    /// `tsn_` terminal session.
    TerminalSession,
    /// `bsn_` browser session.
    BrowserSession,
}

impl Prefix {
    /// The canonical lowercase prefix, including the trailing underscore.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tenant => "tn_",
            Self::Workspace => "ws_",
            Self::User => "usr_",
            Self::ServicePrincipal => "sp_",
            Self::Teammate => "agt_",
            Self::AgentThread => "ath_",
            Self::Thread => "thr_",
            Self::Message => "msg_",
            Self::WorkNode => "wn_",
            Self::WorkEdge => "we_",
            Self::Run => "run_",
            Self::Turn => "trn_",
            Self::Step => "stp_",
            Self::Attempt => "att_",
            Self::Command => "cmd_",
            Self::Event => "evt_",
            Self::Question => "q_",
            Self::Skill => "skl_",
            Self::SkillVersion => "sklv_",
            Self::CapabilityPack => "pck_",
            Self::CapabilityPackVersion => "pckv_",
            Self::Connector => "cnx_",
            Self::ModelRoute => "mr_",
            Self::AuditEntry => "aud_",
            Self::ToolCall => "tc_",
            Self::EffectRecord => "eff_",
            Self::ApprovalRequest => "apr_",
            Self::ApprovalReceipt => "rcp_",
            Self::UserRule => "rule_",
            Self::CapabilityProjection => "cap_",
            Self::Policy => "pol_",
            Self::ExecutionTarget => "tgt_",
            Self::Lease => "lse_",
            Self::Checkpoint => "ckp_",
            Self::CompactionEpoch => "cep_",
            Self::Artifact => "art_",
            Self::ArtifactVersion => "artv_",
            Self::Evidence => "evd_",
            Self::EvidenceBundle => "evb_",
            Self::KnowledgeEntry => "kn_",
            Self::MemoryEntry => "mem_",
            Self::Routine => "rtn_",
            Self::Notification => "ntf_",
            Self::SecretHandle => "sec_",
            Self::UsageRecord => "use_",
            Self::WebhookSubscription => "whk_",
            Self::TerminalSession => "tsn_",
            Self::BrowserSession => "bsn_",
        }
    }

    /// Name of the matching value in the generated `quansio.v1.core.EntityPrefix`
    /// enum (GOV-004 contract). The correspondence is asserted by tests, so a renamed
    /// entity cannot silently drift between DOMAIN.md, the contract and this crate.
    #[must_use]
    pub const fn contract_name(self) -> &'static str {
        match self {
            Self::Tenant => "TENANT",
            Self::Workspace => "WORKSPACE",
            Self::User => "USER",
            Self::ServicePrincipal => "SERVICE_PRINCIPAL",
            Self::Teammate => "TEAMMATE",
            Self::AgentThread => "AGENT_THREAD",
            Self::Thread => "THREAD",
            Self::Message => "MESSAGE",
            Self::WorkNode => "WORK_NODE",
            Self::WorkEdge => "WORK_EDGE",
            Self::Run => "RUN",
            Self::Turn => "TURN",
            Self::Step => "STEP",
            Self::Attempt => "ATTEMPT",
            Self::Command => "COMMAND",
            Self::Event => "RUNTIME_EVENT",
            Self::Question => "QUESTION",
            Self::Skill => "SKILL",
            Self::SkillVersion => "SKILL_VERSION",
            Self::CapabilityPack => "CAPABILITY_PACK",
            Self::CapabilityPackVersion => "CAPABILITY_PACK_VERSION",
            Self::Connector => "CONNECTOR",
            Self::ModelRoute => "MODEL_ROUTE",
            Self::AuditEntry => "AUDIT_ENTRY",
            Self::ToolCall => "TOOL_CALL",
            Self::EffectRecord => "EFFECT_RECORD",
            Self::ApprovalRequest => "APPROVAL_REQUEST",
            Self::ApprovalReceipt => "APPROVAL_RECEIPT",
            Self::UserRule => "USER_RULE",
            Self::CapabilityProjection => "CAPABILITY_PROJECTION",
            Self::Policy => "POLICY",
            Self::ExecutionTarget => "EXECUTION_TARGET",
            Self::Lease => "LEASE",
            Self::Checkpoint => "CHECKPOINT",
            Self::CompactionEpoch => "COMPACTION_EPOCH",
            Self::Artifact => "ARTIFACT",
            Self::ArtifactVersion => "ARTIFACT_VERSION",
            Self::Evidence => "EVIDENCE",
            Self::EvidenceBundle => "EVIDENCE_BUNDLE",
            Self::KnowledgeEntry => "KNOWLEDGE_ENTRY",
            Self::MemoryEntry => "MEMORY_ENTRY",
            Self::Routine => "ROUTINE",
            Self::Notification => "NOTIFICATION",
            Self::SecretHandle => "SECRET_HANDLE",
            Self::UsageRecord => "USAGE_RECORD",
            Self::WebhookSubscription => "WEBHOOK_SUBSCRIPTION",
            Self::TerminalSession => "TERMINAL_SESSION",
            Self::BrowserSession => "BROWSER_SESSION",
        }
    }

    /// Every prefix in the canonical table (DOMAIN.md §1.1).
    #[must_use]
    pub const fn all() -> &'static [Prefix] {
        &[
            Self::Tenant,
            Self::Workspace,
            Self::User,
            Self::ServicePrincipal,
            Self::Teammate,
            Self::AgentThread,
            Self::Thread,
            Self::Message,
            Self::WorkNode,
            Self::WorkEdge,
            Self::Run,
            Self::Turn,
            Self::Step,
            Self::Attempt,
            Self::Command,
            Self::Event,
            Self::Question,
            Self::Skill,
            Self::SkillVersion,
            Self::CapabilityPack,
            Self::CapabilityPackVersion,
            Self::Connector,
            Self::ModelRoute,
            Self::AuditEntry,
            Self::ToolCall,
            Self::EffectRecord,
            Self::ApprovalRequest,
            Self::ApprovalReceipt,
            Self::UserRule,
            Self::CapabilityProjection,
            Self::Policy,
            Self::ExecutionTarget,
            Self::Lease,
            Self::Checkpoint,
            Self::CompactionEpoch,
            Self::Artifact,
            Self::ArtifactVersion,
            Self::Evidence,
            Self::EvidenceBundle,
            Self::KnowledgeEntry,
            Self::MemoryEntry,
            Self::Routine,
            Self::Notification,
            Self::SecretHandle,
            Self::UsageRecord,
            Self::WebhookSubscription,
            Self::TerminalSession,
            Self::BrowserSession,
        ]
    }
}

impl fmt::Display for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A canonical `<prefix>_<ULID>` identifier bound to its entity type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalId {
    prefix: Prefix,
    ulid: Ulid,
}

impl CanonicalId {
    /// Build an id from its parts.
    #[must_use]
    pub const fn new(prefix: Prefix, ulid: Ulid) -> Self {
        Self { prefix, ulid }
    }

    /// Generate a new id with the given prefix.
    pub fn generate<C: crate::ulid::Clock, E: crate::ulid::EntropySource>(
        prefix: Prefix,
        generator: &mut UlidGenerator<C, E>,
    ) -> Self {
        Self::new(prefix, generator.generate())
    }

    /// The entity prefix.
    #[must_use]
    pub const fn prefix(&self) -> Prefix {
        self.prefix
    }

    /// The ULID part.
    #[must_use]
    pub const fn ulid(&self) -> Ulid {
        self.ulid
    }

    /// Creation time in milliseconds since the Unix epoch.
    #[must_use]
    pub fn timestamp_ms(&self) -> u64 {
        self.ulid.timestamp_ms()
    }

    /// Parse and validate an id against an expected prefix.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidPrefix`] when the value carries a different prefix
    /// and [`CoreError::UnknownPrefix`] when the prefix is not canonical.
    pub fn parse_typed(value: &str, expected: Prefix) -> Result<Self, CoreError> {
        let parsed = Self::parse(value)?;
        if parsed.prefix != expected {
            return Err(CoreError::InvalidPrefix {
                value: value.to_string(),
                expected: expected.to_string(),
            });
        }
        Ok(parsed)
    }

    /// Parse any canonical id, accepting every prefix in the table.
    ///
    /// # Errors
    /// Returns [`CoreError::UnknownPrefix`] or [`CoreError::InvalidUlid`].
    pub fn parse(value: &str) -> Result<Self, CoreError> {
        let (prefix_str, ulid_str) = value
            .split_once('_')
            .ok_or_else(|| CoreError::UnknownPrefix(value.to_string()))?;
        let prefix_with_separator = format!("{prefix_str}_");
        let prefix = Prefix::all()
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == prefix_with_separator)
            .ok_or_else(|| CoreError::UnknownPrefix(value.to_string()))?;
        let ulid = Ulid::parse(ulid_str)?;
        Ok(Self::new(prefix, ulid))
    }
}

impl fmt::Display for CanonicalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.prefix, self.ulid)
    }
}

impl FromStr for CanonicalId {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl serde::Serialize for CanonicalId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> serde::Deserialize<'de> for CanonicalId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Shared contract for the typed identifier newtypes below: a canonical id carrying a
/// fixed prefix and a stable ordering key.
pub trait TypedId: Sized {
    /// The prefix every value of this type carries.
    const PREFIX: Prefix;

    /// The underlying canonical id.
    fn as_id(&self) -> &CanonicalId;

    /// Wrap a canonical id, validating the prefix.
    ///
    /// # Errors
    /// Returns [`CoreError::InvalidPrefix`] if the id carries another prefix.
    fn from_id(id: CanonicalId) -> Result<Self, CoreError>;

    /// Generate a new value.
    fn generate<C: crate::ulid::Clock, E: crate::ulid::EntropySource>(
        generator: &mut UlidGenerator<C, E>,
    ) -> Self {
        let id = CanonicalId::new(Self::PREFIX, generator.generate());
        Self::from_id(id).expect("prefix is fixed by the type")
    }

    /// Parse a string as this identifier type.
    ///
    /// # Errors
    /// Returns a [`CoreError`] when the value is not a canonical id of this type.
    fn parse(value: &str) -> Result<Self, CoreError> {
        Self::from_id(CanonicalId::parse_typed(value, Self::PREFIX)?)
    }
}

macro_rules! typed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(CanonicalId);

        impl $name {
            /// Build the identifier from a canonical id of the matching prefix.
            ///
            /// # Errors
            /// Returns [`CoreError::InvalidPrefix`] when the prefix does not match.
            pub fn new(id: CanonicalId) -> Result<Self, CoreError> {
                <Self as TypedId>::from_id(id)
            }

            /// The underlying canonical id.
            #[must_use]
            pub fn as_canonical(&self) -> &CanonicalId {
                &self.0
            }
        }

        impl TypedId for $name {
            const PREFIX: Prefix = $prefix;

            fn as_id(&self) -> &CanonicalId {
                &self.0
            }

            fn from_id(id: CanonicalId) -> Result<Self, CoreError> {
                if id.prefix() == Self::PREFIX {
                    Ok(Self(id))
                } else {
                    Err(CoreError::InvalidPrefix {
                        value: id.to_string(),
                        expected: Self::PREFIX.to_string(),
                    })
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = CoreError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                <Self as TypedId>::parse(s)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                <Self as TypedId>::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

typed_id!(
    /// A client-supplied command identity; unique per tenant (DOMAIN.md §1.2).
    CommandId,
    Prefix::Command
);
/// Shared identity of everything caused by one external input: a bare ULID
/// (DOMAIN.md §1.2 — correlation is not an entity identity, so it carries no prefix).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CorrelationId(Ulid);

impl CorrelationId {
    /// Wrap an existing ULID.
    #[must_use]
    pub const fn from_ulid(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// Generate a new correlation id.
    pub fn generate<C: crate::ulid::Clock, E: crate::ulid::EntropySource>(
        generator: &mut UlidGenerator<C, E>,
    ) -> Self {
        Self(generator.generate())
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(&self) -> Ulid {
        self.0
    }
}

impl fmt::Display for CorrelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for CorrelationId {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::parse(s)?))
    }
}

typed_id!(
    /// A durable runtime event identity.
    EventId,
    Prefix::Event
);

/// What directly caused an event or command: an event or a command identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CausationId {
    /// Caused by a runtime event.
    Event(EventId),
    /// Caused by a command.
    Command(CommandId),
}

impl fmt::Display for CausationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Event(id) => fmt::Display::fmt(id, f),
            Self::Command(id) => fmt::Display::fmt(id, f),
        }
    }
}

impl FromStr for CausationId {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let id = CanonicalId::parse(s)?;
        match id.prefix() {
            Prefix::Event => Ok(Self::Event(EventId::new(id)?)),
            Prefix::Command => Ok(Self::Command(CommandId::new(id)?)),
            _ => Err(CoreError::InvalidPrefix {
                value: s.to_string(),
                expected: format!("{} or {}", Prefix::Event, Prefix::Command),
            }),
        }
    }
}
