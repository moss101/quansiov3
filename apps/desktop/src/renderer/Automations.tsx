/**
 * Automations surface (APP-010, ADP-007): routines and their triggers, the notification center,
 * and where each kind of notification goes.
 *
 * Routines and their fires are the server's projection; creating, editing, pausing, resuming,
 * deleting, running now, dry-running and rotating an inbound hook's secret are commands it decides.
 * An edit is a full replacement under the revision the person started from: when the routine has
 * moved on the server refuses and the draft stays on screen. An inbound hook's signing secret is
 * returned once, by the command that made it, and lives on this page only in the one-time notice
 * until it is dismissed. Each routine shows its recent fires and says so if a window or a run ever
 * appeared twice. Notifications are the person's own; the preference matrix is one command per kind.
 */

import { useCallback, useEffect, useRef, useState, type ReactElement } from "react";

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
import { DRAFT_TARGET, decodeDryRun, dryRunRefusal, parseSamplePayload, type DryRunState } from "../automation/dryrun.js";
import {
  canSave,
  conflictOf,
  decodeSettled,
  editDraft,
  failureText,
  fieldErrorsOf,
  markConflict,
  reviewAfterReload,
  startEdit,
  type EditSession,
  type FieldErrors,
  type Settled,
} from "../automation/failures.js";
import { filterProblem } from "../automation/hooks.js";
import {
  controlParams,
  createRoutineParams,
  decodeRoutines,
  draftOf,
  draftProblem,
  dryRunDraftParams,
  dryRunRoutineParams,
  newCreateRequestId,
  rotateHookParams,
  routineControls,
  summarizeFires,
  updateRoutineParams,
  type RoutineControl,
  type RoutineDraft,
  type RoutineView,
} from "../automation/routines.js";
import { NOTHING_SHOWN, copyText, dismiss, revealFrom, type CopyTarget, type SecretState } from "../automation/secret-once.js";
import { supportedZones, viewerZone } from "../automation/timezones.js";
import { viewerFrom } from "../collaboration/thread.js";
import {
  ConflictNotice,
  DryRunPanel,
  HookSummary,
  RoutineForm,
  ScriptView,
  SecretOnceRegion,
  WaitingNote,
  conflictListText,
} from "./AutomationParts.js";

interface AutomationsProps {
  readonly workspaceId: string;
  /** Opens the approvals surface, where a script's open gate is decided. */
  readonly onOpenApprovals?: () => void;
}

const EMPTY_CENTER: NotificationCenter = { items: [], unread: 0, preferences: [] };

function emptyDraft(): RoutineDraft {
  return { title: "", prompt: "", trigger: { kind: "interval", spec: "1h", timezone: "UTC" }, absencePolicy: "skip", filters: [] };
}

function problemText(error: unknown): string {
  return error instanceof Error ? error.message : t("commandFailed");
}

function refusedText(settled: Settled): string {
  return settled.ok ? "" : failureText(settled.failure, t("refused"));
}

