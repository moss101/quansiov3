//! Capability Projection assembly (DOMAIN.md §6.2, §6.3).
//!
//! [`assemble`] walks the fixed layer order, intersecting the running grant set with
//! each layer's contribution. A layer may only remove or narrow; an attempted widening
//! is ignored and returned as a [`WideningRejection`]. A layer whose input cannot be
//! fetched, an unparseable grant and a policy gap at tier ≥ 1 all fail closed: the
//! result is [`CapabilityError::InputsUnavailable`] carrying an **empty** projection.

use chrono::{DateTime, Utc};
use quansio_core::{CanonicalId, Digest, Prefix, UlidGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::algebra::{narrow, WideningReason, WideningRejection};
use crate::error::{CapabilityError, InputUnavailableReason, InputsUnavailable, StaleReason};
use crate::grant::{Approval, Constraints, EffectClass, Grant, Tier};
use crate::layer::{inputs_digest, inputs_in_canonical_order, Layer, ProjectionInput};
use crate::policy::{PolicyDecision, PolicyDocument, PolicyRule, UserRule, UserRuleDecision};
use crate::selector::ResourceSelector;

/// The kind of subject a projection authorizes (DOMAIN.md §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    /// A Run.
    Run,
    /// An AgentThread.
    AgentThread,
    /// A ToolCall.
    ToolCall,
}

impl SubjectKind {
    /// The canonical wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::AgentThread => "agent_thread",
            Self::ToolCall => "tool_call",
        }
    }
}

/// The subject a projection was computed for (DOMAIN.md §6.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectionSubject {
    /// What kind of subject.
    pub kind: SubjectKind,
    /// The subject id.
    pub id: String,
}

impl ProjectionSubject {
    /// Build a subject.
    #[must_use]
    pub fn new(kind: SubjectKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    /// A Run subject.
    #[must_use]
    pub fn run(id: impl Into<String>) -> Self {
        Self::new(SubjectKind::Run, id)
    }

    /// An AgentThread subject.
    #[must_use]
    pub fn agent_thread(id: impl Into<String>) -> Self {
        Self::new(SubjectKind::AgentThread, id)
    }
}

/// What one layer contributes to assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerPayload {
    /// The layer neither grants nor removes (for example, no active skills).
    None,
    /// A candidate grant set, intersected with the running set.
    Grants(Vec<Grant>),
    /// A policy document that filters the running set.
    Policy(PolicyDocument),
    /// User rules that narrow the running set.
    UserRules(Vec<UserRule>),
}

/// One layer's fetched contribution, with its source reference, digest and expiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInput {
    /// The canonical reference of the layer source.
    pub reference: String,
    /// SHA-256 of the layer source content.
    pub digest: Digest,
    /// The layer's contribution.
    pub payload: LayerPayload,
    /// A layer-imposed expiry that narrows the projection's `expires_at`.
    pub expires_at: Option<DateTime<Utc>>,
}

impl LayerInput {
    /// Build a layer input from its parts.
    #[must_use]
    pub fn new(
        reference: impl Into<String>,
        digest: Digest,
        payload: LayerPayload,
        expires_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            reference: reference.into(),
            digest,
            payload,
            expires_at,
        }
    }

    /// A layer input carrying candidate grants; the digest is over their canonical JSON.
    #[must_use]
    pub fn grants(reference: impl Into<String>, grants: Vec<Grant>) -> Self {
        let digest = digest_of(&grants);
        Self::new(reference, digest, LayerPayload::Grants(grants), None)
    }

    /// A layer input carrying a policy document.
    #[must_use]
    pub fn policy(document: PolicyDocument) -> Self {
        let digest = digest_of(&document);
        Self::new(
            document.reference.clone(),
            digest,
            LayerPayload::Policy(document),
            None,
        )
    }

    /// A layer input carrying user rules; the digest is over their canonical JSON.
    #[must_use]
    pub fn user_rules(reference: impl Into<String>, rules: Vec<UserRule>) -> Self {
        let digest = digest_of(&rules);
        Self::new(reference, digest, LayerPayload::UserRules(rules), None)
    }

    /// A layer input that contributes nothing (the layer is present but empty).
    #[must_use]
    pub fn empty(reference: impl Into<String>) -> Self {
        let reference = reference.into();
        let digest = Digest::of(reference.as_bytes());
        Self::new(reference, digest, LayerPayload::None, None)
    }

    /// Set the layer-imposed expiry.
    #[must_use]
    pub fn with_expires_at(mut self, expires_at: DateTime<Utc>) -> Self {
        self.expires_at = Some(expires_at);
        self
    }
}

