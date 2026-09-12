from quansio.v1.core import errors_pb2 as _errors_pb2
from quansio.v1.work import work_pb2 as _work_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class AgentThreadStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    AGENT_THREAD_STATUS_UNSPECIFIED: _ClassVar[AgentThreadStatus]
    AGENT_THREAD_STATUS_PROVISIONED: _ClassVar[AgentThreadStatus]
    AGENT_THREAD_STATUS_ACTIVE: _ClassVar[AgentThreadStatus]
    AGENT_THREAD_STATUS_SUSPENDED: _ClassVar[AgentThreadStatus]
    AGENT_THREAD_STATUS_HANDING_OFF: _ClassVar[AgentThreadStatus]
    AGENT_THREAD_STATUS_HANDED_OFF: _ClassVar[AgentThreadStatus]
    AGENT_THREAD_STATUS_JOINING: _ClassVar[AgentThreadStatus]
    AGENT_THREAD_STATUS_JOINED: _ClassVar[AgentThreadStatus]
    AGENT_THREAD_STATUS_TERMINATED: _ClassVar[AgentThreadStatus]

class RunStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    RUN_STATUS_UNSPECIFIED: _ClassVar[RunStatus]
    RUN_STATUS_CREATED: _ClassVar[RunStatus]
    RUN_STATUS_QUEUED: _ClassVar[RunStatus]
    RUN_STATUS_RUNNING: _ClassVar[RunStatus]
    RUN_STATUS_WAITING_APPROVAL: _ClassVar[RunStatus]
    RUN_STATUS_WAITING_QUESTION: _ClassVar[RunStatus]
    RUN_STATUS_WAITING_EVENT: _ClassVar[RunStatus]
    RUN_STATUS_WAITING_TIMER: _ClassVar[RunStatus]
    RUN_STATUS_WAITING_CHILD: _ClassVar[RunStatus]
    RUN_STATUS_WAITING_TAKEOVER: _ClassVar[RunStatus]
    RUN_STATUS_VERIFYING: _ClassVar[RunStatus]
    RUN_STATUS_SUCCEEDED: _ClassVar[RunStatus]
    RUN_STATUS_FAILED: _ClassVar[RunStatus]
    RUN_STATUS_CANCELLED: _ClassVar[RunStatus]
    RUN_STATUS_BLOCKED_UNRECOVERABLE: _ClassVar[RunStatus]
    RUN_STATUS_SUSPENDED: _ClassVar[RunStatus]

class StepKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    STEP_KIND_UNSPECIFIED: _ClassVar[StepKind]
    STEP_KIND_MODEL_CALL: _ClassVar[StepKind]
    STEP_KIND_TOOL_CALL: _ClassVar[StepKind]
    STEP_KIND_DELEGATE: _ClassVar[StepKind]
    STEP_KIND_WAIT: _ClassVar[StepKind]
    STEP_KIND_VERIFY: _ClassVar[StepKind]
    STEP_KIND_CHECKPOINT: _ClassVar[StepKind]
    STEP_KIND_COMPACT: _ClassVar[StepKind]

class StepStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    STEP_STATUS_UNSPECIFIED: _ClassVar[StepStatus]
    STEP_STATUS_PENDING: _ClassVar[StepStatus]
    STEP_STATUS_DISPATCHED: _ClassVar[StepStatus]
    STEP_STATUS_COMPLETED: _ClassVar[StepStatus]
    STEP_STATUS_FAILED: _ClassVar[StepStatus]
    STEP_STATUS_CANCELLED: _ClassVar[StepStatus]
    STEP_STATUS_UNKNOWN: _ClassVar[StepStatus]
