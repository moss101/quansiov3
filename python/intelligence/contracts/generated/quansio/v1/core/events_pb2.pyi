from quansio.v1.core import identity_pb2 as _identity_pb2
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class EventFamily(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    EVENT_FAMILY_UNSPECIFIED: _ClassVar[EventFamily]
    EVENT_FAMILY_TENANT: _ClassVar[EventFamily]
    EVENT_FAMILY_WORKSPACE: _ClassVar[EventFamily]
    EVENT_FAMILY_MEMBER: _ClassVar[EventFamily]
    EVENT_FAMILY_THREAD: _ClassVar[EventFamily]
    EVENT_FAMILY_WORK: _ClassVar[EventFamily]
    EVENT_FAMILY_AGENT: _ClassVar[EventFamily]
    EVENT_FAMILY_RUN: _ClassVar[EventFamily]
    EVENT_FAMILY_TURN: _ClassVar[EventFamily]
    EVENT_FAMILY_STEP: _ClassVar[EventFamily]
    EVENT_FAMILY_MODEL: _ClassVar[EventFamily]
    EVENT_FAMILY_TOOL: _ClassVar[EventFamily]
    EVENT_FAMILY_EFFECT: _ClassVar[EventFamily]
    EVENT_FAMILY_APPROVAL: _ClassVar[EventFamily]
    EVENT_FAMILY_QUESTION: _ClassVar[EventFamily]
    EVENT_FAMILY_TARGET: _ClassVar[EventFamily]
    EVENT_FAMILY_LEASE: _ClassVar[EventFamily]
    EVENT_FAMILY_BROWSER: _ClassVar[EventFamily]
    EVENT_FAMILY_TERMINAL: _ClassVar[EventFamily]
    EVENT_FAMILY_CHECKPOINT: _ClassVar[EventFamily]
    EVENT_FAMILY_ARTIFACT: _ClassVar[EventFamily]
    EVENT_FAMILY_EVIDENCE: _ClassVar[EventFamily]
    EVENT_FAMILY_KNOWLEDGE: _ClassVar[EventFamily]
    EVENT_FAMILY_MEMORY: _ClassVar[EventFamily]
    EVENT_FAMILY_SKILL: _ClassVar[EventFamily]
    EVENT_FAMILY_PACK: _ClassVar[EventFamily]
    EVENT_FAMILY_ROUTINE: _ClassVar[EventFamily]
    EVENT_FAMILY_NOTIFICATION: _ClassVar[EventFamily]
    EVENT_FAMILY_CONNECTOR: _ClassVar[EventFamily]
    EVENT_FAMILY_POLICY: _ClassVar[EventFamily]
    EVENT_FAMILY_AUDIT: _ClassVar[EventFamily]
    EVENT_FAMILY_USAGE: _ClassVar[EventFamily]
    EVENT_FAMILY_CAPABILITY: _ClassVar[EventFamily]
    EVENT_FAMILY_COMPACTION: _ClassVar[EventFamily]
    EVENT_FAMILY_OPS: _ClassVar[EventFamily]
EVENT_FAMILY_UNSPECIFIED: EventFamily
EVENT_FAMILY_TENANT: EventFamily
EVENT_FAMILY_WORKSPACE: EventFamily
EVENT_FAMILY_MEMBER: EventFamily
EVENT_FAMILY_THREAD: EventFamily
EVENT_FAMILY_WORK: EventFamily
EVENT_FAMILY_AGENT: EventFamily
EVENT_FAMILY_RUN: EventFamily
EVENT_FAMILY_TURN: EventFamily
EVENT_FAMILY_STEP: EventFamily
EVENT_FAMILY_MODEL: EventFamily
EVENT_FAMILY_TOOL: EventFamily
EVENT_FAMILY_EFFECT: EventFamily
EVENT_FAMILY_APPROVAL: EventFamily
EVENT_FAMILY_QUESTION: EventFamily
EVENT_FAMILY_TARGET: EventFamily
EVENT_FAMILY_LEASE: EventFamily
EVENT_FAMILY_BROWSER: EventFamily
EVENT_FAMILY_TERMINAL: EventFamily
EVENT_FAMILY_CHECKPOINT: EventFamily
EVENT_FAMILY_ARTIFACT: EventFamily
EVENT_FAMILY_EVIDENCE: EventFamily
EVENT_FAMILY_KNOWLEDGE: EventFamily
EVENT_FAMILY_MEMORY: EventFamily
EVENT_FAMILY_SKILL: EventFamily
EVENT_FAMILY_PACK: EventFamily
EVENT_FAMILY_ROUTINE: EventFamily
EVENT_FAMILY_NOTIFICATION: EventFamily
EVENT_FAMILY_CONNECTOR: EventFamily
EVENT_FAMILY_POLICY: EventFamily
EVENT_FAMILY_AUDIT: EventFamily
EVENT_FAMILY_USAGE: EventFamily
EVENT_FAMILY_CAPABILITY: EventFamily
EVENT_FAMILY_COMPACTION: EventFamily
EVENT_FAMILY_OPS: EventFamily

class RuntimeEvent(_message.Message):
    __slots__ = ("schema_version", "event_id", "tenant_id", "workspace_id", "sequence", "aggregate_type", "aggregate_id", "aggregate_version", "type", "occurred_at", "command_id", "correlation_id", "causation_id", "actor", "generation", "payload_json")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    SEQUENCE_FIELD_NUMBER: _ClassVar[int]
    AGGREGATE_TYPE_FIELD_NUMBER: _ClassVar[int]
    AGGREGATE_ID_FIELD_NUMBER: _ClassVar[int]
    AGGREGATE_VERSION_FIELD_NUMBER: _ClassVar[int]
    TYPE_FIELD_NUMBER: _ClassVar[int]
    OCCURRED_AT_FIELD_NUMBER: _ClassVar[int]
    COMMAND_ID_FIELD_NUMBER: _ClassVar[int]
    CORRELATION_ID_FIELD_NUMBER: _ClassVar[int]
    CAUSATION_ID_FIELD_NUMBER: _ClassVar[int]
    ACTOR_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    PAYLOAD_JSON_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    event_id: str
    tenant_id: str
    workspace_id: str
    sequence: int
    aggregate_type: str
    aggregate_id: str
    aggregate_version: int
    type: str
    occurred_at: str
    command_id: str
    correlation_id: str
    causation_id: str
    actor: _identity_pb2.Actor
    generation: int
    payload_json: str
    def __init__(self, schema_version: _Optional[str] = ..., event_id: _Optional[str] = ..., tenant_id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., sequence: _Optional[int] = ..., aggregate_type: _Optional[str] = ..., aggregate_id: _Optional[str] = ..., aggregate_version: _Optional[int] = ..., type: _Optional[str] = ..., occurred_at: _Optional[str] = ..., command_id: _Optional[str] = ..., correlation_id: _Optional[str] = ..., causation_id: _Optional[str] = ..., actor: _Optional[_Union[_identity_pb2.Actor, _Mapping]] = ..., generation: _Optional[int] = ..., payload_json: _Optional[str] = ...) -> None: ...

class StreamFrame(_message.Message):
    __slots__ = ("schema_version", "kind", "cursor", "channel", "event", "live")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[StreamFrame.Kind]
        KIND_EVENT: _ClassVar[StreamFrame.Kind]
        KIND_LIVE: _ClassVar[StreamFrame.Kind]
    KIND_UNSPECIFIED: StreamFrame.Kind
    KIND_EVENT: StreamFrame.Kind
    KIND_LIVE: StreamFrame.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    CURSOR_FIELD_NUMBER: _ClassVar[int]
    CHANNEL_FIELD_NUMBER: _ClassVar[int]
    EVENT_FIELD_NUMBER: _ClassVar[int]
    LIVE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: StreamFrame.Kind
    cursor: str
    channel: str
    event: RuntimeEvent
    live: LiveFrame
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[StreamFrame.Kind, str]] = ..., cursor: _Optional[str] = ..., channel: _Optional[str] = ..., event: _Optional[_Union[RuntimeEvent, _Mapping]] = ..., live: _Optional[_Union[LiveFrame, _Mapping]] = ...) -> None: ...

class LiveFrame(_message.Message):
    __slots__ = ("schema_version", "kind", "payload", "stream_ref")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[LiveFrame.Kind]
        KIND_MODEL_DELTA: _ClassVar[LiveFrame.Kind]
        KIND_TERMINAL_BYTES: _ClassVar[LiveFrame.Kind]
        KIND_BROWSER_FRAME: _ClassVar[LiveFrame.Kind]
        KIND_PROGRESS: _ClassVar[LiveFrame.Kind]
    KIND_UNSPECIFIED: LiveFrame.Kind
    KIND_MODEL_DELTA: LiveFrame.Kind
    KIND_TERMINAL_BYTES: LiveFrame.Kind
    KIND_BROWSER_FRAME: LiveFrame.Kind
    KIND_PROGRESS: LiveFrame.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    PAYLOAD_FIELD_NUMBER: _ClassVar[int]
    STREAM_REF_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: LiveFrame.Kind
    payload: bytes
    stream_ref: str
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[LiveFrame.Kind, str]] = ..., payload: _Optional[bytes] = ..., stream_ref: _Optional[str] = ...) -> None: ...