/// The seam that resolves each projection layer.
///
/// A fetch failure (unreachable source, unparseable payload) is returned as an error so
/// assembly fails closed rather than skipping the layer.
pub trait ProjectionSource {
    /// Fetch one layer's contribution.
    ///
    /// # Errors
    /// Returns [`InputUnavailableReason`] when the layer cannot be resolved.
    fn fetch(&self, layer: Layer) -> Result<LayerInput, InputUnavailableReason>;

    /// The canonical catalogue tier of an effect class, or `None` when unknown.
    fn tier_of(&self, effect_class: &EffectClass) -> Option<Tier>;
}

/// What a projection is computed for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRequest {
    /// The subject the projection authorizes.
    pub subject: ProjectionSubject,
    /// When the projection is computed.
    pub now: DateTime<Utc>,
}

impl ProjectionRequest {
    /// Build a request at the current instant.
    #[must_use]
    pub fn new(subject: ProjectionSubject) -> Self {
        Self {
            subject,
            now: Utc::now(),
        }
    }

    /// Build a request at an explicit instant.
    #[must_use]
    pub fn at(subject: ProjectionSubject, now: DateTime<Utc>) -> Self {
        Self { subject, now }
    }
}

/// The exact authority available to a Run, AgentThread or ToolCall
/// (DOMAIN.md §6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProjection {
    /// The `cap_…` projection id.
    pub id: CanonicalId,
    /// The subject the projection authorizes.
    pub subject: ProjectionSubject,
    /// Layer inputs in the fixed order of DOMAIN.md §6.2.
    pub inputs: Vec<ProjectionInput>,
    /// The effective grants.
    pub grants: Vec<Grant>,
    /// When the projection was computed.
    pub computed_at: DateTime<Utc>,
    /// Earliest expiry across layer inputs and grants.
    pub expires_at: Option<DateTime<Utc>>,
    /// SHA-256 over `inputs`.
    pub inputs_digest: Digest,
}

impl CapabilityProjection {
    /// Build a projection, validating the fixed input order and computing
    /// `inputs_digest`.
    ///
    /// # Errors
    /// Returns [`CapabilityError::MalformedGrant`] when `inputs` is not in the fixed
    /// layer order.
    pub fn from_parts(
        id: CanonicalId,
        subject: ProjectionSubject,
        inputs: Vec<ProjectionInput>,
        grants: Vec<Grant>,
        computed_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<Self, CapabilityError> {
        if !inputs_in_canonical_order(&inputs) {
            return Err(CapabilityError::MalformedGrant(
                "projection inputs are not in the fixed layer order".to_string(),
            ));
        }
        let digest = inputs_digest(&inputs);
        Ok(Self {
            id,
            subject,
            inputs,
            grants,
            computed_at,
            expires_at,
            inputs_digest: digest,
        })
    }

    /// An empty projection over the inputs resolved so far; grants are always empty.
    #[must_use]
    pub fn empty_with_inputs(
        id: CanonicalId,
        subject: ProjectionSubject,
        inputs: Vec<ProjectionInput>,
        computed_at: DateTime<Utc>,
    ) -> Self {
        let digest = inputs_digest(&inputs);
        Self {
            id,
            subject,
            inputs,
            grants: Vec::new(),
            computed_at,
            expires_at: None,
            inputs_digest: digest,
        }
    }

    /// The input recorded for one layer, when it was resolved.
    #[must_use]
    pub fn input(&self, layer: Layer) -> Option<&ProjectionInput> {
        self.inputs.iter().find(|input| input.layer == layer)
    }

    /// Whether the projection is past `expires_at`.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }

    /// Whether the current inputs still produce this projection's `inputs_digest`.
    #[must_use]
    pub fn inputs_match(&self, current: &[ProjectionInput]) -> bool {
        inputs_digest(current) == self.inputs_digest
    }

    /// Why the projection can no longer authorize, if it cannot.
    #[must_use]
    pub fn staleness(
        &self,
        current_inputs: Option<&[ProjectionInput]>,
        now: DateTime<Utc>,
    ) -> Option<StaleReason> {
        if self.is_expired(now) {
            return Some(StaleReason::Expired {
                expires_at: self.expires_at.expect("is_expired implies expires_at"),
                checked_at: now,
            });
        }
        let current_inputs = current_inputs?;
        if !self.inputs_match(current_inputs) {
            return Some(StaleReason::InputsChanged {
                expected: self.inputs_digest.to_string(),
                actual: inputs_digest(current_inputs).to_string(),
            });
        }
        None
    }
}

/// The result of assembly: the projection plus any rejected widening attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembly {
    /// The computed projection (empty when assembly failed closed).
    pub projection: CapabilityProjection,
    /// Widening attempts to record as `capability.widening_rejected`.
    pub rejections: Vec<WideningRejection>,
}

