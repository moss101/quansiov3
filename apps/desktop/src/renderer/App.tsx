/**
 * Desktop renderer (APP-003): projection + command surface, no authority, no secrets.
 *
 * Everything the page knows arrives over `window.quansio`: the active-run projection,
 * durable event frames and live frames (model deltas, terminal bytes, browser frames).
 * Every mutation is a public command sent through the bridge.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";

import { DESKTOP_SURFACES, type SurfaceId } from "../surfaces.js";
import { t, type StringKey } from "../i18n/strings.js";
import { Artifacts } from "./Artifacts.js";
import { Conversation, decodeThreads, type ThreadRow } from "./Conversation.js";
import { PersonIcon, PlusIcon, PulseIcon, PanelIcon, SearchIcon, CloseIcon } from "./icons.js";
import { Knowledge } from "./Knowledge.js";
import { Teammates } from "./Teammates.js";
import { Live } from "./Live.js";
import { Collaboration } from "./Collaboration.js";
import { Automations } from "./Automations.js";
import { Trust } from "./Trust.js";
import { Work } from "./Work.js";
import { Onboarding } from "./Onboarding.js";
import { Settings } from "./Settings.js";

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

const MAX_LOG_LINES = 200;

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

type Page = "conversation" | "artifacts" | "objectives" | "approvals" | "knowledge-controls" | "teammates" | "live-browser-terminal" | "collaboration" | "automations" | "settings";

/** Every DOSSIER §15 surface by a person's name for it (not its internal id). */
const SURFACE_LABELS: Record<SurfaceId, StringKey> = {
  "conversation": "surfaceConversation",
  "objectives": "surfaceObjectives",
  "teammates": "surfaceTeammates",
  "plan-and-agents": "surfacePlanAndAgents",
  "run-status": "surfaceRunStatus",
  "approvals": "surfaceApprovals",
  "evidence-timeline": "surfaceEvidenceTimeline",
  "live-browser-terminal": "surfaceLiveBrowserTerminal",
  "artifacts": "surfaceArtifacts",
  "knowledge-controls": "surfaceKnowledgeControls",
  "collaboration": "surfaceCollaboration",
  "automations": "surfaceAutomations",
  "connectors": "surfaceConnectors",
  "capability-admin": "surfaceCapabilityAdmin",
  "model-usage": "surfaceModelUsage",
};

/**
 * The page each surface opens. Several surfaces live inside another page (the agent roster and
 * plan beside the objectives, run status with the chat, the effect timeline under approvals,
 * capability resolution with knowledge, the models and tools of a turn in the chat inspector),
 * so they open that page; connector setup is administered in the web app and has no page here.
 */
const OPENS: Partial<Record<SurfaceId, Page>> = {
  "conversation": "conversation",
  "objectives": "objectives",
  "teammates": "teammates",
  "plan-and-agents": "objectives",
  "run-status": "conversation",
  "approvals": "approvals",
  "evidence-timeline": "approvals",
  "live-browser-terminal": "live-browser-terminal",
  "artifacts": "artifacts",
  "knowledge-controls": "knowledge-controls",
  "collaboration": "collaboration",
  "automations": "automations",
  "capability-admin": "knowledge-controls",
  "model-usage": "conversation",
};

/** The navigation entry that is a page's own (the others open it as their host). */
const PAGE_ENTRY: Partial<Record<Page, SurfaceId>> = {
  "conversation": "conversation",
  "objectives": "objectives",
  "teammates": "teammates",
  "approvals": "approvals",
  "live-browser-terminal": "live-browser-terminal",
  "artifacts": "artifacts",
  "knowledge-controls": "knowledge-controls",
  "collaboration": "collaboration",
  "automations": "automations",
};

/** What the sidebar lists: the conversations, or every surface of the workspace. */
type SidebarView = "chats" | "workspace";

type Menu = "account" | null;

const AVATAR_TONES = 6;

/** A stable tone for a conversation's avatar, so each one keeps its colour between sessions. */
function avatarTone(id: string): number {
  let hash = 0;
  for (const char of id) {
    hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  }
  return hash % AVATAR_TONES;
}

function threadTitle(thread: ThreadRow): string {
  return thread.title?.trim() ? thread.title : t("untitledThread");
}

function messageCount(count: number): string {
  return `${String(count)} ${t(count === 1 ? "messageOne" : "messageMany")}`;
}