AGENT_THREAD_STATUS_UNSPECIFIED: AgentThreadStatus
AGENT_THREAD_STATUS_PROVISIONED: AgentThreadStatus
AGENT_THREAD_STATUS_ACTIVE: AgentThreadStatus
AGENT_THREAD_STATUS_SUSPENDED: AgentThreadStatus
AGENT_THREAD_STATUS_HANDING_OFF: AgentThreadStatus
AGENT_THREAD_STATUS_HANDED_OFF: AgentThreadStatus
AGENT_THREAD_STATUS_JOINING: AgentThreadStatus
AGENT_THREAD_STATUS_JOINED: AgentThreadStatus
AGENT_THREAD_STATUS_TERMINATED: AgentThreadStatus
RUN_STATUS_UNSPECIFIED: RunStatus
RUN_STATUS_CREATED: RunStatus
RUN_STATUS_QUEUED: RunStatus
RUN_STATUS_RUNNING: RunStatus
RUN_STATUS_WAITING_APPROVAL: RunStatus
RUN_STATUS_WAITING_QUESTION: RunStatus
RUN_STATUS_WAITING_EVENT: RunStatus
RUN_STATUS_WAITING_TIMER: RunStatus
RUN_STATUS_WAITING_CHILD: RunStatus
RUN_STATUS_WAITING_TAKEOVER: RunStatus
RUN_STATUS_VERIFYING: RunStatus
RUN_STATUS_SUCCEEDED: RunStatus
RUN_STATUS_FAILED: RunStatus
RUN_STATUS_CANCELLED: RunStatus
RUN_STATUS_BLOCKED_UNRECOVERABLE: RunStatus
RUN_STATUS_SUSPENDED: RunStatus
STEP_KIND_UNSPECIFIED: StepKind
STEP_KIND_MODEL_CALL: StepKind
STEP_KIND_TOOL_CALL: StepKind
STEP_KIND_DELEGATE: StepKind
STEP_KIND_WAIT: StepKind
STEP_KIND_VERIFY: StepKind
STEP_KIND_CHECKPOINT: StepKind
STEP_KIND_COMPACT: StepKind
STEP_STATUS_UNSPECIFIED: StepStatus
STEP_STATUS_PENDING: StepStatus
STEP_STATUS_DISPATCHED: StepStatus
STEP_STATUS_COMPLETED: StepStatus
STEP_STATUS_FAILED: StepStatus
STEP_STATUS_CANCELLED: StepStatus
STEP_STATUS_UNKNOWN: StepStatus

class AgentThread(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "agent_kind", "definition_id", "parent_id", "work_node_id", "capability_projection_id", "generation", "status", "mailbox_cursor", "execution_target_id", "budget_id", "suspended_reason", "handoff_to_agent_thread_id", "handoff_at")
    class AgentKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        AGENT_KIND_UNSPECIFIED: _ClassVar[AgentThread.AgentKind]
        AGENT_KIND_TEAMMATE: _ClassVar[AgentThread.AgentKind]
        AGENT_KIND_WORKER: _ClassVar[AgentThread.AgentKind]
    AGENT_KIND_UNSPECIFIED: AgentThread.AgentKind
    AGENT_KIND_TEAMMATE: AgentThread.AgentKind
    AGENT_KIND_WORKER: AgentThread.AgentKind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    AGENT_KIND_FIELD_NUMBER: _ClassVar[int]
    DEFINITION_ID_FIELD_NUMBER: _ClassVar[int]
    PARENT_ID_FIELD_NUMBER: _ClassVar[int]
    WORK_NODE_ID_FIELD_NUMBER: _ClassVar[int]
    CAPABILITY_PROJECTION_ID_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    MAILBOX_CURSOR_FIELD_NUMBER: _ClassVar[int]
    EXECUTION_TARGET_ID_FIELD_NUMBER: _ClassVar[int]
    BUDGET_ID_FIELD_NUMBER: _ClassVar[int]
    SUSPENDED_REASON_FIELD_NUMBER: _ClassVar[int]
    HANDOFF_TO_AGENT_THREAD_ID_FIELD_NUMBER: _ClassVar[int]
    HANDOFF_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    agent_kind: AgentThread.AgentKind
    definition_id: str
    parent_id: str
    work_node_id: str
    capability_projection_id: str
    generation: int
    status: AgentThreadStatus
    mailbox_cursor: str
    execution_target_id: str
    budget_id: str
    suspended_reason: str
    handoff_to_agent_thread_id: str
    handoff_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., agent_kind: _Optional[_Union[AgentThread.AgentKind, str]] = ..., definition_id: _Optional[str] = ..., parent_id: _Optional[str] = ..., work_node_id: _Optional[str] = ..., capability_projection_id: _Optional[str] = ..., generation: _Optional[int] = ..., status: _Optional[_Union[AgentThreadStatus, str]] = ..., mailbox_cursor: _Optional[str] = ..., execution_target_id: _Optional[str] = ..., budget_id: _Optional[str] = ..., suspended_reason: _Optional[str] = ..., handoff_to_agent_thread_id: _Optional[str] = ..., handoff_at: _Optional[str] = ...) -> None: ...

