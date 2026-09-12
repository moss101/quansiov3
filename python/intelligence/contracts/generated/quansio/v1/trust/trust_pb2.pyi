from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class TrustLevel(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    TRUST_LEVEL_UNSPECIFIED: _ClassVar[TrustLevel]
    TRUST_LEVEL_TRUSTED_SYSTEM: _ClassVar[TrustLevel]
    TRUST_LEVEL_TRUSTED_USER: _ClassVar[TrustLevel]
    TRUST_LEVEL_VERIFIED_KNOWLEDGE: _ClassVar[TrustLevel]
    TRUST_LEVEL_AGENT_GENERATED: _ClassVar[TrustLevel]
    TRUST_LEVEL_UNTRUSTED_EXTERNAL: _ClassVar[TrustLevel]
TRUST_LEVEL_UNSPECIFIED: TrustLevel
TRUST_LEVEL_TRUSTED_SYSTEM: TrustLevel
TRUST_LEVEL_TRUSTED_USER: TrustLevel
TRUST_LEVEL_VERIFIED_KNOWLEDGE: TrustLevel
TRUST_LEVEL_AGENT_GENERATED: TrustLevel
TRUST_LEVEL_UNTRUSTED_EXTERNAL: TrustLevel

class ContentSegment(_message.Message):
    __slots__ = ("schema_version", "seq", "source_kind", "source_ref", "trust_level", "token_estimate", "snapshot_ref", "redactions", "injection_suspected")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    SOURCE_KIND_FIELD_NUMBER: _ClassVar[int]
    SOURCE_REF_FIELD_NUMBER: _ClassVar[int]
    TRUST_LEVEL_FIELD_NUMBER: _ClassVar[int]
    TOKEN_ESTIMATE_FIELD_NUMBER: _ClassVar[int]
    SNAPSHOT_REF_FIELD_NUMBER: _ClassVar[int]
    REDACTIONS_FIELD_NUMBER: _ClassVar[int]
    INJECTION_SUSPECTED_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    seq: int
    source_kind: str
    source_ref: str
    trust_level: TrustLevel
    token_estimate: int
    snapshot_ref: str
    redactions: _containers.RepeatedScalarFieldContainer[str]
    injection_suspected: bool
    def __init__(self, schema_version: _Optional[str] = ..., seq: _Optional[int] = ..., source_kind: _Optional[str] = ..., source_ref: _Optional[str] = ..., trust_level: _Optional[_Union[TrustLevel, str]] = ..., token_estimate: _Optional[int] = ..., snapshot_ref: _Optional[str] = ..., redactions: _Optional[_Iterable[str]] = ..., injection_suspected: _Optional[bool] = ...) -> None: ...

class TrustEscalation(_message.Message):
    __slots__ = ("schema_version", "derived_from_trust", "original_tier", "escalated_tier", "always_rules_allowed", "approval_required")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    DERIVED_FROM_TRUST_FIELD_NUMBER: _ClassVar[int]
    ORIGINAL_TIER_FIELD_NUMBER: _ClassVar[int]
    ESCALATED_TIER_FIELD_NUMBER: _ClassVar[int]
    ALWAYS_RULES_ALLOWED_FIELD_NUMBER: _ClassVar[int]
    APPROVAL_REQUIRED_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    derived_from_trust: TrustLevel
    original_tier: int
    escalated_tier: int
    always_rules_allowed: bool
    approval_required: bool
    def __init__(self, schema_version: _Optional[str] = ..., derived_from_trust: _Optional[_Union[TrustLevel, str]] = ..., original_tier: _Optional[int] = ..., escalated_tier: _Optional[int] = ..., always_rules_allowed: _Optional[bool] = ..., approval_required: _Optional[bool] = ...) -> None: ...
