/**
 * Live browser / computer / terminal surface (APP-007).
 *
 * The user watches the session the agent is driving and takes it over in place. The page
 * renders the server's projection and the frames on the target's live subscription; the
 * only decisions it makes are which control to offer (`takeoverAllowed`, `handbackAllowed`)
 * and where the terminal replay resumes (`TerminalReplay`). A takeover shows the same
 * session id before and after — there is no second session to switch to — and the banner
 * says whether automation is paused by the user, by policy, or not at all.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { decodeBrowserFrame, frameToPage, latestFrame, wheelToPage, type BrowserFrame } from "../live/frames.js";
import {
  agentInputAllowed,
  applyEvent,
  attentionNeeded,
  decodeLiveSession,
  decodeTargets,
  handbackAllowed,
  inputForKey,
  modifiersOf,
  originOf,
  takeoverAllowed,
  type BrowserInput,
  type LiveSession,
  type TargetView,
} from "../live/session.js";
import { attentionFromEvent, type ComputerAttention } from "../live/permissions.js";
import { TerminalReplay, decodeTerminalBytes } from "../live/terminal.js";
import { ComputerPermissions } from "./ComputerPermissions.js";
import { LoginSuggestions } from "./LoginSuggestions.js";
import { PaymentInput } from "./PaymentInput.js";

interface LiveProps {
  readonly workspaceId: string;
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: unknown;
  readonly error?: { readonly code: string; readonly message: string };
}

function refusal(settled: SettledPayload): string | null {
  return settled.ok ? null : `${t("refused")} ${settled.error?.code ?? "INTERNAL"}: ${settled.error?.message ?? ""}`;
}

function pauseLabel(session: LiveSession): string {
  switch (session.automation.reason) {
    case "user_control":
      return t("livePausedByUser");
    case "policy":
      return t("livePausedByPolicy");
    case "closed":
      return t("liveClosed");
    default:
      return t("liveAgentDriving");
  }
}

export function Live({ workspaceId }: LiveProps): ReactElement {
  const [targets, setTargets] = useState<readonly TargetView[] | null>(null);
  const [session, setSession] = useState<LiveSession | null>(null);
  const [frame, setFrame] = useState<BrowserFrame | null>(null);
  const [liveState, setLiveState] = useState<string>("closed");
  const [notice, setNotice] = useState<string | null>(null);
  const [terminalText, setTerminalText] = useState<string>("");
  const [terminalCursor, setTerminalCursor] = useState<number>(0);
  const [terminalGaps, setTerminalGaps] = useState<number>(0);
  const replay = useRef<TerminalReplay | null>(null);
  const sessionIdRef = useRef<string | null>(null);
  // Why the runtime parked the run on an OS permission, when an event said so (ADP-002).
  const [attention, setAttention] = useState<ComputerAttention | null>(null);
  const runIdRef = useRef<string | null>(null);

  const loadTargets = useCallback(async () => {
    try {
      const decoded = decodeTargets(await window.quansio.listTargets());
      setTargets(decoded);
      return decoded;
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
      return [];
    }
  }, []);

  const open = useCallback(async (sessionId: string) => {
    try {
      const next = decodeLiveSession(await window.quansio.liveSession(sessionId));
      if (next.sessionId !== sessionId) {
        // The projection answers for the id it was asked about; anything else would be a
        // replacement session, and the page never shows one in place of what the user chose.
        throw new Error(t("liveSessionMismatch"));
      }
      setSession(next);
      // A frame belongs to one session: switching sessions never shows the old one's page.
      setFrame((current) => (current && current.sessionId === next.sessionId ? current : null));
      // The terminal replay resumes from the durable cursor: the same session, the same
      // offset; a replay for another terminal session starts over at its cursor.
      const terminal = next.terminals[0];
      if (terminal) {
        if (replay.current?.sessionId === terminal.id) {
          replay.current.resume(terminal.cursor);
        } else {
          replay.current = new TerminalReplay(terminal.id, terminal.cursor);
          setTerminalText("");
        }
        setTerminalCursor(replay.current.cursor);
        setTerminalGaps(replay.current.gaps);
      }
      await window.quansio.attachLive(next.targetId);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, []);

  useEffect(() => {
    void loadTargets().then((decoded) => {
      const first = decoded.flatMap((target) => target.browserSessions.map((entry) => entry.id))[0];
      if (first) {
        void open(first);
      }
    });
    return () => {
      void window.quansio.detachLive();
    };
  }, [loadTargets, open]);

  useEffect(() => {
    sessionIdRef.current = session?.sessionId ?? null;
    runIdRef.current = session?.run?.id ?? null;
  }, [session]);

  useEffect(
    () =>
      window.quansio.onLiveFrame((payload) => {
        const record = payload && typeof payload === "object" ? (payload as Record<string, unknown>) : {};
        if (record["kind"] === "state" && typeof record["state"] === "string") {
          setLiveState(record["state"]);
          if (record["state"] === "open" && sessionIdRef.current) {
            // Reconnected: re-read the projection so the replay resumes from the durable cursor.
            void open(sessionIdRef.current);
          }
          return;
        }
        if (record["kind"] === "event" && record["event"] && typeof record["event"] === "object") {
          setSession((current) => (current ? applyEvent(current, record["event"] as Record<string, unknown>) : current));
          return;
        }
        if (record["kind"] !== "live") {
          return;
        }
        const live = record["frame"];
        const bytes = decodeTerminalBytes(live);
        if (bytes && replay.current) {
          const accepted = replay.current.accept(bytes);
          if (accepted.kind === "appended") {
            setTerminalText(replay.current.text());
          }
          setTerminalCursor(replay.current.cursor);
          setTerminalGaps(replay.current.gaps);
          return;
        }
        setSession((current) => {
          if (current) {
            const decoded = decodeBrowserFrame(live, current.sessionId);
            if (decoded) {
              setFrame((previous) => latestFrame(previous, decoded));
            }
          }
          return current;
        });
      }),
    [open],
  );

  useEffect(
    () =>
      window.quansio.onStreamFrame((payload) => {
        const record = payload && typeof payload === "object" ? (payload as Record<string, unknown>) : {};
        const event = record["event"] && typeof record["event"] === "object" ? (record["event"] as Record<string, unknown>) : null;
        if (!event) {
          return;
        }
        const parked = attentionFromEvent(event, runIdRef.current);
        if (parked) {
          setAttention(parked);
        } else if (event["type"] === "run.resumed" && (event["payload"] as Record<string, unknown> | undefined)?.["run_id"] === runIdRef.current) {
          setAttention(null);
        }
        setSession((current) => (current ? applyEvent(current, event) : current));
      }),
    [],
  );

  const control = useCallback(
    async (kind: "takeover" | "handback") => {
      if (!session) {
        return;
      }
      const settled = (kind === "takeover"
        ? await window.quansio.requestTakeover({ sessionId: session.sessionId })
        : await window.quansio.handback({ sessionId: session.sessionId })) as SettledPayload;
      const refused = refusal(settled);
      if (refused) {
        setNotice(refused);
        return;
      }
      setNotice(null);
      await open(session.sessionId);
    },
    [session, open],
  );

  // A person's input to the session they hold: a click on the picture and printable keys. The server refuses it unless they
  // hold the session, and says so when it could not be delivered; the page shows that and does not pretend it landed.
  const sendInput = useCallback(
    async (input: BrowserInput) => {
      if (!session || session.controller !== "user" || !frame?.tabId) {
        return;
      }
      const settled = (await window.quansio.sendBrowserInput({ sessionId: session.sessionId, tabId: frame.tabId, ...input })) as SettledPayload;
      const refused = refusal(settled);
      setNotice(refused);
    },
    [session, frame],
  );

  // A click is a press and release in one place; a press that is released somewhere else is a drag, and the click the browser
  // reports after it is not sent as well. A second click in quick succession arrives as a click of count 2 (the first was
  // already delivered), so a double click reaches the page as the browser itself would send it.
  const pressedAt = useRef<{ x: number; y: number } | null>(null);
  const dragged = useRef(false);
  type PointerEventLike = { clientX: number; clientY: number; detail?: number; button: number; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean; currentTarget: HTMLElement; preventDefault?: () => void };
  const pressPicture = useCallback((event: PointerEventLike) => {
    pressedAt.current = event.button === 0 ? { x: event.clientX, y: event.clientY } : null;
    dragged.current = false;
  }, []);
  const releasePicture = useCallback(
    (event: PointerEventLike) => {
      const start = pressedAt.current;
      pressedAt.current = null;
      if (!frame || !start || event.button !== 0 || Math.hypot(event.clientX - start.x, event.clientY - start.y) < 6) {
        return;
      }
      const rect = event.currentTarget.getBoundingClientRect();
      const from = frameToPage(frame, rect, start.x, start.y);
      const to = frameToPage(frame, rect, event.clientX, event.clientY);
      if (from && to) {
        dragged.current = true;
        void sendInput({ kind: "drag", fromX: from.x, fromY: from.y, toX: to.x, toY: to.y, modifiers: modifiersOf(event) });
      }
    },
    [frame, sendInput],
  );
  const clickPicture = useCallback(
    (event: PointerEventLike) => {
      if (!frame) {
        return;
      }
      if (dragged.current) {
        dragged.current = false;
        return;
      }
      const point = frameToPage(frame, event.currentTarget.getBoundingClientRect(), event.clientX, event.clientY);
      if (point) {
        const clickCount = Math.min(Math.max(event.detail ?? 1, 1), 3) as 1 | 2 | 3;
        void sendInput({ kind: "click", ...point, clickCount, modifiers: modifiersOf(event) });
      }
    },
    [frame, sendInput],
  );
  // The page's own context menu is the remote page's to show, so the local one is suppressed and the right click is sent.
  const rightClickPicture = useCallback(
    (event: PointerEventLike) => {
      event.preventDefault?.();
      if (!frame) {
        return;
      }
      const point = frameToPage(frame, event.currentTarget.getBoundingClientRect(), event.clientX, event.clientY);
      if (point) {
        void sendInput({ kind: "click", ...point, button: "right", modifiers: modifiersOf(event) });
      }
    },
    [frame, sendInput],
  );
  const middleClickPicture = useCallback(
    (event: PointerEventLike) => {
      if (!frame || event.button !== 1) {
        return;
      }
      const point = frameToPage(frame, event.currentTarget.getBoundingClientRect(), event.clientX, event.clientY);
      if (point) {
        void sendInput({ kind: "click", ...point, button: "middle", modifiers: modifiersOf(event) });
      }
    },
    [frame, sendInput],
  );

  const typeInPicture = useCallback(
    (event: { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean; preventDefault: () => void }) => {
      // Named keys, shortcuts and printable characters are the person's to send to the page they hold.
      const input = inputForKey(event);
      if (input) {
        event.preventDefault();
        void sendInput(input);
      }
    },
    [sendInput],
  );

  // A turn of the wheel over the picture scrolls the page. Turns in a short burst are added up and sent as one scroll, so a
  // fast flick is a single command and not dozens.
  const pendingScroll = useRef<{ x: number; y: number; deltaX: number; deltaY: number } | null>(null);
  const scrollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const wheelOverPicture = useCallback(
    (event: { clientX: number; clientY: number; deltaX: number; deltaY: number; currentTarget: HTMLElement }) => {
      if (!frame) {
        return;
      }
      const turn = wheelToPage(frame, event.currentTarget.getBoundingClientRect(), event.clientX, event.clientY, event.deltaX, event.deltaY);
      if (!turn) {
        return;
      }
      const before = pendingScroll.current;
      pendingScroll.current = before ? { ...turn, deltaX: before.deltaX + turn.deltaX, deltaY: before.deltaY + turn.deltaY } : turn;
      scrollTimer.current ??= setTimeout(() => {
        const send = pendingScroll.current;
        pendingScroll.current = null;
        scrollTimer.current = null;
        if (send) {
          void sendInput({ kind: "scroll", x: send.x, y: send.y, deltaX: Math.max(-10000, Math.min(10000, send.deltaX)), deltaY: Math.max(-10000, Math.min(10000, send.deltaY)) });
        }
      }, 80);
    },
    [frame, sendInput],
  );

  // A certificate error and a media permission are the person's to decide (ADP-001): the server refuses anyone else,
  // and nothing the agent does enables these buttons. A refusal is shown; the view is re-read either way.
  const decideCertificate = useCallback(
    async (tabId: string, certificateId: string, decision: "allow" | "deny") => {
      if (!session) {
        return;
      }
      const settled = (await window.quansio.resolveCertificate({ sessionId: session.sessionId, tabId, certificateId, decision })) as SettledPayload;
      setNotice(refusal(settled));
      await open(session.sessionId);
    },
    [session, open],
  );

  const decideMedia = useCallback(
    async (tabId: string, origin: string, permission: "camera" | "microphone", decision: "allow" | "deny") => {
      if (!session) {
        return;
      }
      const settled = (await window.quansio.decidePermission({ sessionId: session.sessionId, tabId, origin, permission, decision })) as SettledPayload;
      setNotice(refusal(settled));
      await open(session.sessionId);
    },
    [session, open],
  );

  const computerControl = useCallback(
    async (kind: "takeover" | "handback") => {
      if (!session) {
        return;
      }
      const settled = (kind === "takeover"
        ? await window.quansio.requestTakeover({ targetId: session.targetId })
        : await window.quansio.handback({ targetId: session.targetId })) as SettledPayload;
      const refused = refusal(settled);
      setNotice(refused);
      await open(session.sessionId);
    },
    [session, open],
  );

  const sessions = useMemo(() => (targets ?? []).flatMap((target) => target.browserSessions.map((entry) => ({ ...entry, targetId: target.id, substrate: target.substrate }))), [targets]);

  return (
    <section className="live" aria-label={t("liveSurface")}>
      <aside className="live-sessions">
        <h2>{t("liveSessions")}</h2>
        {targets === null ? (
          <p>{t("connecting")}</p>
        ) : sessions.length === 0 ? (
          <p>{t("liveNoSessions")}</p>
        ) : (
          <ul>
            {sessions.map((entry) => (
              <li key={entry.id} className={session?.sessionId === entry.id ? "selected" : undefined}>
                <button type="button" onClick={() => { void open(entry.id); }}>
                  {entry.currentUrl ?? entry.id}
                </button>
                <span className="stage">{entry.controller} · {entry.status}</span>
              </li>
            ))}
          </ul>
        )}
        <p className="stage">{t("workspace")} {workspaceId}</p>
        <p className={`status status-${liveState}`}>{t("liveStream")} {liveState}</p>
      </aside>
      {session ? (
        <div className="live-view">
          <header className="live-header">
            <span className={`live-banner live-banner-${session.automation.reason ?? "driving"}`} role="status">
              {pauseLabel(session)}
            </span>
            <span className="live-session-id" data-session-id={session.sessionId}>{session.sessionId}</span>
            <span className="stage">
              {t("liveController")} {session.controller} · {t("liveAgentInput")} {agentInputAllowed(session) ? t("liveAllowed") : t("liveFenced")}
              {session.run ? ` · run ${session.run.status}` : ""}
            </span>
            {takeoverAllowed(session) ? (
              <button type="button" className="live-takeover" onClick={() => { void control("takeover"); }}>{t("liveTakeOver")}</button>
            ) : null}
            {handbackAllowed(session) ? (
              <button type="button" className="live-handback" onClick={() => { void control("handback"); }}>{t("liveHandBack")}</button>
            ) : null}
          </header>
          {notice ? <p className="notice">{notice}</p> : null}
          <nav className="live-navigation" aria-label={t("liveNavigation")}>
            <span className="live-url">{frame?.url ?? session.currentUrl ?? t("liveNoPage")}</span>
            <ul className="cards">
              {session.tabs.map((tab) => (
                <li key={tab.targetId} className="card">{tab.title || tab.url}</li>
              ))}
            </ul>
          </nav>
          <div className="live-screen">
            {frame ? (
              <img
                src={frame.dataUrl}
                alt={t("liveScreenAlt")}
                className={session.controller === "user" ? "live-picture live-picture-driving" : "live-picture"}
                tabIndex={session.controller === "user" ? 0 : undefined}
                onMouseDown={session.controller === "user" ? pressPicture : undefined}
                onMouseUp={session.controller === "user" ? releasePicture : undefined}
                onClick={session.controller === "user" ? clickPicture : undefined}
                onAuxClick={session.controller === "user" ? middleClickPicture : undefined}
                onContextMenu={session.controller === "user" ? rightClickPicture : undefined}
                onKeyDown={session.controller === "user" ? typeInPicture : undefined}
                onWheel={session.controller === "user" ? wheelOverPicture : undefined}
              />
            ) : (
              <p className="live-no-frame">{session.screencastEnabled ? t("liveWaitingFrames") : t("liveNoScreencast")}</p>
            )}
            {frame?.cursor && session.controller === "agent" ? (
              <span className={`live-cursor live-cursor-${frame.cursor.action}`} style={{ left: `${String(frame.cursor.x)}px`, top: `${String(frame.cursor.y)}px` }} aria-label={`${t("liveAgentCursor")} ${frame.cursor.action}`} />
            ) : null}
            {session.controller === "user" ? <div className="live-user-owns">{t("liveYouOwn")}</div> : null}
          </div>
          <section className="live-guard" aria-label={t("liveGuard")}>
            <h3>{t("liveGuard")}</h3>
            {attentionNeeded(session).length === 0 ? (
              <p className="stage">{t("liveGuardNone")}</p>
            ) : (
              <ul className="cards">
                {attentionNeeded(session).map((item) => (
                  <li key={`${item.kind}:${item.id}`} className={`card live-guard-${item.kind}`} data-attention={item.kind}>
                    <strong>{item.kind === "dialog" ? t("liveDialogPending") : item.kind === "certificate" ? t("liveCertificateRequired") : t("liveInDoubt")}</strong>
                    <span className="live-untrusted">{item.detail}</span>
                    {item.kind === "certificate" && item.tabId ? (
                      <span className="live-decision">
                        <button type="button" className="live-certificate-continue" onClick={() => { void decideCertificate(item.tabId ?? "", item.id, "allow"); }}>{t("liveCertificateContinue")}</button>
                        <button type="button" className="live-certificate-cancel" onClick={() => { void decideCertificate(item.tabId ?? "", item.id, "deny"); }}>{t("liveCertificateCancel")}</button>
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
            <p className="stage">{t("liveGuardUntrusted")}</p>
            {session.guard.tabs.map((tab) => (
              <div key={tab.tabId} className="live-guard-tab" data-tab={tab.tabId}>
                <p className="stage">
                  {t("liveRevision")} {tab.documentRevision}
                  {tab.popupsBlocked > 0 ? ` · ${String(tab.popupsBlocked)} ${t("livePopupsBlocked")}` : ""}
                </p>
                {originOf(tab.url) ? (
                  <div className="live-permission" data-origin={originOf(tab.url) ?? ""}>
                    <p className="stage">{t("livePermissionFor")} <span className="live-untrusted">{originOf(tab.url)}</span></p>
                    <button type="button" className="live-permission-camera" onClick={() => { void decideMedia(tab.tabId, originOf(tab.url) ?? "", "camera", "allow"); }}>{t("livePermissionCamera")}</button>
                    <button type="button" className="live-permission-microphone" onClick={() => { void decideMedia(tab.tabId, originOf(tab.url) ?? "", "microphone", "allow"); }}>{t("livePermissionMicrophone")}</button>
                    <button type="button" className="live-permission-deny" onClick={() => { void decideMedia(tab.tabId, originOf(tab.url) ?? "", "camera", "deny").then(() => decideMedia(tab.tabId, originOf(tab.url) ?? "", "microphone", "deny")); }}>{t("livePermissionDeny")}</button>
                    <p className="stage">{t("livePermissionNote")}</p>
                  </div>
                ) : null}
                <h4>{t("liveDownloads")}</h4>
                {tab.downloads.length === 0 ? (
                  <p className="stage">{t("liveNoDownloads")}</p>
                ) : (
                  <ul>
                    {tab.downloads.map((download) => (
                      <li key={download.id} data-download-state={download.state}>
                        <span className="live-untrusted">{download.suggestedFilename}</span> · {download.state} · {String(download.receivedBytes)} B
                        {download.failure ? ` · ${download.failure}` : ""}
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            ))}
          </section>
          <LoginSuggestions sessionId={session.sessionId} pageUrl={frame?.url ?? session.currentUrl ?? null} />
          <PaymentInput pageUrl={frame?.url ?? session.currentUrl ?? null} />
          {session.computerControl ? (
            <section className="live-computer">
              <h3>{t("liveComputer")}</h3>
              <p className="stage">
                {t("liveController")} {session.computerControl.holder} · gen {session.computerControl.generation}
                {session.computerControl.takeoverPending ? ` · ${t("liveTakeoverPending")}` : ""}
                {session.computerControl.activeToolCallId ? ` · ${session.computerControl.activeToolCallId}` : ""}
              </p>
              {session.computerControl.holder === "agent" && !session.computerControl.takeoverPending ? (
                <button type="button" onClick={() => { void computerControl("takeover"); }}>{t("liveTakeOver")}</button>
              ) : session.computerControl.holder === "user" ? (
                <button type="button" onClick={() => { void computerControl("handback"); }}>{t("liveHandBack")}</button>
              ) : null}
              <ComputerPermissions
                run={session.run ? { id: session.run.id, generation: session.run.generation } : null}
                attention={attention}
                onResumed={() => {
                  setAttention(null);
                  void open(session.sessionId);
                }}
              />
            </section>
          ) : null}
          <section className="live-terminal">
            <h3>{t("liveTerminal")}</h3>
            {session.terminals.length === 0 ? (
              <p>{t("liveNoTerminal")}</p>
            ) : (
              <>
                <p className="stage">
                  {session.terminals[0]?.id} · {t("liveCursor")} {terminalCursor}
                  {terminalGaps > 0 ? ` · ${t("liveGaps")} ${String(terminalGaps)}` : ""}
                  {session.terminals[0]?.lastCommandId ? ` · ${session.terminals[0].lastCommandId}` : ""}
                </p>
                <pre className="live-pty" data-cursor={terminalCursor}>{terminalText || t("liveTerminalEmpty")}</pre>
              </>
            )}
          </section>
        </div>
      ) : (
        <p>{t("liveNoSessions")}</p>
      )}
    </section>
  );
}
