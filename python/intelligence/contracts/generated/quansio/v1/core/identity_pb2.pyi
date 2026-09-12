from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class EntityPrefix(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    ENTITY_PREFIX_UNSPECIFIED: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_TENANT: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_WORKSPACE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_USER: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_SERVICE_PRINCIPAL: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_TEAMMATE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_AGENT_THREAD: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_THREAD: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_MESSAGE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_WORK_NODE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_WORK_EDGE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_RUN: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_TURN: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_STEP: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_ATTEMPT: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_COMMAND: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_RUNTIME_EVENT: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_QUESTION: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_SKILL: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_SKILL_VERSION: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_CAPABILITY_PACK: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_CAPABILITY_PACK_VERSION: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_CONNECTOR: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_MODEL_ROUTE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_AUDIT_ENTRY: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_TOOL_CALL: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_EFFECT_RECORD: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_APPROVAL_REQUEST: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_APPROVAL_RECEIPT: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_USER_RULE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_CAPABILITY_PROJECTION: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_POLICY: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_EXECUTION_TARGET: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_LEASE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_CHECKPOINT: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_COMPACTION_EPOCH: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_ARTIFACT: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_ARTIFACT_VERSION: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_EVIDENCE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_EVIDENCE_BUNDLE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_KNOWLEDGE_ENTRY: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_MEMORY_ENTRY: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_ROUTINE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_NOTIFICATION: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_SECRET_HANDLE: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_USAGE_RECORD: _ClassVar[EntityPrefix]
    ENTITY_PREFIX_WEBHOOK_SUBSCRIPTION: _ClassVar[EntityPrefix]

class MembershipStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    MEMBERSHIP_STATUS_UNSPECIFIED: _ClassVar[MembershipStatus]
    MEMBERSHIP_STATUS_INVITED: _ClassVar[MembershipStatus]
    MEMBERSHIP_STATUS_ACTIVE: _ClassVar[MembershipStatus]
    MEMBERSHIP_STATUS_SUSPENDED: _ClassVar[MembershipStatus]
    MEMBERSHIP_STATUS_REMOVED: _ClassVar[MembershipStatus]

class TenantRole(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    TENANT_ROLE_UNSPECIFIED: _ClassVar[TenantRole]
    TENANT_ROLE_OWNER: _ClassVar[TenantRole]
    TENANT_ROLE_ADMIN: _ClassVar[TenantRole]
    TENANT_ROLE_BILLING: _ClassVar[TenantRole]
    TENANT_ROLE_MEMBER: _ClassVar[TenantRole]

class WorkspaceRole(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    WORKSPACE_ROLE_UNSPECIFIED: _ClassVar[WorkspaceRole]
    WORKSPACE_ROLE_ADMIN: _ClassVar[WorkspaceRole]
    WORKSPACE_ROLE_EDITOR: _ClassVar[WorkspaceRole]
    WORKSPACE_ROLE_APPROVER: _ClassVar[WorkspaceRole]
    WORKSPACE_ROLE_VIEWER: _ClassVar[WorkspaceRole]
ENTITY_PREFIX_UNSPECIFIED: EntityPrefix
ENTITY_PREFIX_TENANT: EntityPrefix
ENTITY_PREFIX_WORKSPACE: EntityPrefix
ENTITY_PREFIX_USER: EntityPrefix
ENTITY_PREFIX_SERVICE_PRINCIPAL: EntityPrefix
ENTITY_PREFIX_TEAMMATE: EntityPrefix
ENTITY_PREFIX_AGENT_THREAD: EntityPrefix
ENTITY_PREFIX_THREAD: EntityPrefix
ENTITY_PREFIX_MESSAGE: EntityPrefix
ENTITY_PREFIX_WORK_NODE: EntityPrefix
ENTITY_PREFIX_WORK_EDGE: EntityPrefix
ENTITY_PREFIX_RUN: EntityPrefix
ENTITY_PREFIX_TURN: EntityPrefix
ENTITY_PREFIX_STEP: EntityPrefix
ENTITY_PREFIX_ATTEMPT: EntityPrefix
ENTITY_PREFIX_COMMAND: EntityPrefix
ENTITY_PREFIX_RUNTIME_EVENT: EntityPrefix
ENTITY_PREFIX_QUESTION: EntityPrefix
ENTITY_PREFIX_SKILL: EntityPrefix
ENTITY_PREFIX_SKILL_VERSION: EntityPrefix
ENTITY_PREFIX_CAPABILITY_PACK: EntityPrefix
ENTITY_PREFIX_CAPABILITY_PACK_VERSION: EntityPrefix
ENTITY_PREFIX_CONNECTOR: EntityPrefix
ENTITY_PREFIX_MODEL_ROUTE: EntityPrefix
ENTITY_PREFIX_AUDIT_ENTRY: EntityPrefix
ENTITY_PREFIX_TOOL_CALL: EntityPrefix
ENTITY_PREFIX_EFFECT_RECORD: EntityPrefix
ENTITY_PREFIX_APPROVAL_REQUEST: EntityPrefix
ENTITY_PREFIX_APPROVAL_RECEIPT: EntityPrefix
ENTITY_PREFIX_USER_RULE: EntityPrefix
ENTITY_PREFIX_CAPABILITY_PROJECTION: EntityPrefix
ENTITY_PREFIX_POLICY: EntityPrefix
ENTITY_PREFIX_EXECUTION_TARGET: EntityPrefix
ENTITY_PREFIX_LEASE: EntityPrefix
ENTITY_PREFIX_CHECKPOINT: EntityPrefix
ENTITY_PREFIX_COMPACTION_EPOCH: EntityPrefix
ENTITY_PREFIX_ARTIFACT: EntityPrefix
ENTITY_PREFIX_ARTIFACT_VERSION: EntityPrefix
ENTITY_PREFIX_EVIDENCE: EntityPrefix
ENTITY_PREFIX_EVIDENCE_BUNDLE: EntityPrefix
ENTITY_PREFIX_KNOWLEDGE_ENTRY: EntityPrefix
ENTITY_PREFIX_MEMORY_ENTRY: EntityPrefix
ENTITY_PREFIX_ROUTINE: EntityPrefix
ENTITY_PREFIX_NOTIFICATION: EntityPrefix
ENTITY_PREFIX_SECRET_HANDLE: EntityPrefix
ENTITY_PREFIX_USAGE_RECORD: EntityPrefix
ENTITY_PREFIX_WEBHOOK_SUBSCRIPTION: EntityPrefix
MEMBERSHIP_STATUS_UNSPECIFIED: MembershipStatus
MEMBERSHIP_STATUS_INVITED: MembershipStatus
MEMBERSHIP_STATUS_ACTIVE: MembershipStatus
MEMBERSHIP_STATUS_SUSPENDED: MembershipStatus
MEMBERSHIP_STATUS_REMOVED: MembershipStatus
TENANT_ROLE_UNSPECIFIED: TenantRole
TENANT_ROLE_OWNER: TenantRole
TENANT_ROLE_ADMIN: TenantRole
TENANT_ROLE_BILLING: TenantRole
TENANT_ROLE_MEMBER: TenantRole
WORKSPACE_ROLE_UNSPECIFIED: WorkspaceRole
WORKSPACE_ROLE_ADMIN: WorkspaceRole
WORKSPACE_ROLE_EDITOR: WorkspaceRole
WORKSPACE_ROLE_APPROVER: WorkspaceRole
WORKSPACE_ROLE_VIEWER: WorkspaceRole

class CanonicalId(_message.Message):
    __slots__ = ("schema_version", "prefix", "ulid")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    PREFIX_FIELD_NUMBER: _ClassVar[int]
    ULID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    prefix: str
    ulid: str
    def __init__(self, schema_version: _Optional[str] = ..., prefix: _Optional[str] = ..., ulid: _Optional[str] = ...) -> None: ...

class ScopeRef(_message.Message):
    __slots__ = ("schema_version", "tenant_id", "workspace_id")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    tenant_id: str
    workspace_id: str
    def __init__(self, schema_version: _Optional[str] = ..., tenant_id: _Optional[str] = ..., workspace_id: _Optional[str] = ...) -> None: ...

class Actor(_message.Message):
    __slots__ = ("schema_version", "kind", "id")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Actor.Kind]
        KIND_USER: _ClassVar[Actor.Kind]
        KIND_AGENT: _ClassVar[Actor.Kind]
        KIND_SYSTEM: _ClassVar[Actor.Kind]
        KIND_SERVICE: _ClassVar[Actor.Kind]
    KIND_UNSPECIFIED: Actor.Kind
    KIND_USER: Actor.Kind
    KIND_AGENT: Actor.Kind
    KIND_SYSTEM: Actor.Kind
    KIND_SERVICE: Actor.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: Actor.Kind
    id: str
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[Actor.Kind, str]] = ..., id: _Optional[str] = ...) -> None: ...

