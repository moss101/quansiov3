/**
 * The permission guide for native computer use (ADP-002).
 *
 * Shown on the live surface while a run waits for an OS permission, or for the person to pause their
 * own input. It lists what the system says, opens the right Privacy pane when the person clicks, and
 * enables Continue — the person's own `ResumeRun` — only when the snapshot says what the run needs is
 * granted (or input has been paused). The page never decides that a permission is granted and never
 * resumes by itself.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { newCommandId } from "../client/ids.js";
import {
  PermissionPoller,
  guideView,
  requestFor,
  resumeCommand,
  type ComputerAttention,
  type PermissionName,
  type PermissionRow,
  type PermissionSnapshot,
} from "../live/permissions.js";

interface Props {
  /** The run the guide would resume, when the live session names one. */
  readonly run: { readonly id: string; readonly generation: number } | null;
  /** Why the runtime parked the run, when it said. Absent, the guide watches Accessibility. */
  readonly attention: ComputerAttention | null;
  /** Called after the person resumed the run, so the live view re-reads it. */
  readonly onResumed: () => void;
}

const NAME_LABEL = {
  accessibility: "permissionAccessibility",
  screen_recording: "permissionScreenRecording",
  input_monitoring: "permissionInputMonitoring",
  microphone: "permissionMicrophone",
} as const satisfies Record<PermissionName, string>;

const STATE_LABEL = {
  granted: "permissionStateGranted",
  denied: "permissionStateDenied",
  not_determined: "permissionStateNotDetermined",
  restricted: "permissionStateRestricted",
} as const;

function RowItem({ row, onOpen }: { readonly row: PermissionRow; readonly onOpen: (permission: PermissionName) => void }): ReactElement {
  return (
    <li className={`card permission-row permission-${row.state}`} data-permission={row.permission} data-state={row.state}>
      <strong>{t(NAME_LABEL[row.permission])}</strong>
      <span className="stage">
        {row.probed ? t(STATE_LABEL[row.state]) : t("permissionNotChecked")}
        {row.needed ? ` · ${t("permissionNeeded")}` : ""}
      </span>
      {row.action === "open_settings" ? (
        <button type="button" onClick={() => { onOpen(row.permission); }}>{t("permissionOpenSettings")}</button>
      ) : null}
      {row.action === "contact_administrator" ? <span>{t("permissionAskAdministrator")}</span> : null}
    </li>
  );
}

export function ComputerPermissions({ run, attention, onResumed }: Props): ReactElement | null {
  const [snapshot, setSnapshot] = useState<PermissionSnapshot | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const request = useMemo(() => requestFor(attention), [attention]);
  const requestRef = useRef(request);
  requestRef.current = request;

  useEffect(() => {
    const poller = new PermissionPoller({
      read: () => window.quansio.computerPermissions(requestRef.current),
      onSnapshot: setSnapshot,
      onError: (error) => {
        setNotice(error instanceof Error ? error.message : t("commandFailed"));
      },
    });
    poller.start();
    return () => {
      poller.stop();
    };
  }, [request]);

  const open = useCallback((permission: PermissionName) => {
    // The click names a permission; main builds the destination. Nothing the agent wrote is involved.
    void window.quansio.openComputerSettings(permission).catch((error: unknown) => {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    });
  }, []);

  const resume = useCallback(async () => {
    if (!run) {
      return;
    }
    const command = resumeCommand(run);
    try {
      // The person's own click is the only thing that sends this; the server validates it again.
      await window.quansio.invokeCommand({ name: command.name, commandId: newCommandId(), params: command.params });
      setNotice(null);
      onResumed();
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, [run, onResumed]);

  if (snapshot === null) {
    return notice ? <p className="notice">{notice}</p> : null;
  }
  const view = guideView(snapshot, attention);
  if (view.kind === "not_applicable" || (view.kind === "ready" && attention === null)) {
    // Nothing is in the way and nothing is parked on a permission: the guide stays out of the way.
    return notice ? <p className="notice">{notice}</p> : null;
  }
  return (
    <section className="live-permissions" aria-label={t("permissionGuide")} data-guide={view.kind}>
      <h3>{t("permissionGuide")}</h3>
      {view.kind === "pause_input" ? (
        <p role="status">{t("permissionPauseInput")}</p>
      ) : view.kind === "contact_administrator" ? (
        <p role="status">{t("permissionRestrictedByPolicy")}</p>
      ) : view.kind === "open_settings" ? (
        <p role="status">{t("permissionOpenSettingsHelp")}</p>
      ) : (
        <p role="status">{t("permissionReady")}</p>
      )}
      {view.rows.length > 0 ? (
        <ul className="cards">
          {view.rows.map((row) => (
            <RowItem key={row.permission} row={row} onOpen={open} />
          ))}
        </ul>
      ) : null}
      {notice ? <p className="notice">{notice}</p> : null}
      <button type="button" className="permission-continue" disabled={!view.canContinue || run === null} onClick={() => { void resume(); }}>
        {t("permissionContinue")}
      </button>
    </section>
  );
}
