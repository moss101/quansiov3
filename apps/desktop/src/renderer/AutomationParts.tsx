/**
 * The Automations surface's parts (ADP-007): the trigger fields of a routine form, the inbound
 * hook's one-time secret, the dry run, the workflow script and the revision conflict.
 *
 * Presentation and commands only. Every value from the server or from a payload goes into a text
 * node; a signing secret exists on the page only inside `SecretOnceRegion`, which the page empties
 * on dismissal; and nothing here decides whether a script may spend more calls — the page points
 * at the approvals surface, where a person does.
 */

import { useId, type ReactElement, type Ref } from "react";

import { describeFilters, HOOK_OPS, valueKind, type FilterDraft, type FilterGroup, type HookOp } from "../automation/hooks.js";
import type { DryRunState, DryRunView } from "../automation/dryrun.js";
import { filterVerdict } from "../automation/dryrun.js";
import type { Conflict, EditSession, FieldErrors } from "../automation/failures.js";
import { failureText } from "../automation/failures.js";
import type { RoutineDraft, RoutineView, TriggerKind } from "../automation/routines.js";
import { addCondition, addGroup, removeCondition, removeGroup, updateCondition } from "../automation/hooks.js";
import { copyText, type CopyTarget, type SecretState } from "../automation/secret-once.js";
import { attentionOf, budgetOf, itemProgress, latestRun, stepRows, type ScriptOutline, type ScriptRun, type StepKind, type StepStatus } from "../automation/script-view.js";
import { filterZones } from "../automation/timezones.js";
import { fill, t, type StringKey } from "../i18n/strings.js";

// ---- the sentence that says what a trigger is for ----

/** A trigger or a script is how a teammate waits; it does not sleep and poll. */
export function WaitingNote(): ReactElement {
  return (
    <p className="hint auto-waiting" role="note">
      {t("autoWaitingNote")}
    </p>
  );
}

// ---- time zone ----

interface TimezonePickerProps {
  readonly value: string;
  readonly zones: readonly string[];
  readonly viewerZone: string;
  readonly error?: string | undefined;
  readonly onChange: (zone: string) => void;
}