class ReplayContext(_message.Message):
    __slots__ = ("schema_version", "command_id", "idempotency_key", "generation", "correlation_id", "causation_id")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    COMMAND_ID_FIELD_NUMBER: _ClassVar[int]
    IDEMPOTENCY_KEY_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    CORRELATION_ID_FIELD_NUMBER: _ClassVar[int]
    CAUSATION_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    command_id: str
    idempotency_key: str
    generation: int
    correlation_id: str
    causation_id: str
    def __init__(self, schema_version: _Optional[str] = ..., command_id: _Optional[str] = ..., idempotency_key: _Optional[str] = ..., generation: _Optional[int] = ..., correlation_id: _Optional[str] = ..., causation_id: _Optional[str] = ...) -> None: ...

class Tenant(_message.Message):
    __slots__ = ("schema_version", "id", "name", "personal", "created_at", "updated_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    PERSONAL_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    UPDATED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    name: str
    personal: bool
    created_at: str
    updated_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., name: _Optional[str] = ..., personal: _Optional[bool] = ..., created_at: _Optional[str] = ..., updated_at: _Optional[str] = ...) -> None: ...

class Workspace(_message.Message):
    __slots__ = ("schema_version", "id", "tenant_id", "name", "created_at", "updated_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    UPDATED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    tenant_id: str
    name: str
    created_at: str
    updated_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., tenant_id: _Optional[str] = ..., name: _Optional[str] = ..., created_at: _Optional[str] = ..., updated_at: _Optional[str] = ...) -> None: ...

