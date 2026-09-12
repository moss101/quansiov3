from quansio.v1.core import identity_pb2 as _identity_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class WorkNodeKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    WORK_NODE_KIND_UNSPECIFIED: _ClassVar[WorkNodeKind]
    WORK_NODE_KIND_OBJECTIVE: _ClassVar[WorkNodeKind]
    WORK_NODE_KIND_TASK: _ClassVar[WorkNodeKind]
    WORK_NODE_KIND_SUBTASK: _ClassVar[WorkNodeKind]
    WORK_NODE_KIND_WAIT: _ClassVar[WorkNodeKind]
    WORK_NODE_KIND_MILESTONE: _ClassVar[WorkNodeKind]

class WorkNodeStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    WORK_NODE_STATUS_UNSPECIFIED: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_DRAFT: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_READY: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_BLOCKED: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_IN_PROGRESS: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_WAITING: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_VERIFYING: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_DONE: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_FAILED: _ClassVar[WorkNodeStatus]
    WORK_NODE_STATUS_CANCELLED: _ClassVar[WorkNodeStatus]

class Origin(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ORIGIN_UNSPECIFIED: _ClassVar[Origin]
    ORIGIN_USER: _ClassVar[Origin]
    ORIGIN_PLAN_PROPOSAL: _ClassVar[Origin]
    ORIGIN_ROUTINE: _ClassVar[Origin]
    ORIGIN_PACK: _ClassVar[Origin]
WORK_NODE_KIND_UNSPECIFIED: WorkNodeKind
WORK_NODE_KIND_OBJECTIVE: WorkNodeKind
WORK_NODE_KIND_TASK: WorkNodeKind
WORK_NODE_KIND_SUBTASK: WorkNodeKind
WORK_NODE_KIND_WAIT: WorkNodeKind
WORK_NODE_KIND_MILESTONE: WorkNodeKind
WORK_NODE_STATUS_UNSPECIFIED: WorkNodeStatus
WORK_NODE_STATUS_DRAFT: WorkNodeStatus
WORK_NODE_STATUS_READY: WorkNodeStatus
WORK_NODE_STATUS_BLOCKED: WorkNodeStatus
WORK_NODE_STATUS_IN_PROGRESS: WorkNodeStatus
WORK_NODE_STATUS_WAITING: WorkNodeStatus
WORK_NODE_STATUS_VERIFYING: WorkNodeStatus
WORK_NODE_STATUS_DONE: WorkNodeStatus
WORK_NODE_STATUS_FAILED: WorkNodeStatus
WORK_NODE_STATUS_CANCELLED: WorkNodeStatus
ORIGIN_UNSPECIFIED: Origin
ORIGIN_USER: Origin
ORIGIN_PLAN_PROPOSAL: Origin
ORIGIN_ROUTINE: Origin
ORIGIN_PACK: Origin

class CompletionCheck(_message.Message):
    __slots__ = ("schema_version", "kind", "artifact_role", "min_versions", "command", "expect_exit", "expr", "effect_classes", "min_coverage")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[CompletionCheck.Kind]
        KIND_ARTIFACT_EXISTS: _ClassVar[CompletionCheck.Kind]
        KIND_TEST_COMMAND: _ClassVar[CompletionCheck.Kind]
        KIND_ASSERTION: _ClassVar[CompletionCheck.Kind]
        KIND_EFFECTS_SETTLED: _ClassVar[CompletionCheck.Kind]
        KIND_CITATIONS_VALID: _ClassVar[CompletionCheck.Kind]
    KIND_UNSPECIFIED: CompletionCheck.Kind
    KIND_ARTIFACT_EXISTS: CompletionCheck.Kind
    KIND_TEST_COMMAND: CompletionCheck.Kind
    KIND_ASSERTION: CompletionCheck.Kind
    KIND_EFFECTS_SETTLED: CompletionCheck.Kind
    KIND_CITATIONS_VALID: CompletionCheck.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    ARTIFACT_ROLE_FIELD_NUMBER: _ClassVar[int]
    MIN_VERSIONS_FIELD_NUMBER: _ClassVar[int]
    COMMAND_FIELD_NUMBER: _ClassVar[int]
    EXPECT_EXIT_FIELD_NUMBER: _ClassVar[int]
    EXPR_FIELD_NUMBER: _ClassVar[int]
    EFFECT_CLASSES_FIELD_NUMBER: _ClassVar[int]
    MIN_COVERAGE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: CompletionCheck.Kind
    artifact_role: str
    min_versions: int
    command: str
    expect_exit: int
    expr: str
    effect_classes: _containers.RepeatedScalarFieldContainer[str]
    min_coverage: float
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[CompletionCheck.Kind, str]] = ..., artifact_role: _Optional[str] = ..., min_versions: _Optional[int] = ..., command: _Optional[str] = ..., expect_exit: _Optional[int] = ..., expr: _Optional[str] = ..., effect_classes: _Optional[_Iterable[str]] = ..., min_coverage: _Optional[float] = ...) -> None: ...

