from quansio.v1.intelligence import intelligence_pb2 as _intelligence_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ScopeContext(_message.Message):
    __slots__ = ("schema_version", "tenant_id", "workspace_id", "correlation_id", "deadline_ms", "capability_projection_id")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    CORRELATION_ID_FIELD_NUMBER: _ClassVar[int]
    DEADLINE_MS_FIELD_NUMBER: _ClassVar[int]
    CAPABILITY_PROJECTION_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    tenant_id: str
    workspace_id: str
    correlation_id: str
    deadline_ms: int
    capability_projection_id: str
    def __init__(self, schema_version: _Optional[str] = ..., tenant_id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., correlation_id: _Optional[str] = ..., deadline_ms: _Optional[int] = ..., capability_projection_id: _Optional[str] = ...) -> None: ...

class ContextBuildRequest(_message.Message):
    __slots__ = ("schema_version", "scope", "run_id", "turn_id", "segment_requests_json", "token_budget", "policy_snapshot_digest")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    TURN_ID_FIELD_NUMBER: _ClassVar[int]
    SEGMENT_REQUESTS_JSON_FIELD_NUMBER: _ClassVar[int]
    TOKEN_BUDGET_FIELD_NUMBER: _ClassVar[int]
    POLICY_SNAPSHOT_DIGEST_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    scope: ScopeContext
    run_id: str
    turn_id: str
    segment_requests_json: _containers.RepeatedScalarFieldContainer[str]
    token_budget: int
    policy_snapshot_digest: str
    def __init__(self, schema_version: _Optional[str] = ..., scope: _Optional[_Union[ScopeContext, _Mapping]] = ..., run_id: _Optional[str] = ..., turn_id: _Optional[str] = ..., segment_requests_json: _Optional[_Iterable[str]] = ..., token_budget: _Optional[int] = ..., policy_snapshot_digest: _Optional[str] = ...) -> None: ...

class SearchResultSet(_message.Message):
    __slots__ = ("schema_version", "results", "snapshot")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    RESULTS_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    results: _containers.RepeatedCompositeFieldContainer[_intelligence_pb2.SearchResult]
    snapshot: str
    def __init__(self, schema_version: _Optional[str] = ..., results: _Optional[_Iterable[_Union[_intelligence_pb2.SearchResult, _Mapping]]] = ..., snapshot: _Optional[str] = ...) -> None: ...

class MemoryCandidate(_message.Message):
    __slots__ = ("schema_version", "scope", "subject_ref", "content", "provenance_kind", "provenance_ref", "confidence")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    SUBJECT_REF_FIELD_NUMBER: _ClassVar[int]
    CONTENT_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_KIND_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_REF_FIELD_NUMBER: _ClassVar[int]
    CONFIDENCE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    scope: ScopeContext
    subject_ref: str
    content: str
    provenance_kind: str
    provenance_ref: str
    confidence: float
    def __init__(self, schema_version: _Optional[str] = ..., scope: _Optional[_Union[ScopeContext, _Mapping]] = ..., subject_ref: _Optional[str] = ..., content: _Optional[str] = ..., provenance_kind: _Optional[str] = ..., provenance_ref: _Optional[str] = ..., confidence: _Optional[float] = ...) -> None: ...

class ProposalAck(_message.Message):
    __slots__ = ("schema_version", "accepted", "proposal_id", "reason")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ACCEPTED_FIELD_NUMBER: _ClassVar[int]
    PROPOSAL_ID_FIELD_NUMBER: _ClassVar[int]
    REASON_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    accepted: bool
    proposal_id: str
    reason: str
    def __init__(self, schema_version: _Optional[str] = ..., accepted: _Optional[bool] = ..., proposal_id: _Optional[str] = ..., reason: _Optional[str] = ...) -> None: ...

