from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class RoutineTrigger(_message.Message):
    __slots__ = ("schema_version", "kind", "spec", "timezone")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[RoutineTrigger.Kind]
        KIND_CRON: _ClassVar[RoutineTrigger.Kind]
        KIND_INTERVAL: _ClassVar[RoutineTrigger.Kind]
        KIND_EVENT: _ClassVar[RoutineTrigger.Kind]
    KIND_UNSPECIFIED: RoutineTrigger.Kind
    KIND_CRON: RoutineTrigger.Kind
    KIND_INTERVAL: RoutineTrigger.Kind
    KIND_EVENT: RoutineTrigger.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    SPEC_FIELD_NUMBER: _ClassVar[int]
    TIMEZONE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: RoutineTrigger.Kind
    spec: str
    timezone: str
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[RoutineTrigger.Kind, str]] = ..., spec: _Optional[str] = ..., timezone: _Optional[str] = ...) -> None: ...

class Routine(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "owner_user_id", "teammate_id", "trigger", "objective_template_json", "budget_json", "notification_prefs_json", "absence_policy", "status", "last_fired_at", "next_due_at")
    class AbsencePolicy(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        ABSENCE_POLICY_UNSPECIFIED: _ClassVar[Routine.AbsencePolicy]
        ABSENCE_POLICY_SKIP: _ClassVar[Routine.AbsencePolicy]
        ABSENCE_POLICY_QUEUE: _ClassVar[Routine.AbsencePolicy]
        ABSENCE_POLICY_CATCH_UP_ONCE: _ClassVar[Routine.AbsencePolicy]
    ABSENCE_POLICY_UNSPECIFIED: Routine.AbsencePolicy
    ABSENCE_POLICY_SKIP: Routine.AbsencePolicy
    ABSENCE_POLICY_QUEUE: Routine.AbsencePolicy
    ABSENCE_POLICY_CATCH_UP_ONCE: Routine.AbsencePolicy
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[Routine.Status]
        STATUS_ACTIVE: _ClassVar[Routine.Status]
        STATUS_PAUSED: _ClassVar[Routine.Status]
        STATUS_DELETED: _ClassVar[Routine.Status]
    STATUS_UNSPECIFIED: Routine.Status
    STATUS_ACTIVE: Routine.Status
    STATUS_PAUSED: Routine.Status
    STATUS_DELETED: Routine.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    OWNER_USER_ID_FIELD_NUMBER: _ClassVar[int]
    TEAMMATE_ID_FIELD_NUMBER: _ClassVar[int]
    TRIGGER_FIELD_NUMBER: _ClassVar[int]
    OBJECTIVE_TEMPLATE_JSON_FIELD_NUMBER: _ClassVar[int]
    BUDGET_JSON_FIELD_NUMBER: _ClassVar[int]
    NOTIFICATION_PREFS_JSON_FIELD_NUMBER: _ClassVar[int]
    ABSENCE_POLICY_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    LAST_FIRED_AT_FIELD_NUMBER: _ClassVar[int]
    NEXT_DUE_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    owner_user_id: str
    teammate_id: str
    trigger: RoutineTrigger
    objective_template_json: str
    budget_json: str
    notification_prefs_json: str
    absence_policy: Routine.AbsencePolicy
    status: Routine.Status
    last_fired_at: str
    next_due_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., owner_user_id: _Optional[str] = ..., teammate_id: _Optional[str] = ..., trigger: _Optional[_Union[RoutineTrigger, _Mapping]] = ..., objective_template_json: _Optional[str] = ..., budget_json: _Optional[str] = ..., notification_prefs_json: _Optional[str] = ..., absence_policy: _Optional[_Union[Routine.AbsencePolicy, str]] = ..., status: _Optional[_Union[Routine.Status, str]] = ..., last_fired_at: _Optional[str] = ..., next_due_at: _Optional[str] = ...) -> None: ...