class SemanticVerification(_message.Message):
    __slots__ = ("schema_version", "required", "rubric_id", "independent_model")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    REQUIRED_FIELD_NUMBER: _ClassVar[int]
    RUBRIC_ID_FIELD_NUMBER: _ClassVar[int]
    INDEPENDENT_MODEL_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    required: bool
    rubric_id: str
    independent_model: bool
    def __init__(self, schema_version: _Optional[str] = ..., required: _Optional[bool] = ..., rubric_id: _Optional[str] = ..., independent_model: _Optional[bool] = ...) -> None: ...

class CompletionContract(_message.Message):
    __slots__ = ("schema_version", "deterministic_checks", "semantic_verification", "human_signoff_required")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    DETERMINISTIC_CHECKS_FIELD_NUMBER: _ClassVar[int]
    SEMANTIC_VERIFICATION_FIELD_NUMBER: _ClassVar[int]
    HUMAN_SIGNOFF_REQUIRED_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    deterministic_checks: _containers.RepeatedCompositeFieldContainer[CompletionCheck]
    semantic_verification: SemanticVerification
    human_signoff_required: bool
    def __init__(self, schema_version: _Optional[str] = ..., deterministic_checks: _Optional[_Iterable[_Union[CompletionCheck, _Mapping]]] = ..., semantic_verification: _Optional[_Union[SemanticVerification, _Mapping]] = ..., human_signoff_required: _Optional[bool] = ...) -> None: ...

class CapabilityNeed(_message.Message):
    __slots__ = ("schema_version", "effect_class", "resource_kind", "resource_selector", "max_tier")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    EFFECT_CLASS_FIELD_NUMBER: _ClassVar[int]
    RESOURCE_KIND_FIELD_NUMBER: _ClassVar[int]
    RESOURCE_SELECTOR_FIELD_NUMBER: _ClassVar[int]
    MAX_TIER_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    effect_class: str
    resource_kind: str
    resource_selector: str
    max_tier: int
    def __init__(self, schema_version: _Optional[str] = ..., effect_class: _Optional[str] = ..., resource_kind: _Optional[str] = ..., resource_selector: _Optional[str] = ..., max_tier: _Optional[int] = ...) -> None: ...

