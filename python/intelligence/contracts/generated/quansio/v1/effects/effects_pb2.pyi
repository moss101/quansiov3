from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class EffectClass(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    EFFECT_CLASS_UNSPECIFIED: _ClassVar[EffectClass]
    EFFECT_CLASS_READ_INTERNAL: _ClassVar[EffectClass]
    EFFECT_CLASS_READ_EXTERNAL: _ClassVar[EffectClass]
    EFFECT_CLASS_FS_WRITE_WORKSPACE: _ClassVar[EffectClass]
    EFFECT_CLASS_FS_WRITE_HOST: _ClassVar[EffectClass]
    EFFECT_CLASS_PROCESS_EXEC_SANDBOXED: _ClassVar[EffectClass]
    EFFECT_CLASS_PROCESS_EXEC_HOST: _ClassVar[EffectClass]
    EFFECT_CLASS_NETWORK_EGRESS_NEW_DESTINATION: _ClassVar[EffectClass]
    EFFECT_CLASS_CONTENT_UPLOAD: _ClassVar[EffectClass]
    EFFECT_CLASS_RECORD_CREATE: _ClassVar[EffectClass]
    EFFECT_CLASS_RECORD_UPDATE: _ClassVar[EffectClass]
    EFFECT_CLASS_RECORD_DELETE: _ClassVar[EffectClass]
    EFFECT_CLASS_MESSAGE_SEND: _ClassVar[EffectClass]
    EFFECT_CLASS_CONTENT_PUBLISH: _ClassVar[EffectClass]
    EFFECT_CLASS_SCM_REMOTE_WRITE: _ClassVar[EffectClass]
    EFFECT_CLASS_COMPUTER_INPUT_PRIVILEGED: _ClassVar[EffectClass]
    EFFECT_CLASS_BROWSER_SESSION_IMPORT: _ClassVar[EffectClass]
    EFFECT_CLASS_PAYMENT_EXECUTE: _ClassVar[EffectClass]
    EFFECT_CLASS_DATA_UPLOAD_PROTECTED: _ClassVar[EffectClass]
    EFFECT_CLASS_CREDENTIAL_ACCESS: _ClassVar[EffectClass]
    EFFECT_CLASS_IDENTITY_CHANGE: _ClassVar[EffectClass]
    EFFECT_CLASS_MEMORY_WRITE: _ClassVar[EffectClass]
    EFFECT_CLASS_KNOWLEDGE_WRITE: _ClassVar[EffectClass]
    EFFECT_CLASS_SKILL_PROMOTE: _ClassVar[EffectClass]
    EFFECT_CLASS_PACK_PUBLISH: _ClassVar[EffectClass]
    EFFECT_CLASS_RUNTIME_CONTROL: _ClassVar[EffectClass]

class EffectTier(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    EFFECT_TIER_UNSPECIFIED: _ClassVar[EffectTier]
    EFFECT_TIER_0_READ: _ClassVar[EffectTier]
    EFFECT_TIER_1_INTERNAL_REVERSIBLE: _ClassVar[EffectTier]
    EFFECT_TIER_2_EXTERNAL_REVERSIBLE: _ClassVar[EffectTier]
    EFFECT_TIER_3_EXTERNAL_IRREVERSIBLE: _ClassVar[EffectTier]
    EFFECT_TIER_4_PROTECTED: _ClassVar[EffectTier]

class EffectStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    EFFECT_STATUS_UNSPECIFIED: _ClassVar[EffectStatus]
    EFFECT_STATUS_PROPOSED: _ClassVar[EffectStatus]
    EFFECT_STATUS_AUTHORIZED: _ClassVar[EffectStatus]
    EFFECT_STATUS_RESERVED: _ClassVar[EffectStatus]
    EFFECT_STATUS_DISPATCHED: _ClassVar[EffectStatus]
    EFFECT_STATUS_SETTLED_SUCCESS: _ClassVar[EffectStatus]
    EFFECT_STATUS_SETTLED_FAILED: _ClassVar[EffectStatus]
    EFFECT_STATUS_OUTCOME_UNKNOWN: _ClassVar[EffectStatus]
    EFFECT_STATUS_RECONCILING: _ClassVar[EffectStatus]
    EFFECT_STATUS_RECONCILED_SUCCESS: _ClassVar[EffectStatus]
    EFFECT_STATUS_RECONCILED_FAILED: _ClassVar[EffectStatus]
    EFFECT_STATUS_RECONCILIATION_MANUAL: _ClassVar[EffectStatus]
    EFFECT_STATUS_DENIED: _ClassVar[EffectStatus]
    EFFECT_STATUS_EXPIRED: _ClassVar[EffectStatus]
    EFFECT_STATUS_CANCELLED: _ClassVar[EffectStatus]

class ReconciliationStrategy(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    RECONCILIATION_STRATEGY_UNSPECIFIED: _ClassVar[ReconciliationStrategy]
    RECONCILIATION_STRATEGY_NONE: _ClassVar[ReconciliationStrategy]
    RECONCILIATION_STRATEGY_IDEMPOTENT: _ClassVar[ReconciliationStrategy]
    RECONCILIATION_STRATEGY_QUERY: _ClassVar[ReconciliationStrategy]
    RECONCILIATION_STRATEGY_MANUAL: _ClassVar[ReconciliationStrategy]
EFFECT_CLASS_UNSPECIFIED: EffectClass
EFFECT_CLASS_READ_INTERNAL: EffectClass
EFFECT_CLASS_READ_EXTERNAL: EffectClass
EFFECT_CLASS_FS_WRITE_WORKSPACE: EffectClass
EFFECT_CLASS_FS_WRITE_HOST: EffectClass
EFFECT_CLASS_PROCESS_EXEC_SANDBOXED: EffectClass
EFFECT_CLASS_PROCESS_EXEC_HOST: EffectClass
EFFECT_CLASS_NETWORK_EGRESS_NEW_DESTINATION: EffectClass
EFFECT_CLASS_CONTENT_UPLOAD: EffectClass
EFFECT_CLASS_RECORD_CREATE: EffectClass
EFFECT_CLASS_RECORD_UPDATE: EffectClass
EFFECT_CLASS_RECORD_DELETE: EffectClass
EFFECT_CLASS_MESSAGE_SEND: EffectClass
EFFECT_CLASS_CONTENT_PUBLISH: EffectClass
EFFECT_CLASS_SCM_REMOTE_WRITE: EffectClass
EFFECT_CLASS_COMPUTER_INPUT_PRIVILEGED: EffectClass
EFFECT_CLASS_BROWSER_SESSION_IMPORT: EffectClass
EFFECT_CLASS_PAYMENT_EXECUTE: EffectClass
EFFECT_CLASS_DATA_UPLOAD_PROTECTED: EffectClass
EFFECT_CLASS_CREDENTIAL_ACCESS: EffectClass
EFFECT_CLASS_IDENTITY_CHANGE: EffectClass
EFFECT_CLASS_MEMORY_WRITE: EffectClass
EFFECT_CLASS_KNOWLEDGE_WRITE: EffectClass
EFFECT_CLASS_SKILL_PROMOTE: EffectClass
EFFECT_CLASS_PACK_PUBLISH: EffectClass
EFFECT_CLASS_RUNTIME_CONTROL: EffectClass
EFFECT_TIER_UNSPECIFIED: EffectTier
EFFECT_TIER_0_READ: EffectTier
EFFECT_TIER_1_INTERNAL_REVERSIBLE: EffectTier
EFFECT_TIER_2_EXTERNAL_REVERSIBLE: EffectTier
EFFECT_TIER_3_EXTERNAL_IRREVERSIBLE: EffectTier
EFFECT_TIER_4_PROTECTED: EffectTier
EFFECT_STATUS_UNSPECIFIED: EffectStatus
EFFECT_STATUS_PROPOSED: EffectStatus
EFFECT_STATUS_AUTHORIZED: EffectStatus
EFFECT_STATUS_RESERVED: EffectStatus
EFFECT_STATUS_DISPATCHED: EffectStatus
EFFECT_STATUS_SETTLED_SUCCESS: EffectStatus
EFFECT_STATUS_SETTLED_FAILED: EffectStatus
EFFECT_STATUS_OUTCOME_UNKNOWN: EffectStatus
EFFECT_STATUS_RECONCILING: EffectStatus
EFFECT_STATUS_RECONCILED_SUCCESS: EffectStatus
EFFECT_STATUS_RECONCILED_FAILED: EffectStatus
EFFECT_STATUS_RECONCILIATION_MANUAL: EffectStatus
EFFECT_STATUS_DENIED: EffectStatus
EFFECT_STATUS_EXPIRED: EffectStatus
EFFECT_STATUS_CANCELLED: EffectStatus
RECONCILIATION_STRATEGY_UNSPECIFIED: ReconciliationStrategy
RECONCILIATION_STRATEGY_NONE: ReconciliationStrategy
RECONCILIATION_STRATEGY_IDEMPOTENT: ReconciliationStrategy
RECONCILIATION_STRATEGY_QUERY: ReconciliationStrategy
RECONCILIATION_STRATEGY_MANUAL: ReconciliationStrategy

class Resource(_message.Message):
    __slots__ = ("schema_version", "kind", "selector")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    SELECTOR_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: str
    selector: str
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[str] = ..., selector: _Optional[str] = ...) -> None: ...

class EffectOutcome(_message.Message):
    __slots__ = ("schema_version", "kind", "remote_ref", "evidence_ids")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    REMOTE_REF_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_IDS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: str
    remote_ref: str
    evidence_ids: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[str] = ..., remote_ref: _Optional[str] = ..., evidence_ids: _Optional[_Iterable[str]] = ...) -> None: ...

class Reconciliation(_message.Message):
    __slots__ = ("schema_version", "strategy", "attempts", "last_at", "result")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    STRATEGY_FIELD_NUMBER: _ClassVar[int]
    ATTEMPTS_FIELD_NUMBER: _ClassVar[int]
    LAST_AT_FIELD_NUMBER: _ClassVar[int]
    RESULT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    strategy: ReconciliationStrategy
    attempts: int
    last_at: str
    result: str
    def __init__(self, schema_version: _Optional[str] = ..., strategy: _Optional[_Union[ReconciliationStrategy, str]] = ..., attempts: _Optional[int] = ..., last_at: _Optional[str] = ..., result: _Optional[str] = ...) -> None: ...

class EffectRecord(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "run_id", "step_id", "tool_call_id", "effect_class", "tier", "resource", "params_digest", "idempotency_key", "capability_projection_id", "policy_decision_id", "approval_receipt_id", "status", "dispatch_token", "target_kind", "target_id", "reserved_at", "dispatched_at", "settled_at", "outcome", "reconciliation", "generation")
    class TargetKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        TARGET_KIND_UNSPECIFIED: _ClassVar[EffectRecord.TargetKind]
        TARGET_KIND_SERVER: _ClassVar[EffectRecord.TargetKind]
        TARGET_KIND_QWORKERD: _ClassVar[EffectRecord.TargetKind]
        TARGET_KIND_BROWSER: _ClassVar[EffectRecord.TargetKind]
        TARGET_KIND_ADAPTER: _ClassVar[EffectRecord.TargetKind]
    TARGET_KIND_UNSPECIFIED: EffectRecord.TargetKind
    TARGET_KIND_SERVER: EffectRecord.TargetKind
    TARGET_KIND_QWORKERD: EffectRecord.TargetKind
    TARGET_KIND_BROWSER: EffectRecord.TargetKind
    TARGET_KIND_ADAPTER: EffectRecord.TargetKind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    STEP_ID_FIELD_NUMBER: _ClassVar[int]
    TOOL_CALL_ID_FIELD_NUMBER: _ClassVar[int]
    EFFECT_CLASS_FIELD_NUMBER: _ClassVar[int]
    TIER_FIELD_NUMBER: _ClassVar[int]
    RESOURCE_FIELD_NUMBER: _ClassVar[int]
    PARAMS_DIGEST_FIELD_NUMBER: _ClassVar[int]
    IDEMPOTENCY_KEY_FIELD_NUMBER: _ClassVar[int]
    CAPABILITY_PROJECTION_ID_FIELD_NUMBER: _ClassVar[int]
    POLICY_DECISION_ID_FIELD_NUMBER: _ClassVar[int]
    APPROVAL_RECEIPT_ID_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    DISPATCH_TOKEN_FIELD_NUMBER: _ClassVar[int]
    TARGET_KIND_FIELD_NUMBER: _ClassVar[int]
    TARGET_ID_FIELD_NUMBER: _ClassVar[int]
    RESERVED_AT_FIELD_NUMBER: _ClassVar[int]
    DISPATCHED_AT_FIELD_NUMBER: _ClassVar[int]
    SETTLED_AT_FIELD_NUMBER: _ClassVar[int]
    OUTCOME_FIELD_NUMBER: _ClassVar[int]
    RECONCILIATION_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    run_id: str
    step_id: str
    tool_call_id: str
    effect_class: EffectClass
    tier: EffectTier
    resource: Resource
    params_digest: str
    idempotency_key: str
    capability_projection_id: str
    policy_decision_id: str
    approval_receipt_id: str
    status: EffectStatus
    dispatch_token: str
    target_kind: EffectRecord.TargetKind
    target_id: str
    reserved_at: str
    dispatched_at: str
    settled_at: str
    outcome: EffectOutcome
    reconciliation: Reconciliation
    generation: int
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., run_id: _Optional[str] = ..., step_id: _Optional[str] = ..., tool_call_id: _Optional[str] = ..., effect_class: _Optional[_Union[EffectClass, str]] = ..., tier: _Optional[_Union[EffectTier, str]] = ..., resource: _Optional[_Union[Resource, _Mapping]] = ..., params_digest: _Optional[str] = ..., idempotency_key: _Optional[str] = ..., capability_projection_id: _Optional[str] = ..., policy_decision_id: _Optional[str] = ..., approval_receipt_id: _Optional[str] = ..., status: _Optional[_Union[EffectStatus, str]] = ..., dispatch_token: _Optional[str] = ..., target_kind: _Optional[_Union[EffectRecord.TargetKind, str]] = ..., target_id: _Optional[str] = ..., reserved_at: _Optional[str] = ..., dispatched_at: _Optional[str] = ..., settled_at: _Optional[str] = ..., outcome: _Optional[_Union[EffectOutcome, _Mapping]] = ..., reconciliation: _Optional[_Union[Reconciliation, _Mapping]] = ..., generation: _Optional[int] = ...) -> None: ...