class Notification(_message.Message):
    __slots__ = ("schema_version", "id", "user_id", "workspace_id", "kind", "ref_json", "channels_delivered", "read_at", "created_at")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Notification.Kind]
        KIND_NEEDS_APPROVAL: _ClassVar[Notification.Kind]
        KIND_NEEDS_ANSWER: _ClassVar[Notification.Kind]
        KIND_BLOCKED: _ClassVar[Notification.Kind]
        KIND_COMPLETED: _ClassVar[Notification.Kind]
        KIND_FAILED: _ClassVar[Notification.Kind]
        KIND_MENTION: _ClassVar[Notification.Kind]
        KIND_ATTENTION: _ClassVar[Notification.Kind]
        KIND_SYSTEM: _ClassVar[Notification.Kind]
    KIND_UNSPECIFIED: Notification.Kind
    KIND_NEEDS_APPROVAL: Notification.Kind
    KIND_NEEDS_ANSWER: Notification.Kind
    KIND_BLOCKED: Notification.Kind
    KIND_COMPLETED: Notification.Kind
    KIND_FAILED: Notification.Kind
    KIND_MENTION: Notification.Kind
    KIND_ATTENTION: Notification.Kind
    KIND_SYSTEM: Notification.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    USER_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    REF_JSON_FIELD_NUMBER: _ClassVar[int]
    CHANNELS_DELIVERED_FIELD_NUMBER: _ClassVar[int]
    READ_AT_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    user_id: str
    workspace_id: str
    kind: Notification.Kind
    ref_json: str
    channels_delivered: _containers.RepeatedScalarFieldContainer[str]
    read_at: str
    created_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., user_id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., kind: _Optional[_Union[Notification.Kind, str]] = ..., ref_json: _Optional[str] = ..., channels_delivered: _Optional[_Iterable[str]] = ..., read_at: _Optional[str] = ..., created_at: _Optional[str] = ...) -> None: ...

class UsageRecord(_message.Message):
    __slots__ = ("schema_version", "id", "tenant_id", "workspace_id", "scope_refs_json", "meter", "quantity", "unit", "cost_minor_units", "source_event_id", "occurred_at")
    class Meter(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        METER_UNSPECIFIED: _ClassVar[UsageRecord.Meter]
        METER_MODEL_INPUT_TOKENS: _ClassVar[UsageRecord.Meter]
        METER_MODEL_OUTPUT_TOKENS: _ClassVar[UsageRecord.Meter]
        METER_MODEL_CACHE_TOKENS: _ClassVar[UsageRecord.Meter]
        METER_MODEL_COST: _ClassVar[UsageRecord.Meter]
        METER_MACHINE_SECONDS: _ClassVar[UsageRecord.Meter]
        METER_STORAGE_BYTES: _ClassVar[UsageRecord.Meter]
        METER_CONNECTOR_CALLS: _ClassVar[UsageRecord.Meter]
        METER_BROWSER_SECONDS: _ClassVar[UsageRecord.Meter]
    METER_UNSPECIFIED: UsageRecord.Meter
    METER_MODEL_INPUT_TOKENS: UsageRecord.Meter
    METER_MODEL_OUTPUT_TOKENS: UsageRecord.Meter
    METER_MODEL_CACHE_TOKENS: UsageRecord.Meter
    METER_MODEL_COST: UsageRecord.Meter
    METER_MACHINE_SECONDS: UsageRecord.Meter
    METER_STORAGE_BYTES: UsageRecord.Meter
    METER_CONNECTOR_CALLS: UsageRecord.Meter
    METER_BROWSER_SECONDS: UsageRecord.Meter
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    SCOPE_REFS_JSON_FIELD_NUMBER: _ClassVar[int]
    METER_FIELD_NUMBER: _ClassVar[int]
    QUANTITY_FIELD_NUMBER: _ClassVar[int]
    UNIT_FIELD_NUMBER: _ClassVar[int]
    COST_MINOR_UNITS_FIELD_NUMBER: _ClassVar[int]
    SOURCE_EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    OCCURRED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    tenant_id: str
    workspace_id: str
    scope_refs_json: str
    meter: UsageRecord.Meter
    quantity: float
    unit: str
    cost_minor_units: int
    source_event_id: str
    occurred_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., tenant_id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., scope_refs_json: _Optional[str] = ..., meter: _Optional[_Union[UsageRecord.Meter, str]] = ..., quantity: _Optional[float] = ..., unit: _Optional[str] = ..., cost_minor_units: _Optional[int] = ..., source_event_id: _Optional[str] = ..., occurred_at: _Optional[str] = ...) -> None: ...

