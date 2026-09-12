from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class Artifact(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "kind", "title", "role", "origin", "current_version_id", "grants", "retention", "deleted_at")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Artifact.Kind]
        KIND_DOCUMENT: _ClassVar[Artifact.Kind]
        KIND_SPREADSHEET: _ClassVar[Artifact.Kind]
        KIND_PRESENTATION: _ClassVar[Artifact.Kind]
        KIND_CODE: _ClassVar[Artifact.Kind]
        KIND_DATA: _ClassVar[Artifact.Kind]
        KIND_IMAGE: _ClassVar[Artifact.Kind]
        KIND_AUDIO: _ClassVar[Artifact.Kind]
        KIND_VIDEO: _ClassVar[Artifact.Kind]
        KIND_ARCHIVE: _ClassVar[Artifact.Kind]
        KIND_OTHER: _ClassVar[Artifact.Kind]
    KIND_UNSPECIFIED: Artifact.Kind
    KIND_DOCUMENT: Artifact.Kind
    KIND_SPREADSHEET: Artifact.Kind
    KIND_PRESENTATION: Artifact.Kind
    KIND_CODE: Artifact.Kind
    KIND_DATA: Artifact.Kind
    KIND_IMAGE: Artifact.Kind
    KIND_AUDIO: Artifact.Kind
    KIND_VIDEO: Artifact.Kind
    KIND_ARCHIVE: Artifact.Kind
    KIND_OTHER: Artifact.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    TITLE_FIELD_NUMBER: _ClassVar[int]
    ROLE_FIELD_NUMBER: _ClassVar[int]
    ORIGIN_FIELD_NUMBER: _ClassVar[int]
    CURRENT_VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    GRANTS_FIELD_NUMBER: _ClassVar[int]
    RETENTION_FIELD_NUMBER: _ClassVar[int]
    DELETED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    kind: Artifact.Kind
    title: str
    role: str
    origin: Origin
    current_version_id: str
    grants: _containers.RepeatedCompositeFieldContainer[ArtifactGrant]
    retention: Retention
    deleted_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., kind: _Optional[_Union[Artifact.Kind, str]] = ..., title: _Optional[str] = ..., role: _Optional[str] = ..., origin: _Optional[_Union[Origin, _Mapping]] = ..., current_version_id: _Optional[str] = ..., grants: _Optional[_Iterable[_Union[ArtifactGrant, _Mapping]]] = ..., retention: _Optional[_Union[Retention, _Mapping]] = ..., deleted_at: _Optional[str] = ...) -> None: ...

class Origin(_message.Message):
    __slots__ = ("schema_version", "kind", "ref")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Origin.Kind]
        KIND_MESSAGE: _ClassVar[Origin.Kind]
        KIND_RUN: _ClassVar[Origin.Kind]
        KIND_ROUTINE: _ClassVar[Origin.Kind]
        KIND_UPLOAD: _ClassVar[Origin.Kind]
        KIND_CONNECTOR: _ClassVar[Origin.Kind]
    KIND_UNSPECIFIED: Origin.Kind
    KIND_MESSAGE: Origin.Kind
    KIND_RUN: Origin.Kind
    KIND_ROUTINE: Origin.Kind
    KIND_UPLOAD: Origin.Kind
    KIND_CONNECTOR: Origin.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    REF_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: Origin.Kind
    ref: str
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[Origin.Kind, str]] = ..., ref: _Optional[str] = ...) -> None: ...

