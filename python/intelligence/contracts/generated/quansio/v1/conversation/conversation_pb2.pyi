from quansio.v1.trust import trust_pb2 as _trust_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class QuestionStatus(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    QUESTION_STATUS_UNSPECIFIED: _ClassVar[QuestionStatus]
    QUESTION_STATUS_OPEN: _ClassVar[QuestionStatus]
    QUESTION_STATUS_ANSWERED: _ClassVar[QuestionStatus]
    QUESTION_STATUS_EXPIRED: _ClassVar[QuestionStatus]
    QUESTION_STATUS_CANCELLED: _ClassVar[QuestionStatus]
QUESTION_STATUS_UNSPECIFIED: QuestionStatus
QUESTION_STATUS_OPEN: QuestionStatus
QUESTION_STATUS_ANSWERED: QuestionStatus
QUESTION_STATUS_EXPIRED: QuestionStatus
QUESTION_STATUS_CANCELLED: QuestionStatus

class Participant(_message.Message):
    __slots__ = ("schema_version", "kind", "id", "role", "joined_at", "left_at")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Participant.Kind]
        KIND_USER: _ClassVar[Participant.Kind]
        KIND_TEAMMATE: _ClassVar[Participant.Kind]
        KIND_WORKER: _ClassVar[Participant.Kind]
    KIND_UNSPECIFIED: Participant.Kind
    KIND_USER: Participant.Kind
    KIND_TEAMMATE: Participant.Kind
    KIND_WORKER: Participant.Kind
    class Role(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        ROLE_UNSPECIFIED: _ClassVar[Participant.Role]
        ROLE_OWNER: _ClassVar[Participant.Role]
        ROLE_MEMBER: _ClassVar[Participant.Role]
    ROLE_UNSPECIFIED: Participant.Role
    ROLE_OWNER: Participant.Role
    ROLE_MEMBER: Participant.Role
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    ROLE_FIELD_NUMBER: _ClassVar[int]
    JOINED_AT_FIELD_NUMBER: _ClassVar[int]
    LEFT_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: Participant.Kind
    id: str
    role: Participant.Role
    joined_at: str
    left_at: str
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[Participant.Kind, str]] = ..., id: _Optional[str] = ..., role: _Optional[_Union[Participant.Role, str]] = ..., joined_at: _Optional[str] = ..., left_at: _Optional[str] = ...) -> None: ...

class Thread(_message.Message):
    __slots__ = ("schema_version", "id", "workspace_id", "kind", "title", "participants", "objective_id", "archived_at", "last_activity_at")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Thread.Kind]
        KIND_DIRECT: _ClassVar[Thread.Kind]
        KIND_GROUP: _ClassVar[Thread.Kind]
        KIND_OBJECTIVE: _ClassVar[Thread.Kind]
    KIND_UNSPECIFIED: Thread.Kind
    KIND_DIRECT: Thread.Kind
    KIND_GROUP: Thread.Kind
    KIND_OBJECTIVE: Thread.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    TITLE_FIELD_NUMBER: _ClassVar[int]
    PARTICIPANTS_FIELD_NUMBER: _ClassVar[int]
    OBJECTIVE_ID_FIELD_NUMBER: _ClassVar[int]
    ARCHIVED_AT_FIELD_NUMBER: _ClassVar[int]
    LAST_ACTIVITY_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    workspace_id: str
    kind: Thread.Kind
    title: str
    participants: _containers.RepeatedCompositeFieldContainer[Participant]
    objective_id: str
    archived_at: str
    last_activity_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., workspace_id: _Optional[str] = ..., kind: _Optional[_Union[Thread.Kind, str]] = ..., title: _Optional[str] = ..., participants: _Optional[_Iterable[_Union[Participant, _Mapping]]] = ..., objective_id: _Optional[str] = ..., archived_at: _Optional[str] = ..., last_activity_at: _Optional[str] = ...) -> None: ...

class ContentBlock(_message.Message):
    __slots__ = ("schema_version", "kind", "text", "ref", "language")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[ContentBlock.Kind]
        KIND_TEXT: _ClassVar[ContentBlock.Kind]
        KIND_CODE: _ClassVar[ContentBlock.Kind]
        KIND_ARTIFACT_REF: _ClassVar[ContentBlock.Kind]
        KIND_EVIDENCE_REF: _ClassVar[ContentBlock.Kind]
        KIND_QUESTION: _ClassVar[ContentBlock.Kind]
        KIND_TOOL_CARD_REF: _ClassVar[ContentBlock.Kind]
        KIND_SYSTEM_NOTICE: _ClassVar[ContentBlock.Kind]
    KIND_UNSPECIFIED: ContentBlock.Kind
    KIND_TEXT: ContentBlock.Kind
    KIND_CODE: ContentBlock.Kind
    KIND_ARTIFACT_REF: ContentBlock.Kind
    KIND_EVIDENCE_REF: ContentBlock.Kind
    KIND_QUESTION: ContentBlock.Kind
    KIND_TOOL_CARD_REF: ContentBlock.Kind
    KIND_SYSTEM_NOTICE: ContentBlock.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    REF_FIELD_NUMBER: _ClassVar[int]
    LANGUAGE_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    kind: ContentBlock.Kind
    text: str
    ref: str
    language: str
    def __init__(self, schema_version: _Optional[str] = ..., kind: _Optional[_Union[ContentBlock.Kind, str]] = ..., text: _Optional[str] = ..., ref: _Optional[str] = ..., language: _Optional[str] = ...) -> None: ...

