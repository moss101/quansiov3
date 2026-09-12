from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class TargetClass(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    TARGET_CLASS_UNSPECIFIED: _ClassVar[TargetClass]
    TARGET_CLASS_PERSISTENT_WORKSPACE_COMPUTER: _ClassVar[TargetClass]
    TARGET_CLASS_ISOLATED_TASK_RUNTIME: _ClassVar[TargetClass]

class Substrate(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    SUBSTRATE_UNSPECIFIED: _ClassVar[Substrate]
    SUBSTRATE_CLOUD_MICROVM: _ClassVar[Substrate]
    SUBSTRATE_LOCAL_CAPSULE_MACOS: _ClassVar[Substrate]
    SUBSTRATE_LOCAL_CAPSULE_WINDOWS: _ClassVar[Substrate]
    SUBSTRATE_WINDOWS_NATIVE: _ClassVar[Substrate]
    SUBSTRATE_CUSTOMER_PRIVATE_WORKER: _ClassVar[Substrate]

class TargetStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    TARGET_STATUS_UNSPECIFIED: _ClassVar[TargetStatus]
    TARGET_STATUS_REQUESTED: _ClassVar[TargetStatus]
    TARGET_STATUS_PROVISIONING: _ClassVar[TargetStatus]
    TARGET_STATUS_READY: _ClassVar[TargetStatus]
    TARGET_STATUS_BUSY: _ClassVar[TargetStatus]
    TARGET_STATUS_DRAINING: _ClassVar[TargetStatus]
    TARGET_STATUS_STOPPED: _ClassVar[TargetStatus]
    TARGET_STATUS_SNAPSHOTTED: _ClassVar[TargetStatus]
    TARGET_STATUS_DESTROYED: _ClassVar[TargetStatus]
    TARGET_STATUS_FAILED: _ClassVar[TargetStatus]
    TARGET_STATUS_REPLACING: _ClassVar[TargetStatus]
TARGET_CLASS_UNSPECIFIED: TargetClass
TARGET_CLASS_PERSISTENT_WORKSPACE_COMPUTER: TargetClass
TARGET_CLASS_ISOLATED_TASK_RUNTIME: TargetClass
SUBSTRATE_UNSPECIFIED: Substrate
SUBSTRATE_CLOUD_MICROVM: Substrate
SUBSTRATE_LOCAL_CAPSULE_MACOS: Substrate
SUBSTRATE_LOCAL_CAPSULE_WINDOWS: Substrate
SUBSTRATE_WINDOWS_NATIVE: Substrate
SUBSTRATE_CUSTOMER_PRIVATE_WORKER: Substrate
TARGET_STATUS_UNSPECIFIED: TargetStatus
TARGET_STATUS_REQUESTED: TargetStatus
TARGET_STATUS_PROVISIONING: TargetStatus
TARGET_STATUS_READY: TargetStatus
TARGET_STATUS_BUSY: TargetStatus
TARGET_STATUS_DRAINING: TargetStatus
TARGET_STATUS_STOPPED: TargetStatus
TARGET_STATUS_SNAPSHOTTED: TargetStatus
TARGET_STATUS_DESTROYED: TargetStatus
TARGET_STATUS_FAILED: TargetStatus
TARGET_STATUS_REPLACING: TargetStatus

class TargetResources(_message.Message):
    __slots__ = ("schema_version", "cpu", "mem_mb", "disk_mb")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    CPU_FIELD_NUMBER: _ClassVar[int]
    MEM_MB_FIELD_NUMBER: _ClassVar[int]
    DISK_MB_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    cpu: int
    mem_mb: int
    disk_mb: int
    def __init__(self, schema_version: _Optional[str] = ..., cpu: _Optional[int] = ..., mem_mb: _Optional[int] = ..., disk_mb: _Optional[int] = ...) -> None: ...

class ExecutionTarget(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "target_class", "substrate", "status", "desired_state", "observed_state", "image_digest", "lease_id", "generation", "network_policy_id", "resources", "endpoint_ref", "last_heartbeat_at", "checkpoint_ids", "owner_kind", "owner_ref")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    TARGET_CLASS_FIELD_NUMBER: _ClassVar[int]
    SUBSTRATE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    DESIRED_STATE_FIELD_NUMBER: _ClassVar[int]
    OBSERVED_STATE_FIELD_NUMBER: _ClassVar[int]
    IMAGE_DIGEST_FIELD_NUMBER: _ClassVar[int]
    LEASE_ID_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    NETWORK_POLICY_ID_FIELD_NUMBER: _ClassVar[int]
    RESOURCES_FIELD_NUMBER: _ClassVar[int]
    ENDPOINT_REF_FIELD_NUMBER: _ClassVar[int]
    LAST_HEARTBEAT_AT_FIELD_NUMBER: _ClassVar[int]
    CHECKPOINT_IDS_FIELD_NUMBER: _ClassVar[int]
    OWNER_KIND_FIELD_NUMBER: _ClassVar[int]
    OWNER_REF_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    target_class: TargetClass
    substrate: Substrate
    status: TargetStatus
    desired_state: str
    observed_state: str
    image_digest: str
    lease_id: str
    generation: int
    network_policy_id: str
    resources: TargetResources
    endpoint_ref: str
    last_heartbeat_at: str
    checkpoint_ids: _containers.RepeatedScalarFieldContainer[str]
    owner_kind: str
    owner_ref: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., target_class: _Optional[_Union[TargetClass, str]] = ..., substrate: _Optional[_Union[Substrate, str]] = ..., status: _Optional[_Union[TargetStatus, str]] = ..., desired_state: _Optional[str] = ..., observed_state: _Optional[str] = ..., image_digest: _Optional[str] = ..., lease_id: _Optional[str] = ..., generation: _Optional[int] = ..., network_policy_id: _Optional[str] = ..., resources: _Optional[_Union[TargetResources, _Mapping]] = ..., endpoint_ref: _Optional[str] = ..., last_heartbeat_at: _Optional[str] = ..., checkpoint_ids: _Optional[_Iterable[str]] = ..., owner_kind: _Optional[str] = ..., owner_ref: _Optional[str] = ...) -> None: ...

class Lease(_message.Message):
    __slots__ = ("schema_version", "id", "target_id", "holder_controller_id", "holder_generation", "acquired_at", "expires_at", "renewed_at", "status")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[Lease.Status]
        STATUS_HELD: _ClassVar[Lease.Status]
        STATUS_RELEASED: _ClassVar[Lease.Status]
        STATUS_EXPIRED: _ClassVar[Lease.Status]
        STATUS_REVOKED: _ClassVar[Lease.Status]
    STATUS_UNSPECIFIED: Lease.Status
    STATUS_HELD: Lease.Status
    STATUS_RELEASED: Lease.Status
    STATUS_EXPIRED: Lease.Status
    STATUS_REVOKED: Lease.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TARGET_ID_FIELD_NUMBER: _ClassVar[int]
    HOLDER_CONTROLLER_ID_FIELD_NUMBER: _ClassVar[int]
    HOLDER_GENERATION_FIELD_NUMBER: _ClassVar[int]
    ACQUIRED_AT_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    RENEWED_AT_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    target_id: str
    holder_controller_id: str
    holder_generation: int
    acquired_at: str
    expires_at: str
    renewed_at: str
    status: Lease.Status
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., target_id: _Optional[str] = ..., holder_controller_id: _Optional[str] = ..., holder_generation: _Optional[int] = ..., acquired_at: _Optional[str] = ..., expires_at: _Optional[str] = ..., renewed_at: _Optional[str] = ..., status: _Optional[_Union[Lease.Status, str]] = ...) -> None: ...

