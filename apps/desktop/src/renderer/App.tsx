/**
 * Desktop renderer (APP-003): projection + command surface, no authority, no secrets.
 *
 * Everything the page knows arrives over `window.quansio`: the active-run projection,
 * durable event frames and live frames (model deltas, terminal bytes, browser frames).
 * Every mutation is a public command sent through the bridge.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";

import {
  byAgentThread,
  faceLetter,
  faceTone,
  initials as initialsOf,
  sidebarRow,
  type RosterEntry,
  type SidebarRow,
  type ThreadSummary,
} from "../conversation/presentation.js";
import { applyTheme, themeFromMe } from "../appearance.js";
import { decodeMessages, type ConversationMessage } from "../conversation/state.js";
import { t } from "../i18n/strings.js";
import { decodeRoster } from "../teammates/roster.js";
import { DESKTOP_SURFACES } from "../surfaces.js";
import { Artifacts } from "./Artifacts.js";
import { Automations } from "./Automations.js";
import { Collaboration } from "./Collaboration.js";
import { Conversation } from "./Conversation.js";
import { Faces } from "./Face.js";
import { ChevronRightIcon, CloseIcon, GearIcon, LeaveIcon, PanelIcon, PlusIcon, PulseIcon } from "./icons.js";
import { Knowledge } from "./Knowledge.js";
import { Live } from "./Live.js";
import { OPENS, PAGE_ENTRY, SURFACE_ICONS, SURFACE_LABELS, type Page } from "./navigation.js";
import { Onboarding } from "./Onboarding.js";
import { Palette, type PaletteItem } from "./Palette.js";
import { Settings } from "./Settings.js";
import { Sidebar, type SidebarView } from "./Sidebar.js";
import { Teammates } from "./Teammates.js";
import { Trust } from "./Trust.js";
import { Work } from "./Work.js";

interface RunProjection {
  readonly connected: boolean;
  readonly workspaceId: string | null;
  readonly activeRun: { readonly runId: string; readonly threadId: string; readonly streamCursor: string } | null;
  readonly streamState: "connecting" | "open" | "reconnecting" | "closed";
}

interface LogLine {
  readonly key: string;
  readonly text: string;
}

/** A thread's messages as last read, and the list entry they were read for. */
interface ReadMessages {
  readonly messageCount: number;
  readonly lastActivityAt: string | null;
  readonly messages: readonly ConversationMessage[];
}

const MAX_LOG_LINES = 200;
/** Threads whose latest message the sidebar reads (the list shows the most recent first). */
const PREVIEWED_THREADS = 40;
/** Events arrive in bursts (a turn posts, streams, completes); the list is re-read once per burst. */
const REFRESH_DEBOUNCE_MS = 350;

function describeFrame(frame: unknown): string {
  if (!frame || typeof frame !== "object") {
    return String(frame);
  }
  const record = frame as Record<string, unknown>;
  const text = (value: unknown): string => (typeof value === "string" ? value : "?");
  if (record["kind"] === "event") {
    const event = record["event"] as Record<string, unknown> | undefined;
    return `event ${text(event?.["type"] ?? event?.["kind"])} @ ${text(record["cursor"])}`;
  }
  if (record["kind"] === "live") {
    const live = record["frame"] as Record<string, unknown> | undefined;
    return `live ${text(live?.["kind"])} on ${text(record["channel"])}`;
  }
  return JSON.stringify(record).slice(0, 120);
}

/** The event type of a durable frame, when the frame is one. */
function eventType(frame: unknown): string | null {
  const record = frame && typeof frame === "object" ? (frame as Record<string, unknown>) : {};
  const event = record["event"] && typeof record["event"] === "object" ? (record["event"] as Record<string, unknown>) : null;
  const type = event?.["type"];
  return record["kind"] === "event" && typeof type === "string" ? type : null;
}

