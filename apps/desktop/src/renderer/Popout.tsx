/**
 * A thread in its own window (ADP-004, DOMAIN.md §13.6 B).
 *
 * The pop-out is the same conversation surface the main window shows, bound to one thread that main validated
 * and recorded when it opened the window. It holds nothing the main window does not: every read and command goes
 * through the same bridge, and main refuses anything outside this thread. The page can keep the window on top and
 * close it; the rest is the conversation.
 */

import { useCallback, useEffect, useState, type ReactElement } from "react";

import type { RosterEntry } from "../conversation/presentation.js";
import { t } from "../i18n/strings.js";
import { decodeRoster } from "../teammates/roster.js";
import { Conversation } from "./Conversation.js";
import { CloseIcon, PinIcon } from "./icons.js";

interface PopoutProps {
  readonly threadId: string;
  readonly inspectorOpen: boolean;
}

function record(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

export function Popout({ threadId, inspectorOpen: initialInspector }: PopoutProps): ReactElement {
  const [workspaceId, setWorkspaceId] = useState<string | null>(null);
  const [personId, setPersonId] = useState<string | null>(null);
  const [roster, setRoster] = useState<readonly RosterEntry[]>([]);
  const [pinned, setPinned] = useState(false);
  const [inspectorOpen, setInspectorOpen] = useState(initialInspector);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const gone = (): boolean => cancelled;
    void (async () => {
      try {
        const session = record(await window.quansio.sessionState());
        const workspace = typeof session["workspaceId"] === "string" ? session["workspaceId"] : null;
        const me = record(record(await window.quansio.me())["user"]);
        const teammates = decodeRoster(await window.quansio.listTeammates());
        if (!gone()) {
          setWorkspaceId(workspace);
          setPersonId(typeof me["id"] === "string" ? me["id"] : null);
          setRoster(teammates.teammates.map((teammate) => ({ id: teammate.id, name: teammate.name, agentThreadId: teammate.thread?.id ?? null })));
        }
      } catch {
        if (!gone()) {
          setFailed(true);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Nothing is queued for a pop-out, but pulling once tells main this page has subscribed.
  useEffect(() => {
    void window.quansio.consumePending();
  }, []);

  const togglePin = useCallback(() => {
    void window.quansio.pinPopout(!pinned).then((answer) => {
      if (record(answer)["ok"] === true) {
        setPinned(!pinned);
      }
    });
  }, [pinned]);

  const close = useCallback(() => {
    void window.quansio.closeWindow();
  }, []);

  const noop = useCallback(() => undefined, []);
  const noopAsync = useCallback(() => Promise.resolve(), []);

  return (
    <main className="popout">
      <header className="popout-bar">
        <span className="popout-drag" aria-hidden="true" />
        <button type="button" className="icon-button ghost small" aria-label={t("popoutPin")} data-tip={t("popoutPin")} aria-pressed={pinned} onClick={togglePin}>
          <PinIcon />
        </button>
        <button type="button" className="icon-button ghost small" aria-label={t("close")} data-tip={t("close")} onClick={close}>
          <CloseIcon size={14} />
        </button>
      </header>
      <div className="popout-body">
        {failed || !workspaceId ? (
          <p className="empty">{failed ? t("popoutGone") : t("connecting")}</p>
        ) : (
          <Conversation
            workspaceId={workspaceId}
            threadId={threadId}
            teammateId={null}
            teammateName={null}
            threadFace={null}
            roster={roster}
            personId={personId}
            onThreadChange={noop}
            onThreadsChanged={noopAsync}
            inspectorOpen={inspectorOpen}
            onInspectorOpen={setInspectorOpen}
          />
        )}
      </div>
    </main>
  );
}