class WebhookSubscription(_message.Message):
    __slots__ = ("schema_version", "id", "tenant_id", "workspace_id", "url", "secret_handle_id", "event_types", "status", "delivery_json")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[WebhookSubscription.Status]
        STATUS_ACTIVE: _ClassVar[WebhookSubscription.Status]
        STATUS_PAUSED: _ClassVar[WebhookSubscription.Status]
        STATUS_FAILING: _ClassVar[WebhookSubscription.Status]
    STATUS_UNSPECIFIED: WebhookSubscription.Status
    STATUS_ACTIVE: WebhookSubscription.Status
    STATUS_PAUSED: WebhookSubscription.Status
    STATUS_FAILING: WebhookSubscription.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    URL_FIELD_NUMBER: _ClassVar[int]
    SECRET_HANDLE_ID_FIELD_NUMBER: _ClassVar[int]
    EVENT_TYPES_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    DELIVERY_JSON_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    tenant_id: str
    workspace_id: str
    url: str
    secret_handle_id: str
    event_types: _containers.RepeatedScalarFieldContainer[str]
    status: WebhookSubscription.Status
    delivery_json: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., tenant_id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., url: _Optional[str] = ..., secret_handle_id: _Optional[str] = ..., event_types: _Optional[_Iterable[str]] = ..., status: _Optional[_Union[WebhookSubscription.Status, str]] = ..., delivery_json: _Optional[str] = ...) -> None: ...

class AuditEntry(_message.Message):
    __slots__ = ("schema_version", "id", "tenant_id", "workspace_id", "actor_json", "action", "target_ref", "decision", "reason", "correlation_id", "occurred_at", "prev_hash", "hash")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    ACTOR_JSON_FIELD_NUMBER: _ClassVar[int]
    ACTION_FIELD_NUMBER: _ClassVar[int]
    TARGET_REF_FIELD_NUMBER: _ClassVar[int]
    DECISION_FIELD_NUMBER: _ClassVar[int]
    REASON_FIELD_NUMBER: _ClassVar[int]
    CORRELATION_ID_FIELD_NUMBER: _ClassVar[int]
    OCCURRED_AT_FIELD_NUMBER: _ClassVar[int]
    PREV_HASH_FIELD_NUMBER: _ClassVar[int]
    HASH_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    tenant_id: str
    workspace_id: str
    actor_json: str
    action: str
    target_ref: str
    decision: str
    reason: str
    correlation_id: str
    occurred_at: str
    prev_hash: str
    hash: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., tenant_id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., actor_json: _Optional[str] = ..., action: _Optional[str] = ..., target_ref: _Optional[str] = ..., decision: _Optional[str] = ..., reason: _Optional[str] = ..., correlation_id: _Optional[str] = ..., occurred_at: _Optional[str] = ..., prev_hash: _Optional[str] = ..., hash: _Optional[str] = ...) -> None: ...

class SecretHandle(_message.Message):
    __slots__ = ("schema_version", "id", "tenant_id", "provider", "label", "status", "last_used_at")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[SecretHandle.Status]
        STATUS_ACTIVE: _ClassVar[SecretHandle.Status]
        STATUS_REVOKED: _ClassVar[SecretHandle.Status]
        STATUS_EXPIRED: _ClassVar[SecretHandle.Status]
    STATUS_UNSPECIFIED: SecretHandle.Status
    STATUS_ACTIVE: SecretHandle.Status
    STATUS_REVOKED: SecretHandle.Status
    STATUS_EXPIRED: SecretHandle.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    LABEL_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    LAST_USED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    tenant_id: str
    provider: str
    label: str
    status: SecretHandle.Status
    last_used_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., tenant_id: _Optional[str] = ..., provider: _Optional[str] = ..., label: _Optional[str] = ..., status: _Optional[_Union[SecretHandle.Status, str]] = ..., last_used_at: _Optional[str] = ...) -> None: ...