function connectionLabel(state: RunProjection["streamState"]): string {
  switch (state) {
    case "open":
      return t("connected");
    case "connecting":
      return t("connecting");
    case "reconnecting":
      return t("reconnecting");
    default:
      return t("disconnected");
  }
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

/** Decode `GET /v1/threads` with the attention projection. */
export function decodeThreadSummaries(payload: unknown): ThreadSummary[] {
  const rows = Array.isArray(record(payload)["threads"]) ? (record(payload)["threads"] as unknown[]) : [];
  return rows.flatMap((raw) => {
    const row = record(raw);
    const id = row["id"];
    if (typeof id !== "string") {
      return [];
    }
    return [{
      id,
      title: typeof row["title"] === "string" ? row["title"] : null,
      kind: typeof row["kind"] === "string" ? row["kind"] : "direct",
      messageCount: typeof row["message_count"] === "number" ? row["message_count"] : 0,
      lastActivityAt: typeof row["last_activity_at"] === "string" ? row["last_activity_at"] : null,
      needsYou: record(row["attention"])["needs_you"] === true,
    }];
  });
}

export function App(): ReactElement {
  const [ready, setReady] = useState(false);
  const onReady = useCallback(() => {
    setReady(true);
  }, []);
  // Signing out — by the button, or because the server revoked or expired the session and a
  // refresh found it gone — returns the person to sign-in rather than leaving a dead shell.
  useEffect(
    () =>
      window.quansio.onSessionState((session) => {
        if ((session as { readonly kind?: string } | null)?.kind === "signed-out") {
          setReady(false);
        }
      }),
    [],
  );
  if (!ready) {
    return (
      <>
        {/* The window has no title bar; this strip is where it is dragged from before sign-in. */}
        <div className="window-drag" aria-hidden="true" />
        <Onboarding onReady={onReady} />
      </>
    );
  }
  return <Shell />;
}

function Shell(): ReactElement {
  const [projection, setProjection] = useState<RunProjection | null>(null);
  const [log, setLog] = useState<readonly LogLine[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [page, setPage] = useState<Page>("conversation");
  const [view, setView] = useState<SidebarView>("chats");
  const [threads, setThreads] = useState<readonly ThreadSummary[]>([]);
  const [read, setRead] = useState<ReadonlyMap<string, ReadMessages>>(new Map());
  const [roster, setRoster] = useState<readonly RosterEntry[]>([]);
  const [person, setPerson] = useState<{ readonly initials: string; readonly userId: string | null }>({ initials: "", userId: null });
  // `undefined` until the durable session says which thread was open; `null` is a new one.
  const [threadId, setThreadId] = useState<string | null | undefined>(undefined);
  const [seen, setSeen] = useState<Readonly<Record<string, number>>>({});
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [activityOpen, setActivityOpen] = useState(false);
  const readRef = useRef(read);
  readRef.current = read;
  const baseline = useRef(false);
  const refreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const report = useCallback((error: unknown) => {
    setNotice(error instanceof Error ? error.message : t("commandFailed"));
  }, []);

  const refresh = useCallback(async () => {
    const next = (await window.quansio.activeRun()) as RunProjection;
    setProjection(next);
    setThreadId((current) => (current === undefined ? (next.activeRun?.threadId ?? null) : current));
  }, []);

  // The list, then the latest messages of each listed thread that changed since it was read.
  const loadThreads = useCallback(async () => {
    try {
      const listed = decodeThreadSummaries(await window.quansio.listThreads());
      setThreads(listed);
      const stale = listed.slice(0, PREVIEWED_THREADS).filter((thread) => {
        const known = readRef.current.get(thread.id);
        return !known || known.messageCount !== thread.messageCount || known.lastActivityAt !== thread.lastActivityAt;
      });
      const fresh = await Promise.all(
        stale.map(async (thread) => [thread.id, { messageCount: thread.messageCount, lastActivityAt: thread.lastActivityAt, messages: decodeMessages(await window.quansio.threadMessages(thread.id)) }] as const),
      );
      if (fresh.length > 0) {
        setRead((current) => new Map([...current, ...fresh]));
      }
      // Unread is relative to this session: what was there when it began counts as seen.
      setSeen((current) => {
        const next: Record<string, number> = { ...current };
        for (const thread of listed) {
          if (next[thread.id] === undefined) {
            next[thread.id] = baseline.current ? 0 : thread.messageCount;
          }
        }
        baseline.current = true;
        return next;
      });
    } catch (error) {
      report(error);
    }
  }, [report]);

  const loadRoster = useCallback(async () => {
    try {
      const decoded = decodeRoster(await window.quansio.listTeammates());
      setRoster(decoded.teammates.map((teammate) => ({ id: teammate.id, name: teammate.name, agentThreadId: teammate.thread?.id ?? null })));
    } catch (error) {
      report(error);
    }
  }, [report]);

  const scheduleRefresh = useCallback(() => {
    if (refreshTimer.current) {
      clearTimeout(refreshTimer.current);
    }
    refreshTimer.current = setTimeout(() => {
      refreshTimer.current = null;
      void loadThreads();
    }, REFRESH_DEBOUNCE_MS);
  }, [loadThreads]);

  useEffect(() => {
    void window.quansio.subscribeStream();
    void refresh();
    void window.quansio.me().then((me) => {
      const user = record(record(me)["user"]);
      const name = typeof user["display_name"] === "string" ? user["display_name"] : null;
      const email = typeof user["primary_email"] === "string" ? user["primary_email"] : null;
      setPerson({ initials: initialsOf(name, email), userId: typeof user["id"] === "string" ? user["id"] : null });
    }, report);
    const unsubscribeFrame = window.quansio.onStreamFrame((frame) => {
      setLog((lines) => [...lines, { key: `${String(Date.now())}-${String(lines.length)}`, text: describeFrame(frame) }].slice(-MAX_LOG_LINES));
      const type = eventType(frame);
      if (type && (type.startsWith("thread.") || type.startsWith("question.") || type.startsWith("run."))) {
        scheduleRefresh();
      }
      if (type?.startsWith("teammate.")) {
        void loadRoster();
      }
    });
    const unsubscribeState = window.quansio.onStreamState(() => {
      void refresh();
    });
    const unsubscribeDeepLink = window.quansio.onDeepLink(() => {
      setNotice(t("deepLinkOpened"));
    });
    return () => {
      unsubscribeFrame();
      unsubscribeState();
      unsubscribeDeepLink();
      if (refreshTimer.current) {
        clearTimeout(refreshTimer.current);
      }
    };
  }, [refresh, report, scheduleRefresh, loadRoster]);

  const workspaceId = projection?.workspaceId ?? null;
  useEffect(() => {
    if (workspaceId) {
      void loadThreads();
      void loadRoster();
    }
  }, [workspaceId, loadThreads, loadRoster]);

  // The person's theme preference for this workspace, on the page and the window chrome.
  useEffect(() => {
    if (!workspaceId) {
      return;
    }
    void window.quansio.me().then((me) => {
      const theme = themeFromMe(me, workspaceId);
      applyTheme(theme, document.documentElement);
      return window.quansio.setAppearance(theme);
    }, report);
  }, [workspaceId, report]);

  // The open thread is seen as it grows.
  const openCount = threads.find((thread) => thread.id === threadId)?.messageCount;
  useEffect(() => {
    if (page === "conversation" && threadId && openCount !== undefined) {
      setSeen((current) => (current[threadId] === openCount ? current : { ...current, [threadId]: openCount }));
    }
  }, [page, threadId, openCount]);

  // ⌘K / Ctrl+K opens the palette from anywhere.
  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPaletteOpen(true);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
    };
  }, []);

  const openThread = useCallback((id: string | null) => {
    setThreadId(id);
    setPage("conversation");
    setView("chats");
  }, []);

  const openPage = useCallback((next: Page) => {
    setPage(next);
  }, []);

  const signOut = useCallback(() => {
    void window.quansio.signOut();
  }, []);

  const rosterIndex = useMemo(() => byAgentThread(roster), [roster]);
  const rows: readonly SidebarRow[] = useMemo(
    () =>
      threads.map((thread) => {
        const row = sidebarRow(thread, read.get(thread.id)?.messages ?? null, rosterIndex, seen[thread.id]);
        // A conversation nobody has answered yet is marked by its own letter.
        return row.faces.length > 0 ? row : { ...row, faces: [{ key: thread.id, letter: faceLetter(row.name ?? t("untitledThread")), tone: faceTone(thread.id) }] };
      }),
    [threads, read, rosterIndex, seen],
  );

  const current = rows.find((row) => row.id === threadId) ?? null;
  const connection = projection?.streamState ?? "closed";
  const title =
    page === "conversation"
      ? (current?.name ?? t("untitledThread"))
      : page === "settings"
        ? t("settings")
        : t(SURFACE_LABELS[PAGE_ENTRY[page] ?? "conversation"]);

  const paletteItems = useMemo((): readonly PaletteItem[] => {
    const conversations = rows.map((row): PaletteItem => ({
      key: `thread-${row.id}`,
      group: t("conversations"),
      label: row.name ?? t("untitledThread"),
      detail: row.preview,
      icon: <Faces faces={row.faces} more={row.more} size="xs" />,
      run: () => { openThread(row.id); },
    }));
    const pages = DESKTOP_SURFACES.flatMap((entry): PaletteItem[] => {
      const opens = OPENS[entry.id];
      const Icon = SURFACE_ICONS[entry.id];
      return opens ? [{ key: `surface-${entry.id}`, group: t("workspace"), label: t(SURFACE_LABELS[entry.id]), icon: <Icon />, run: () => { openPage(opens); } }] : [];
    });
    const actions: PaletteItem[] = [
      { key: "action-new", group: t("actions"), label: t("newThread"), icon: <PlusIcon />, run: () => { openThread(null); } },
      { key: "action-settings", group: t("actions"), label: t("settings"), icon: <GearIcon />, run: () => { openPage("settings"); } },
      { key: "action-stream", group: t("actions"), label: t("streamTitle"), icon: <PulseIcon />, run: () => { setActivityOpen(true); } },
      { key: "action-sign-out", group: t("actions"), label: t("signOut"), icon: <LeaveIcon />, run: signOut },
    ];
    return [...conversations, ...pages, ...actions];
  }, [rows, openThread, openPage, signOut]);

  const content = workspaceId ? (
    page === "artifacts" ? (
      <Artifacts key={`artifacts-${workspaceId}`} workspaceId={workspaceId} />
    ) : page === "objectives" ? (
      <Work key={`work-${workspaceId}`} workspaceId={workspaceId} />
    ) : page === "approvals" ? (
      <Trust key={`trust-${workspaceId}`} workspaceId={workspaceId} />
    ) : page === "knowledge-controls" ? (
      <Knowledge key={`knowledge-${workspaceId}`} workspaceId={workspaceId} runId={projection?.activeRun?.runId ?? null} />
    ) : page === "teammates" ? (
      <Teammates key={`teammates-${workspaceId}`} workspaceId={workspaceId} />
    ) : page === "live-browser-terminal" ? (
      <Live key={`live-${workspaceId}`} workspaceId={workspaceId} />
    ) : page === "collaboration" ? (
      <Collaboration key={`collaboration-${workspaceId}`} workspaceId={workspaceId} />
    ) : page === "automations" ? (
      <Automations key={`automations-${workspaceId}`} workspaceId={workspaceId} />
    ) : page === "settings" ? (
      <Settings key={`settings-${workspaceId}`} workspaceId={workspaceId} />
    ) : (
      <Conversation
        key={workspaceId}
        workspaceId={workspaceId}
        threadId={threadId ?? null}
        teammateId={current?.teammateId ?? null}
        // A conversation with one teammate names them in the composer; a group does not.
        teammateName={current?.teammateId && current.faces.length === 1 && current.more === 0 ? (roster.find((entry) => entry.id === current.teammateId)?.name ?? null) : null}
        threadFace={current?.faces[0] ?? null}
        roster={roster}
        personId={person.userId}
        onThreadChange={setThreadId}
        onThreadsChanged={loadThreads}
        inspectorOpen={inspectorOpen}
        onInspectorOpen={setInspectorOpen}
      />
    )
  ) : (
    <p className="empty">{t("connecting")}</p>
  );

  const PageIcon = page !== "conversation" && page !== "settings" ? SURFACE_ICONS[PAGE_ENTRY[page] ?? "conversation"] : null;

  return (
    <main className="shell">
      <Sidebar
        view={view}
        onView={setView}
        rows={rows}
        openThreadId={threadId ?? null}
        page={page}
        onOpenThread={openThread}
        onOpenPage={openPage}
        onSearch={() => { setPaletteOpen(true); }}
        initials={person.initials}
        onSignOut={signOut}
      />

      <div className={`pane pane-${page === "conversation" ? "chat" : "page"}`}>
        <header className="pane-header">
          <div className="pane-lead">
            {connection === "open" ? (
              <span className="sr-only" role="status">{connectionLabel(connection)}</span>
            ) : (
              <span className={`status status-${connection}`} role="status">
                <span className="dot" aria-hidden="true" />
                {connectionLabel(connection)}
              </span>
            )}
          </div>
          <h2 className="sr-only">{title}</h2>
          {page === "conversation" ? (
            <button type="button" className="island" aria-expanded={inspectorOpen} aria-controls="details" onClick={() => { setInspectorOpen(!inspectorOpen); }}>
              {current ? <Faces faces={current.faces} more={current.more} size="sm" /> : null}
              <span className="island-name">{title}</span>
              <span className="island-chevron" aria-hidden="true"><ChevronRightIcon size={12} /></span>
            </button>
          ) : (
            <span className="island static" aria-hidden="true">
              {PageIcon ? <PageIcon /> : <GearIcon />}
              <span className="island-name">{title}</span>
            </span>
          )}
          <div className="pane-actions">
            {notice ? <span className="pane-notice" role="status">{notice}</span> : null}
            {page === "conversation" ? (
              <button type="button" className="icon-button ghost" aria-label={t("inspector")} data-tip={t("inspector")} aria-pressed={inspectorOpen} onClick={() => { setInspectorOpen(!inspectorOpen); }}>
                <PanelIcon />
              </button>
            ) : null}
            <button type="button" className="icon-button ghost" aria-label={t("streamTitle")} data-tip={t("streamTitle")} aria-pressed={activityOpen} onClick={() => { setActivityOpen(!activityOpen); }}>
              <PulseIcon />
            </button>
          </div>
        </header>
        <div className={`surface surface-${page}`}>{content}</div>
        {activityOpen ? (
          <section className="activity" aria-label={t("streamTitle")}>
            <header>
              <h2>{t("streamTitle")}</h2>
              <span className="count">{log.length}</span>
              <button type="button" className="icon-button ghost small" aria-label={t("close")} onClick={() => { setActivityOpen(false); }}>
                <CloseIcon size={14} />
              </button>
            </header>
            {log.length === 0 ? (
              <p>{t("streamEmpty")}</p>
            ) : (
              <ol>
                {log.map((line) => (
                  <li key={line.key}>{line.text}</li>
                ))}
              </ol>
            )}
          </section>
        ) : null}
      </div>

      {paletteOpen ? <Palette items={paletteItems} onClose={() => { setPaletteOpen(false); }} /> : null}
    </main>
  );
}
