from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class Grant(_message.Message):
    __slots__ = ("schema_version", "effect_class", "resource", "constraints")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    EFFECT_CLASS_FIELD_NUMBER: _ClassVar[int]
    RESOURCE_FIELD_NUMBER: _ClassVar[int]
    CONSTRAINTS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    effect_class: str
    resource: ResourceSelector
    constraints: Constraints
    def __init__(self, schema_version: _Optional[str] = ..., effect_class: _Optional[str] = ..., resource: _Optional[_Union[ResourceSelector, _Mapping]] = ..., constraints: _Optional[_Union[Constraints, _Mapping]] = ...) -> None: ...

class ResourceSelector(_message.Message):
    __slots__ = ("schema_version", "kind", "selector", "ports")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    SELECTOR_FIELD_NUMBER: _ClassVar[int]
    PORTS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: str
    selector: str
    ports: _containers.RepeatedScalarFieldContainer[int]
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[str] = ..., selector: _Optional[str] = ..., ports: _Optional[_Iterable[int]] = ...) -> None: ...

class Constraints(_message.Message):
    __slots__ = ("schema_version", "max_tier", "approval", "expires_at", "budget_ref")
    class Approval(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        APPROVAL_UNSPECIFIED: _ClassVar[Constraints.Approval]
        APPROVAL_ASK: _ClassVar[Constraints.Approval]
        APPROVAL_ALWAYS: _ClassVar[Constraints.Approval]
        APPROVAL_NEVER: _ClassVar[Constraints.Approval]
    APPROVAL_UNSPECIFIED: Constraints.Approval
    APPROVAL_ASK: Constraints.Approval
    APPROVAL_ALWAYS: Constraints.Approval
    APPROVAL_NEVER: Constraints.Approval
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    MAX_TIER_FIELD_NUMBER: _ClassVar[int]
    APPROVAL_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    BUDGET_REF_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    max_tier: int
    approval: Constraints.Approval
    expires_at: str
    budget_ref: str
    def __init__(self, schema_version: _Optional[str] = ..., max_tier: _Optional[int] = ..., approval: _Optional[_Union[Constraints.Approval, str]] = ..., expires_at: _Optional[str] = ..., budget_ref: _Optional[str] = ...) -> None: ...

class ProjectionInput(_message.Message):
    __slots__ = ("schema_version", "layer", "ref", "digest")
    class Layer(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        LAYER_UNSPECIFIED: _ClassVar[ProjectionInput.Layer]
        LAYER_PLATFORM: _ClassVar[ProjectionInput.Layer]
        LAYER_TENANT_POLICY: _ClassVar[ProjectionInput.Layer]
        LAYER_WORKSPACE_POLICY: _ClassVar[ProjectionInput.Layer]
        LAYER_USER_ROLE: _ClassVar[ProjectionInput.Layer]
        LAYER_AGENT_DEFINITION: _ClassVar[ProjectionInput.Layer]
        LAYER_DELEGATION: _ClassVar[ProjectionInput.Layer]
        LAYER_ACTIVE_SKILLS: _ClassVar[ProjectionInput.Layer]
        LAYER_TOOL_DECLARATION: _ClassVar[ProjectionInput.Layer]
        LAYER_EXECUTION_TARGET_CLASS: _ClassVar[ProjectionInput.Layer]
        LAYER_USER_RULES: _ClassVar[ProjectionInput.Layer]
    LAYER_UNSPECIFIED: ProjectionInput.Layer
    LAYER_PLATFORM: ProjectionInput.Layer
    LAYER_TENANT_POLICY: ProjectionInput.Layer
    LAYER_WORKSPACE_POLICY: ProjectionInput.Layer
    LAYER_USER_ROLE: ProjectionInput.Layer
    LAYER_AGENT_DEFINITION: ProjectionInput.Layer
    LAYER_DELEGATION: ProjectionInput.Layer
    LAYER_ACTIVE_SKILLS: ProjectionInput.Layer
    LAYER_TOOL_DECLARATION: ProjectionInput.Layer
    LAYER_EXECUTION_TARGET_CLASS: ProjectionInput.Layer
    LAYER_USER_RULES: ProjectionInput.Layer
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    LAYER_FIELD_NUMBER: _ClassVar[int]
    REF_FIELD_NUMBER: _ClassVar[int]
    DIGEST_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    layer: ProjectionInput.Layer
    ref: str
    digest: str
    def __init__(self, schema_version: _Optional[str] = ..., layer: _Optional[_Union[ProjectionInput.Layer, str]] = ..., ref: _Optional[str] = ..., digest: _Optional[str] = ...) -> None: ...

class CapabilityProjection(_message.Message):
    __slots__ = ("schema_version", "id", "subject_kind", "subject_id", "inputs", "grants", "computed_at", "expires_at", "inputs_digest")
    class SubjectKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        SUBJECT_KIND_UNSPECIFIED: _ClassVar[CapabilityProjection.SubjectKind]
        SUBJECT_KIND_RUN: _ClassVar[CapabilityProjection.SubjectKind]
        SUBJECT_KIND_AGENT_THREAD: _ClassVar[CapabilityProjection.SubjectKind]
        SUBJECT_KIND_TOOL_CALL: _ClassVar[CapabilityProjection.SubjectKind]
    SUBJECT_KIND_UNSPECIFIED: CapabilityProjection.SubjectKind
    SUBJECT_KIND_RUN: CapabilityProjection.SubjectKind
    SUBJECT_KIND_AGENT_THREAD: CapabilityProjection.SubjectKind
    SUBJECT_KIND_TOOL_CALL: CapabilityProjection.SubjectKind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    SUBJECT_KIND_FIELD_NUMBER: _ClassVar[int]
    SUBJECT_ID_FIELD_NUMBER: _ClassVar[int]
    INPUTS_FIELD_NUMBER: _ClassVar[int]
    GRANTS_FIELD_NUMBER: _ClassVar[int]
    COMPUTED_AT_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    INPUTS_DIGEST_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    subject_kind: CapabilityProjection.SubjectKind
    subject_id: str
    inputs: _containers.RepeatedCompositeFieldContainer[ProjectionInput]
    grants: _containers.RepeatedCompositeFieldContainer[Grant]
    computed_at: str
    expires_at: str
    inputs_digest: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., subject_kind: _Optional[_Union[CapabilityProjection.SubjectKind, str]] = ..., subject_id: _Optional[str] = ..., inputs: _Optional[_Iterable[_Union[ProjectionInput, _Mapping]]] = ..., grants: _Optional[_Iterable[_Union[Grant, _Mapping]]] = ..., computed_at: _Optional[str] = ..., expires_at: _Optional[str] = ..., inputs_digest: _Optional[str] = ...) -> None: ...

class UserRule(_message.Message):
    __slots__ = ("schema_version", "id", "user_id", "workspace_id", "effect_class", "resource", "decision", "expires_at")
    class Decision(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        DECISION_UNSPECIFIED: _ClassVar[UserRule.Decision]
        DECISION_ASK: _ClassVar[UserRule.Decision]
        DECISION_ALWAYS: _ClassVar[UserRule.Decision]
        DECISION_NEVER: _ClassVar[UserRule.Decision]
    DECISION_UNSPECIFIED: UserRule.Decision
    DECISION_ASK: UserRule.Decision
    DECISION_ALWAYS: UserRule.Decision
    DECISION_NEVER: UserRule.Decision
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    USER_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    EFFECT_CLASS_FIELD_NUMBER: _ClassVar[int]
    RESOURCE_FIELD_NUMBER: _ClassVar[int]
    DECISION_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    user_id: str
    workspace_id: str
    effect_class: str
    resource: ResourceSelector
    decision: UserRule.Decision
    expires_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., user_id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., effect_class: _Optional[str] = ..., resource: _Optional[_Union[ResourceSelector, _Mapping]] = ..., decision: _Optional[_Union[UserRule.Decision, str]] = ..., expires_at: _Optional[str] = ...) -> None: ...

class Policy(_message.Message):
    __slots__ = ("schema_version", "id", "scope", "rules", "question_default_ttl_seconds", "approval_default_ttl_seconds", "max_plan_nodes", "version")
    class Scope(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        SCOPE_UNSPECIFIED: _ClassVar[Policy.Scope]
        SCOPE_TENANT: _ClassVar[Policy.Scope]
        SCOPE_WORKSPACE: _ClassVar[Policy.Scope]
    SCOPE_UNSPECIFIED: Policy.Scope
    SCOPE_TENANT: Policy.Scope
    SCOPE_WORKSPACE: Policy.Scope
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    RULES_FIELD_NUMBER: _ClassVar[int]
    QUESTION_DEFAULT_TTL_SECONDS_FIELD_NUMBER: _ClassVar[int]
    APPROVAL_DEFAULT_TTL_SECONDS_FIELD_NUMBER: _ClassVar[int]
    MAX_PLAN_NODES_FIELD_NUMBER: _ClassVar[int]
    VERSION_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    scope: Policy.Scope
    rules: _containers.RepeatedCompositeFieldContainer[PolicyRule]
    question_default_ttl_seconds: int
    approval_default_ttl_seconds: int
    max_plan_nodes: int
    version: int
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., scope: _Optional[_Union[Policy.Scope, str]] = ..., rules: _Optional[_Iterable[_Union[PolicyRule, _Mapping]]] = ..., question_default_ttl_seconds: _Optional[int] = ..., approval_default_ttl_seconds: _Optional[int] = ..., max_plan_nodes: _Optional[int] = ..., version: _Optional[int] = ...) -> None: ...

class PolicyRule(_message.Message):
    __slots__ = ("schema_version", "effect_class", "resource", "decision", "trust_max", "tier_max", "data_classes", "time_window", "sequence_guards")
    class Decision(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        DECISION_UNSPECIFIED: _ClassVar[PolicyRule.Decision]
        DECISION_ALLOW: _ClassVar[PolicyRule.Decision]
        DECISION_ASK: _ClassVar[PolicyRule.Decision]
        DECISION_DENY: _ClassVar[PolicyRule.Decision]
    DECISION_UNSPECIFIED: PolicyRule.Decision
    DECISION_ALLOW: PolicyRule.Decision
    DECISION_ASK: PolicyRule.Decision
    DECISION_DENY: PolicyRule.Decision
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    EFFECT_CLASS_FIELD_NUMBER: _ClassVar[int]
    RESOURCE_FIELD_NUMBER: _ClassVar[int]
    DECISION_FIELD_NUMBER: _ClassVar[int]
    TRUST_MAX_FIELD_NUMBER: _ClassVar[int]
    TIER_MAX_FIELD_NUMBER: _ClassVar[int]
    DATA_CLASSES_FIELD_NUMBER: _ClassVar[int]
    TIME_WINDOW_FIELD_NUMBER: _ClassVar[int]
    SEQUENCE_GUARDS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    effect_class: str
    resource: ResourceSelector
    decision: PolicyRule.Decision
    trust_max: _containers.RepeatedScalarFieldContainer[str]
    tier_max: int
    data_classes: _containers.RepeatedScalarFieldContainer[str]
    time_window: str
    sequence_guards: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, schema_version: _Optional[str] = ..., effect_class: _Optional[str] = ..., resource: _Optional[_Union[ResourceSelector, _Mapping]] = ..., decision: _Optional[_Union[PolicyRule.Decision, str]] = ..., trust_max: _Optional[_Iterable[str]] = ..., tier_max: _Optional[int] = ..., data_classes: _Optional[_Iterable[str]] = ..., time_window: _Optional[str] = ..., sequence_guards: _Optional[_Iterable[str]] = ...) -> None: ...

class ApprovalRequest(_message.Message):
    __slots__ = ("schema_version", "id", "run_id", "effect_id", "requested_of", "summary", "consequence_preview_json", "params_digest", "capability_projection_id", "expires_at", "status")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[ApprovalRequest.Status]
        STATUS_REQUESTED: _ClassVar[ApprovalRequest.Status]
        STATUS_GRANTED: _ClassVar[ApprovalRequest.Status]
        STATUS_DENIED: _ClassVar[ApprovalRequest.Status]
        STATUS_EXPIRED: _ClassVar[ApprovalRequest.Status]
        STATUS_SUPERSEDED: _ClassVar[ApprovalRequest.Status]
    STATUS_UNSPECIFIED: ApprovalRequest.Status
    STATUS_REQUESTED: ApprovalRequest.Status
    STATUS_GRANTED: ApprovalRequest.Status
    STATUS_DENIED: ApprovalRequest.Status
    STATUS_EXPIRED: ApprovalRequest.Status
    STATUS_SUPERSEDED: ApprovalRequest.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    EFFECT_ID_FIELD_NUMBER: _ClassVar[int]
    REQUESTED_OF_FIELD_NUMBER: _ClassVar[int]
    SUMMARY_FIELD_NUMBER: _ClassVar[int]
    CONSEQUENCE_PREVIEW_JSON_FIELD_NUMBER: _ClassVar[int]
    PARAMS_DIGEST_FIELD_NUMBER: _ClassVar[int]
    CAPABILITY_PROJECTION_ID_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    run_id: str
    effect_id: str
    requested_of: _containers.RepeatedScalarFieldContainer[str]
    summary: str
    consequence_preview_json: str
    params_digest: str
    capability_projection_id: str
    expires_at: str
    status: ApprovalRequest.Status
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., run_id: _Optional[str] = ..., effect_id: _Optional[str] = ..., requested_of: _Optional[_Iterable[str]] = ..., summary: _Optional[str] = ..., consequence_preview_json: _Optional[str] = ..., params_digest: _Optional[str] = ..., capability_projection_id: _Optional[str] = ..., expires_at: _Optional[str] = ..., status: _Optional[_Union[ApprovalRequest.Status, str]] = ...) -> None: ...