class User(_message.Message):
    __slots__ = ("schema_version", "id", "primary_email", "display_name", "status", "auth_methods", "created_at")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[User.Status]
        STATUS_ACTIVE: _ClassVar[User.Status]
        STATUS_SUSPENDED: _ClassVar[User.Status]
        STATUS_DELETED: _ClassVar[User.Status]
    STATUS_UNSPECIFIED: User.Status
    STATUS_ACTIVE: User.Status
    STATUS_SUSPENDED: User.Status
    STATUS_DELETED: User.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    PRIMARY_EMAIL_FIELD_NUMBER: _ClassVar[int]
    DISPLAY_NAME_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    AUTH_METHODS_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    primary_email: str
    display_name: str
    status: User.Status
    auth_methods: _containers.RepeatedScalarFieldContainer[str]
    created_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., primary_email: _Optional[str] = ..., display_name: _Optional[str] = ..., status: _Optional[_Union[User.Status, str]] = ..., auth_methods: _Optional[_Iterable[str]] = ..., created_at: _Optional[str] = ...) -> None: ...

class TenantMembership(_message.Message):
    __slots__ = ("schema_version", "tenant_id", "user_id", "role", "status", "invited_by")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    USER_ID_FIELD_NUMBER: _ClassVar[int]
    ROLE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    INVITED_BY_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    tenant_id: str
    user_id: str
    role: TenantRole
    status: MembershipStatus
    invited_by: str
    def __init__(self, schema_version: _Optional[str] = ..., tenant_id: _Optional[str] = ..., user_id: _Optional[str] = ..., role: _Optional[_Union[TenantRole, str]] = ..., status: _Optional[_Union[MembershipStatus, str]] = ..., invited_by: _Optional[str] = ...) -> None: ...

class WorkspaceMembership(_message.Message):
    __slots__ = ("schema_version", "workspace_id", "user_id", "role", "status")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    USER_ID_FIELD_NUMBER: _ClassVar[int]
    ROLE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    workspace_id: str
    user_id: str
    role: WorkspaceRole
    status: MembershipStatus
    def __init__(self, schema_version: _Optional[str] = ..., workspace_id: _Optional[str] = ..., user_id: _Optional[str] = ..., role: _Optional[_Union[WorkspaceRole, str]] = ..., status: _Optional[_Union[MembershipStatus, str]] = ...) -> None: ...

class ServicePrincipal(_message.Message):
    __slots__ = ("schema_version", "id", "tenant_id", "name", "scopes", "key_hash", "status", "last_used_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TENANT_ID_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    SCOPES_FIELD_NUMBER: _ClassVar[int]
    KEY_HASH_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    LAST_USED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    tenant_id: str
    name: str
    scopes: _containers.RepeatedScalarFieldContainer[str]
    key_hash: str
    status: MembershipStatus
    last_used_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., tenant_id: _Optional[str] = ..., name: _Optional[str] = ..., scopes: _Optional[_Iterable[str]] = ..., key_hash: _Optional[str] = ..., status: _Optional[_Union[MembershipStatus, str]] = ..., last_used_at: _Optional[str] = ...) -> None: ...