class ArtifactGrant(_message.Message):
    __slots__ = ("schema_version", "principal", "level")
    class Level(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        LEVEL_UNSPECIFIED: _ClassVar[ArtifactGrant.Level]
        LEVEL_READ: _ClassVar[ArtifactGrant.Level]
        LEVEL_WRITE: _ClassVar[ArtifactGrant.Level]
    LEVEL_UNSPECIFIED: ArtifactGrant.Level
    LEVEL_READ: ArtifactGrant.Level
    LEVEL_WRITE: ArtifactGrant.Level
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    PRINCIPAL_FIELD_NUMBER: _ClassVar[int]
    LEVEL_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    principal: str
    level: ArtifactGrant.Level
    def __init__(self, schema_version: _Optional[str] = ..., principal: _Optional[str] = ..., level: _Optional[_Union[ArtifactGrant.Level, str]] = ...) -> None: ...

class Retention(_message.Message):
    __slots__ = ("schema_version", "retention_class", "expires_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    RETENTION_CLASS_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    retention_class: str
    expires_at: str
    def __init__(self, schema_version: _Optional[str] = ..., retention_class: _Optional[str] = ..., expires_at: _Optional[str] = ...) -> None: ...

class ArtifactVersion(_message.Message):
    __slots__ = ("schema_version", "id", "artifact_id", "seq", "content_digest", "size_bytes", "media_type", "object_key", "produced_by_run_id", "produced_by_step_id", "produced_by_user_id", "parent_version_id", "created_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    ARTIFACT_ID_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    CONTENT_DIGEST_FIELD_NUMBER: _ClassVar[int]
    SIZE_BYTES_FIELD_NUMBER: _ClassVar[int]
    MEDIA_TYPE_FIELD_NUMBER: _ClassVar[int]
    OBJECT_KEY_FIELD_NUMBER: _ClassVar[int]
    PRODUCED_BY_RUN_ID_FIELD_NUMBER: _ClassVar[int]
    PRODUCED_BY_STEP_ID_FIELD_NUMBER: _ClassVar[int]
    PRODUCED_BY_USER_ID_FIELD_NUMBER: _ClassVar[int]
    PARENT_VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    artifact_id: str
    seq: int
    content_digest: str
    size_bytes: int
    media_type: str
    object_key: str
    produced_by_run_id: str
    produced_by_step_id: str
    produced_by_user_id: str
    parent_version_id: str
    created_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., artifact_id: _Optional[str] = ..., seq: _Optional[int] = ..., content_digest: _Optional[str] = ..., size_bytes: _Optional[int] = ..., media_type: _Optional[str] = ..., object_key: _Optional[str] = ..., produced_by_run_id: _Optional[str] = ..., produced_by_step_id: _Optional[str] = ..., produced_by_user_id: _Optional[str] = ..., parent_version_id: _Optional[str] = ..., created_at: _Optional[str] = ...) -> None: ...

class Evidence(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "run_id", "step_id", "effect_id", "kind", "content_digest", "object_key", "inline_summary", "captured_at", "captured_by_kind", "captured_by_id", "redaction_applied")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Evidence.Kind]
        KIND_TOOL_OUTPUT: _ClassVar[Evidence.Kind]
        KIND_SCREENSHOT: _ClassVar[Evidence.Kind]
        KIND_DOM_SNAPSHOT: _ClassVar[Evidence.Kind]
        KIND_LOG: _ClassVar[Evidence.Kind]
        KIND_TEST_RESULT: _ClassVar[Evidence.Kind]
        KIND_HTTP_EXCHANGE: _ClassVar[Evidence.Kind]
        KIND_DIFF: _ClassVar[Evidence.Kind]
        KIND_DIGEST: _ClassVar[Evidence.Kind]
        KIND_EXTERNAL_REF: _ClassVar[Evidence.Kind]
    KIND_UNSPECIFIED: Evidence.Kind
    KIND_TOOL_OUTPUT: Evidence.Kind
    KIND_SCREENSHOT: Evidence.Kind
    KIND_DOM_SNAPSHOT: Evidence.Kind
    KIND_LOG: Evidence.Kind
    KIND_TEST_RESULT: Evidence.Kind
    KIND_HTTP_EXCHANGE: Evidence.Kind
    KIND_DIFF: Evidence.Kind
    KIND_DIGEST: Evidence.Kind
    KIND_EXTERNAL_REF: Evidence.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    STEP_ID_FIELD_NUMBER: _ClassVar[int]
    EFFECT_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    CONTENT_DIGEST_FIELD_NUMBER: _ClassVar[int]
    OBJECT_KEY_FIELD_NUMBER: _ClassVar[int]
    INLINE_SUMMARY_FIELD_NUMBER: _ClassVar[int]
    CAPTURED_AT_FIELD_NUMBER: _ClassVar[int]
    CAPTURED_BY_KIND_FIELD_NUMBER: _ClassVar[int]
    CAPTURED_BY_ID_FIELD_NUMBER: _ClassVar[int]
    REDACTION_APPLIED_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    run_id: str
    step_id: str
    effect_id: str
    kind: Evidence.Kind
    content_digest: str
    object_key: str
    inline_summary: str
    captured_at: str
    captured_by_kind: str
    captured_by_id: str
    redaction_applied: bool
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., run_id: _Optional[str] = ..., step_id: _Optional[str] = ..., effect_id: _Optional[str] = ..., kind: _Optional[_Union[Evidence.Kind, str]] = ..., content_digest: _Optional[str] = ..., object_key: _Optional[str] = ..., inline_summary: _Optional[str] = ..., captured_at: _Optional[str] = ..., captured_by_kind: _Optional[str] = ..., captured_by_id: _Optional[str] = ..., redaction_applied: _Optional[bool] = ...) -> None: ...