class Budget(_message.Message):
    __slots__ = ("schema_version", "id", "scope", "scope_ref", "tokens", "cost_minor_units", "wall_time_ms", "tool_calls", "concurrency", "machine_minutes", "max_steps", "consumed_tokens", "consumed_cost_minor_units", "consumed_wall_time_ms", "consumed_tool_calls", "parent_budget_id", "status")
    class Scope(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        SCOPE_UNSPECIFIED: _ClassVar[Budget.Scope]
        SCOPE_TENANT: _ClassVar[Budget.Scope]
        SCOPE_WORKSPACE: _ClassVar[Budget.Scope]
        SCOPE_RUN: _ClassVar[Budget.Scope]
        SCOPE_AGENT_THREAD: _ClassVar[Budget.Scope]
    SCOPE_UNSPECIFIED: Budget.Scope
    SCOPE_TENANT: Budget.Scope
    SCOPE_WORKSPACE: Budget.Scope
    SCOPE_RUN: Budget.Scope
    SCOPE_AGENT_THREAD: Budget.Scope
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[Budget.Status]
        STATUS_ACTIVE: _ClassVar[Budget.Status]
        STATUS_EXHAUSTED: _ClassVar[Budget.Status]
        STATUS_SUSPENDED: _ClassVar[Budget.Status]
    STATUS_UNSPECIFIED: Budget.Status
    STATUS_ACTIVE: Budget.Status
    STATUS_EXHAUSTED: Budget.Status
    STATUS_SUSPENDED: Budget.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    SCOPE_REF_FIELD_NUMBER: _ClassVar[int]
    TOKENS_FIELD_NUMBER: _ClassVar[int]
    COST_MINOR_UNITS_FIELD_NUMBER: _ClassVar[int]
    WALL_TIME_MS_FIELD_NUMBER: _ClassVar[int]
    TOOL_CALLS_FIELD_NUMBER: _ClassVar[int]
    CONCURRENCY_FIELD_NUMBER: _ClassVar[int]
    MACHINE_MINUTES_FIELD_NUMBER: _ClassVar[int]
    MAX_STEPS_FIELD_NUMBER: _ClassVar[int]
    CONSUMED_TOKENS_FIELD_NUMBER: _ClassVar[int]
    CONSUMED_COST_MINOR_UNITS_FIELD_NUMBER: _ClassVar[int]
    CONSUMED_WALL_TIME_MS_FIELD_NUMBER: _ClassVar[int]
    CONSUMED_TOOL_CALLS_FIELD_NUMBER: _ClassVar[int]
    PARENT_BUDGET_ID_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    scope: Budget.Scope
    scope_ref: str
    tokens: int
    cost_minor_units: int
    wall_time_ms: int
    tool_calls: int
    concurrency: int
    machine_minutes: int
    max_steps: int
    consumed_tokens: int
    consumed_cost_minor_units: int
    consumed_wall_time_ms: int
    consumed_tool_calls: int
    parent_budget_id: str
    status: Budget.Status
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., scope: _Optional[_Union[Budget.Scope, str]] = ..., scope_ref: _Optional[str] = ..., tokens: _Optional[int] = ..., cost_minor_units: _Optional[int] = ..., wall_time_ms: _Optional[int] = ..., tool_calls: _Optional[int] = ..., concurrency: _Optional[int] = ..., machine_minutes: _Optional[int] = ..., max_steps: _Optional[int] = ..., consumed_tokens: _Optional[int] = ..., consumed_cost_minor_units: _Optional[int] = ..., consumed_wall_time_ms: _Optional[int] = ..., consumed_tool_calls: _Optional[int] = ..., parent_budget_id: _Optional[str] = ..., status: _Optional[_Union[Budget.Status, str]] = ...) -> None: ...

class WorkNode(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "kind", "title", "description", "parent_id", "status", "owner_agent_thread_id", "created_by", "completion_contract", "capability_needs", "budget", "priority", "revision", "thread_id", "origin")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    TITLE_FIELD_NUMBER: _ClassVar[int]
    DESCRIPTION_FIELD_NUMBER: _ClassVar[int]
    PARENT_ID_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    OWNER_AGENT_THREAD_ID_FIELD_NUMBER: _ClassVar[int]
    CREATED_BY_FIELD_NUMBER: _ClassVar[int]
    COMPLETION_CONTRACT_FIELD_NUMBER: _ClassVar[int]
    CAPABILITY_NEEDS_FIELD_NUMBER: _ClassVar[int]
    BUDGET_FIELD_NUMBER: _ClassVar[int]
    PRIORITY_FIELD_NUMBER: _ClassVar[int]
    REVISION_FIELD_NUMBER: _ClassVar[int]
    THREAD_ID_FIELD_NUMBER: _ClassVar[int]
    ORIGIN_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    kind: WorkNodeKind
    title: str
    description: str
    parent_id: str
    status: WorkNodeStatus
    owner_agent_thread_id: str
    created_by: _identity_pb2.Actor
    completion_contract: CompletionContract
    capability_needs: _containers.RepeatedCompositeFieldContainer[CapabilityNeed]
    budget: Budget
    priority: int
    revision: int
    thread_id: str
    origin: Origin
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., kind: _Optional[_Union[WorkNodeKind, str]] = ..., title: _Optional[str] = ..., description: _Optional[str] = ..., parent_id: _Optional[str] = ..., status: _Optional[_Union[WorkNodeStatus, str]] = ..., owner_agent_thread_id: _Optional[str] = ..., created_by: _Optional[_Union[_identity_pb2.Actor, _Mapping]] = ..., completion_contract: _Optional[_Union[CompletionContract, _Mapping]] = ..., capability_needs: _Optional[_Iterable[_Union[CapabilityNeed, _Mapping]]] = ..., budget: _Optional[_Union[Budget, _Mapping]] = ..., priority: _Optional[int] = ..., revision: _Optional[int] = ..., thread_id: _Optional[str] = ..., origin: _Optional[_Union[Origin, str]] = ...) -> None: ...