class Run(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "work_node_id", "agent_thread_id", "generation", "status", "trigger_kind", "trigger_ref", "current_turn_id", "budget_snapshot", "started_at", "ended_at", "terminal_reason", "execution_target_id")
    class TriggerKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        TRIGGER_KIND_UNSPECIFIED: _ClassVar[Run.TriggerKind]
        TRIGGER_KIND_MESSAGE: _ClassVar[Run.TriggerKind]
        TRIGGER_KIND_ROUTINE: _ClassVar[Run.TriggerKind]
        TRIGGER_KIND_WAKE: _ClassVar[Run.TriggerKind]
        TRIGGER_KIND_CHILD_RESULT: _ClassVar[Run.TriggerKind]
        TRIGGER_KIND_MANUAL: _ClassVar[Run.TriggerKind]
    TRIGGER_KIND_UNSPECIFIED: Run.TriggerKind
    TRIGGER_KIND_MESSAGE: Run.TriggerKind
    TRIGGER_KIND_ROUTINE: Run.TriggerKind
    TRIGGER_KIND_WAKE: Run.TriggerKind
    TRIGGER_KIND_CHILD_RESULT: Run.TriggerKind
    TRIGGER_KIND_MANUAL: Run.TriggerKind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    WORK_NODE_ID_FIELD_NUMBER: _ClassVar[int]
    AGENT_THREAD_ID_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    TRIGGER_KIND_FIELD_NUMBER: _ClassVar[int]
    TRIGGER_REF_FIELD_NUMBER: _ClassVar[int]
    CURRENT_TURN_ID_FIELD_NUMBER: _ClassVar[int]
    BUDGET_SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    STARTED_AT_FIELD_NUMBER: _ClassVar[int]
    ENDED_AT_FIELD_NUMBER: _ClassVar[int]
    TERMINAL_REASON_FIELD_NUMBER: _ClassVar[int]
    EXECUTION_TARGET_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    work_node_id: str
    agent_thread_id: str
    generation: int
    status: RunStatus
    trigger_kind: Run.TriggerKind
    trigger_ref: str
    current_turn_id: str
    budget_snapshot: _work_pb2.Budget
    started_at: str
    ended_at: str
    terminal_reason: str
    execution_target_id: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., work_node_id: _Optional[str] = ..., agent_thread_id: _Optional[str] = ..., generation: _Optional[int] = ..., status: _Optional[_Union[RunStatus, str]] = ..., trigger_kind: _Optional[_Union[Run.TriggerKind, str]] = ..., trigger_ref: _Optional[str] = ..., current_turn_id: _Optional[str] = ..., budget_snapshot: _Optional[_Union[_work_pb2.Budget, _Mapping]] = ..., started_at: _Optional[str] = ..., ended_at: _Optional[str] = ..., terminal_reason: _Optional[str] = ..., execution_target_id: _Optional[str] = ...) -> None: ...