export function Automations({ workspaceId, onOpenApprovals }: AutomationsProps): ReactElement {
  const [routines, setRoutines] = useState<readonly RoutineView[]>([]);
  const [center, setCenter] = useState<NotificationCenter>(EMPTY_CENTER);
  const [me, setMe] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [draft, setDraft] = useState<RoutineDraft>(emptyDraft);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [busy, setBusy] = useState(false);
  const [edit, setEdit] = useState<EditSession | null>(null);
  const [editErrors, setEditErrors] = useState<FieldErrors>({});
  const [reveal, setReveal] = useState<SecretState>(NOTHING_SHOWN);
  const [copied, setCopied] = useState<CopyTarget | "failed" | null>(null);
  const [rotateConfirm, setRotateConfirm] = useState<string | null>(null);
  const [dry, setDry] = useState<DryRunState | null>(null);
  const [dryOpen, setDryOpen] = useState<string | null>(null);
  const [samples, setSamples] = useState<Readonly<Record<string, string>>>({});
  const [zones] = useState(() => supportedZones());
  const [viewer] = useState(() => viewerZone());

  const secretRegion = useRef<HTMLElement>(null);
  const returnFocus = useRef<HTMLElement | null>(null);
  const createRequestId = useRef(newCreateRequestId());

  const reload = useCallback(async (): Promise<readonly RoutineView[] | null> => {
    try {
      const list = decodeRoutines(await window.quansio.listRoutines());
      setRoutines(list);
      setCenter(decodeNotifications(await window.quansio.listNotifications()));
      return list;
    } catch (error) {
      setNotice(problemText(error));
      return null;
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

  // A new secret is announced by its live region and takes focus, so a keyboard user lands on it.
  useEffect(() => {
    if (reveal) {
      secretRegion.current?.focus();
    }
  }, [reveal]);

  // A conflict takes focus when it appears and again when it has been reloaded, since the button that was pressed is gone.
  const conflictState = edit?.conflict?.reloaded;
  const hasConflict = Boolean(edit?.conflict);
  useEffect(() => {
    if (hasConflict) {
      document.querySelector<HTMLElement>(".conflict")?.focus();
    }
  }, [hasConflict, conflictState]);

  /** One routine command, decoded; a throw from the bridge is a refusal too. */
  const invoke = useCallback(async (name: string, params: Record<string, unknown>): Promise<Settled> => {
    try {
      return decodeSettled(await window.quansio.routineCommand(name, params));
    } catch (error) {
      return { ok: false, failure: { code: "INTERNAL", message: problemText(error), details: null } };
    }
  }, []);

  /** Notification commands: a refusal is the notice. */
  const run = useCallback(
    async (work: () => Promise<unknown>) => {
      const settled = decodeSettled(await work());
      setNotice(settled.ok ? null : refusedText(settled));
      await reload();
      return settled;
    },
    [reload],
  );

  const keepFocusForReveal = (): void => {
    returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  };

  const dismissReveal = (): void => {
    setReveal(dismiss());
    setCopied(null);
    const target = returnFocus.current;
    returnFocus.current = null;
    if (target?.isConnected) {
      target.focus();
    }
  };

  const copy = async (target: CopyTarget): Promise<void> => {
    const text = copyText(reveal, target);
    if (text === null) {
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      setCopied(target);
    } catch {
      setCopied("failed");
    }
  };

  // ---- create ----

  const problem = draftProblem(draft);

  const changeDraft = (next: RoutineDraft): void => {
    setDraft(next);
    setErrors({});
    // A different definition is a different create: it takes a new request id.
    createRequestId.current = newCreateRequestId();
  };

  const create = async (): Promise<void> => {
    if (busy) {
      return;
    }
    let params: Record<string, unknown>;
    try {
      params = createRoutineParams(workspaceId, draft, createRequestId.current);
    } catch (error) {
      setErrors({ form: problemText(error) });
      return;
    }
    setBusy(true);
    keepFocusForReveal();
    const settled = await invoke("CreateRoutine", params);
    setBusy(false);
    if (settled.ok) {
      setNotice(null);
      setErrors({});
      setDraft(emptyDraft());
      createRequestId.current = newCreateRequestId();
      const made = revealFrom("created", settled.result);
      if (made) {
        setCopied(null);
        setReveal(made);
      }
    } else {
      const mapped = fieldErrorsOf(settled.failure);
      if (Object.keys(mapped).length > 0) {
        setErrors(mapped);
        setNotice(null);
      } else {
        setNotice(refusedText(settled));
      }
    }
    await reload();
  };

  // ---- controls on a routine ----

  const control = async (name: RoutineControl, routine: RoutineView): Promise<void> => {
    const settled = await invoke(name, controlParams(name, routine));
    if (settled.ok) {
      setNotice(null);
    } else {
      const conflict = conflictOf(settled.failure);
      setNotice(conflict ? conflictListText(conflict) : refusedText(settled));
    }
    await reload();
  };

  const rotate = async (routine: RoutineView): Promise<void> => {
    if (rotateConfirm !== routine.id) {
      setRotateConfirm(routine.id);
      return;
    }
    setRotateConfirm(null);
    keepFocusForReveal();
    const settled = await invoke("RotateRoutineHook", rotateHookParams(routine));
    if (settled.ok) {
      setNotice(null);
      const rotated = revealFrom("rotated", settled.result);
      if (rotated) {
        setCopied(null);
        setReveal(rotated);
      }
    } else {
      const conflict = conflictOf(settled.failure);
      setNotice(conflict ? conflictListText(conflict) : refusedText(settled));
    }
    await reload();
  };

  // ---- edit ----

  const openEdit = (routine: RoutineView): void => {
    setEdit(startEdit(routine, draftOf(routine)));
    setEditErrors({});
    setDryOpen(null);
    window.setTimeout(() => {
      document.querySelector<HTMLElement>(`[data-edit="${routine.id}"] input`)?.focus();
    }, 0);
  };

  const closeEdit = (routineId: string): void => {
    setEdit(null);
    setEditErrors({});
    window.setTimeout(() => {
      document.getElementById(`auto-edit-${routineId}`)?.focus();
    }, 0);
  };

  const saveEdit = async (): Promise<void> => {
    if (!edit || !canSave(edit) || busy) {
      return;
    }
    let params: Record<string, unknown>;
    try {
      params = updateRoutineParams(edit.routineId, edit.baseRevision, edit.draft);
    } catch (error) {
      setEditErrors({ form: problemText(error) });
      return;
    }
    setBusy(true);
    keepFocusForReveal();
    const settled = await invoke("UpdateRoutine", params);
    setBusy(false);
    if (settled.ok) {
      // Changing a routine to a webhook mints its secret in this result; an edit of an existing hook returns none.
      const made = revealFrom("created", settled.result);
      if (made?.secret) {
        setCopied(null);
        setReveal(made);
      }
      closeEdit(edit.routineId);
      setNotice(null);
      await reload();
      return;
    }
    const conflict = conflictOf(settled.failure);
    if (conflict) {
      setEdit(markConflict(edit, conflict));
      setEditErrors({});
      return;
    }
    const mapped = fieldErrorsOf(settled.failure);
    setEditErrors(Object.keys(mapped).length > 0 ? mapped : { form: refusedText(settled) });
  };

  const reloadAndReview = async (): Promise<void> => {
    if (!edit) {
      return;
    }
    const list = await reload();
    if (list === null) {
      return;
    }
    const latest = list.find((routine) => routine.id === edit.routineId) ?? null;
    setEdit((current) => (current ? reviewAfterReload(current, latest, latest ? draftOf(latest) : null) : current));
  };

  // ---- dry run ----

  const runDry = async (target: string, source: { readonly routine: RoutineView } | { readonly draft: RoutineDraft }): Promise<void> => {
    const sample = parseSamplePayload(samples[target] ?? "");
    if (!sample.ok) {
      setDry({ target, status: "refused", failure: { code: "VALIDATION_SCHEMA", message: sample.message, details: null } });
      return;
    }
    let params: Record<string, unknown>;
    if ("routine" in source) {
      params = dryRunRoutineParams(source.routine.id, sample.value);
    } else {
      const filters = source.draft.trigger.kind === "webhook" ? filterProblem(source.draft.filters ?? []) : null;
      if (filters) {
        setDry({ target, status: "refused", failure: { code: "VALIDATION_SCHEMA", message: filters, details: null } });
        return;
      }
      params = dryRunDraftParams(workspaceId, source.draft, sample.value);
    }
    setDry({ target, status: "running" });
    const settled = await invoke("DryRunRoutine", params);
    setDry(settled.ok ? { target, status: "done", view: decodeDryRun(settled.result) } : { target, status: dryRunRefusal(settled.failure), failure: settled.failure });
  };

  const toggleDry = (routine: RoutineView): void => {
    if (dryOpen === routine.id) {
      setDryOpen(null);
      return;
    }
    setDryOpen(routine.id);
    setDry(null);
    // Only a webhook needs a sample payload before it can be checked; every other entry is checked at once.
    if (routine.trigger.kind !== "webhook") {
      void runDry(routine.id, { routine });
    }
  };

  return (
    <section className="automations" aria-label={t("autoSurface")}>
      <div className="auto-routines">
        <h2>{t("autoRoutines")}</h2>
        <WaitingNote />
        <div role="status">{notice ? <p className="notice">{notice}</p> : null}</div>
        <SecretOnceRegion
          reveal={reveal}
          copied={copied}
          regionRef={secretRegion}
          onCopy={(target) => {
            void copy(target);
          }}
          onDismiss={dismissReveal}
        />
        {routines.length === 0 ? <p>{t("autoNoRoutines")}</p> : null}
        <ul>
          {routines.map((routine) => {
            const summary = summarizeFires(routine.fires);
            const editing = edit?.routineId === routine.id ? edit : null;
            return (
              <li key={routine.id} className={`routine routine-${routine.status}`}>
                <strong>{routine.title}</strong>
                <span className="stage">
                  {routine.trigger.kind} {routine.trigger.spec}
                  {routine.trigger.kind === "cron" && routine.trigger.timezone ? ` (${routine.trigger.timezone})` : ""} · {routine.status}
                  {routine.nextDueAt ? ` · ${t("autoNext")} ${routine.nextDueAt}` : ""}
                  {` · ${t("autoFires")} ${String(summary.fired)}`}
                  {summary.pending > 0 ? ` (+${String(summary.pending)})` : ""}
                  {` · ${t("autoRevision")} ${String(routine.revision)}`}
                </span>
                <HookSummary routine={routine} />
                {summary.duplicateRuns + summary.duplicateWindows > 0 ? <span className="notice">{t("autoDuplicate")}</span> : null}
                <span className="controls">
                  {routine.status !== "deleted" ? (
                    <>
                      <button
                        type="button"
                        id={`auto-edit-${routine.id}`}
                        onClick={() => {
                          openEdit(routine);
                        }}
                      >
                        {t("autoEdit")}
                      </button>
                      <button
                        type="button"
                        aria-expanded={dryOpen === routine.id}
                        onClick={() => {
                          toggleDry(routine);
                        }}
                      >
                        {t("autoDryRun")}
                      </button>
                    </>
                  ) : null}
                  {routine.hook && routine.status !== "deleted" ? (
                    <button
                      type="button"
                      onClick={() => {
                        void rotate(routine);
                      }}
                    >
                      {rotateConfirm === routine.id ? t("autoRotateConfirm") : t("autoRotate")}
                    </button>
                  ) : null}
                  {routineControls(routine).map((name) => (
                    <button
                      key={name}
                      type="button"
                      onClick={() => {
                        void control(name, routine);
                      }}
                    >
                      {t(`autoControl${name}` as StringKey)}
                    </button>
                  ))}
                </span>
                {routine.script ? (
                  <div className="auto-block">
                    <ScriptView outline={routine.script} runs={routine.scriptRuns} onOpenApprovals={onOpenApprovals} />
                  </div>
                ) : null}
                {dryOpen === routine.id ? (
                  <div className="auto-block">
                    {routine.trigger.kind === "webhook" ? (
                      <>
                        <label htmlFor={`auto-sample-${routine.id}`}>{t("autoDrySample")}</label>
                        <textarea
                          id={`auto-sample-${routine.id}`}
                          className="auto-sample"
                          value={samples[routine.id] ?? ""}
                          spellCheck={false}
                          placeholder={'{"action": "opened"}'}
                          onChange={(event) => {
                            setSamples({ ...samples, [routine.id]: event.target.value });
                          }}
                        />
                        <button
                          type="button"
                          onClick={() => {
                            void runDry(routine.id, { routine });
                          }}
                        >
                          {t("autoDryRunNow")}
                        </button>
                      </>
                    ) : null}
                    <DryRunPanel state={dry?.target === routine.id ? dry : null} />
                  </div>
                ) : null}
                {editing ? (
                  <div className="auto-block" data-edit={routine.id}>
                    <ConflictNotice
                      session={editing}
                      onReload={() => {
                        void reloadAndReview();
                      }}
                    />
                    <RoutineForm
                      mode="edit"
                      draft={editing.draft}
                      zones={zones}
                      viewerZone={viewer}
                      problem={draftProblem(editing.draft)}
                      errors={editErrors}
                      busy={busy}
                      canSubmit={canSave(editing)}
                      sample={samples[`edit-${routine.id}`] ?? ""}
                      onChange={(next) => {
                        setEdit(editDraft(editing, next));
                        setEditErrors({});
                      }}
                      onSample={(text) => {
                        setSamples({ ...samples, [`edit-${routine.id}`]: text });
                      }}
                      onSubmit={() => {
                        void saveEdit();
                      }}
                      onDryRun={() => {
                        void runDry(`edit-${routine.id}`, { draft: editing.draft });
                      }}
                      onCancel={() => {
                        closeEdit(routine.id);
                      }}
                    />
                    <DryRunPanel state={dry?.target === `edit-${routine.id}` ? dry : null} />
                  </div>
                ) : null}
              </li>
            );
          })}
        </ul>
        <RoutineForm
          mode="create"
          draft={draft}
          zones={zones}
          viewerZone={viewer}
          problem={problem}
          errors={errors}
          busy={busy}
          canSubmit
          sample={samples[DRAFT_TARGET] ?? ""}
          onChange={changeDraft}
          onSample={(text) => {
            setSamples({ ...samples, [DRAFT_TARGET]: text });
          }}
          onSubmit={() => {
            void create();
          }}
          onDryRun={() => {
            void runDry(DRAFT_TARGET, { draft });
          }}
        />
        <DryRunPanel state={dry?.target === DRAFT_TARGET ? dry : null} />
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
