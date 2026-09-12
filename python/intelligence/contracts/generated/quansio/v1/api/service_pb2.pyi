from quansio.v1.core import errors_pb2 as _errors_pb2
from quansio.v1.execution import execution_pb2 as _execution_pb2
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class CommandRequest(_message.Message):
    __slots__ = ("schema_version", "command_id", "command_name", "tenant_id", "workspace_id", "params_json", "correlation_id")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    COMMAND_ID_FIELD_NUMBER: _ClassVar[int]
    COMMAND_NAME_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    PARAMS_JSON_FIELD_NUMBER: _ClassVar[int]
    CORRELATION_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    command_id: str
    command_name: str
    tenant_id: str
    workspace_id: str
    params_json: str
    correlation_id: str
    def __init__(self, schema_version: _Optional[str] = ..., command_id: _Optional[str] = ..., command_name: _Optional[str] = ..., tenant_id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., params_json: _Optional[str] = ..., correlation_id: _Optional[str] = ...) -> None: ...

class CommandResponse(_message.Message):
    __slots__ = ("schema_version", "result")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    RESULT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    result: _errors_pb2.CommandResult
    def __init__(self, schema_version: _Optional[str] = ..., result: _Optional[_Union[_errors_pb2.CommandResult, _Mapping]] = ...) -> None: ...

class ProjectionRequest(_message.Message):
    __slots__ = ("schema_version", "resource", "resource_id", "limit", "cursor", "filter_json")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    RESOURCE_FIELD_NUMBER: _ClassVar[int]
    RESOURCE_ID_FIELD_NUMBER: _ClassVar[int]
    LIMIT_FIELD_NUMBER: _ClassVar[int]
    CURSOR_FIELD_NUMBER: _ClassVar[int]
    FILTER_JSON_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    resource: str
    resource_id: str
    limit: int
    cursor: str
    filter_json: str
    def __init__(self, schema_version: _Optional[str] = ..., resource: _Optional[str] = ..., resource_id: _Optional[str] = ..., limit: _Optional[int] = ..., cursor: _Optional[str] = ..., filter_json: _Optional[str] = ...) -> None: ...

class ProjectionResponse(_message.Message):
    __slots__ = ("schema_version", "json", "next_cursor")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    JSON_FIELD_NUMBER: _ClassVar[int]
    NEXT_CURSOR_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    json: str
    next_cursor: str
    def __init__(self, schema_version: _Optional[str] = ..., json: _Optional[str] = ..., next_cursor: _Optional[str] = ...) -> None: ...