class ApprovalReceipt(_message.Message):
    __slots__ = ("schema_version", "id", "request_id", "effect_id", "approver_user_id", "params_digest", "scope", "generation", "granted_at", "expires_at", "signature")
    class Scope(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        SCOPE_UNSPECIFIED: _ClassVar[ApprovalReceipt.Scope]
        SCOPE_SINGLE_USE: _ClassVar[ApprovalReceipt.Scope]
    SCOPE_UNSPECIFIED: ApprovalReceipt.Scope
    SCOPE_SINGLE_USE: ApprovalReceipt.Scope
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    REQUEST_ID_FIELD_NUMBER: _ClassVar[int]
    EFFECT_ID_FIELD_NUMBER: _ClassVar[int]
    APPROVER_USER_ID_FIELD_NUMBER: _ClassVar[int]
    PARAMS_DIGEST_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    GRANTED_AT_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    SIGNATURE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    request_id: str
    effect_id: str
    approver_user_id: str
    params_digest: str
    scope: ApprovalReceipt.Scope
    generation: int
    granted_at: str
    expires_at: str
    signature: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., request_id: _Optional[str] = ..., effect_id: _Optional[str] = ..., approver_user_id: _Optional[str] = ..., params_digest: _Optional[str] = ..., scope: _Optional[_Union[ApprovalReceipt.Scope, str]] = ..., generation: _Optional[int] = ..., granted_at: _Optional[str] = ..., expires_at: _Optional[str] = ..., signature: _Optional[str] = ...) -> None: ...