impl Assembly {
    /// Whether the assembly produced any authority.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.projection.grants.is_empty()
    }
}

/// Assemble a projection through the fixed layer order (DOMAIN.md §6.2).
///
/// # Errors
/// Returns [`CapabilityError::InputsUnavailable`] when any layer cannot be resolved or a
/// policy gap exists at tier ≥ 1; the error carries an empty projection.
pub fn assemble<S: ProjectionSource + ?Sized>(
    source: &S,
    request: &ProjectionRequest,
) -> Result<Assembly, CapabilityError> {
    let mut generator = UlidGenerator::new();
    let projection_id = CanonicalId::generate(Prefix::CapabilityProjection, &mut generator);

    let mut inputs: Vec<ProjectionInput> = Vec::with_capacity(Layer::all().len());
    let mut running: Option<Vec<Grant>> = None;
    let mut rejections: Vec<WideningRejection> = Vec::new();
    let mut allowance = PolicyAllowance::default();
    let mut layer_expires_at: Option<DateTime<Utc>> = None;

    for layer in Layer::all() {
        let input = match source.fetch(layer) {
            Ok(input) => input,
            Err(reason) => {
                return Err(unavailable(&projection_id, request, &inputs, layer, reason));
            }
        };
        if let Some(expires_at) = input.expires_at {
            layer_expires_at = earliest(Some(expires_at), layer_expires_at);
        }
        inputs.push(ProjectionInput::new(
            layer,
            input.reference.clone(),
            input.digest.clone(),
        ));

        match input.payload {
            LayerPayload::None => {}
            LayerPayload::Grants(candidates) => {
                running = Some(match &running {
                    None => candidates,
                    Some(held) => {
                        let (narrowed, mut rejected) = narrow(held, &candidates, layer);
                        rejections.append(&mut rejected);
                        narrowed
                    }
                });
            }
            LayerPayload::Policy(document) => {
                let held = running.take().unwrap_or_default();
                match apply_policy(&held, &document, source) {
                    Ok((narrowed, policy_allowance)) => {
                        allowance = policy_allowance;
                        running = Some(narrowed);
                    }
                    Err(reason) => {
                        return Err(unavailable(&projection_id, request, &inputs, layer, reason));
                    }
                }
            }
            LayerPayload::UserRules(rules) => {
                let held = running.take().unwrap_or_default();
                match apply_user_rules(
                    &held,
                    &rules,
                    layer,
                    source,
                    &allowance,
                    request.now,
                    &mut rejections,
                ) {
                    Ok(narrowed) => running = Some(narrowed),
                    Err(reason) => {
                        return Err(unavailable(&projection_id, request, &inputs, layer, reason));
                    }
                }
            }
        }
    }

    let grants = running.unwrap_or_default();
    let expires_at = earliest(layer_expires_at, earliest_grant_expiry(&grants));
    let projection = CapabilityProjection::from_parts(
        projection_id,
        request.subject.clone(),
        inputs,
        grants,
        request.now,
        expires_at,
    )?;
    Ok(Assembly {
        projection,
        rejections,
    })
}

fn unavailable(
    projection_id: &CanonicalId,
    request: &ProjectionRequest,
    inputs: &[ProjectionInput],
    layer: Layer,
    reason: InputUnavailableReason,
) -> CapabilityError {
    let projection = CapabilityProjection::empty_with_inputs(
        *projection_id,
        request.subject.clone(),
        inputs.to_vec(),
        request.now,
    );
    CapabilityError::InputsUnavailable(Box::new(InputsUnavailable {
        layer,
        reason,
        projection,
    }))
}

/// The policy allowances observed while filtering, so user rules can honour
/// "where Policy permits".
#[derive(Debug, Clone, Default)]
struct PolicyAllowance {
    permitted: Vec<(EffectClass, ResourceSelector)>,
}

impl PolicyAllowance {
    fn permits(&self, grant: &Grant) -> bool {
        self.permitted.iter().any(|(effect_class, resource)| {
            effect_class == &grant.effect_class && resource.covers(&grant.resource)
        })
    }
}

