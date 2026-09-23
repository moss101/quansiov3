# APP-009 — collaboration, group chat, reactions and handoffs

## What was built (task branch `task/APP-009-collaboration`)

The 2026-09-15 record was a placeholder (a sort and a string compare; nothing wrote
`thread_participants`). Ownership decisions are in `evidence/APP-009/RECONCILIATION.md`.

### Server

- `control/collaboration/` (the task's path) owns `thread_participants`:
  `CollaborationStore` creates a thread with its owner and members in one transaction,
  adds / rejoins / changes a role (idempotent), removes or lets someone leave with the
  last-owner guard, names the thread's responsible teammate, posts a message with the
  teammate it addresses joining atomically, and `hand_off`s a thread (source leaves,
  target joins, system notice, `thread.handed_off` — one transaction; refused while the
  source has live work in the thread). `access` decides who may read a thread
  (`thread_access`) and filters the stream per subscriber (`ThreadGate`: `thread.*`,
  `question.*` and live frames of a protected thread's runs; a person always hears their
  own addition or removal; membership events and workspace membership changes invalidate
  what it remembered). `attention` derives attention from the rows.
- `control/conversation.rs` (still the owner of `threads`/`messages`): `seq` assigned
  under the thread row lock (the old `MAX(seq)+1` without a lock made concurrent posts
  collide on `UNIQUE (thread_id, seq)`); thread events carry the thread's own
  `aggregate_version` (every thread event used to be version 1); mentions stored and
  announced (`thread.attention_changed` mentioned/none); idempotent reactions
  (`thread.reaction_added/removed`); system notices; `*_in` functions so collaboration
  composes it in one transaction. `EventBatch::staged_version` (events crate) lets an
  owner continue versions it staged in the same transaction.
- API: `api/collaboration.rs` — `CreateThread`, `AddParticipant`, `RemoveParticipant`,
  `ReactToMessage`, `HandOffThread` (new in DOMAIN.md §14, contracts regenerated), and the
  protected projections `GET /v1/threads`, `/v1/threads/{id}` (participants, attention,
  responsible teammate, my role), `/v1/messages` (mentions, reactions);
  `/v1/questions` is gated by the question's thread. `PostMessage` now requires the
  author-work role, participation in a protected thread, validated mentions, and routes an
  un-addressed message to the first mentioned teammate or the thread's responsible one.
  `/v1/stream` refuses a protected `thread:` channel to a non-participant and passes every
  frame through the subscriber's gate.
- Migration `0015_thread_participant_stamp.sql`: 0001 attached the shared `updated_at`
  trigger to `thread_participants` without the column, so any membership update failed.

### Desktop (`apps/desktop/src/collaboration/`)

`thread.ts`: decode the thread view, list and messages; `mergeMessages` (one copy per id,
`seq` order); `applyThreadEvent` (reactions, membership, handoff; the viewer's own
removal revokes the view and drops its messages); reactions summary and toggle; mentions
from `@handles` of known people only; handoff targets; command builders. Main exposes
`threads.view`, `threads.create`, `participants.add/remove`, `messages.react`,
`threads.handoff` (settled) and passes `mentions` on `messages.post`. The
`Collaboration` page (the `collaboration` surface).

## Acceptance → tests

- *Concurrent messages preserve stable order/IDs.* `collaboration.rs`
  `concurrent_posts_from_two_people_keep_one_stable_order` (10 concurrent posts from two
  people: all accepted, `seq` gap-free 1..n, ids unique, two readers read the same list,
  `thread.message_posted` events commit in `seq` order, thread versions consecutive);
  `collaboration.e2e.test.ts` (two desktop clients post at once; both streams deliver the
  posts in the same `seq` order and both read the same conversation); `collaboration.test.ts`
  (`mergeMessages` gives one order whatever the arrival order).
- *Removed participant cannot receive new protected events.* `collaboration.rs`
  `a_removed_participant_receives_no_new_protected_event` (the gate over the real event
  log: after removal the person receives their removal and no later thread event nor the
  run's live frame; reads, posts and reactions are refused; re-added, they receive again);
  the E2E (the removed editor's live stream carries `thread.participant_removed` and
  nothing more of the thread while the owner keeps posting; view, messages and post are
  refused `SCOPE_FORBIDDEN`; the thread leaves their list).
- *Permissions for every participant.* `membership_rules_protect_a_thread_for_its_participants`
  (non-participant editor refused on every read and post; viewer refused authoring; member
  adds but cannot grant ownership or remove others; no workspace membership, no join;
  last owner cannot leave; an admin who is not a participant cannot read but manages
  membership; an objective thread is the workspace's).
- *Handoff E2E.* `a_handoff_moves_the_thread_to_the_next_teammate` and the desktop E2E
  (notice, `thread.handed_off`, RUN-002 mailbox item with the source's last run, next
  un-addressed message routed to the new teammate, repeat refused, refused while the
  source has a run parked on a question).
- Reactions, mentions, attention: `reactions_mentions_and_attention`.

## Changed expectations

`conversation_surface.rs` `live_frames_reach_a_workspace_subscriber_while_the_turn_runs`
subscribed without a user; the thread its post opens is now protected, so the subscriber
now identifies as the person who posted (an anonymous subscriber receives none of a
protected thread's frames — which the collaboration test asserts).
`tests/e2e/journeys.spec.ts` (QA-008's file) imported the removed placeholder; its step now
drives `applyThreadEvent`.