class Turn(_message.Message):
    __slots__ = ("schema_version", "id", "run_id", "seq", "input_kind", "input_ref", "context_projection_id", "status", "step_count", "started_at", "ended_at")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[Turn.Status]
        STATUS_ACTIVE: _ClassVar[Turn.Status]
        STATUS_COMPLETED: _ClassVar[Turn.Status]
        STATUS_ABORTED: _ClassVar[Turn.Status]
    STATUS_UNSPECIFIED: Turn.Status
    STATUS_ACTIVE: Turn.Status
    STATUS_COMPLETED: Turn.Status
    STATUS_ABORTED: Turn.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    INPUT_KIND_FIELD_NUMBER: _ClassVar[int]
    INPUT_REF_FIELD_NUMBER: _ClassVar[int]
    CONTEXT_PROJECTION_ID_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    STEP_COUNT_FIELD_NUMBER: _ClassVar[int]
    STARTED_AT_FIELD_NUMBER: _ClassVar[int]
    ENDED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    run_id: str
    seq: int
    input_kind: str
    input_ref: str
    context_projection_id: str
    status: Turn.Status
    step_count: int
    started_at: str
    ended_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., run_id: _Optional[str] = ..., seq: _Optional[int] = ..., input_kind: _Optional[str] = ..., input_ref: _Optional[str] = ..., context_projection_id: _Optional[str] = ..., status: _Optional[_Union[Turn.Status, str]] = ..., step_count: _Optional[int] = ..., started_at: _Optional[str] = ..., ended_at: _Optional[str] = ...) -> None: ...

class Step(_message.Message):
    __slots__ = ("schema_version", "id", "turn_id", "seq", "kind", "status", "ref", "evidence_ids", "effect_id")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TURN_ID_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    REF_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_IDS_FIELD_NUMBER: _ClassVar[int]
    EFFECT_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    turn_id: str
    seq: int
    kind: StepKind
    status: StepStatus
    ref: str
    evidence_ids: _containers.RepeatedScalarFieldContainer[str]
    effect_id: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., turn_id: _Optional[str] = ..., seq: _Optional[int] = ..., kind: _Optional[_Union[StepKind, str]] = ..., status: _Optional[_Union[StepStatus, str]] = ..., ref: _Optional[str] = ..., evidence_ids: _Optional[_Iterable[str]] = ..., effect_id: _Optional[str] = ...) -> None: ...

class Attempt(_message.Message):
    __slots__ = ("schema_version", "id", "step_id", "seq", "generation", "status", "dispatched_at", "finished_at", "error")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[Attempt.Status]
        STATUS_STARTED: _ClassVar[Attempt.Status]
        STATUS_SUCCEEDED: _ClassVar[Attempt.Status]
        STATUS_FAILED: _ClassVar[Attempt.Status]
        STATUS_TIMED_OUT: _ClassVar[Attempt.Status]
        STATUS_FENCED: _ClassVar[Attempt.Status]
    STATUS_UNSPECIFIED: Attempt.Status
    STATUS_STARTED: Attempt.Status
    STATUS_SUCCEEDED: Attempt.Status
    STATUS_FAILED: Attempt.Status
    STATUS_TIMED_OUT: Attempt.Status
    STATUS_FENCED: Attempt.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    STEP_ID_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    DISPATCHED_AT_FIELD_NUMBER: _ClassVar[int]
    FINISHED_AT_FIELD_NUMBER: _ClassVar[int]
    ERROR_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    step_id: str
    seq: int
    generation: int
    status: Attempt.Status
    dispatched_at: str
    finished_at: str
    error: _errors_pb2.Error
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., step_id: _Optional[str] = ..., seq: _Optional[int] = ..., generation: _Optional[int] = ..., status: _Optional[_Union[Attempt.Status, str]] = ..., dispatched_at: _Optional[str] = ..., finished_at: _Optional[str] = ..., error: _Optional[_Union[_errors_pb2.Error, _Mapping]] = ...) -> None: ...