function Shell(): ReactElement {
  const [projection, setProjection] = useState<RunProjection | null>(null);
  const [log, setLog] = useState<readonly LogLine[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [surface, setSurface] = useState<Page>("conversation");
  const [view, setView] = useState<SidebarView>("chats");
  const [threads, setThreads] = useState<readonly ThreadRow[]>([]);
  // `undefined` until the durable session says which thread was open; `null` is a new one.
  const [threadId, setThreadId] = useState<string | null | undefined>(undefined);
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [menu, setMenu] = useState<Menu>(null);
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [activityOpen, setActivityOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);

  const refresh = useCallback(async () => {
    const next = (await window.quansio.activeRun()) as RunProjection;
    setProjection(next);
    setThreadId((current) => (current === undefined ? (next.activeRun?.threadId ?? null) : current));
  }, []);

  const loadThreads = useCallback(async () => {
    try {
      setThreads(decodeThreads(await window.quansio.listThreads()));
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, []);

  useEffect(() => {
    void window.quansio.subscribeStream();
    void refresh();
    const unsubscribeFrame = window.quansio.onStreamFrame((frame) => {
      setLog((lines) => [...lines, { key: `${String(Date.now())}-${String(lines.length)}`, text: describeFrame(frame) }].slice(-MAX_LOG_LINES));
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
    };
  }, [refresh]);

  const workspaceId = projection?.workspaceId ?? null;
  useEffect(() => {
    if (workspaceId) {
      void loadThreads();
    }
  }, [workspaceId, loadThreads]);

  // The account menu closes on Escape and on any press outside it.
  useEffect(() => {
    if (!menu) {
      return;
    }
    const onKey = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        setMenu(null);
      }
    };
    const onPress = (event: MouseEvent): void => {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        setMenu(null);
      }
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onPress);
    return () => {
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("mousedown", onPress);
    };
  }, [menu]);

  const openThread = useCallback((id: string | null) => {
    setThreadId(id);
    setSurface("conversation");
    setView("chats");
  }, []);

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return needle ? threads.filter((thread) => threadTitle(thread).toLowerCase().includes(needle)) : threads;
  }, [threads, query]);

  const connection = projection?.streamState ?? "closed";
  const current = threads.find((thread) => thread.id === threadId);
  const title =
    surface === "conversation"
      ? current ? threadTitle(current) : t("untitledThread")
      : surface === "settings"
        ? t("settings")
        : t(SURFACE_LABELS[PAGE_ENTRY[surface] ?? "conversation"]);

  const page = workspaceId ? (
    surface === "artifacts" ? (
      <Artifacts key={`artifacts-${workspaceId}`} workspaceId={workspaceId} />
    ) : surface === "objectives" ? (
      <Work key={`work-${workspaceId}`} workspaceId={workspaceId} />
    ) : surface === "approvals" ? (
      <Trust key={`trust-${workspaceId}`} workspaceId={workspaceId} />
    ) : surface === "knowledge-controls" ? (
      <Knowledge key={`knowledge-${workspaceId}`} workspaceId={workspaceId} runId={projection?.activeRun?.runId ?? null} />
    ) : surface === "teammates" ? (
      <Teammates key={`teammates-${workspaceId}`} workspaceId={workspaceId} />
    ) : surface === "live-browser-terminal" ? (
      <Live key={`live-${workspaceId}`} workspaceId={workspaceId} />
    ) : surface === "collaboration" ? (
      <Collaboration key={`collaboration-${workspaceId}`} workspaceId={workspaceId} />
    ) : surface === "automations" ? (
      <Automations key={`automations-${workspaceId}`} workspaceId={workspaceId} />
    ) : surface === "settings" ? (
      <Settings key={`settings-${workspaceId}`} workspaceId={workspaceId} />
    ) : (
      <Conversation
        key={workspaceId}
        workspaceId={workspaceId}
        threadId={threadId ?? null}
        onThreadChange={setThreadId}
        onThreadsChanged={loadThreads}
        inspectorOpen={inspectorOpen}
        onInspectorOpen={setInspectorOpen}
      />
    )
  ) : (
    <p className="empty">{t("connecting")}</p>
  );

  return (
    <main className="shell">
      <aside className="sidebar">
        <h1 className="sr-only">{t("appName")}</h1>
        <div className="sidebar-top">
          <button
            type="button"
            className="icon-button"
            aria-label={t("searchConversations")}
            aria-pressed={searching}
            onClick={() => {
              setSearching(!searching);
              setQuery("");
              setView("chats");
            }}
          >
            <SearchIcon />
          </button>
          <button type="button" className="icon-button" aria-label={t("newThread")} onClick={() => { openThread(null); }}>
            <PlusIcon />
          </button>
        </div>

        {view === "chats" ? (
          <nav className="chat-list" aria-label={t("conversations")}>
            {searching ? (
              <input
                type="search"
                className="chat-search"
                aria-label={t("searchConversations")}
                placeholder={t("searchConversations")}
                value={query}
                autoFocus
                onChange={(event) => { setQuery(event.target.value); }}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    setSearching(false);
                    setQuery("");
                  }
                }}
              />
            ) : null}
            {shown.length === 0 ? (
              <p className="chat-empty">{t("noConversations")}</p>
            ) : (
              <ul>
                {shown.map((thread) => {
                  const selected = surface === "conversation" && thread.id === threadId;
                  const name = threadTitle(thread);
                  return (
                    <li key={thread.id}>
                      <button
                        type="button"
                        className={selected ? "chat-row selected" : "chat-row"}
                        aria-current={selected ? "page" : undefined}
                        onClick={() => { openThread(thread.id); }}
                      >
                        <span className={`avatar tone-${String(avatarTone(thread.id))}`} aria-hidden="true">
                          {name.slice(0, 1).toUpperCase()}
                        </span>
                        <span className="chat-text">
                          <span className="chat-name">{name}</span>
                          <span className="chat-preview">{messageCount(thread.messageCount)}</span>
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </nav>
        ) : (
          <nav className="surface-list" aria-label={t("surfaces")}>
            <ul>
              {DESKTOP_SURFACES.map((entry) => {
                const opens = OPENS[entry.id];
                // Several entries open the same page; only the page's own entry is marked current.
                const isCurrent = opens === surface && entry.id === PAGE_ENTRY[surface];
                return (
                  <li key={entry.id} className={isCurrent ? "selected" : undefined}>
                    {opens ? (
                      <button type="button" aria-current={isCurrent ? "page" : undefined} onClick={() => { setSurface(opens); }}>{t(SURFACE_LABELS[entry.id])}</button>
                    ) : (
                      <span className="nav-note">{t(SURFACE_LABELS[entry.id])}</span>
                    )}
                  </li>
                );
              })}
            </ul>
          </nav>
        )}

        <div className="sidebar-foot">
          <div className="menu-anchor" ref={menuRef}>
            <button
              type="button"
              className="icon-button account-button"
              aria-label={t("account")}
              aria-haspopup="menu"
              aria-expanded={menu === "account"}
              onClick={() => { setMenu(menu === "account" ? null : "account"); }}
            >
              <PersonIcon />
            </button>
            {menu === "account" ? (
              <div className="menu" role="menu" aria-label={t("account")}>
                <button
                  type="button"
                  role="menuitem"
                  autoFocus
                  onClick={() => {
                    setMenu(null);
                    setSurface("settings");
                  }}
                >
                  {t("settings")}
                </button>
                <button type="button" role="menuitem" onClick={() => { void window.quansio.signOut(); }}>
                  {t("signOut")}
                </button>
              </div>
            ) : null}
          </div>
          <div className="segmented" role="group" aria-label={t("sidebarView")}>
            <button type="button" aria-pressed={view === "chats"} onClick={() => { setView("chats"); }}>
              {t("chats")}
            </button>
            <button type="button" aria-pressed={view === "workspace"} onClick={() => { setView("workspace"); }}>
              {t("workspace")}
            </button>
          </div>
        </div>
      </aside>

      <div className="pane">
        <header className="pane-header">
          <span className={`status status-${connection}`}>
            <span className="dot" aria-hidden="true" />
            {connectionLabel(connection)}
          </span>
          <h2 className="pane-title">{title}</h2>
          <div className="pane-actions">
            {notice ? <span className="pane-notice" role="status">{notice}</span> : null}
            {surface === "conversation" ? (
              <button
                type="button"
                className="icon-button"
                aria-label={t("inspector")}
                aria-pressed={inspectorOpen}
                onClick={() => { setInspectorOpen(!inspectorOpen); }}
              >
                <PanelIcon />
              </button>
            ) : null}
            <button
              type="button"
              className="icon-button"
              aria-label={t("streamTitle")}
              aria-pressed={activityOpen}
              onClick={() => { setActivityOpen(!activityOpen); }}
            >
              <PulseIcon />
            </button>
          </div>
        </header>
        <div className={`surface surface-${surface}`}>{page}</div>
        {activityOpen ? (
          <section className="activity" aria-label={t("streamTitle")}>
            <header>
              <h2>{t("streamTitle")}</h2>
              <span className="count">{log.length}</span>
              <button type="button" className="icon-button small" aria-label={t("close")} onClick={() => { setActivityOpen(false); }}>
                <CloseIcon />
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
    </main>
  );
}
