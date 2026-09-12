//! Policy, RBAC, privacy guards, content-trust escalation and approval verification
//! (RUN-006, INT-012).
//!
//! Canonical owner (DOSSIER.md §17): `crates/server/src/policy`. This module is the
//! single policy engine and the single approval verifier: it evaluates `Policy` and
//! `UserRule` decisions (DOMAIN.md §7.3), enforces the RBAC role families (§2), the
//! data-class/egress privacy guard and the sequence guards (§7.1, §12), escalates
//! proposals derived from `UNTRUSTED_EXTERNAL` content, and verifies server-signed
//! `ApprovalReceipt`s before dispatch (§7.3, §16).
//!
//! Precedence is fixed and fail-closed: policy first, then user rules, which may only
//! move `ask → always` where policy permits and never on tier 4 or an escalated
//! proposal. A missing or ambiguous security input — no matching rule for a tier ≥ 1
//! class, an unparseable rule, a missing capability projection, an unverifiable
//! signature — is a *deny* with a typed reason, never an allow. Nothing here opens a
//! store of its own: the durable rows and their `policy.*`/`approval.*` RuntimeEvents
//! are written through `quansio_events` in one transaction.
#![forbid(unsafe_code)]

/// Repository path of this module's canonical owner, used by the workspace conformance
/// check to prove one owner maps to exactly one module.
pub const POLICY_OWNER: &str = "crates/server/src/policy";

pub mod approval;
pub mod error;
pub mod escalation;
pub mod evaluation;
pub mod guards;
pub mod rbac;
pub mod rules;
pub mod store;

pub use approval::{
    Amount, ApprovalReceipt, ApprovalRequestRecord, ApprovalRequestStatus, ApprovalSigner,
    ConsequencePreview, DispatchBinding, NewApprovalRequest, ReceiptScope, RequestedOf,
    UntrustedOrigin, APPROVAL_SIGNING_KEY_ENV,
};
pub use error::{ApprovalFailure, PolicyError};
pub use escalation::{
    derived_from_trust, escalate, Escalation, TrustLabellingSource, TrustLevel,
    UnavailableTrustLabelling, FAIL_CLOSED_TRUST,
};
pub use evaluation::{
    EvaluationRequest, PolicyEvaluator, PolicyOutcome, PolicyReason, RejectedUserRule,
    UserRuleRejection,
};
pub use guards::{
    check_privacy, check_sequence, DataClass, EgressDestination, EgressGrantSet, GuardFailure,
    GuardOutcome, PrivacyInputs, SequenceContext, SequenceGuard,
};
pub use rbac::{ActionFamily, ActorRoles, RbacFailure, TenantRole, WorkspaceRole};
pub use rules::{
    validate_user_rule, Policy, PolicyRule, PolicyScope, PolicySet, RuleConditions, RuleContext,
    UserRule,
};
pub use store::{ApprovalRuntime, DecisionRow, ParkedApproval, PolicyStore, POLICY_OWNER_EVENTS};