class EmbedRequest(_message.Message):
    __slots__ = ("schema_version", "scope", "texts", "embedding_model")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    TEXTS_FIELD_NUMBER: _ClassVar[int]
    EMBEDDING_MODEL_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    scope: ScopeContext
    texts: _containers.RepeatedScalarFieldContainer[str]
    embedding_model: str
    def __init__(self, schema_version: _Optional[str] = ..., scope: _Optional[_Union[ScopeContext, _Mapping]] = ..., texts: _Optional[_Iterable[str]] = ..., embedding_model: _Optional[str] = ...) -> None: ...

class Embedding(_message.Message):
    __slots__ = ("schema_version", "index", "vector")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    INDEX_FIELD_NUMBER: _ClassVar[int]
    VECTOR_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    index: int
    vector: _containers.RepeatedScalarFieldContainer[float]
    def __init__(self, schema_version: _Optional[str] = ..., index: _Optional[int] = ..., vector: _Optional[_Iterable[float]] = ...) -> None: ...

class EmbedResponse(_message.Message):
    __slots__ = ("schema_version", "embeddings", "model_id", "dimensions")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    EMBEDDINGS_FIELD_NUMBER: _ClassVar[int]
    MODEL_ID_FIELD_NUMBER: _ClassVar[int]
    DIMENSIONS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    embeddings: _containers.RepeatedCompositeFieldContainer[Embedding]
    model_id: str
    dimensions: int
    def __init__(self, schema_version: _Optional[str] = ..., embeddings: _Optional[_Iterable[_Union[Embedding, _Mapping]]] = ..., model_id: _Optional[str] = ..., dimensions: _Optional[int] = ...) -> None: ...

class TrustClassifyRequest(_message.Message):
    __slots__ = ("schema_version", "scope", "source_kind", "content")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    SOURCE_KIND_FIELD_NUMBER: _ClassVar[int]
    CONTENT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    scope: ScopeContext
    source_kind: str
    content: str
    def __init__(self, schema_version: _Optional[str] = ..., scope: _Optional[_Union[ScopeContext, _Mapping]] = ..., source_kind: _Optional[str] = ..., content: _Optional[str] = ...) -> None: ...

class TrustClassifyResponse(_message.Message):
    __slots__ = ("schema_version", "trust_level", "injection_suspected", "matched_patterns")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    TRUST_LEVEL_FIELD_NUMBER: _ClassVar[int]
    INJECTION_SUSPECTED_FIELD_NUMBER: _ClassVar[int]
    MATCHED_PATTERNS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    trust_level: int
    injection_suspected: bool
    matched_patterns: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, schema_version: _Optional[str] = ..., trust_level: _Optional[int] = ..., injection_suspected: _Optional[bool] = ..., matched_patterns: _Optional[_Iterable[str]] = ...) -> None: ...

class EvaluationRequest(_message.Message):
    __slots__ = ("schema_version", "scope", "suite_id", "cases_json")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SCOPE_FIELD_NUMBER: _ClassVar[int]
    SUITE_ID_FIELD_NUMBER: _ClassVar[int]
    CASES_JSON_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    scope: ScopeContext
    suite_id: str
    cases_json: str
    def __init__(self, schema_version: _Optional[str] = ..., scope: _Optional[_Union[ScopeContext, _Mapping]] = ..., suite_id: _Optional[str] = ..., cases_json: _Optional[str] = ...) -> None: ...

class EvaluationReport(_message.Message):
    __slots__ = ("schema_version", "suite_id", "passed", "failed", "details_json")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SUITE_ID_FIELD_NUMBER: _ClassVar[int]
    PASSED_FIELD_NUMBER: _ClassVar[int]
    FAILED_FIELD_NUMBER: _ClassVar[int]
    DETAILS_JSON_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    suite_id: str
    passed: int
    failed: int
    details_json: str
    def __init__(self, schema_version: _Optional[str] = ..., suite_id: _Optional[str] = ..., passed: _Optional[int] = ..., failed: _Optional[int] = ..., details_json: _Optional[str] = ...) -> None: ...