class BrowserSession(_message.Message):
    __slots__ = ("schema_version", "id", "target_id", "run_id", "profile_ref", "control_holder", "control_since", "current_url", "tabs", "screencast", "checkpoint_id", "status")
    class ControlHolder(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        CONTROL_HOLDER_UNSPECIFIED: _ClassVar[BrowserSession.ControlHolder]
        CONTROL_HOLDER_AGENT: _ClassVar[BrowserSession.ControlHolder]
        CONTROL_HOLDER_USER: _ClassVar[BrowserSession.ControlHolder]
        CONTROL_HOLDER_NONE: _ClassVar[BrowserSession.ControlHolder]
    CONTROL_HOLDER_UNSPECIFIED: BrowserSession.ControlHolder
    CONTROL_HOLDER_AGENT: BrowserSession.ControlHolder
    CONTROL_HOLDER_USER: BrowserSession.ControlHolder
    CONTROL_HOLDER_NONE: BrowserSession.ControlHolder
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[BrowserSession.Status]
        STATUS_ACTIVE: _ClassVar[BrowserSession.Status]
        STATUS_PAUSED_TAKEOVER: _ClassVar[BrowserSession.Status]
        STATUS_PAUSED_POLICY: _ClassVar[BrowserSession.Status]
        STATUS_CLOSED: _ClassVar[BrowserSession.Status]
    STATUS_UNSPECIFIED: BrowserSession.Status
    STATUS_ACTIVE: BrowserSession.Status
    STATUS_PAUSED_TAKEOVER: BrowserSession.Status
    STATUS_PAUSED_POLICY: BrowserSession.Status
    STATUS_CLOSED: BrowserSession.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TARGET_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    PROFILE_REF_FIELD_NUMBER: _ClassVar[int]
    CONTROL_HOLDER_FIELD_NUMBER: _ClassVar[int]
    CONTROL_SINCE_FIELD_NUMBER: _ClassVar[int]
    CURRENT_URL_FIELD_NUMBER: _ClassVar[int]
    TABS_FIELD_NUMBER: _ClassVar[int]
    SCREENCAST_FIELD_NUMBER: _ClassVar[int]
    CHECKPOINT_ID_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    target_id: str
    run_id: str
    profile_ref: str
    control_holder: BrowserSession.ControlHolder
    control_since: str
    current_url: str
    tabs: _containers.RepeatedCompositeFieldContainer[BrowserTab]
    screencast: Screencast
    checkpoint_id: str
    status: BrowserSession.Status
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., target_id: _Optional[str] = ..., run_id: _Optional[str] = ..., profile_ref: _Optional[str] = ..., control_holder: _Optional[_Union[BrowserSession.ControlHolder, str]] = ..., control_since: _Optional[str] = ..., current_url: _Optional[str] = ..., tabs: _Optional[_Iterable[_Union[BrowserTab, _Mapping]]] = ..., screencast: _Optional[_Union[Screencast, _Mapping]] = ..., checkpoint_id: _Optional[str] = ..., status: _Optional[_Union[BrowserSession.Status, str]] = ...) -> None: ...

