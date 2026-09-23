/**
 * Collaboration surface (APP-009): group and direct threads with their members, mentions,
 * reactions, attention and teammate handoff.
 *
 * Everything shown is the server's: the thread list and view are projections that already
 * left out what this person may not see, and every change is one §14 command the server
 * decides again. The page offers only what `collaboration/thread.ts` says is legal from
 * here, keeps messages in `seq` order however they arrive, and drops a thread the moment
 * the stream says this person was removed from it.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import {
  activeParticipants,
  applyThreadEvent,
  createThreadParams,
  decodeThreadList,
  decodeThreadMessages,
  decodeThreadView,
  handOffParams,
  handoffTargets,
  mayLeave,
  mayManage,
  mentionCandidates,
  mentionsIn,
  participantParams,
  reactionToggle,
  summarizeReactions,
  viewerFrom,
  type Mentionable,
  type ThreadListEntry,
  type ThreadState,
} from "../collaboration/thread.js";
import { decodeRoster, type TeammateView } from "../teammates/roster.js";

interface CollaborationProps {
  readonly workspaceId: string;
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: unknown;
  readonly error?: { readonly code: string; readonly message: string };
}

const QUICK_REACTIONS = ["👍", "🎉", "👀", "✅"] as const;

function refusal(settled: SettledPayload): string | null {
  return settled.ok ? null : `${t("refused")} ${settled.error?.code ?? "INTERNAL"}: ${settled.error?.message ?? ""}`;
}

export function Collaboration({ workspaceId }: CollaborationProps): ReactElement {
  const [threads, setThreads] = useState<readonly ThreadListEntry[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [thread, setThread] = useState<ThreadState | null>(null);
  const [roster, setRoster] = useState<readonly TeammateView[]>([]);
  const [me, setMe] = useState<string | null>(null);
  const [admin, setAdmin] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [title, setTitle] = useState("");
  const [people, setPeople] = useState("");
  const [picked, setPicked] = useState<readonly string[]>([]);
  const [addId, setAddId] = useState("");
  const selectedRef = useRef<string | null>(null);

  const loadList = useCallback(async () => {
    try {
      setThreads(decodeThreadList(await window.quansio.listThreads()));
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, []);

  const open = useCallback(async (threadId: string) => {
    selectedRef.current = threadId;
    setSelected(threadId);
    const settled = (await window.quansio.viewThread(threadId)) as SettledPayload;
    const refused = refusal(settled);
    if (refused) {
      setThread(null);
      setNotice(refused);
      return;
    }
    const view = decodeThreadView(settled.result);
    const messages = decodeThreadMessages(await window.quansio.threadMessages(threadId));
    setThread({ view, messages, revoked: false, stale: false });
    setNotice(null);
  }, []);

  useEffect(() => {
    void loadList();
    void (async () => {
      try {
        setRoster(decodeRoster(await window.quansio.listTeammates()).teammates);
      } catch {
        setRoster([]);
      }
      try {
        const viewer = viewerFrom(await window.quansio.me(), workspaceId);
        setMe(viewer.userId);
        setAdmin(viewer.workspaceAdmin);
      } catch {
        setMe(null);
      }
    })();
  }, [loadList, workspaceId]);

  useEffect(
    () =>
      window.quansio.onStreamFrame((frame) => {
        const record = frame && typeof frame === "object" ? (frame as Record<string, unknown>) : {};
        const event = record["event"] && typeof record["event"] === "object" ? (record["event"] as Record<string, unknown>) : null;
        if (!event || typeof event["type"] !== "string" || !event["type"].startsWith("thread.")) {
          return;
        }
        void loadList();
        setThread((current) => (current ? applyThreadEvent(current, event, me) : current));
      }),
    [loadList, me],
  );

  useEffect(() => {
    if (thread?.revoked) {
      setNotice(t("collabRemoved"));
      setThread(null);
      setSelected(null);
      selectedRef.current = null;
    } else if (thread?.stale && selectedRef.current) {
      void open(selectedRef.current);
    }
  }, [thread, open]);

  const mentionable: readonly Mentionable[] = useMemo(() => {
    if (!thread) {
      return [];
    }
    return activeParticipants(thread.view).map((participant) => ({
      kind: participant.kind === "teammate" ? "teammate" : "user",
      id: participant.id,
      handle: participant.kind === "teammate" ? (roster.find((teammate) => teammate.id === participant.id)?.name ?? participant.id) : participant.id,
    }));
  }, [thread, roster]);

  const run = useCallback(
    async (work: () => Promise<unknown>) => {
      const settled = (await work()) as SettledPayload;
      const refused = refusal(settled);
      setNotice(refused);
      await loadList();
      if (!refused && selectedRef.current) {
        await open(selectedRef.current);
      }
      return settled;
    },
    [loadList, open],
  );

  const send = useCallback(async () => {
    if (!thread || !draft.trim()) {
      return;
    }
    try {
      await window.quansio.postMessage({ threadId: thread.view.id, content: draft, mentions: mentionsIn(draft, mentionable) });
      setDraft("");
      await open(thread.view.id);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, [thread, draft, mentionable, open]);

  const create = useCallback(async () => {
    try {
      const members = [
        ...people.split(",").map((id) => id.trim()).filter((id) => id.length > 0).map((id) => ({ kind: "user" as const, id })),
        ...picked.map((id) => ({ kind: "teammate" as const, id })),
      ];
      const settled = await run(() => window.quansio.createThread(createThreadParams(workspaceId, "group", title, members)));
      if (settled.ok) {
        setTitle("");
        setPeople("");
        setPicked([]);
        const created = settled.result as { thread_id?: string } | undefined;
        if (created?.thread_id) {
          await open(created.thread_id);
        }
      }
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, [people, picked, title, workspaceId, run, open]);

  const candidates = useMemo(() => {
    const last = /@([\p{L}\p{N}_.-]*)$/u.exec(draft);
    return last ? mentionCandidates(last[1] ?? "", mentionable) : [];
  }, [draft, mentionable]);

  return (
    <section className="collaboration" aria-label={t("collabSurface")}>
      <aside className="collab-threads">
        <h2>{t("collabThreads")}</h2>
        <ul>
          {threads.map((entry) => (
            <li key={entry.id} className={entry.id === selected ? "selected" : undefined}>
              <button type="button" onClick={() => { void open(entry.id); }}>
                {entry.title ?? entry.id}
              </button>
              <span className="stage">
                {entry.kind}
                {entry.protected ? ` · ${t("collabProtected")}` : ""}
                {entry.needsYou ? ` · ${t("collabNeedsYou")} (${String(entry.mentionsPending + entry.openQuestions)})` : ""}
              </span>
            </li>
          ))}
        </ul>
        <form className="collab-new" onSubmit={(event) => { event.preventDefault(); void create(); }}>
          <h3>{t("collabNewGroup")}</h3>
          <input value={title} placeholder={t("collabTitle")} onChange={(event) => { setTitle(event.target.value); }} />
          <input value={people} placeholder={t("collabPeople")} onChange={(event) => { setPeople(event.target.value); }} />
          {roster.filter((teammate) => teammate.status === "active").map((teammate) => (
            <label key={teammate.id}>
              <input
                type="checkbox"
                checked={picked.includes(teammate.id)}
                onChange={(event) => { setPicked((current) => (event.target.checked ? [...current, teammate.id] : current.filter((id) => id !== teammate.id))); }}
              />
              {teammate.name}
            </label>
          ))}
          <button type="submit">{t("collabCreate")}</button>
        </form>
      </aside>
      {thread ? (
        <div className="collab-thread">
          <header>
            <h2>{thread.view.title ?? thread.view.id}</h2>
            <span className="stage">
              {thread.view.kind}
              {thread.view.protected ? ` · ${t("collabProtected")}` : ""}
            </span>
          </header>
          {notice ? <p className="notice">{notice}</p> : null}
          <ol className="messages">
            {thread.messages.map((message) => (
              <li key={message.id} className={`message message-${message.authorKind}`} data-seq={message.seq}>
                <span className="author">{message.authorKind === "system" ? t("collabNotice") : message.authorId}</span>
                <p>{message.text}</p>
                <div className="reactions">
                  {summarizeReactions(message.reactions, me).map((summary) => (
                    <button key={summary.emoji} type="button" className={summary.mine ? "mine" : undefined} disabled={!me} onClick={() => { if (me) { void run(() => window.quansio.react(reactionToggle(message, me, summary.emoji))); } }}>
                      {summary.emoji} {summary.count}
                    </button>
                  ))}
                  {me
                    ? QUICK_REACTIONS.filter((emoji) => !message.reactions.some((reaction) => reaction.emoji === emoji)).map((emoji) => (
                        <button key={emoji} type="button" className="add-reaction" onClick={() => { void run(() => window.quansio.react(reactionToggle(message, me, emoji))); }}>
                          {emoji}
                        </button>
                      ))
                    : null}
                </div>
              </li>
            ))}
          </ol>
          <div className="composer">
            <textarea value={draft} placeholder={t("collabComposer")} onChange={(event) => { setDraft(event.target.value); }} />
            {candidates.length > 0 ? (
              <ul className="cards">
                {candidates.map((candidate) => (
                  <li key={candidate.id} className="card">
                    <button type="button" onClick={() => { setDraft((text) => text.replace(/@[\p{L}\p{N}_.-]*$/u, `@${candidate.handle} `)); }}>@{candidate.handle}</button>
                  </li>
                ))}
              </ul>
            ) : null}
            <button type="button" onClick={() => { void send(); }}>{t("send")}</button>
          </div>
        </div>
      ) : (
        <p className="collab-empty">{notice ?? t("collabPick")}</p>
      )}
      {thread ? (
        <aside className="collab-members">
          <h3>{t("collabMembers")}</h3>
          <ul>
            {thread.view.participants.map((participant) => {
              const attention = thread.view.attention.find((entry) => entry.kind === participant.kind && entry.id === participant.id);
              const name = participant.kind === "teammate" ? (roster.find((teammate) => teammate.id === participant.id)?.name ?? participant.id) : participant.id;
              return (
                <li key={`${participant.kind}:${participant.id}`} className={participant.active ? undefined : "left"}>
                  {name} <span className="stage">{participant.kind} · {participant.role}{participant.active ? "" : ` · ${t("collabLeft")}`}{participant.id === thread.view.responsibleTeammateId ? ` · ${t("collabResponsible")}` : ""}</span>
                  {attention?.needsHuman ? <span className="card">{t("collabNeedsYou")}</span> : null}
                  {participant.active && participant.id !== me && mayManage(thread.view, admin) ? (
                    <button type="button" onClick={() => { void run(() => window.quansio.removeParticipant(participantParams(thread.view.id, participant.kind === "teammate" ? "teammate" : "user", participant.id))); }}>{t("collabRemove")}</button>
                  ) : null}
                </li>
              );
            })}
          </ul>
          <form onSubmit={(event) => { event.preventDefault(); const id = addId.trim(); void run(() => window.quansio.addParticipant(participantParams(thread.view.id, id.startsWith("agt_") ? "teammate" : "user", id))).then(() => { setAddId(""); }); }}>
            <input value={addId} placeholder={t("collabAddPlaceholder")} onChange={(event) => { setAddId(event.target.value); }} />
            <button type="submit">{t("collabAdd")}</button>
          </form>
          {me && mayLeave(thread.view, me) ? (
            <button type="button" onClick={() => { void run(() => window.quansio.removeParticipant(participantParams(thread.view.id, "user", me))); }}>{t("collabLeave")}</button>
          ) : null}
          {handoffTargets(thread.view, roster).length > 0 ? (
            <div className="collab-handoff">
              <h3>{t("collabHandoff")}</h3>
              {handoffTargets(thread.view, roster).map((teammate) => (
                <button key={teammate.id} type="button" onClick={() => { void run(() => window.quansio.handOffThread(handOffParams(thread.view.id, teammate.id))); }}>
                  {t("collabHandTo")} {teammate.name}
                </button>
              ))}
            </div>
          ) : null}
        </aside>
      ) : null}
    </section>
  );
}