/** A searchable zone field: type to narrow the list, or take the viewer's own zone. */
export function TimezonePicker({ value, zones, viewerZone, error, onChange }: TimezonePickerProps): ReactElement {
  const id = useId();
  const options = filterZones(zones, zones.includes(value) ? "" : value, 500);
  const matches = filterZones(zones, zones.includes(value) ? "" : value, zones.length).length;
  return (
    <div className="auto-field auto-timezone">
      <label htmlFor={`${id}-zone`}>{t("autoTimezone")}</label>
      <input
        id={`${id}-zone`}
        type="text"
        list={`${id}-zones`}
        value={value}
        autoComplete="off"
        spellCheck={false}
        aria-describedby={`${id}-help${error ? ` ${id}-error` : ""}`}
        aria-invalid={error ? true : undefined}
        onChange={(event) => {
          onChange(event.target.value);
        }}
      />
      <datalist id={`${id}-zones`}>
        {options.map((zone) => (
          <option key={zone} value={zone} />
        ))}
      </datalist>
      <button
        type="button"
        className="link"
        onClick={() => {
          onChange(viewerZone);
        }}
      >
        {fill("autoTimezoneUse", { zone: viewerZone })}
      </button>
      <span className="sr-only" role="status" aria-live="polite">
        {fill("autoTimezoneMatches", { n: matches })}
      </span>
      <p id={`${id}-help`} className="hint">
        {t("autoTimezoneHelp")}
      </p>
      {error ? (
        <p id={`${id}-error`} className="error" role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

// ---- filters ----

const OP_LABEL: Readonly<Record<HookOp, StringKey>> = {
  eq: "autoOpEq",
  ne: "autoOpNe",
  in: "autoOpIn",
  exists: "autoOpExists",
  prefix: "autoOpPrefix",
  contains: "autoOpContains",
  gt: "autoOpGt",
  gte: "autoOpGte",
  lt: "autoOpLt",
  lte: "autoOpLte",
};

interface FilterEditorProps {
  readonly groups: FilterDraft;
  readonly problem?: string | null | undefined;
  readonly serverError?: string | undefined;
  readonly onChange: (groups: FilterDraft) => void;
}

/** Groups of typed conditions: any group (OR) of every condition in it (AND). */
export function FilterEditor({ groups, problem, serverError, onChange }: FilterEditorProps): ReactElement {
  const id = useId();
  const shown = problem ?? serverError ?? null;
  return (
    <fieldset className="auto-filters" aria-describedby={`${id}-help`}>
      <legend>{t("autoFilters")}</legend>
      <p id={`${id}-help`} className="hint">
        {t("autoFiltersHelp")}
      </p>
      {groups.length === 0 ? <p className="empty">{t("autoFiltersNone")}</p> : null}
      {groups.map((group: FilterGroup, groupIndex) => (
        // The editor's groups have no identity but their position; removing one re-keys the rest.
        <div key={groupIndex} className="auto-filter-group">
          {groupIndex > 0 ? (
            <p className="auto-filter-or" aria-hidden="true">
              {t("autoFilterOr")}
            </p>
          ) : null}
          <fieldset>
            <legend>{`${t("autoFilterGroup")} ${String(groupIndex + 1)}`}</legend>
            {group.map((condition, conditionIndex) => {
              const where = `${String(groupIndex + 1)}.${String(conditionIndex + 1)}`;
              const kind = valueKind(condition.op);
              const valueId = `${id}-v-${where}`;
              return (
                <div key={conditionIndex} className="auto-filter-condition">
                  <input
                    type="text"
                    value={condition.path}
                    placeholder="/action"
                    spellCheck={false}
                    aria-label={`${t("autoFilterPath")} ${where}`}
                    onChange={(event) => {
                      onChange(updateCondition(groups, groupIndex, conditionIndex, { path: event.target.value }));
                    }}
                  />
                  <select
                    value={condition.op}
                    aria-label={`${t("autoFilterOp")} ${where}`}
                    onChange={(event) => {
                      onChange(updateCondition(groups, groupIndex, conditionIndex, { op: event.target.value as HookOp }));
                    }}
                  >
                    {HOOK_OPS.map((op) => (
                      <option key={op} value={op}>
                        {t(OP_LABEL[op])}
                      </option>
                    ))}
                  </select>
                  {kind === "boolean" ? (
                    <select
                      id={valueId}
                      value={condition.text === "false" ? "false" : "true"}
                      aria-label={`${t("autoFilterValue")} ${where}`}
                      onChange={(event) => {
                        onChange(updateCondition(groups, groupIndex, conditionIndex, { text: event.target.value }));
                      }}
                    >
                      <option value="true">{t("autoFilterPresent")}</option>
                      <option value="false">{t("autoFilterAbsent")}</option>
                    </select>
                  ) : (
                    <input
                      id={valueId}
                      type="text"
                      value={condition.text}
                      inputMode={kind === "number" ? "decimal" : undefined}
                      placeholder={kind === "list" ? "opened, closed" : kind === "number" ? "10" : kind === "text" ? "release/" : "opened"}
                      spellCheck={false}
                      aria-label={`${kind === "list" ? t("autoFilterValueList") : kind === "number" ? t("autoFilterValueNumber") : kind === "text" ? t("autoFilterValueText") : t("autoFilterValue")} ${where}`}
                      onChange={(event) => {
                        onChange(updateCondition(groups, groupIndex, conditionIndex, { text: event.target.value }));
                      }}
                    />
                  )}
                  <button
                    type="button"
                    className="quiet"
                    aria-label={`${t("autoFilterRemoveCondition")} ${where}`}
                    onClick={() => {
                      onChange(removeCondition(groups, groupIndex, conditionIndex));
                    }}
                  >
                    ×
                  </button>
                </div>
              );
            })}
            <span className="auto-filter-actions">
              <button
                type="button"
                className="link"
                onClick={() => {
                  onChange(addCondition(groups, groupIndex));
                }}
              >
                {t("autoFilterAddCondition")}
              </button>
              <button
                type="button"
                className="link"
                onClick={() => {
                  onChange(removeGroup(groups, groupIndex));
                }}
              >
                {`${t("autoFilterRemoveGroup")} ${String(groupIndex + 1)}`}
              </button>
            </span>
          </fieldset>
        </div>
      ))}
      <button
        type="button"
        className="outline"
        onClick={() => {
          onChange(addGroup(groups));
        }}
      >
        {t("autoFilterAddGroup")}
      </button>
      {shown ? (
        <p className="error" role="alert">
          {shown}
        </p>
      ) : null}
    </fieldset>
  );
}

// ---- the routine form ----

interface RoutineFormProps {
  readonly mode: "create" | "edit";
  readonly draft: RoutineDraft;
  readonly zones: readonly string[];
  readonly viewerZone: string;
  /** What the client found wrong with the draft, and what the server refused with. */
  readonly problem: string | null;
  readonly errors: FieldErrors;
  readonly busy: boolean;
  /** An edit that has a conflict to review cannot be saved yet. */
  readonly canSubmit: boolean;
  readonly sample: string;
  readonly onChange: (draft: RoutineDraft) => void;
  readonly onSample: (text: string) => void;
  readonly onSubmit: () => void;
  readonly onDryRun: () => void;
  readonly onCancel?: (() => void) | undefined;
}

const KINDS: readonly TriggerKind[] = ["interval", "cron", "event", "webhook"];

/** The routine form, for a new routine and for an edit (which is a full replacement). */
export function RoutineForm(props: RoutineFormProps): ReactElement {
  const { mode, draft, zones, viewerZone, problem, errors, busy, canSubmit, sample, onChange, onSample, onSubmit, onDryRun, onCancel } = props;
  const id = useId();
  const kind = draft.trigger.kind;
  const touched = draft.title.trim() !== "" || draft.prompt.trim() !== "";
  const setTrigger = (change: Partial<RoutineDraft["trigger"]>): void => {
    onChange({ ...draft, trigger: { ...draft.trigger, ...change } });
  };
  return (
    <form
      className="auto-new"
      aria-label={mode === "create" ? t("autoNewRoutine") : t("autoEditing")}
      onSubmit={(event) => {
        event.preventDefault();
        onSubmit();
      }}
    >
      <h3>{mode === "create" ? t("autoNewRoutine") : t("autoEditing")}</h3>
      <input
        value={draft.title}
        placeholder={t("autoTitle")}
        aria-label={t("autoTitle")}
        onChange={(event) => {
          onChange({ ...draft, title: event.target.value });
        }}
      />
      <textarea
        value={draft.prompt}
        placeholder={t("autoPrompt")}
        aria-label={t("autoPrompt")}
        onChange={(event) => {
          onChange({ ...draft, prompt: event.target.value });
        }}
      />
      <label>
        {t("autoTrigger")}
        <select
          value={kind}
          onChange={(event) => {
            const next = event.target.value as TriggerKind;
            const zone = draft.trigger.timezone ?? "UTC";
            // A cron schedule is wall-clock time in a zone: start from the viewer's own unless one was chosen.
            setTrigger({ kind: next, spec: next === "webhook" ? "" : draft.trigger.spec, timezone: next === "cron" && (zone === "UTC" || zone === "") ? viewerZone : zone });
          }}
        >
          {KINDS.map((option) => (
            <option key={option} value={option}>
              {option === "webhook" ? t("autoKindWebhook") : option}
            </option>
          ))}
        </select>
      </label>
      {kind === "webhook" ? (
        <>
          <p className="hint">{t("autoHookNoSpec")}</p>
          <FilterEditor
            groups={draft.filters ?? []}
            problem={problem !== null && /^group |^filters /.test(problem) ? problem : null}
            serverError={errors.filters}
            onChange={(filters) => {
              onChange({ ...draft, filters });
            }}
          />
          <label htmlFor={`${id}-sample`}>{t("autoDrySample")}</label>
          <textarea
            id={`${id}-sample`}
            className="auto-sample"
            value={sample}
            spellCheck={false}
            placeholder={'{"action": "opened"}'}
            onChange={(event) => {
              onSample(event.target.value);
            }}
          />
        </>
      ) : (
        <>
          <input
            value={draft.trigger.spec}
            placeholder={t("autoSpec")}
            aria-label={t("autoSpec")}
            aria-invalid={errors.trigger ? true : undefined}
            onChange={(event) => {
              setTrigger({ spec: event.target.value });
            }}
          />
          {errors.trigger ? (
            <p className="error" role="alert">
              {errors.trigger}
            </p>
          ) : null}
        </>
      )}
      {kind === "cron" || errors.timezone ? (
        <TimezonePicker
          value={draft.trigger.timezone ?? "UTC"}
          zones={zones}
          viewerZone={viewerZone}
          error={errors.timezone}
          onChange={(timezone) => {
            setTrigger({ timezone });
          }}
        />
      ) : null}
      <label>
        {t("autoMode")}
        <select
          value={draft.mode ?? "act"}
          onChange={(event) => {
            onChange({ ...draft, mode: event.target.value === "observe" ? "observe" : "act" });
          }}
        >
          <option value="act">{t("autoModeAct")}</option>
          <option value="observe">{t("autoModeObserve")}</option>
        </select>
      </label>
      <label>
        {t("autoAbsence")}
        <select
          value={draft.absencePolicy ?? "skip"}
          onChange={(event) => {
            onChange({ ...draft, absencePolicy: event.target.value as NonNullable<RoutineDraft["absencePolicy"]> });
          }}
        >
          <option value="skip">skip</option>
          <option value="queue">queue</option>
          <option value="catch_up_once">catch_up_once</option>
        </select>
      </label>
      {problem && touched ? (
        <p className="notice" role="status">
          {problem}
        </p>
      ) : null}
      {errors.budget ? (
        <p className="error" role="alert">
          {errors.budget}
        </p>
      ) : null}
      {errors.form ? (
        <p className="error" role="alert">
          {errors.form}
        </p>
      ) : null}
      <span className="controls">
        <button type="submit" disabled={problem !== null || busy || !canSubmit}>
          {mode === "create" ? t("autoCreate") : t("autoSave")}
        </button>
        <button type="button" className="outline" onClick={onDryRun}>
          {t("autoDryRun")}
        </button>
        {onCancel ? (
          <button type="button" className="quiet" onClick={onCancel}>
            {t("autoCancel")}
          </button>
        ) : null}
      </span>
    </form>
  );
}

// ---- the secret, once ----

interface SecretOnceRegionProps {
  readonly reveal: SecretState;
  /** What was last copied, `"failed"` when the clipboard refused. */
  readonly copied: CopyTarget | "failed" | null;
  readonly regionRef?: Ref<HTMLElement> | undefined;
  readonly onCopy: (target: CopyTarget) => void;
  readonly onDismiss: () => void;
}

/**
 * The live region that announces a new inbound hook's endpoint and signing secret, and the
 * dismissible notice inside it. The region is always rendered so the announcement is made when the
 * notice appears; the notice is rendered only while the page holds a reveal, so after dismissal
 * there is nothing on the page that contains the secret.
 */
export function SecretOnceRegion({ reveal, copied, regionRef, onCopy, onDismiss }: SecretOnceRegionProps): ReactElement {
  const id = useId();
  return (
    <div className="auto-secret-region" aria-live="polite">
      {reveal ? (
        <section ref={regionRef} className="notice secret-once" aria-labelledby={`${id}-title`} tabIndex={-1} data-kind={reveal.kind}>
          <h3 id={`${id}-title`}>{reveal.kind === "rotated" ? t("autoSecretRotated") : t("autoSecretCreated")}</h3>
          <p>{reveal.secret !== null ? t("autoSecretOnce") : reveal.replayed ? t("autoSecretReplayed") : t("autoSecretNone")}</p>
          {reveal.kind === "rotated" ? <p>{t("autoSecretRotatedNote")}</p> : null}
          <dl>
            <dt>{t("autoEndpoint")}</dt>
            <dd>
              <code className="hook-path">{reveal.path}</code>{" "}
              <button
                type="button"
                className="outline"
                onClick={() => {
                  onCopy("path");
                }}
              >
                {t("autoCopyEndpoint")}
              </button>
            </dd>
            {copyText(reveal, "secret") !== null ? (
              <>
                <dt>{t("autoSecret")}</dt>
                <dd>
                  <code className="hook-secret">{copyText(reveal, "secret")}</code>{" "}
                  <button
                    type="button"
                    className="outline"
                    onClick={() => {
                      onCopy("secret");
                    }}
                  >
                    {t("autoCopySecret")}
                  </button>
                </dd>
              </>
            ) : null}
          </dl>
          <p className="hint">{t("autoSecretHowTo")}</p>
          <p className="copy-status" role="status">
            {copied === "path" ? t("autoCopiedEndpoint") : copied === "secret" ? t("autoCopiedSecret") : copied === "failed" ? t("autoCopyFailed") : ""}
          </p>
          <button type="button" onClick={onDismiss}>
            {t("autoDismissSecret")}
          </button>
        </section>
      ) : null}
    </div>
  );
}

// ---- the dry run ----

function DryRunResult({ view }: { readonly view: DryRunView }): ReactElement {
  const verdict = filterVerdict(view);
  return (
    <div className="auto-dry-result" data-valid={String(view.valid)}>
      <p className="verdict">
        <strong>{t("autoDryTitle")}</strong>: {view.valid ? t("autoDryValid") : t("autoDryInvalid")}
      </p>
      <p className="hint">{t("autoDryNothing")}</p>
      {view.problems.length > 0 ? (
        <>
          <h4>{t("autoDryProblems")}</h4>
          <ul className="auto-dry-problems" role="alert">
            {view.problems.map((problem, index) => (
              <li key={`${problem.code}-${String(index)}`}>
                <code>{problem.code}</code>
                {problem.at ? <small> · {problem.at}</small> : null} {problem.message}
              </li>
            ))}
          </ul>
        </>
      ) : null}
      <h4>{t("autoDryNext")}</h4>
      {view.nextWindows.length > 0 ? (
        <>
          {view.zone ? (
            <p className="hint">
              {t("autoDryZone")}: {view.zone.name} ({view.zone.offset})
            </p>
          ) : null}
          <ol className="auto-dry-windows">
            {view.nextWindows.map((window) => (
              <li key={window}>
                <time dateTime={window}>{window}</time>
              </li>
            ))}
          </ol>
        </>
      ) : (
        <p className="empty">{t("autoDryNoWindows")}</p>
      )}
      {view.triggerKind === "webhook" ? (
        <p className="auto-dry-filter" data-verdict={verdict}>
          {verdict === "not_evaluated" ? t("autoDryFilterNone") : verdict === "passes" ? (view.filter.matchedGroup !== null ? fill("autoDryFilterPassGroup", { n: view.filter.matchedGroup + 1 }) : t("autoDryFilterPass")) : t("autoDryFilterBlocked")}
        </p>
      ) : null}
      {view.instruction.blocks.length > 0 ? (
        <>
          <h4>
            {t("autoDryPlan")} <small>({fill("autoDryBytes", { n: view.instruction.bytes })})</small>
          </h4>
          <ol className="auto-dry-blocks">
            {view.instruction.blocks.map((block, index) => (
              <li key={`${block.type}-${String(index)}`} data-block={block.type}>
                <span className="kind">{block.type === "untrusted_data" ? fill("autoDryBlockData", { source: block.source ?? "outside" }) : t("autoDryBlockText")}</span>
                <pre>{block.text}</pre>
              </li>
            ))}
          </ol>
        </>
      ) : null}
      {view.script ? (
        <>
          <h4>{t("autoDryScript")}</h4>
          <p>{fill("autoDryScriptCalls", { fixed: view.script.staticCalls, estimated: view.script.estimatedAgentCalls, max: view.script.maxAgentCalls })}</p>
          {view.script.needsApproval ? <p className="notice">{fill("autoDryNeedsApproval", { n: view.script.approvalThresholdCalls ?? 0 })}</p> : view.script.mayNeedApproval ? <p className="hint">{t("autoDryMayNeedApproval")}</p> : null}
        </>
      ) : null}
    </div>
  );
}

/** What a dry run said, or that it is running or was refused. Always says nothing was started. */
export function DryRunPanel({ state }: { readonly state: DryRunState | null }): ReactElement {
  return (
    <div className="auto-dry" aria-live="polite">
      {state === null ? null : state.status === "running" ? (
        <p role="status">{t("autoDryRunning")}</p>
      ) : state.status === "done" ? (
        <DryRunResult view={state.view} />
      ) : state.status === "rate_limited" ? (
        <p className="notice" role="alert">
          {t("autoDryRateLimited")}
        </p>
      ) : (
        <p className="error" role="alert">
          {failureText(state.failure, t("refused"))}
        </p>
      )}
    </div>
  );
}

// ---- the revision conflict ----

interface ConflictNoticeProps {
  readonly session: EditSession;
  readonly onReload: () => void;
}

/** The routine moved on while it was being edited: say so, keep the draft, offer to reload and review. */
export function ConflictNotice({ session, onReload }: ConflictNoticeProps): ReactElement | null {
  const conflict = session.conflict;
  if (!conflict) {
    return null;
  }
  const revision = (value: number | null): string => (value === null ? t("autoConflictUnknown") : String(value));
  if (!conflict.reloaded) {
    return (
      <div className="notice conflict" role="alert" tabIndex={-1}>
        <p>{fill("autoConflict", { current: revision(conflict.currentRevision), expected: session.baseRevision })}</p>
        <button type="button" onClick={onReload}>
          {t("autoConflictReload")}
        </button>
      </div>
    );
  }
  return (
    <div className="notice conflict" role="status" tabIndex={-1}>
      <p>
        {conflict.currentRevision === null
          ? t("autoConflictGone")
          : conflict.changedFields.length === 0
            ? fill("autoConflictSame", { current: conflict.currentRevision })
            : fill("autoConflictReloaded", { current: conflict.currentRevision, fields: conflict.changedFields.join(", ") })}
      </p>
    </div>
  );
}

/** The notice for a refusal of a control (pause, resume, delete, rotate) from a list that was out of date. */
export function conflictListText(conflict: Conflict): string {
  const revision = (value: number | null): string => (value === null ? t("autoConflictUnknown") : String(value));
  return fill("autoConflictList", { current: revision(conflict.currentRevision), expected: revision(conflict.expectedRevision) });
}

// ---- the script ----

const STEP_KIND_LABEL: Readonly<Record<StepKind, StringKey>> = {
  agent: "autoStepAgent",
  parallel: "autoStepParallel",
  reduce: "autoStepReduce",
  log: "autoStepLog",
};

const STEP_STATUS_LABEL: Readonly<Record<StepStatus, StringKey>> = {
  pending: "autoStatusPending",
  running: "autoStatusRunning",
  done: "autoStatusDone",
  failed: "autoStatusFailed",
};

const RUN_STATUS_LABEL: Readonly<Record<ScriptRun["status"], StringKey>> = {
  running: "autoRunRunning",
  waiting: "autoRunWaiting",
  succeeded: "autoRunSucceeded",
  failed: "autoRunFailed",
  cancelled: "autoRunCancelled",
  finished: "autoRunFinished",
};

interface ScriptViewProps {
  readonly outline: ScriptOutline;
  readonly runs: readonly ScriptRun[];
  /** Where a person decides an open gate: the approvals surface. */
  readonly onOpenApprovals?: (() => void) | undefined;
}

/**
 * A routine's workflow script as a list of steps with each one's status, the call budget of the
 * latest run, and any open gate. A fan-out above the threshold and a run held at its call cap are
 * attention messages that point at the approvals surface; the page never approves.
 */
export function ScriptView({ outline, runs, onOpenApprovals }: ScriptViewProps): ReactElement {
  const id = useId();
  const run = latestRun(runs);
  const budget = budgetOf(outline, run);
  const attention = attentionOf(run);
  const rows = stepRows(outline, run);
  return (
    <section className="auto-script" aria-labelledby={`${id}-title`}>
      <h4 id={`${id}-title`}>{t("autoScript")}</h4>
      {budget ? (
        <p className="auto-budget" data-level={budget.level}>
          <progress aria-label={t("autoScriptCalls")} max={Math.max(budget.max, 1)} value={budget.used} />{" "}
          {run ? fill("autoScriptBudget", { used: budget.used, max: budget.max, estimated: budget.estimated }) : fill("autoScriptBudgetOutline", { max: budget.max, estimated: budget.estimated })}
        </p>
      ) : null}
      {attention.kind !== "none" ? (
        <div className="notice auto-attention" role="alert" data-attention={attention.kind}>
          <p>{attention.kind === "paused_budget" ? fill("autoScriptPaused", { calls: attention.calls ?? budget?.max ?? 0 }) : fill("autoScriptNeedsApproval", { calls: attention.calls ?? budget?.estimated ?? 0 })}</p>
          {onOpenApprovals ? (
            <button type="button" className="link" onClick={onOpenApprovals}>
              {t("autoScriptReview")}
            </button>
          ) : null}
        </div>
      ) : null}
      {run ? (
        <p className="hint">
          {t("autoScriptLatest")}: {t(RUN_STATUS_LABEL[run.status])}
          {run.error ? ` · ${t("autoScriptRunFailed")}: ${run.error}` : ""}
        </p>
      ) : (
        <p className="hint">{t("autoScriptNoRuns")}</p>
      )}
      <ol className="auto-steps" aria-label={t("autoScriptSteps")}>
        {rows.map((row) => {
          const progress = itemProgress(row.items);
          return (
            <li key={row.id} data-kind={row.kind} data-status={row.status}>
              <code>{row.id}</code> <span className="kind">{t(STEP_KIND_LABEL[row.kind])}</span>
              {row.after ? <small> · {fill("autoScriptAfter", { step: row.after })}</small> : null}
              <span className="stage">{t(STEP_STATUS_LABEL[row.status])}</span>
              {progress ? (
                <span className="auto-items">
                  {progress.total !== null ? <progress aria-label={fill("autoScriptItems", { finished: progress.finished, total: progress.total })} max={Math.max(progress.total, 1)} value={progress.finished} /> : null}{" "}
                  {progress.total !== null ? fill("autoScriptItems", { finished: progress.finished, total: progress.total }) : fill("autoScriptItemsOpen", { finished: progress.finished })}
                  {progress.failed > 0 ? <span className="error"> · {fill("autoScriptItemsFailed", { failed: progress.failed })}</span> : null}
                </span>
              ) : null}
            </li>
          );
        })}
      </ol>
    </section>
  );
}

// ---- a routine's hook line in the list ----

/** The endpoint path (never the secret) and the last day's delivery counts of a webhook routine. */
export function HookSummary({ routine }: { readonly routine: RoutineView }): ReactElement | null {
  const hook = routine.hook;
  if (!hook) {
    return null;
  }
  const filters = describeFilters(routine.trigger.filters ?? []);
  return (
    <span className="auto-hook">
      <code className="hook-path">{hook.path}</code>
      {hook.secretSet ? <span className="stage">{t("autoHookSecretSet")}</span> : null}
      {hook.deliveries ? <span className="stage">{fill("autoHookDeliveries", hook.deliveries)}</span> : null}
      {filters ? (
        <small className="hook-filters">
          {t("autoHookFilters")}: {filters}
        </small>
      ) : null}
    </span>
  );
}