class EvidenceBundle(_message.Message):
    __slots__ = ("schema_version", "id", "run_id", "artifact_version_id", "claims", "sources", "coverage", "verification")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    ARTIFACT_VERSION_ID_FIELD_NUMBER: _ClassVar[int]
    CLAIMS_FIELD_NUMBER: _ClassVar[int]
    SOURCES_FIELD_NUMBER: _ClassVar[int]
    COVERAGE_FIELD_NUMBER: _ClassVar[int]
    VERIFICATION_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    run_id: str
    artifact_version_id: str
    claims: _containers.RepeatedCompositeFieldContainer[Claim]
    sources: _containers.RepeatedCompositeFieldContainer[Source]
    coverage: float
    verification: Verification
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., run_id: _Optional[str] = ..., artifact_version_id: _Optional[str] = ..., claims: _Optional[_Iterable[_Union[Claim, _Mapping]]] = ..., sources: _Optional[_Iterable[_Union[Source, _Mapping]]] = ..., coverage: _Optional[float] = ..., verification: _Optional[_Union[Verification, _Mapping]] = ...) -> None: ...

class Claim(_message.Message):
    __slots__ = ("schema_version", "claim_id", "text_span", "evidence_ids", "support_score")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    CLAIM_ID_FIELD_NUMBER: _ClassVar[int]
    TEXT_SPAN_FIELD_NUMBER: _ClassVar[int]
    EVIDENCE_IDS_FIELD_NUMBER: _ClassVar[int]
    SUPPORT_SCORE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    claim_id: str
    text_span: str
    evidence_ids: _containers.RepeatedScalarFieldContainer[str]
    support_score: float
    def __init__(self, schema_version: _Optional[str] = ..., claim_id: _Optional[str] = ..., text_span: _Optional[str] = ..., evidence_ids: _Optional[_Iterable[str]] = ..., support_score: _Optional[float] = ...) -> None: ...

class Source(_message.Message):
    __slots__ = ("schema_version", "source_id", "uri", "artifact_ref", "retrieved_at", "excerpt_locator", "excerpt_digest")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SOURCE_ID_FIELD_NUMBER: _ClassVar[int]
    URI_FIELD_NUMBER: _ClassVar[int]
    ARTIFACT_REF_FIELD_NUMBER: _ClassVar[int]
    RETRIEVED_AT_FIELD_NUMBER: _ClassVar[int]
    EXCERPT_LOCATOR_FIELD_NUMBER: _ClassVar[int]
    EXCERPT_DIGEST_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    source_id: str
    uri: str
    artifact_ref: str
    retrieved_at: str
    excerpt_locator: str
    excerpt_digest: str
    def __init__(self, schema_version: _Optional[str] = ..., source_id: _Optional[str] = ..., uri: _Optional[str] = ..., artifact_ref: _Optional[str] = ..., retrieved_at: _Optional[str] = ..., excerpt_locator: _Optional[str] = ..., excerpt_digest: _Optional[str] = ...) -> None: ...

class Verification(_message.Message):
    __slots__ = ("schema_version", "deterministic_passed", "semantic_score", "verified_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    DETERMINISTIC_PASSED_FIELD_NUMBER: _ClassVar[int]
    SEMANTIC_SCORE_FIELD_NUMBER: _ClassVar[int]
    VERIFIED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    deterministic_passed: bool
    semantic_score: float
    verified_at: str
    def __init__(self, schema_version: _Optional[str] = ..., deterministic_passed: _Optional[bool] = ..., semantic_score: _Optional[float] = ..., verified_at: _Optional[str] = ...) -> None: ...