class BrowserTab(_message.Message):
    __slots__ = ("schema_version", "id", "url", "title")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    URL_FIELD_NUMBER: _ClassVar[int]
    TITLE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    url: str
    title: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., url: _Optional[str] = ..., title: _Optional[str] = ...) -> None: ...

class Screencast(_message.Message):
    __slots__ = ("schema_version", "enabled", "stream_ref")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ENABLED_FIELD_NUMBER: _ClassVar[int]
    STREAM_REF_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    enabled: bool
    stream_ref: str
    def __init__(self, schema_version: _Optional[str] = ..., enabled: _Optional[bool] = ..., stream_ref: _Optional[str] = ...) -> None: ...

class TerminalSession(_message.Message):
    __slots__ = ("schema_version", "id", "target_id", "run_id", "pty_ref", "cursor", "status", "last_command_id")
    class Status(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        STATUS_UNSPECIFIED: _ClassVar[TerminalSession.Status]
        STATUS_OPEN: _ClassVar[TerminalSession.Status]
        STATUS_CLOSED: _ClassVar[TerminalSession.Status]
        STATUS_LOST: _ClassVar[TerminalSession.Status]
    STATUS_UNSPECIFIED: TerminalSession.Status
    STATUS_OPEN: TerminalSession.Status
    STATUS_CLOSED: TerminalSession.Status
    STATUS_LOST: TerminalSession.Status
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    TARGET_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    PTY_REF_FIELD_NUMBER: _ClassVar[int]
    CURSOR_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    LAST_COMMAND_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    target_id: str
    run_id: str
    pty_ref: str
    cursor: str
    status: TerminalSession.Status
    last_command_id: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., target_id: _Optional[str] = ..., run_id: _Optional[str] = ..., pty_ref: _Optional[str] = ..., cursor: _Optional[str] = ..., status: _Optional[_Union[TerminalSession.Status, str]] = ..., last_command_id: _Optional[str] = ...) -> None: ...

class WorkerEnvelope(_message.Message):
    __slots__ = ("schema_version", "lease_id", "generation", "fence_token", "correlation_id", "kind", "payload_json")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    LEASE_ID_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    FENCE_TOKEN_FIELD_NUMBER: _ClassVar[int]
    CORRELATION_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    PAYLOAD_JSON_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    lease_id: str
    generation: int
    fence_token: str
    correlation_id: str
    kind: str
    payload_json: str
    def __init__(self, schema_version: _Optional[str] = ..., lease_id: _Optional[str] = ..., generation: _Optional[int] = ..., fence_token: _Optional[str] = ..., correlation_id: _Optional[str] = ..., kind: _Optional[str] = ..., payload_json: _Optional[str] = ...) -> None: ...