fn apply_policy(
    held: &[Grant],
    document: &PolicyDocument,
    source: &(impl ProjectionSource + ?Sized),
) -> Result<(Vec<Grant>, PolicyAllowance), InputUnavailableReason> {
    let mut output = Vec::with_capacity(held.len());
    let mut allowance = PolicyAllowance::default();

    for grant in held {
        let matching: Vec<&PolicyRule> = document
            .rules
            .iter()
            .filter(|rule| rule.matches(grant))
            .collect();
        if matching.is_empty() {
            let tier = source.tier_of(&grant.effect_class).ok_or_else(|| {
                InputUnavailableReason::UnknownTier {
                    effect_class: grant.effect_class.clone(),
                }
            })?;
            if tier.get() >= 1 {
                return Err(InputUnavailableReason::PolicyGap {
                    effect_class: grant.effect_class.clone(),
                    tier,
                });
            }
            output.push(grant.clone());
            continue;
        }
        if matching
            .iter()
            .any(|rule| rule.decision == PolicyDecision::Deny)
        {
            continue;
        }
        let mut narrowed = grant.clone();
        for rule in matching {
            if let Some(tier_max) = rule.tier_max {
                narrowed.constraints.max_tier = Some(match narrowed.constraints.max_tier {
                    Some(current) => current.min(tier_max),
                    None => tier_max,
                });
            }
            if rule.decision == PolicyDecision::Ask {
                narrowed.constraints.approval = narrowed
                    .constraints
                    .approval
                    .most_restrictive(Approval::Ask);
            }
            if rule.decision == PolicyDecision::Allow {
                allowance
                    .permitted
                    .push((rule.effect_class.clone(), rule.resource.clone()));
            }
        }
        output.push(narrowed);
    }

    Ok((output, allowance))
}

fn apply_user_rules(
    held: &[Grant],
    rules: &[UserRule],
    layer: Layer,
    source: &(impl ProjectionSource + ?Sized),
    allowance: &PolicyAllowance,
    now: DateTime<Utc>,
    rejections: &mut Vec<WideningRejection>,
) -> Result<Vec<Grant>, InputUnavailableReason> {
    let mut output = Vec::with_capacity(held.len());

    for grant in held {
        let matching: Vec<&UserRule> = rules
            .iter()
            .filter(|rule| rule.matches(grant, now))
            .collect();
        if matching.is_empty() {
            output.push(grant.clone());
            continue;
        }
        if matching
            .iter()
            .any(|rule| rule.decision == UserRuleDecision::Never)
        {
            continue;
        }
        let mut narrowed = grant.clone();
        let mut permitted_always = false;
        for rule in matching {
            match rule.decision {
                UserRuleDecision::Never => {}
                UserRuleDecision::Ask => {
                    narrowed.constraints.approval = narrowed
                        .constraints
                        .approval
                        .most_restrictive(Approval::Ask);
                }
                UserRuleDecision::Always => {
                    let tier = source.tier_of(&grant.effect_class).ok_or_else(|| {
                        InputUnavailableReason::UnknownTier {
                            effect_class: grant.effect_class.clone(),
                        }
                    })?;
                    if tier.get() >= 4 {
                        rejections.push(WideningRejection::new(
                            layer,
                            always_grant(rule),
                            WideningReason::TierFourAlwaysRejected,
                        ));
                    } else if !allowance.permits(grant)
                        || narrowed.constraints.approval == Approval::Never
                    {
                        rejections.push(WideningRejection::new(
                            layer,
                            always_grant(rule),
                            WideningReason::ApprovalWideningNotPermitted,
                        ));
                    } else {
                        permitted_always = true;
                    }
                }
            }
        }
        if permitted_always {
            narrowed.constraints.approval = Approval::Always;
        }
        output.push(narrowed);
    }

    Ok(output)
}

fn always_grant(rule: &UserRule) -> Grant {
    Grant {
        effect_class: rule.effect_class.clone(),
        resource: rule.resource.clone(),
        constraints: Constraints {
            approval: Approval::Always,
            ..Constraints::default()
        },
    }
}

fn earliest(first: Option<DateTime<Utc>>, second: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (first, second) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn earliest_grant_expiry(grants: &[Grant]) -> Option<DateTime<Utc>> {
    grants
        .iter()
        .filter_map(|grant| grant.constraints.expires_at)
        .min()
}

fn digest_of<T: Serialize>(value: &T) -> Digest {
    Digest::of_canonical_json(&serde_json::to_string(value).expect("value is JSON-serializable"))
}

impl Serialize for ProjectionInput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ProjectionInput", 3)?;
        state.serialize_field("layer", &self.layer)?;
        state.serialize_field("ref", &self.reference)?;
        state.serialize_field("digest", &self.digest.to_string())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ProjectionInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            layer: Layer,
            #[serde(rename = "ref")]
            reference: String,
            digest: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        let digest = wire
            .digest
            .parse::<Digest>()
            .map_err(serde::de::Error::custom)?;
        Ok(Self::new(wire.layer, wire.reference, digest))
    }
}
