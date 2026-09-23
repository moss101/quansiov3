/**
 * Automations surface (APP-010): routines, the notification center, and where each kind of
 * notification goes.
 *
 * Routines and their fires are the server's projection; creating, pausing, resuming,
 * deleting and running now are commands it decides. Each routine shows its recent fires and
 * says so if a window or a run ever appeared twice. Notifications are the person's own;
 * the preference matrix is one command per kind.
 */

import { useCallback, useEffect, useState, type ReactElement } from "react";

import { t, type StringKey } from "../i18n/strings.js";
import {
  NOTIFICATION_CHANNELS,
  applyNotificationEvent,
  decodeNotifications,
  inAppItems,
  preferenceParams,
  toggleChannel,
  type NotificationCenter,
} from "../automation/notifications.js";
import {
  createRoutineParams,
  decodeRoutines,
  draftProblem,
  routineControls,
  summarizeFires,
  type AbsencePolicy,
  type RoutineView,
  type TriggerKind,
} from "../automation/routines.js";
import { viewerFrom } from "../collaboration/thread.js";

interface AutomationsProps {
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

const EMPTY_CENTER: NotificationCenter = { items: [], unread: 0, preferences: [] };

export function Automations({ workspaceId }: AutomationsProps): ReactElement {
  const [routines, setRoutines] = useState<readonly RoutineView[]>([]);
  const [center, setCenter] = useState<NotificationCenter>(EMPTY_CENTER);
  const [me, setMe] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [prompt, setPrompt] = useState("");
  const [kind, setKind] = useState<TriggerKind>("interval");
  const [spec, setSpec] = useState("1h");
  const [absence, setAbsence] = useState<AbsencePolicy>("skip");

  const reload = useCallback(async () => {
    try {
      setRoutines(decodeRoutines(await window.quansio.listRoutines()));
      setCenter(decodeNotifications(await window.quansio.listNotifications()));
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, []);

  useEffect(() => {
    void reload();
    void (async () => {
      try {
        setMe(viewerFrom(await window.quansio.me(), workspaceId).userId);
      } catch {
        setMe(null);
      }
    })();
  }, [reload, workspaceId]);

  useEffect(
    () =>
      window.quansio.onStreamFrame((frame) => {
        const record = frame && typeof frame === "object" ? (frame as Record<string, unknown>) : {};
        const event = record["event"] && typeof record["event"] === "object" ? (record["event"] as Record<string, unknown>) : null;
        const type = event && typeof event["type"] === "string" ? event["type"] : "";
        if (event && type.startsWith("notification.")) {
          setCenter((current) => applyNotificationEvent(current, event, me));
        } else if (type.startsWith("routine.")) {
          void reload();
        }
      }),
    [me, reload],
  );

  const run = useCallback(
    async (work: () => Promise<unknown>) => {
      const settled = (await work()) as SettledPayload;
      setNotice(refusal(settled));
      await reload();
      return settled;
    },
    [reload],
  );

  const draft = { title, prompt, trigger: { kind, spec }, absencePolicy: absence };
  const problem = draftProblem(draft);

  return (
    <section className="automations" aria-label={t("autoSurface")}>
      <div className="auto-routines">
        <h2>{t("autoRoutines")}</h2>
        {notice ? <p className="notice">{notice}</p> : null}
        {routines.length === 0 ? <p>{t("autoNoRoutines")}</p> : null}
        <ul>
          {routines.map((routine) => {
            const summary = summarizeFires(routine.fires);
            return (
              <li key={routine.id} className={`routine routine-${routine.status}`}>
                <strong>{routine.title}</strong>
                <span className="stage">
                  {routine.trigger.kind} {routine.trigger.spec} · {routine.status}
                  {routine.nextDueAt ? ` · ${t("autoNext")} ${routine.nextDueAt}` : ""}
                  {` · ${t("autoFires")} ${String(summary.fired)}`}
                  {summary.pending > 0 ? ` (+${String(summary.pending)})` : ""}
                </span>
                {summary.duplicateRuns + summary.duplicateWindows > 0 ? <span className="notice">{t("autoDuplicate")}</span> : null}
                <span className="controls">
                  {routineControls(routine).map((control) => (
                    <button key={control} type="button" onClick={() => { void run(() => window.quansio.routineCommand(control, { routine_id: routine.id })); }}>
                      {t(`autoControl${control}` as StringKey)}
                    </button>
                  ))}
                </span>
              </li>
            );
          })}
        </ul>
        <form
          className="auto-new"
          onSubmit={(event) => {
            event.preventDefault();
            void run(() => window.quansio.routineCommand("CreateRoutine", createRoutineParams(workspaceId, draft))).then((settled) => {
              if (settled.ok) {
                setTitle("");
                setPrompt("");
              }
            });
          }}
        >
          <h3>{t("autoNewRoutine")}</h3>
          <input value={title} placeholder={t("autoTitle")} onChange={(event) => { setTitle(event.target.value); }} />
          <textarea value={prompt} placeholder={t("autoPrompt")} onChange={(event) => { setPrompt(event.target.value); }} />
          <label>
            {t("autoTrigger")}
            <select value={kind} onChange={(event) => { setKind(event.target.value as TriggerKind); }}>
              <option value="interval">interval</option>
              <option value="cron">cron</option>
              <option value="event">event</option>
            </select>
          </label>
          <input value={spec} placeholder={t("autoSpec")} onChange={(event) => { setSpec(event.target.value); }} />
          <label>
            {t("autoAbsence")}
            <select value={absence} onChange={(event) => { setAbsence(event.target.value as AbsencePolicy); }}>
              <option value="skip">skip</option>
              <option value="queue">queue</option>
              <option value="catch_up_once">catch_up_once</option>
            </select>
          </label>
          {problem && (title || prompt) ? <p className="notice">{problem}</p> : null}
          <button type="submit" disabled={problem !== null}>{t("autoCreate")}</button>
        </form>
      </div>
      <div className="auto-notifications">
        <h2>
          {t("autoNotifications")} <span className="stage">{center.unread} {t("autoUnread")}</span>
        </h2>
        {inAppItems(center).length === 0 ? <p>{t("autoNoNotifications")}</p> : null}
        <ul>
          {inAppItems(center).map((item) => (
            <li key={item.id} className={item.readAt ? "read" : "unread"}>
              <span className="card">{item.kind}</span> {item.title}
              {item.readAt ? null : (
                <button type="button" onClick={() => { void run(() => window.quansio.markNotificationRead(item.id)); }}>{t("autoMarkRead")}</button>
              )}
            </li>
          ))}
        </ul>
        <h3>{t("autoPreferences")}</h3>
        <table className="auto-preferences">
          <tbody>
            {center.preferences.map((preference) => (
              <tr key={preference.kind}>
                <th scope="row">{preference.kind}</th>
                {NOTIFICATION_CHANNELS.map((channel) => (
                  <td key={channel}>
                    <label>
                      <input
                        type="checkbox"
                        checked={preference.channels.includes(channel)}
                        onChange={(event) => {
                          const channels = toggleChannel(preference.channels, channel, event.target.checked);
                          void run(() => window.quansio.setNotificationPreference(preferenceParams(workspaceId, preference.kind, channels)));
                        }}
                      />
                      {channel}
                    </label>
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}
