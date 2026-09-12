from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ErrorCode(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ERROR_CODE_UNSPECIFIED: _ClassVar[ErrorCode]
    ERROR_CODE_AUTH_REQUIRED: _ClassVar[ErrorCode]
    ERROR_CODE_AUTH_INVALID_TOKEN: _ClassVar[ErrorCode]
    ERROR_CODE_AUTH_EXPIRED: _ClassVar[ErrorCode]
    ERROR_CODE_SCOPE_FORBIDDEN: _ClassVar[ErrorCode]
    ERROR_CODE_TENANT_MISMATCH: _ClassVar[ErrorCode]
    ERROR_CODE_VALIDATION_SCHEMA: _ClassVar[ErrorCode]
    ERROR_CODE_VALIDATION_UNKNOWN_FIELD: _ClassVar[ErrorCode]
    ERROR_CODE_VALIDATION_BOUNDS: _ClassVar[ErrorCode]
    ERROR_CODE_CONFLICT_REVISION: _ClassVar[ErrorCode]
    ERROR_CODE_CONFLICT_IDEMPOTENCY_MISMATCH: _ClassVar[ErrorCode]
    ERROR_CODE_CONFLICT_STATE: _ClassVar[ErrorCode]
    ERROR_CODE_CAPABILITY_DENIED: _ClassVar[ErrorCode]
    ERROR_CODE_CAPABILITY_INPUTS_UNAVAILABLE: _ClassVar[ErrorCode]
    ERROR_CODE_POLICY_DENIED: _ClassVar[ErrorCode]
    ERROR_CODE_APPROVAL_REQUIRED: _ClassVar[ErrorCode]
    ERROR_CODE_APPROVAL_INVALID: _ClassVar[ErrorCode]
    ERROR_CODE_APPROVAL_EXPIRED: _ClassVar[ErrorCode]
    ERROR_CODE_APPROVAL_SUPERSEDED: _ClassVar[ErrorCode]
    ERROR_CODE_RUNTIME_ILLEGAL_TRANSITION: _ClassVar[ErrorCode]
    ERROR_CODE_FENCED_STALE_GENERATION: _ClassVar[ErrorCode]
    ERROR_CODE_LEASE_LOST: _ClassVar[ErrorCode]
    ERROR_CODE_EFFECT_UNKNOWN_PENDING_RECONCILIATION: _ClassVar[ErrorCode]
    ERROR_CODE_BUDGET_EXHAUSTED: _ClassVar[ErrorCode]
    ERROR_CODE_EFFECTS_FROZEN: _ClassVar[ErrorCode]
    ERROR_CODE_TARGET_UNAVAILABLE: _ClassVar[ErrorCode]
    ERROR_CODE_TARGET_PROVISION_FAILED: _ClassVar[ErrorCode]
    ERROR_CODE_TOOL_TIMEOUT: _ClassVar[ErrorCode]
    ERROR_CODE_TOOL_OUTPUT_TRUNCATED: _ClassVar[ErrorCode]
    ERROR_CODE_EGRESS_DENIED: _ClassVar[ErrorCode]
    ERROR_CODE_PROVIDER_UNAVAILABLE: _ClassVar[ErrorCode]
    ERROR_CODE_PROVIDER_RATE_LIMITED: _ClassVar[ErrorCode]
    ERROR_CODE_PROVIDER_REFUSAL: _ClassVar[ErrorCode]
    ERROR_CODE_ROUTE_UNAVAILABLE: _ClassVar[ErrorCode]
    ERROR_CODE_DLP_DENIED: _ClassVar[ErrorCode]
    ERROR_CODE_NOT_FOUND: _ClassVar[ErrorCode]
    ERROR_CODE_RATE_LIMITED: _ClassVar[ErrorCode]
    ERROR_CODE_STREAM_BACKPRESSURE: _ClassVar[ErrorCode]
    ERROR_CODE_INTERNAL: _ClassVar[ErrorCode]
ERROR_CODE_UNSPECIFIED: ErrorCode
ERROR_CODE_AUTH_REQUIRED: ErrorCode
ERROR_CODE_AUTH_INVALID_TOKEN: ErrorCode
ERROR_CODE_AUTH_EXPIRED: ErrorCode
ERROR_CODE_SCOPE_FORBIDDEN: ErrorCode
ERROR_CODE_TENANT_MISMATCH: ErrorCode
ERROR_CODE_VALIDATION_SCHEMA: ErrorCode
ERROR_CODE_VALIDATION_UNKNOWN_FIELD: ErrorCode
ERROR_CODE_VALIDATION_BOUNDS: ErrorCode
ERROR_CODE_CONFLICT_REVISION: ErrorCode
ERROR_CODE_CONFLICT_IDEMPOTENCY_MISMATCH: ErrorCode
ERROR_CODE_CONFLICT_STATE: ErrorCode
ERROR_CODE_CAPABILITY_DENIED: ErrorCode
ERROR_CODE_CAPABILITY_INPUTS_UNAVAILABLE: ErrorCode
ERROR_CODE_POLICY_DENIED: ErrorCode
ERROR_CODE_APPROVAL_REQUIRED: ErrorCode
ERROR_CODE_APPROVAL_INVALID: ErrorCode
ERROR_CODE_APPROVAL_EXPIRED: ErrorCode
ERROR_CODE_APPROVAL_SUPERSEDED: ErrorCode
ERROR_CODE_RUNTIME_ILLEGAL_TRANSITION: ErrorCode
ERROR_CODE_FENCED_STALE_GENERATION: ErrorCode
ERROR_CODE_LEASE_LOST: ErrorCode
ERROR_CODE_EFFECT_UNKNOWN_PENDING_RECONCILIATION: ErrorCode
ERROR_CODE_BUDGET_EXHAUSTED: ErrorCode
ERROR_CODE_EFFECTS_FROZEN: ErrorCode
ERROR_CODE_TARGET_UNAVAILABLE: ErrorCode
ERROR_CODE_TARGET_PROVISION_FAILED: ErrorCode
ERROR_CODE_TOOL_TIMEOUT: ErrorCode
ERROR_CODE_TOOL_OUTPUT_TRUNCATED: ErrorCode
ERROR_CODE_EGRESS_DENIED: ErrorCode
ERROR_CODE_PROVIDER_UNAVAILABLE: ErrorCode
ERROR_CODE_PROVIDER_RATE_LIMITED: ErrorCode
ERROR_CODE_PROVIDER_REFUSAL: ErrorCode
ERROR_CODE_ROUTE_UNAVAILABLE: ErrorCode
ERROR_CODE_DLP_DENIED: ErrorCode
ERROR_CODE_NOT_FOUND: ErrorCode
ERROR_CODE_RATE_LIMITED: ErrorCode
ERROR_CODE_STREAM_BACKPRESSURE: ErrorCode
ERROR_CODE_INTERNAL: ErrorCode

class ErrorDetail(_message.Message):
    __slots__ = ("schema_version", "key", "value")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KEY_FIELD_NUMBER: _ClassVar[int]
    VALUE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    key: str
    value: str
    def __init__(self, schema_version: _Optional[str] = ..., key: _Optional[str] = ..., value: _Optional[str] = ...) -> None: ...

class Error(_message.Message):
    __slots__ = ("schema_version", "code", "message", "correlation_id", "retryable", "details")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    CODE_FIELD_NUMBER: _ClassVar[int]
    MESSAGE_FIELD_NUMBER: _ClassVar[int]
    CORRELATION_ID_FIELD_NUMBER: _ClassVar[int]
    RETRYABLE_FIELD_NUMBER: _ClassVar[int]
    DETAILS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    code: ErrorCode
    message: str
    correlation_id: str
    retryable: bool
    details: _containers.RepeatedCompositeFieldContainer[ErrorDetail]
    def __init__(self, schema_version: _Optional[str] = ..., code: _Optional[_Union[ErrorCode, str]] = ..., message: _Optional[str] = ..., correlation_id: _Optional[str] = ..., retryable: _Optional[bool] = ..., details: _Optional[_Iterable[_Union[ErrorDetail, _Mapping]]] = ...) -> None: ...

class CommandResult(_message.Message):
    __slots__ = ("schema_version", "command_id", "accepted_at", "result_json", "error")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    COMMAND_ID_FIELD_NUMBER: _ClassVar[int]
    ACCEPTED_AT_FIELD_NUMBER: _ClassVar[int]
    RESULT_JSON_FIELD_NUMBER: _ClassVar[int]
    ERROR_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    command_id: str
    accepted_at: str
    result_json: str
    error: Error
    def __init__(self, schema_version: _Optional[str] = ..., command_id: _Optional[str] = ..., accepted_at: _Optional[str] = ..., result_json: _Optional[str] = ..., error: _Optional[_Union[Error, _Mapping]] = ...) -> None: ...
