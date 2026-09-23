# APP-009 reconciliation (2026-09-23, agent:claude-opus-5)

The 2026-09-15 record was a placeholder: `crates/server/src/control/collaboration` was a
sort (`order_messages`) and a string compare (`can_receive(status == "active")`), the
desktop module 12 lines, and nothing wrote `thread_participants`
(docs/review/2026-09-18-m5-placeholder-audit.md). Its branch is parked as
`stale/APP-009-collaboration-2026-09-15`.

- **Canonical owners.** `crates/server/src/control/collaboration/` (the task's path) owns
  thread membership (`thread_participants`, DOMAIN.md §3.1), who may read a thread and
  receive its events, the derived attention state and the thread handoff.
  `crates/server/src/control/conversation.rs` (APP-001/APP-004) stays the owner of
  `threads` and `messages`: the message-row changes this task needs (a stable `seq` under
  concurrency, `mentions`, `reactions`, a system notice) are conversation methods that
  collaboration calls, never SQL of its own. `apps/desktop/src/collaboration/` is the
  client half.
- **Existing code, coverage PARTIAL.** Kept: the conversation owner, PostMessage /
  CreateThread, teammate routing (APP-014), RUN-002's AgentStore mailbox, the stream
  (CORE-009/APP-001), RBAC (APP-012). Replaced: the placeholder sort and predicate.
- **Gaps found.** (1) `insert_message` computes `MAX(seq)+1` without a lock, so two
  concurrent posts to one thread collide on `UNIQUE (thread_id, seq)` and one fails;
  thread events all carry `aggregate_version` 1. (2) Nothing writes participants; every
  thread's messages and events reach every tenant user who asks. (3) `PostMessage` has no
  role check. (4) `ReactToMessage`, `AddParticipant`, `RemoveParticipant` are catalogued
  with no handler. (5) No handoff between teammates in a thread; no attention state.
- **Contracts (DOMAIN.md edits, additive, same commit as the code).**
  - §3.1: protection and permission rules — a direct or group thread with participants is
    *protected*: only its active participants who hold an active workspace membership read
    its messages and questions, receive its `thread.*`/`question.*` events and live frames,
    subscribe to its `thread:` channel and post to it; an objective thread stays readable by
    every workspace reader. Adding requires the author-work role and active participation;
    removing another requires thread `owner` or workspace `admin`; anyone may leave; the
    last owner cannot leave while other people remain.
  - §3.1: attention state is derived, not stored: per user participant, the messages that
    mention them after their last message plus the thread's open questions; per teammate,
    its AgentThread status and its run in the thread.
  - §3.1/§14: `HandOffThread {thread_id, to_teammate_id}` (Conversation family): the
    thread's responsible teammate leaves, the target joins, a system notice is posted, and
    RUN-002 delivers a handoff item to the target's mailbox. It is refused while the
    source's run in the thread is not terminal. RUN-002's own `begin_handoff` is not used:
    it moves the source AgentThread to `HANDED_OFF`, which may only be terminated — a
    persistent teammate would be consumed by one conversation.
  - §9.2: `thread.*` gains `reaction_removed` and `handed_off`.
  - `PostMessage` gains `mentions[] ({kind: user|teammate, id})`, validated against the
    thread's participants and the workspace.
- **Persistent state.** `thread_participants` (existing table, first writer), `messages`
  (`mentions`, `reactions`, `seq` via the conversation owner), `agent_mailbox_items` (via
  RUN-002). No migration.
- **Events.** `thread.participant_added/removed`, `thread.reaction_added/removed`,
  `thread.attention_changed`, `thread.handed_off`, `thread.message_posted` (now with a
  per-thread `aggregate_version`), `agent.mailbox_delivered` (RUN-002).
- **External effects / approvals.** None; tier-0 workspace-internal state.
- **Real boundaries.** None required (`real_boundary: false`); the tests run against real
  PostgreSQL and the real server binary.
- **Rollback.** Code-only; `thread_participants` rows are additive. A thread with no
  participant rows (every thread created before this task) behaves as before.
