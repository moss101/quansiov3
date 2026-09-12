from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ModelStopReason(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    MODEL_STOP_REASON_UNSPECIFIED: _ClassVar[ModelStopReason]
    MODEL_STOP_REASON_END_TURN: _ClassVar[ModelStopReason]
    MODEL_STOP_REASON_TOOL_USE: _ClassVar[ModelStopReason]
    MODEL_STOP_REASON_MAX_TOKENS: _ClassVar[ModelStopReason]
    MODEL_STOP_REASON_REFUSAL: _ClassVar[ModelStopReason]
    MODEL_STOP_REASON_CANCELLED: _ClassVar[ModelStopReason]
    MODEL_STOP_REASON_ERROR: _ClassVar[ModelStopReason]
MODEL_STOP_REASON_UNSPECIFIED: ModelStopReason
MODEL_STOP_REASON_END_TURN: ModelStopReason
MODEL_STOP_REASON_TOOL_USE: ModelStopReason
MODEL_STOP_REASON_MAX_TOKENS: ModelStopReason
MODEL_STOP_REASON_REFUSAL: ModelStopReason
MODEL_STOP_REASON_CANCELLED: ModelStopReason
MODEL_STOP_REASON_ERROR: ModelStopReason

class ModelCallRequest(_message.Message):
    __slots__ = ("schema_version", "call_id", "run_id", "turn_id", "route_hint", "capability_projection_id", "context_projection_id", "messages", "tools", "output_constraints_json", "effort", "max_output_tokens", "stream", "dlp_profile", "timeout_ms", "cancellation_token")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    CALL_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    TURN_ID_FIELD_NUMBER: _ClassVar[int]
    ROUTE_HINT_FIELD_NUMBER: _ClassVar[int]
    CAPABILITY_PROJECTION_ID_FIELD_NUMBER: _ClassVar[int]
    CONTEXT_PROJECTION_ID_FIELD_NUMBER: _ClassVar[int]
    MESSAGES_FIELD_NUMBER: _ClassVar[int]
    TOOLS_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_CONSTRAINTS_JSON_FIELD_NUMBER: _ClassVar[int]
    EFFORT_FIELD_NUMBER: _ClassVar[int]
    MAX_OUTPUT_TOKENS_FIELD_NUMBER: _ClassVar[int]
    STREAM_FIELD_NUMBER: _ClassVar[int]
    DLP_PROFILE_FIELD_NUMBER: _ClassVar[int]
    TIMEOUT_MS_FIELD_NUMBER: _ClassVar[int]
    CANCELLATION_TOKEN_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    call_id: str
    run_id: str
    turn_id: str
    route_hint: str
    capability_projection_id: str
    context_projection_id: str
    messages: _containers.RepeatedCompositeFieldContainer[RenderedMessage]
    tools: _containers.RepeatedScalarFieldContainer[str]
    output_constraints_json: str
    effort: str
    max_output_tokens: int
    stream: bool
    dlp_profile: str
    timeout_ms: int
    cancellation_token: str
    def __init__(self, schema_version: _Optional[str] = ..., call_id: _Optional[str] = ..., run_id: _Optional[str] = ..., turn_id: _Optional[str] = ..., route_hint: _Optional[str] = ..., capability_projection_id: _Optional[str] = ..., context_projection_id: _Optional[str] = ..., messages: _Optional[_Iterable[_Union[RenderedMessage, _Mapping]]] = ..., tools: _Optional[_Iterable[str]] = ..., output_constraints_json: _Optional[str] = ..., effort: _Optional[str] = ..., max_output_tokens: _Optional[int] = ..., stream: _Optional[bool] = ..., dlp_profile: _Optional[str] = ..., timeout_ms: _Optional[int] = ..., cancellation_token: _Optional[str] = ...) -> None: ...

class RenderedMessage(_message.Message):
    __slots__ = ("schema_version", "role", "trust_level", "content_json", "cache_hint")
    class Role(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        ROLE_UNSPECIFIED: _ClassVar[RenderedMessage.Role]
        ROLE_SYSTEM: _ClassVar[RenderedMessage.Role]
        ROLE_OPERATOR: _ClassVar[RenderedMessage.Role]
        ROLE_USER: _ClassVar[RenderedMessage.Role]
        ROLE_ASSISTANT: _ClassVar[RenderedMessage.Role]
        ROLE_TOOL: _ClassVar[RenderedMessage.Role]
    ROLE_UNSPECIFIED: RenderedMessage.Role
    ROLE_SYSTEM: RenderedMessage.Role
    ROLE_OPERATOR: RenderedMessage.Role
    ROLE_USER: RenderedMessage.Role
    ROLE_ASSISTANT: RenderedMessage.Role
    ROLE_TOOL: RenderedMessage.Role
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ROLE_FIELD_NUMBER: _ClassVar[int]
    TRUST_LEVEL_FIELD_NUMBER: _ClassVar[int]
    CONTENT_JSON_FIELD_NUMBER: _ClassVar[int]
    CACHE_HINT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    role: RenderedMessage.Role
    trust_level: str
    content_json: str
    cache_hint: str
    def __init__(self, schema_version: _Optional[str] = ..., role: _Optional[_Union[RenderedMessage.Role, str]] = ..., trust_level: _Optional[str] = ..., content_json: _Optional[str] = ..., cache_hint: _Optional[str] = ...) -> None: ...

class ModelEvent(_message.Message):
    __slots__ = ("schema_version", "call_id", "kind", "route_id", "text_delta", "tool_call_id", "tool_name", "tool_args_json", "stop_reason", "error", "usage", "latency_ms", "cost_estimate_minor_units")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[ModelEvent.Kind]
        KIND_CALL_STARTED: _ClassVar[ModelEvent.Kind]
        KIND_DELTA: _ClassVar[ModelEvent.Kind]
        KIND_TOOL_CALL: _ClassVar[ModelEvent.Kind]
        KIND_THINKING_SUMMARY: _ClassVar[ModelEvent.Kind]
        KIND_USAGE: _ClassVar[ModelEvent.Kind]
        KIND_STOP: _ClassVar[ModelEvent.Kind]
        KIND_ERROR: _ClassVar[ModelEvent.Kind]
        KIND_CALL_COMPLETED: _ClassVar[ModelEvent.Kind]
    KIND_UNSPECIFIED: ModelEvent.Kind
    KIND_CALL_STARTED: ModelEvent.Kind
    KIND_DELTA: ModelEvent.Kind
    KIND_TOOL_CALL: ModelEvent.Kind
    KIND_THINKING_SUMMARY: ModelEvent.Kind
    KIND_USAGE: ModelEvent.Kind
    KIND_STOP: ModelEvent.Kind
    KIND_ERROR: ModelEvent.Kind
    KIND_CALL_COMPLETED: ModelEvent.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    CALL_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    ROUTE_ID_FIELD_NUMBER: _ClassVar[int]
    TEXT_DELTA_FIELD_NUMBER: _ClassVar[int]
    TOOL_CALL_ID_FIELD_NUMBER: _ClassVar[int]
    TOOL_NAME_FIELD_NUMBER: _ClassVar[int]
    TOOL_ARGS_JSON_FIELD_NUMBER: _ClassVar[int]
    STOP_REASON_FIELD_NUMBER: _ClassVar[int]
    ERROR_FIELD_NUMBER: _ClassVar[int]
    USAGE_FIELD_NUMBER: _ClassVar[int]
    LATENCY_MS_FIELD_NUMBER: _ClassVar[int]
    COST_ESTIMATE_MINOR_UNITS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    call_id: str
    kind: ModelEvent.Kind
    route_id: str
    text_delta: str
    tool_call_id: str
    tool_name: str
    tool_args_json: str
    stop_reason: ModelStopReason
    error: ModelError
    usage: Usage
    latency_ms: int
    cost_estimate_minor_units: int
    def __init__(self, schema_version: _Optional[str] = ..., call_id: _Optional[str] = ..., kind: _Optional[_Union[ModelEvent.Kind, str]] = ..., route_id: _Optional[str] = ..., text_delta: _Optional[str] = ..., tool_call_id: _Optional[str] = ..., tool_name: _Optional[str] = ..., tool_args_json: _Optional[str] = ..., stop_reason: _Optional[_Union[ModelStopReason, str]] = ..., error: _Optional[_Union[ModelError, _Mapping]] = ..., usage: _Optional[_Union[Usage, _Mapping]] = ..., latency_ms: _Optional[int] = ..., cost_estimate_minor_units: _Optional[int] = ...) -> None: ...

class Usage(_message.Message):
    __slots__ = ("schema_version", "input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    INPUT_TOKENS_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_TOKENS_FIELD_NUMBER: _ClassVar[int]
    CACHE_READ_TOKENS_FIELD_NUMBER: _ClassVar[int]
    CACHE_WRITE_TOKENS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    input_tokens: int
    output_tokens: int
    cache_read_tokens: int
    cache_write_tokens: int
    def __init__(self, schema_version: _Optional[str] = ..., input_tokens: _Optional[int] = ..., output_tokens: _Optional[int] = ..., cache_read_tokens: _Optional[int] = ..., cache_write_tokens: _Optional[int] = ...) -> None: ...

class ModelError(_message.Message):
    __slots__ = ("schema_version", "code", "retryable")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    CODE_FIELD_NUMBER: _ClassVar[int]
    RETRYABLE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    code: str
    retryable: bool
    def __init__(self, schema_version: _Optional[str] = ..., code: _Optional[str] = ..., retryable: _Optional[bool] = ...) -> None: ...

class ModelRoute(_message.Message):
    __slots__ = ("schema_version", "id", "request_class", "provider", "model_id", "config_json", "dlp_profile", "fallbacks", "cost_class", "chosen_by", "selected_at")
    class RequestClass(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        REQUEST_CLASS_UNSPECIFIED: _ClassVar[ModelRoute.RequestClass]
        REQUEST_CLASS_CHAT: _ClassVar[ModelRoute.RequestClass]
        REQUEST_CLASS_PLANNING: _ClassVar[ModelRoute.RequestClass]
        REQUEST_CLASS_TOOL_HEAVY: _ClassVar[ModelRoute.RequestClass]
        REQUEST_CLASS_SYNTHESIS: _ClassVar[ModelRoute.RequestClass]
        REQUEST_CLASS_VERIFICATION: _ClassVar[ModelRoute.RequestClass]
        REQUEST_CLASS_EMBEDDING: _ClassVar[ModelRoute.RequestClass]
        REQUEST_CLASS_CHEAP_WORKER: _ClassVar[ModelRoute.RequestClass]
    REQUEST_CLASS_UNSPECIFIED: ModelRoute.RequestClass
    REQUEST_CLASS_CHAT: ModelRoute.RequestClass
    REQUEST_CLASS_PLANNING: ModelRoute.RequestClass
    REQUEST_CLASS_TOOL_HEAVY: ModelRoute.RequestClass
    REQUEST_CLASS_SYNTHESIS: ModelRoute.RequestClass
    REQUEST_CLASS_VERIFICATION: ModelRoute.RequestClass
    REQUEST_CLASS_EMBEDDING: ModelRoute.RequestClass
    REQUEST_CLASS_CHEAP_WORKER: ModelRoute.RequestClass
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    REQUEST_CLASS_FIELD_NUMBER: _ClassVar[int]
    PROVIDER_FIELD_NUMBER: _ClassVar[int]
    MODEL_ID_FIELD_NUMBER: _ClassVar[int]
    CONFIG_JSON_FIELD_NUMBER: _ClassVar[int]
    DLP_PROFILE_FIELD_NUMBER: _ClassVar[int]
    FALLBACKS_FIELD_NUMBER: _ClassVar[int]
    COST_CLASS_FIELD_NUMBER: _ClassVar[int]
    CHOSEN_BY_FIELD_NUMBER: _ClassVar[int]
    SELECTED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    request_class: ModelRoute.RequestClass
    provider: str
    model_id: str
    config_json: str
    dlp_profile: str
    fallbacks: _containers.RepeatedScalarFieldContainer[str]
    cost_class: str
    chosen_by: str
    selected_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., request_class: _Optional[_Union[ModelRoute.RequestClass, str]] = ..., provider: _Optional[str] = ..., model_id: _Optional[str] = ..., config_json: _Optional[str] = ..., dlp_profile: _Optional[str] = ..., fallbacks: _Optional[_Iterable[str]] = ..., cost_class: _Optional[str] = ..., chosen_by: _Optional[str] = ..., selected_at: _Optional[str] = ...) -> None: ...

class ContextProjection(_message.Message):
    __slots__ = ("schema_version", "id", "run_id", "turn_id", "segments", "token_ledger", "degradation_json", "search_program_id", "policy_snapshot_digest", "created_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    TURN_ID_FIELD_NUMBER: _ClassVar[int]
    SEGMENTS_FIELD_NUMBER: _ClassVar[int]
    TOKEN_LEDGER_FIELD_NUMBER: _ClassVar[int]
    DEGRADATION_JSON_FIELD_NUMBER: _ClassVar[int]
    SEARCH_PROGRAM_ID_FIELD_NUMBER: _ClassVar[int]
    POLICY_SNAPSHOT_DIGEST_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    run_id: str
    turn_id: str
    segments: _containers.RepeatedCompositeFieldContainer[ContextSegment]
    token_ledger: TokenLedger
    degradation_json: _containers.RepeatedScalarFieldContainer[str]
    search_program_id: str
    policy_snapshot_digest: str
    created_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., run_id: _Optional[str] = ..., turn_id: _Optional[str] = ..., segments: _Optional[_Iterable[_Union[ContextSegment, _Mapping]]] = ..., token_ledger: _Optional[_Union[TokenLedger, _Mapping]] = ..., degradation_json: _Optional[_Iterable[str]] = ..., search_program_id: _Optional[str] = ..., policy_snapshot_digest: _Optional[str] = ..., created_at: _Optional[str] = ...) -> None: ...

class ContextSegment(_message.Message):
    __slots__ = ("schema_version", "seq", "source_kind", "source_ref", "trust_level", "token_estimate", "snapshot_ref", "redactions")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    SOURCE_KIND_FIELD_NUMBER: _ClassVar[int]
    SOURCE_REF_FIELD_NUMBER: _ClassVar[int]
    TRUST_LEVEL_FIELD_NUMBER: _ClassVar[int]
    TOKEN_ESTIMATE_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_REF_FIELD_NUMBER: _ClassVar[int]
    REDACTIONS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    seq: int
    source_kind: str
    source_ref: str
    trust_level: str
    token_estimate: int
    snapshot_ref: str
    redactions: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, schema_version: _Optional[str] = ..., seq: _Optional[int] = ..., source_kind: _Optional[str] = ..., source_ref: _Optional[str] = ..., trust_level: _Optional[str] = ..., token_estimate: _Optional[int] = ..., snapshot_ref: _Optional[str] = ..., redactions: _Optional[_Iterable[str]] = ...) -> None: ...

class TokenLedger(_message.Message):
    __slots__ = ("schema_version", "budget", "used", "by_source_json")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    BUDGET_FIELD_NUMBER: _ClassVar[int]
    USED_FIELD_NUMBER: _ClassVar[int]
    BY_SOURCE_JSON_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    budget: int
    used: int
    by_source_json: str
    def __init__(self, schema_version: _Optional[str] = ..., budget: _Optional[int] = ..., used: _Optional[int] = ..., by_source_json: _Optional[str] = ...) -> None: ...

class SearchProgram(_message.Message):
    __slots__ = ("schema_version", "channels", "filters_json", "max_results", "max_tokens", "snapshot", "scope", "scope_ref")
    class Scope(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        SCOPE_UNSPECIFIED: _ClassVar[SearchProgram.Scope]
        SCOPE_TENANT: _ClassVar[SearchProgram.Scope]
        SCOPE_WORKSPACE: _ClassVar[SearchProgram.Scope]
        SCOPE_TARGET: _ClassVar[SearchProgram.Scope]
    SCOPE_UNSPECIFIED: SearchProgram.Scope
    SCOPE_TENANT: SearchProgram.Scope
    SCOPE_WORKSPACE: SearchProgram.Scope
    SCOPE_TARGET: SearchProgram.Scope
    class Channel(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        CHANNEL_UNSPECIFIED: _ClassVar[SearchProgram.Channel]
        CHANNEL_EXACT: _ClassVar[SearchProgram.Channel]
        CHANNEL_LEXICAL: _ClassVar[SearchProgram.Channel]
        CHANNEL_SEMANTIC: _ClassVar[SearchProgram.Channel]
        CHANNEL_GRAPH: _ClassVar[SearchProgram.Channel]
        CHANNEL_HISTORY: _ClassVar[SearchProgram.Channel]
        CHANNEL_MEMORY: _ClassVar[SearchProgram.Channel]
    CHANNEL_UNSPECIFIED: SearchProgram.Channel
    CHANNEL_EXACT: SearchProgram.Channel
    CHANNEL_LEXICAL: SearchProgram.Channel
    CHANNEL_SEMANTIC: SearchProgram.Channel
    CHANNEL_GRAPH: SearchProgram.Channel
    CHANNEL_HISTORY: SearchProgram.Channel
    CHANNEL_MEMORY: SearchProgram.Channel
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    CHANNELS_FIELD_NUMBER: _ClassVar[int]
    FILTERS_JSON_FIELD_NUMBER: _ClassVar[int]
    MAX_RESULTS_FIELD_NUMBER: _ClassVar[int]
    MAX_TOKENS_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    SCOPE_REF_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    channels: _containers.RepeatedScalarFieldContainer[SearchProgram.Channel]
    filters_json: str
    max_results: int
    max_tokens: int
    snapshot: str
    scope: SearchProgram.Scope
    scope_ref: str
    def __init__(self, schema_version: _Optional[str] = ..., channels: _Optional[_Iterable[_Union[SearchProgram.Channel, str]]] = ..., filters_json: _Optional[str] = ..., max_results: _Optional[int] = ..., max_tokens: _Optional[int] = ..., snapshot: _Optional[str] = ..., scope: _Optional[_Union[SearchProgram.Scope, str]] = ..., scope_ref: _Optional[str] = ...) -> None: ...

class SearchResult(_message.Message):
    __slots__ = ("schema_version", "source_id", "locator", "snapshot", "score")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SOURCE_ID_FIELD_NUMBER: _ClassVar[int]
    LOCATOR_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    SCORE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    source_id: str
    locator: str
    snapshot: str
    score: float
    def __init__(self, schema_version: _Optional[str] = ..., source_id: _Optional[str] = ..., locator: _Optional[str] = ..., snapshot: _Optional[str] = ..., score: _Optional[float] = ...) -> None: ...

class KnowledgeEntry(_message.Message):
    __slots__ = ("schema_version", "id", "scope", "kind", "content_ref", "provenance", "confidence", "version", "status", "superseded_by", "embedding_ref")
    class Scope(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        SCOPE_UNSPECIFIED: _ClassVar[KnowledgeEntry.Scope]
        SCOPE_TENANT: _ClassVar[KnowledgeEntry.Scope]
        SCOPE_WORKSPACE: _ClassVar[KnowledgeEntry.Scope]
        SCOPE_PACK: _ClassVar[KnowledgeEntry.Scope]
    SCOPE_UNSPECIFIED: KnowledgeEntry.Scope
    SCOPE_TENANT: KnowledgeEntry.Scope
    SCOPE_WORKSPACE: KnowledgeEntry.Scope
    SCOPE_PACK: KnowledgeEntry.Scope
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[KnowledgeEntry.Status]
        STATUS_CANDIDATE: _ClassVar[KnowledgeEntry.Status]
        STATUS_VERIFIED: _ClassVar[KnowledgeEntry.Status]
        STATUS_ACTIVE: _ClassVar[KnowledgeEntry.Status]
        STATUS_SUPERSEDED: _ClassVar[KnowledgeEntry.Status]
        STATUS_QUARANTINED: _ClassVar[KnowledgeEntry.Status]
        STATUS_DELETED: _ClassVar[KnowledgeEntry.Status]
    STATUS_UNSPECIFIED: KnowledgeEntry.Status
    STATUS_CANDIDATE: KnowledgeEntry.Status
    STATUS_VERIFIED: KnowledgeEntry.Status
    STATUS_ACTIVE: KnowledgeEntry.Status
    STATUS_SUPERSEDED: KnowledgeEntry.Status
    STATUS_QUARANTINED: KnowledgeEntry.Status
    STATUS_DELETED: KnowledgeEntry.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    CONTENT_REF_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_FIELD_NUMBER: _ClassVar[int]
    CONFIDENCE_FIELD_NUMBER: _ClassVar[int]
    VERSION_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    SUPERSEDED_BY_FIELD_NUMBER: _ClassVar[int]
    EMBEDDING_REF_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    scope: KnowledgeEntry.Scope
    kind: str
    content_ref: str
    provenance: _containers.RepeatedCompositeFieldContainer[Provenance]
    confidence: float
    version: int
    status: KnowledgeEntry.Status
    superseded_by: str
    embedding_ref: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., scope: _Optional[_Union[KnowledgeEntry.Scope, str]] = ..., kind: _Optional[str] = ..., content_ref: _Optional[str] = ..., provenance: _Optional[_Iterable[_Union[Provenance, _Mapping]]] = ..., confidence: _Optional[float] = ..., version: _Optional[int] = ..., status: _Optional[_Union[KnowledgeEntry.Status, str]] = ..., superseded_by: _Optional[str] = ..., embedding_ref: _Optional[str] = ...) -> None: ...

class Provenance(_message.Message):
    __slots__ = ("schema_version", "source_kind", "ref", "digest", "retrieved_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SOURCE_KIND_FIELD_NUMBER: _ClassVar[int]
    REF_FIELD_NUMBER: _ClassVar[int]
    DIGEST_FIELD_NUMBER: _ClassVar[int]
    RETRIEVED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    source_kind: str
    ref: str
    digest: str
    retrieved_at: str
    def __init__(self, schema_version: _Optional[str] = ..., source_kind: _Optional[str] = ..., ref: _Optional[str] = ..., digest: _Optional[str] = ..., retrieved_at: _Optional[str] = ...) -> None: ...

class MemoryEntry(_message.Message):
    __slots__ = ("schema_version", "id", "scope", "subject_ref", "content", "provenance_kind", "provenance_ref", "confidence", "status", "last_used_at", "expires_at")
    class Scope(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        SCOPE_UNSPECIFIED: _ClassVar[MemoryEntry.Scope]
        SCOPE_USER: _ClassVar[MemoryEntry.Scope]
        SCOPE_WORKSPACE: _ClassVar[MemoryEntry.Scope]
        SCOPE_TEAMMATE: _ClassVar[MemoryEntry.Scope]
    SCOPE_UNSPECIFIED: MemoryEntry.Scope
    SCOPE_USER: MemoryEntry.Scope
    SCOPE_WORKSPACE: MemoryEntry.Scope
    SCOPE_TEAMMATE: MemoryEntry.Scope
    class ProvenanceKind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        PROVENANCE_KIND_UNSPECIFIED: _ClassVar[MemoryEntry.ProvenanceKind]
        PROVENANCE_KIND_EXPLICIT_USER: _ClassVar[MemoryEntry.ProvenanceKind]
        PROVENANCE_KIND_VERIFIED_RUN: _ClassVar[MemoryEntry.ProvenanceKind]
    PROVENANCE_KIND_UNSPECIFIED: MemoryEntry.ProvenanceKind
    PROVENANCE_KIND_EXPLICIT_USER: MemoryEntry.ProvenanceKind
    PROVENANCE_KIND_VERIFIED_RUN: MemoryEntry.ProvenanceKind
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[MemoryEntry.Status]
        STATUS_CANDIDATE: _ClassVar[MemoryEntry.Status]
        STATUS_ACTIVE: _ClassVar[MemoryEntry.Status]
        STATUS_DELETED: _ClassVar[MemoryEntry.Status]
    STATUS_UNSPECIFIED: MemoryEntry.Status
    STATUS_CANDIDATE: MemoryEntry.Status
    STATUS_ACTIVE: MemoryEntry.Status
    STATUS_DELETED: MemoryEntry.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    SUBJECT_REF_FIELD_NUMBER: _ClassVar[int]
    CONTENT_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_KIND_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_REF_FIELD_NUMBER: _ClassVar[int]
    CONFIDENCE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    LAST_USED_AT_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    scope: MemoryEntry.Scope
    subject_ref: str
    content: str
    provenance_kind: MemoryEntry.ProvenanceKind
    provenance_ref: str
    confidence: float
    status: MemoryEntry.Status
    last_used_at: str
    expires_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., scope: _Optional[_Union[MemoryEntry.Scope, str]] = ..., subject_ref: _Optional[str] = ..., content: _Optional[str] = ..., provenance_kind: _Optional[_Union[MemoryEntry.ProvenanceKind, str]] = ..., provenance_ref: _Optional[str] = ..., confidence: _Optional[float] = ..., status: _Optional[_Union[MemoryEntry.Status, str]] = ..., last_used_at: _Optional[str] = ..., expires_at: _Optional[str] = ...) -> None: ...

class Skill(_message.Message):
    __slots__ = ("schema_version", "id", "scope", "name", "owner", "current_active_version_id")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    OWNER_FIELD_NUMBER: _ClassVar[int]
    CURRENT_ACTIVE_VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    scope: str
    name: str
    owner: str
    current_active_version_id: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., scope: _Optional[str] = ..., name: _Optional[str] = ..., owner: _Optional[str] = ..., current_active_version_id: _Optional[str] = ...) -> None: ...

class SkillVersion(_message.Message):
    __slots__ = ("schema_version", "id", "skill_id", "semver", "manifest", "provenance", "status")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[SkillVersion.Status]
        STATUS_DRAFT: _ClassVar[SkillVersion.Status]
        STATUS_CANDIDATE: _ClassVar[SkillVersion.Status]
        STATUS_EVALUATING: _ClassVar[SkillVersion.Status]
        STATUS_APPROVED: _ClassVar[SkillVersion.Status]
        STATUS_ACTIVE: _ClassVar[SkillVersion.Status]
        STATUS_DEPRECATED: _ClassVar[SkillVersion.Status]
        STATUS_RETIRED: _ClassVar[SkillVersion.Status]
        STATUS_REJECTED: _ClassVar[SkillVersion.Status]
    STATUS_UNSPECIFIED: SkillVersion.Status
    STATUS_DRAFT: SkillVersion.Status
    STATUS_CANDIDATE: SkillVersion.Status
    STATUS_EVALUATING: SkillVersion.Status
    STATUS_APPROVED: SkillVersion.Status
    STATUS_ACTIVE: SkillVersion.Status
    STATUS_DEPRECATED: SkillVersion.Status
    STATUS_RETIRED: SkillVersion.Status
    STATUS_REJECTED: SkillVersion.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    SKILL_ID_FIELD_NUMBER: _ClassVar[int]
    SEMVER_FIELD_NUMBER: _ClassVar[int]
    MANIFEST_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    skill_id: str
    semver: str
    manifest: SkillManifest
    provenance: str
    status: SkillVersion.Status
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., skill_id: _Optional[str] = ..., semver: _Optional[str] = ..., manifest: _Optional[_Union[SkillManifest, _Mapping]] = ..., provenance: _Optional[str] = ..., status: _Optional[_Union[SkillVersion.Status, str]] = ...) -> None: ...

class SkillManifest(_message.Message):
    __slots__ = ("schema_version", "instructions", "examples", "tool_needs", "capability_needs", "eval_suite_id", "compatibility", "recovery_guidance")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    INSTRUCTIONS_FIELD_NUMBER: _ClassVar[int]
    EXAMPLES_FIELD_NUMBER: _ClassVar[int]
    TOOL_NEEDS_FIELD_NUMBER: _ClassVar[int]
    CAPABILITY_NEEDS_FIELD_NUMBER: _ClassVar[int]
    EVAL_SUITE_ID_FIELD_NUMBER: _ClassVar[int]
    COMPATIBILITY_FIELD_NUMBER: _ClassVar[int]
    RECOVERY_GUIDANCE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    instructions: str
    examples: _containers.RepeatedScalarFieldContainer[str]
    tool_needs: _containers.RepeatedScalarFieldContainer[str]
    capability_needs: _containers.RepeatedScalarFieldContainer[str]
    eval_suite_id: str
    compatibility: str
    recovery_guidance: str
    def __init__(self, schema_version: _Optional[str] = ..., instructions: _Optional[str] = ..., examples: _Optional[_Iterable[str]] = ..., tool_needs: _Optional[_Iterable[str]] = ..., capability_needs: _Optional[_Iterable[str]] = ..., eval_suite_id: _Optional[str] = ..., compatibility: _Optional[str] = ..., recovery_guidance: _Optional[str] = ...) -> None: ...

class CapabilityPack(_message.Message):
    __slots__ = ("schema_version", "id", "tenant_id", "name", "current_published_version_id")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    CURRENT_PUBLISHED_VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    tenant_id: str
    name: str
    current_published_version_id: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., tenant_id: _Optional[str] = ..., name: _Optional[str] = ..., current_published_version_id: _Optional[str] = ...) -> None: ...

class CapabilityPackVersion(_message.Message):
    __slots__ = ("schema_version", "id", "semver", "contents", "provenance", "evidence_requirements", "status")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[CapabilityPackVersion.Status]
        STATUS_DRAFT: _ClassVar[CapabilityPackVersion.Status]
        STATUS_CANDIDATE: _ClassVar[CapabilityPackVersion.Status]
        STATUS_QUALIFYING: _ClassVar[CapabilityPackVersion.Status]
        STATUS_QUALIFIED: _ClassVar[CapabilityPackVersion.Status]
        STATUS_PUBLISHED: _ClassVar[CapabilityPackVersion.Status]
        STATUS_DEPRECATED: _ClassVar[CapabilityPackVersion.Status]
        STATUS_WITHDRAWN: _ClassVar[CapabilityPackVersion.Status]
    STATUS_UNSPECIFIED: CapabilityPackVersion.Status
    STATUS_DRAFT: CapabilityPackVersion.Status
    STATUS_CANDIDATE: CapabilityPackVersion.Status
    STATUS_QUALIFYING: CapabilityPackVersion.Status
    STATUS_QUALIFIED: CapabilityPackVersion.Status
    STATUS_PUBLISHED: CapabilityPackVersion.Status
    STATUS_DEPRECATED: CapabilityPackVersion.Status
    STATUS_WITHDRAWN: CapabilityPackVersion.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    SEMVER_FIELD_NUMBER: _ClassVar[int]
    CONTENTS_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_REQUIREMENTS_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    semver: str
    contents: PackContents
    provenance: str
    evidence_requirements: _containers.RepeatedScalarFieldContainer[str]
    status: CapabilityPackVersion.Status
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., semver: _Optional[str] = ..., contents: _Optional[_Union[PackContents, _Mapping]] = ..., provenance: _Optional[str] = ..., evidence_requirements: _Optional[_Iterable[str]] = ..., status: _Optional[_Union[CapabilityPackVersion.Status, str]] = ...) -> None: ...

class PackContents(_message.Message):
    __slots__ = ("schema_version", "knowledge_ids", "skill_version_ids", "tool_names", "connector_requirements", "policy_requirements", "rbac_requirements", "approval_requirements", "workflow_templates", "examples", "eval_suite_id", "compatibility")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KNOWLEDGE_IDS_FIELD_NUMBER: _ClassVar[int]
    SKILL_VERSION_IDS_FIELD_NUMBER: _ClassVar[int]
    TOOL_NAMES_FIELD_NUMBER: _ClassVar[int]
    CONNECTOR_REQUIREMENTS_FIELD_NUMBER: _ClassVar[int]
    POLICY_REQUIREMENTS_FIELD_NUMBER: _ClassVar[int]
    RBAC_REQUIREMENTS_FIELD_NUMBER: _ClassVar[int]
    APPROVAL_REQUIREMENTS_FIELD_NUMBER: _ClassVar[int]
    WORKFLOW_TEMPLATES_FIELD_NUMBER: _ClassVar[int]
    EXAMPLES_FIELD_NUMBER: _ClassVar[int]
    EVAL_SUITE_ID_FIELD_NUMBER: _ClassVar[int]
    COMPATIBILITY_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    knowledge_ids: _containers.RepeatedScalarFieldContainer[str]
    skill_version_ids: _containers.RepeatedScalarFieldContainer[str]
    tool_names: _containers.RepeatedScalarFieldContainer[str]
    connector_requirements: _containers.RepeatedScalarFieldContainer[str]
    policy_requirements: _containers.RepeatedScalarFieldContainer[str]
    rbac_requirements: _containers.RepeatedScalarFieldContainer[str]
    approval_requirements: _containers.RepeatedScalarFieldContainer[str]
    workflow_templates: _containers.RepeatedScalarFieldContainer[str]
    examples: _containers.RepeatedScalarFieldContainer[str]
    eval_suite_id: str
    compatibility: str
    def __init__(self, schema_version: _Optional[str] = ..., knowledge_ids: _Optional[_Iterable[str]] = ..., skill_version_ids: _Optional[_Iterable[str]] = ..., tool_names: _Optional[_Iterable[str]] = ..., connector_requirements: _Optional[_Iterable[str]] = ..., policy_requirements: _Optional[_Iterable[str]] = ..., rbac_requirements: _Optional[_Iterable[str]] = ..., approval_requirements: _Optional[_Iterable[str]] = ..., workflow_templates: _Optional[_Iterable[str]] = ..., examples: _Optional[_Iterable[str]] = ..., eval_suite_id: _Optional[str] = ..., compatibility: _Optional[str] = ...) -> None: ...