class WorkEdge(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "from_node_id", "to_node_id", "kind", "revision")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[WorkEdge.Kind]
        KIND_DEPENDS_ON: _ClassVar[WorkEdge.Kind]
        KIND_PARENT_OF: _ClassVar[WorkEdge.Kind]
        KIND_PRODUCES_ARTIFACT: _ClassVar[WorkEdge.Kind]
        KIND_VERIFIED_BY: _ClassVar[WorkEdge.Kind]
        KIND_BLOCKED_BY: _ClassVar[WorkEdge.Kind]
    KIND_UNSPECIFIED: WorkEdge.Kind
    KIND_DEPENDS_ON: WorkEdge.Kind
    KIND_PARENT_OF: WorkEdge.Kind
    KIND_PRODUCES_ARTIFACT: WorkEdge.Kind
    KIND_VERIFIED_BY: WorkEdge.Kind
    KIND_BLOCKED_BY: WorkEdge.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    FROM_NODE_ID_FIELD_NUMBER: _ClassVar[int]
    TO_NODE_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    REVISION_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    from_node_id: str
    to_node_id: str
    kind: WorkEdge.Kind
    revision: int
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., from_node_id: _Optional[str] = ..., to_node_id: _Optional[str] = ..., kind: _Optional[_Union[WorkEdge.Kind, str]] = ..., revision: _Optional[int] = ...) -> None: ...

class PlanProposal(_message.Message):
    __slots__ = ("schema_version", "proposal_id", "run_id", "base_revision", "nodes_add", "nodes_update", "edges_add", "edges_remove", "rationale", "capability_needs")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    PROPOSAL_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    BASE_REVISION_FIELD_NUMBER: _ClassVar[int]
    NODES_ADD_FIELD_NUMBER: _ClassVar[int]
    NODES_UPDATE_FIELD_NUMBER: _ClassVar[int]
    EDGES_ADD_FIELD_NUMBER: _ClassVar[int]
    EDGES_REMOVE_FIELD_NUMBER: _ClassVar[int]
    RATIONALE_FIELD_NUMBER: _ClassVar[int]
    CAPABILITY_NEEDS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    proposal_id: str
    run_id: str
    base_revision: int
    nodes_add: _containers.RepeatedCompositeFieldContainer[WorkNode]
    nodes_update: _containers.RepeatedCompositeFieldContainer[WorkNode]
    edges_add: _containers.RepeatedCompositeFieldContainer[WorkEdge]
    edges_remove: _containers.RepeatedScalarFieldContainer[str]
    rationale: str
    capability_needs: _containers.RepeatedCompositeFieldContainer[CapabilityNeed]
    def __init__(self, schema_version: _Optional[str] = ..., proposal_id: _Optional[str] = ..., run_id: _Optional[str] = ..., base_revision: _Optional[int] = ..., nodes_add: _Optional[_Iterable[_Union[WorkNode, _Mapping]]] = ..., nodes_update: _Optional[_Iterable[_Union[WorkNode, _Mapping]]] = ..., edges_add: _Optional[_Iterable[_Union[WorkEdge, _Mapping]]] = ..., edges_remove: _Optional[_Iterable[str]] = ..., rationale: _Optional[str] = ..., capability_needs: _Optional[_Iterable[_Union[CapabilityNeed, _Mapping]]] = ...) -> None: ...