class ProtocolState(_message.Message):
    __slots__ = ("schema_version", "run_id", "generation", "pending_model_call_json", "pending_tool_calls_json", "pending_approvals", "open_questions", "waits", "browser_control", "terminal_sessions", "child_agent_threads", "cancellation_requested", "cancellation_at", "last_compaction_epoch_id", "updated_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    PENDING_MODEL_CALL_JSON_FIELD_NUMBER: _ClassVar[int]
    PENDING_TOOL_CALLS_JSON_FIELD_NUMBER: _ClassVar[int]
    PENDING_APPROVALS_FIELD_NUMBER: _ClassVar[int]
    OPEN_QUESTIONS_FIELD_NUMBER: _ClassVar[int]
    WAITS_FIELD_NUMBER: _ClassVar[int]
    BROWSER_CONTROL_FIELD_NUMBER: _ClassVar[int]
    TERMINAL_SESSIONS_FIELD_NUMBER: _ClassVar[int]
    CHILD_AGENT_THREADS_FIELD_NUMBER: _ClassVar[int]
    CANCELLATION_REQUESTED_FIELD_NUMBER: _ClassVar[int]
    CANCELLATION_AT_FIELD_NUMBER: _ClassVar[int]
    LAST_COMPACTION_EPOCH_ID_FIELD_NUMBER: _ClassVar[int]
    UPDATED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    run_id: str
    generation: int
    pending_model_call_json: str
    pending_tool_calls_json: _containers.RepeatedScalarFieldContainer[str]
    pending_approvals: _containers.RepeatedScalarFieldContainer[str]
    open_questions: _containers.RepeatedScalarFieldContainer[str]
    waits: _containers.RepeatedCompositeFieldContainer[Wait]
    browser_control: BrowserControl
    terminal_sessions: _containers.RepeatedCompositeFieldContainer[TerminalSessionRef]
    child_agent_threads: _containers.RepeatedScalarFieldContainer[str]
    cancellation_requested: bool
    cancellation_at: str
    last_compaction_epoch_id: str
    updated_at: str
    def __init__(self, schema_version: _Optional[str] = ..., run_id: _Optional[str] = ..., generation: _Optional[int] = ..., pending_model_call_json: _Optional[str] = ..., pending_tool_calls_json: _Optional[_Iterable[str]] = ..., pending_approvals: _Optional[_Iterable[str]] = ..., open_questions: _Optional[_Iterable[str]] = ..., waits: _Optional[_Iterable[_Union[Wait, _Mapping]]] = ..., browser_control: _Optional[_Union[BrowserControl, _Mapping]] = ..., terminal_sessions: _Optional[_Iterable[_Union[TerminalSessionRef, _Mapping]]] = ..., child_agent_threads: _Optional[_Iterable[str]] = ..., cancellation_requested: _Optional[bool] = ..., cancellation_at: _Optional[str] = ..., last_compaction_epoch_id: _Optional[str] = ..., updated_at: _Optional[str] = ...) -> None: ...

class Wait(_message.Message):
    __slots__ = ("schema_version", "kind", "key", "expires_at")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Wait.Kind]
        KIND_APPROVAL: _ClassVar[Wait.Kind]
        KIND_QUESTION: _ClassVar[Wait.Kind]
        KIND_EVENT: _ClassVar[Wait.Kind]
        KIND_TIMER: _ClassVar[Wait.Kind]
        KIND_CHILD: _ClassVar[Wait.Kind]
        KIND_TAKEOVER: _ClassVar[Wait.Kind]
    KIND_UNSPECIFIED: Wait.Kind
    KIND_APPROVAL: Wait.Kind
    KIND_QUESTION: Wait.Kind
    KIND_EVENT: Wait.Kind
    KIND_TIMER: Wait.Kind
    KIND_CHILD: Wait.Kind
    KIND_TAKEOVER: Wait.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: Wait.Kind
    key: str
    expires_at: str
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[Wait.Kind, str]] = ..., key: _Optional[str] = ..., expires_at: _Optional[str] = ...) -> None: ...

class BrowserControl(_message.Message):
    __slots__ = ("schema_version", "session_id", "holder", "since")
    class Holder(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        HOLDER_UNSPECIFIED: _ClassVar[BrowserControl.Holder]
        HOLDER_AGENT: _ClassVar[BrowserControl.Holder]
        HOLDER_USER: _ClassVar[BrowserControl.Holder]
    HOLDER_UNSPECIFIED: BrowserControl.Holder
    HOLDER_AGENT: BrowserControl.Holder
    HOLDER_USER: BrowserControl.Holder
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    HOLDER_FIELD_NUMBER: _ClassVar[int]
    SINCE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    session_id: str
    holder: BrowserControl.Holder
    since: str
    def __init__(self, schema_version: _Optional[str] = ..., session_id: _Optional[str] = ..., holder: _Optional[_Union[BrowserControl.Holder, str]] = ..., since: _Optional[str] = ...) -> None: ...

class TerminalSessionRef(_message.Message):
    __slots__ = ("schema_version", "id", "cursor")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    CURSOR_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    cursor: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., cursor: _Optional[str] = ...) -> None: ...

