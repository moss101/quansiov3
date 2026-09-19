/**
 * Desktop renderer (APP-003): projection + command surface, no authority, no secrets.
 *
 * Everything the page knows arrives over `window.quansio`: the active-run projection,
 * durable event frames and live frames (model deltas, terminal bytes, browser frames).
 * Every mutation is a public command sent through the bridge.
 */

import { useCallback, useEffect, useState, type ReactElement } from "react";

import { DESKTOP_SURFACES } from "../surfaces.js";
import { t } from "../i18n/strings.js";
import { Artifacts } from "./Artifacts.js";
import { Conversation } from "./Conversation.js";
import { Onboarding } from "./Onboarding.js";

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
  if (!ready) {
    return <Onboarding onReady={onReady} />;
  }
  return <Shell />;
}

function Shell(): ReactElement {
  const [projection, setProjection] = useState<RunProjection | null>(null);
  const [log, setLog] = useState<readonly LogLine[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [surface, setSurface] = useState<"conversation" | "artifacts">("conversation");

  const refresh = useCallback(async () => {
    const next = (await window.quansio.activeRun()) as RunProjection;
    setProjection(next);
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

  return (
    <main className="shell">
      <header>
        <h1>{t("appName")}</h1>
        <span className={`status status-${projection?.streamState ?? "closed"}`}>
          {connectionLabel(projection?.streamState ?? "closed")}
        </span>
        <button type="button" onClick={() => { void window.quansio.signOut(); }}>
          {t("signOut")}
        </button>
      </header>
      <nav aria-label={t("surfaces")}>
        <ul>
          {DESKTOP_SURFACES.map((entry) => (
            <li key={entry.id} className={entry.id === surface ? "selected" : undefined}>
              {entry.id === "conversation" || entry.id === "artifacts" ? (
                <button type="button" onClick={() => { setSurface(entry.id === "artifacts" ? "artifacts" : "conversation"); }}>{entry.id}</button>
              ) : (
                entry.id
              )}
            </li>
          ))}
        </ul>
        <section className="run">
          <h2>{t("activeRun")}</h2>
          {projection?.activeRun ? (
            <dl>
              <dt>{t("workspace")}</dt>
              <dd>{projection.workspaceId}</dd>
              <dt>run</dt>
              <dd>{projection.activeRun.runId}</dd>
              <dt>thread</dt>
              <dd>{projection.activeRun.threadId}</dd>
            </dl>
          ) : (
            <p>{t("noActiveRun")}</p>
          )}
          {notice ? <p className="notice">{notice}</p> : null}
        </section>
      </nav>
      {projection?.workspaceId ? (
        surface === "artifacts" ? (
          <Artifacts key={`artifacts-${projection.workspaceId}`} workspaceId={projection.workspaceId} />
        ) : (
          <Conversation
            key={projection.workspaceId}
            workspaceId={projection.workspaceId}
            initialThreadId={projection.activeRun?.threadId ?? null}
          />
        )
      ) : (
        <p>{t("connecting")}</p>
      )}
      <section className="stream">
        <h2>{t("streamTitle")}</h2>
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
    </main>
  );
}