class Reaction(_message.Message):
    __slots__ = ("schema_version", "emoji", "user_id")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    EMOJI_FIELD_NUMBER: _ClassVar[int]
    USER_ID_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    emoji: str
    user_id: str
    def __init__(self, schema_version: _Optional[str] = ..., emoji: _Optional[str] = ..., user_id: _Optional[str] = ...) -> None: ...

class Message(_message.Message):
    __slots__ = ("schema_version", "id", "thread_id", "seq", "author_kind", "author_id", "run_id", "turn_id", "content_blocks", "attachments", "reply_to", "reactions", "mentions", "trust_level", "created_at", "edited_at", "deleted_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    THREAD_ID_FIELD_NUMBER: _ClassVar[int]
    SEQ_FIELD_NUMBER: _ClassVar[int]
    AUTHOR_KIND_FIELD_NUMBER: _ClassVar[int]
    AUTHOR_ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    TURN_ID_FIELD_NUMBER: _ClassVar[int]
    CONTENT_BLOCKS_FIELD_NUMBER: _ClassVar[int]
    ATTACHMENTS_FIELD_NUMBER: _ClassVar[int]
    REPLY_TO_FIELD_NUMBER: _ClassVar[int]
    REACTIONS_FIELD_NUMBER: _ClassVar[int]
    MENTIONS_FIELD_NUMBER: _ClassVar[int]
    TRUST_LEVEL_FIELD_NUMBER: _ClassVar[int]
    CREATED_AT_FIELD_NUMBER: _ClassVar[int]
    EDITED_AT_FIELD_NUMBER: _ClassVar[int]
    DELETED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    thread_id: str
    seq: int
    author_kind: str
    author_id: str
    run_id: str
    turn_id: str
    content_blocks: _containers.RepeatedCompositeFieldContainer[ContentBlock]
    attachments: _containers.RepeatedScalarFieldContainer[str]
    reply_to: str
    reactions: _containers.RepeatedCompositeFieldContainer[Reaction]
    mentions: _containers.RepeatedScalarFieldContainer[str]
    trust_level: _trust_pb2.TrustLevel
    created_at: str
    edited_at: str
    deleted_at: str
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., thread_id: _Optional[str] = ..., seq: _Optional[int] = ..., author_kind: _Optional[str] = ..., author_id: _Optional[str] = ..., run_id: _Optional[str] = ..., turn_id: _Optional[str] = ..., content_blocks: _Optional[_Iterable[_Union[ContentBlock, _Mapping]]] = ..., attachments: _Optional[_Iterable[str]] = ..., reply_to: _Optional[str] = ..., reactions: _Optional[_Iterable[_Union[Reaction, _Mapping]]] = ..., mentions: _Optional[_Iterable[str]] = ..., trust_level: _Optional[_Union[_trust_pb2.TrustLevel, str]] = ..., created_at: _Optional[str] = ..., edited_at: _Optional[str] = ..., deleted_at: _Optional[str] = ...) -> None: ...

class Question(_message.Message):
    __slots__ = ("schema_version", "id", "run_id", "step_id", "thread_id", "kind", "prompt", "options", "required", "expires_at", "status", "answer")
    class Kind(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
        __slots__ = ()
        KIND_UNSPECIFIED: _ClassVar[Question.Kind]
        KIND_FREE_TEXT: _ClassVar[Question.Kind]
        KIND_SINGLE_CHOICE: _ClassVar[Question.Kind]
        KIND_MULTI_CHOICE: _ClassVar[Question.Kind]
        KIND_CONFIRM: _ClassVar[Question.Kind]
    KIND_UNSPECIFIED: Question.Kind
    KIND_FREE_TEXT: Question.Kind
    KIND_SINGLE_CHOICE: Question.Kind
    KIND_MULTI_CHOICE: Question.Kind
    KIND_CONFIRM: Question.Kind
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    RUN_ID_FIELD_NUMBER: _ClassVar[int]
    STEP_ID_FIELD_NUMBER: _ClassVar[int]
    THREAD_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    PROMPT_FIELD_NUMBER: _ClassVar[int]
    OPTIONS_FIELD_NUMBER: _ClassVar[int]
    REQUIRED_FIELD_NUMBER: _ClassVar[int]
    EXPIRES_AT_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    ANSWER_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    id: str
    run_id: str
    step_id: str
    thread_id: str
    kind: Question.Kind
    prompt: str
    options: _containers.RepeatedScalarFieldContainer[str]
    required: bool
    expires_at: str
    status: QuestionStatus
    answer: Answer
    def __init__(self, schema_version: _Optional[str] = ..., id: _Optional[str] = ..., run_id: _Optional[str] = ..., step_id: _Optional[str] = ..., thread_id: _Optional[str] = ..., kind: _Optional[_Union[Question.Kind, str]] = ..., prompt: _Optional[str] = ..., options: _Optional[_Iterable[str]] = ..., required: _Optional[bool] = ..., expires_at: _Optional[str] = ..., status: _Optional[_Union[QuestionStatus, str]] = ..., answer: _Optional[_Union[Answer, _Mapping]] = ...) -> None: ...

class Answer(_message.Message):
    __slots__ = ("schema_version", "user_id", "value", "answered_at")
    SCHEMA_VERSION_FIELD_NUMBER: _ClassVar[int]
    USER_ID_FIELD_NUMBER: _ClassVar[int]
    VALUE_FIELD_NUMBER: _ClassVar[int]
    ANSWERED_AT_FIELD_NUMBER: _ClassVar[int]
    schema_version: str
    user_id: str
    value: str
    answered_at: str
    def __init__(self, schema_version: _Optional[str] = ..., user_id: _Optional[str] = ..., value: _Optional[str] = ..., answered_at: _Optional[str] = ...) -> None: ...