class Checkpoint(_message.Message):
    __slots__ = ("schema_version", "id", "execution_target_id", "run_id", "kind", "storage_ref", "generation", "size_bytes", "created_at", "expires_at", "restore_policy")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Checkpoint.Kind]
        KIND_WORKSPACE_FILES: _ClassVar[Checkpoint.Kind]
        KIND_BROWSER_SESSION: _ClassVar[Checkpoint.Kind]
        KIND_TERMINAL: _ClassVar[Checkpoint.Kind]
        KIND_FULL: _ClassVar[Checkpoint.Kind]
    KIND_UNSPECIFIED: Checkpoint.Kind
    KIND_WORKSPACE_FILES: Checkpoint.Kind
    KIND_BROWSER_SESSION: Checkpoint.Kind
    KIND_TERMINAL: Checkpoint.Kind
    KIND_FULL: Checkpoint.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    EXECUTION_TARGET_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    STORAGE_REF_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    SIZE_BYTES_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    RESTORE_POLICY_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    execution_target_id: str
    run_id: str
    kind: Checkpoint.Kind
    storage_ref: str
    generation: int
    size_bytes: int
    created_at: str
    expires_at: str
    restore_policy: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., execution_target_id: _Optional[str] = ..., run_id: _Optional[str] = ..., kind: _Optional[_Union[Checkpoint.Kind, str]] = ..., storage_ref: _Optional[str] = ..., generation: _Optional[int] = ..., size_bytes: _Optional[int] = ..., created_at: _Optional[str] = ..., expires_at: _Optional[str] = ..., restore_policy: _Optional[str] = ...) -> None: ...

class CompactionEpoch(_message.Message):
    __slots__ = ("schema_version", "id", "thread_id", "run_id", "seq", "source_from_sequence", "source_to_sequence", "summary_artifact_id", "token_estimate", "status", "created_by_model_route_id")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[CompactionEpoch.Status]
        STATUS_PENDING: _ClassVar[CompactionEpoch.Status]
        STATUS_INSTALLED: _ClassVar[CompactionEpoch.Status]
        STATUS_REJECTED_STALE: _ClassVar[CompactionEpoch.Status]
    STATUS_UNSPECIFIED: CompactionEpoch.Status
    STATUS_PENDING: CompactionEpoch.Status
    STATUS_INSTALLED: CompactionEpoch.Status
    STATUS_REJECTED_STALE: CompactionEpoch.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    THREAD_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    SOURCE_FROM_SEQUENCE_FIELD_NUMBER: _ClassVar[int]
    SOURCE_TO_SEQUENCE_FIELD_NUMBER: _ClassVar[int]
    SUMMARY_ARTIFACT_ID_FIELD_NUMBER: _ClassVar[int]
    TOKEN_ESTIMATE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    CREATED_BY_MODEL_ROUTE_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    thread_id: str
    run_id: str
    seq: int
    source_from_sequence: int
    source_to_sequence: int
    summary_artifact_id: str
    token_estimate: int
    status: CompactionEpoch.Status
    created_by_model_route_id: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., thread_id: _Optional[str] = ..., run_id: _Optional[str] = ..., seq: _Optional[int] = ..., source_from_sequence: _Optional[int] = ..., source_to_sequence: _Optional[int] = ..., summary_artifact_id: _Optional[str] = ..., token_estimate: _Optional[int] = ..., status: _Optional[_Union[CompactionEpoch.Status, str]] = ..., created_by_model_route_id: _Optional[str] = ...) -> None: ...
