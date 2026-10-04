/**
 * The recording flow on the skills surface (ADP-003, DOMAIN.md §11.7 F): start, review what was removed,
 * decide every value, submit as a DRAFT.
 *
 * It is projection plus command surface. The recorder's state comes from main, the person's recordings from
 * the server, and every command is the person's own click: the page offers `Finish` only through the
 * indicator (a genuine click), never builds an owner fence (main holds it), and refuses a submission until
 * every slot is classified. A recording's text (control titles, screen text, narration) is untrusted data and
 * is shown as text; no control here acts on it. Promotion is the skills ladder's, not this page's.
 */

import { useCallback, useEffect, useMemo, useState, type ReactElement } from "react";

import { t, type StringKey } from "../i18n/strings.js";
import {
  IDLE_RECORDER,
  REDACTION_CATEGORIES,
  canBeConstant,
  canCompile,
  decodeRecorder,
  decodeRecordings,
  isCapturing,
  submission,
  type Decisions,
  type RecorderView,
  type RecordingView,
  type RedactionCategory,
  type SlotClassification,
} from "../recording/state.js";
import type { ComputerAttention, PermissionName, PermissionState } from "../live/permissions.js";
import { ComputerPermissions } from "./ComputerPermissions.js";

interface Props {
  /** Called when a draft skill was created, so the skills list re-reads. */
  readonly onSubmitted: () => void;
}

interface Settled {
  readonly ok?: boolean;
  readonly code?: string;
  readonly message?: string;
  readonly permission?: string;
  readonly status?: string;
  readonly error?: { readonly code: string; readonly message: string };
  readonly result?: unknown;
}

const CATEGORY_LABEL: Record<RedactionCategory, StringKey> = {
  api_key: "recordingCategoryApiKey",
  bearer_token: "recordingCategoryBearerToken",
  card_number: "recordingCategoryCardNumber",
  verification_code: "recordingCategoryVerificationCode",
  password_field: "recordingCategoryPasswordField",
};

const PANE: Record<PermissionName, string> = {
  accessibility: "Privacy_Accessibility",
  screen_recording: "Privacy_ScreenCapture",
  input_monitoring: "Privacy_ListenEvent",
  microphone: "Privacy_Microphone",
};

function describe(settled: Settled): string {
  const code = settled.error?.code ?? settled.code ?? "INTERNAL";
  const message = settled.error?.message ?? settled.message ?? "";
  return `${t("commandFailed")} ${code}: ${message}`;
}

/** The permission refusal as the guide's attention item: it opens a Privacy pane only on a click. */
function attentionFor(settled: Settled): ComputerAttention | null {
  const permission = settled.permission;
  if (settled.code !== "RECORDING_PERMISSION_REQUIRED" || typeof permission !== "string" || !(permission in PANE)) {
    return null;
  }
  const name = permission as PermissionName;
  const status = (settled.status ?? "denied") as PermissionState;
  return {
    code: "COMPUTER_PERMISSION_REQUIRED",
    permission: name,
    status,
    guide: status === "restricted" ? { action: "contact_administrator", pane: null } : { action: "open_settings", pane: PANE[name] },
  };
}

export function Recording({ onSubmitted }: Props): ReactElement {
  const [recorder, setRecorder] = useState<RecorderView>(IDLE_RECORDER);
  const [recordings, setRecordings] = useState<readonly RecordingView[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const [attention, setAttention] = useState<ComputerAttention | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [decisions, setDecisions] = useState<Decisions>({});
  const [name, setName] = useState("");
  const [semver, setSemver] = useState("1.0.0");

  const reload = useCallback(async () => {
    try {
      setRecordings(decodeRecordings(await window.quansio.listRecordings()));
      setRecorder(decodeRecorder(await window.quansio.recordingStatus()));
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, []);

  useEffect(() => {
    void reload();
    const timer = setInterval(() => {
      void reload();
    }, 2000);
    return () => {
      clearInterval(timer);
    };
  }, [reload]);

  const current = useMemo(() => recordings.find((entry) => entry.id === selected) ?? recordings.find((entry) => entry.status === "finished" || entry.status === "compiled") ?? null, [recordings, selected]);

  const start = useCallback(async () => {
    setNotice(null);
    setAttention(null);
    const answer = (await window.quansio.startRecording()) as Settled;
    if (answer.ok === true) {
      await reload();
      return;
    }
    const guide = attentionFor(answer);
    setAttention(guide);
    setNotice(guide ? t("recordingPermissionNeeded") : answer.code === "RECORDER_UNAVAILABLE" ? t("recordingUnavailable") : describe(answer));
  }, [reload]);

  const compile = useCallback(
    async (recording: RecordingView) => {
      setNotice(null);
      const answer = (await window.quansio.compileRecording(recording.id)) as Settled;
      if (answer.ok !== true) {
        setNotice(describe(answer));
      }
      setDecisions({});
      setSelected(recording.id);
      await reload();
    },
    [reload],
  );

  const submit = useCallback(
    async (recording: RecordingView) => {
      const built = submission(recording, name, semver, decisions);
      if (!built.ok) {
        setNotice(`${t("commandFailed")} ${built.code}: ${built.reason}`);
        return;
      }
      const answer = (await window.quansio.submitRecordingSkill({
        recordingId: recording.id,
        name: built.value.name,
        semver: built.value.semver,
        slots: built.value.slots,
      })) as Settled;
      if (answer.ok === true) {
        setNotice(t("recordingSubmitted"));
        setName("");
        onSubmitted();
      } else {
        setNotice(describe(answer));
      }
      await reload();
    },
    [decisions, name, semver, onSubmitted, reload],
  );

  const decide = useCallback((slotId: string, choice: SlotClassification) => {
    setDecisions((previous) => ({ ...previous, [slotId]: choice }));
  }, []);

  return (
    <section className="recording" aria-label={t("recordingTitle")}>
      <h2>{t("recordingTitle")}</h2>
      <p className="hint">{t("recordingIntro")}</p>
      {notice ? <p className="notice" role="status">{notice}</p> : null}
      {attention ? <ComputerPermissions run={null} attention={attention} onResumed={() => undefined} /> : null}
      <button type="button" className="recording-start" disabled={isCapturing(recorder)} onClick={() => { void start(); }}>
        {attention ? t("recordingTryAgain") : t("recordingStart")}
      </button>
      <h3>{t("recordingYours")}</h3>
      {recordings.length === 0 ? <p className="hint">{t("recordingNone")}</p> : null}
      <ul className="recordings">
        {recordings.map((recording) => (
          <li key={recording.id} data-recording={recording.id} data-status={recording.status} className={recording.id === current?.id ? "selected" : undefined}>
            <button type="button" className="link" onClick={() => { setSelected(recording.id); setDecisions({}); }}>
              <code>{recording.id}</code>
            </button>{" "}
            · {t("recordingStatus")}: {recording.status}
            {recording.abortReason ? ` (${recording.abortReason})` : ""} · {String(recording.actions)} {t("recordingActions")}
          </li>
        ))}
      </ul>
      {current ? (
        <div className="recording-review" data-recording={current.id}>
          {current.redaction ? (
            <div className="recording-redaction">
              <h3>{t("recordingRedacted")}</h3>
              <p className="hint">{t("recordingRedactedHelp")}</p>
              <ul>
                {REDACTION_CATEGORIES.map((category) => (
                  <li key={category} data-category={category}>
                    {String(current.redaction?.counts[category] ?? 0)} {t(CATEGORY_LABEL[category])}
                  </li>
                ))}
                <li data-category="screenshots_withheld">
                  {String(current.redaction.screenshotsWithheld)} {t("recordingScreenshotsWithheld")}
                </li>
              </ul>
            </div>
          ) : null}
          {canCompile(current) ? (
            <button type="button" className="recording-compile" onClick={() => { void compile(current); }}>
              {t("recordingCompile")}
            </button>
          ) : null}
          {current.proposal ? (
            <div className="recording-proposal">
              <h3>{t("recordingProposal")}</h3>
              <p className="hint">{t("recordingProposalUntrusted")}</p>
              {current.proposal.injectionSuspected > 0 ? <p className="notice" role="alert">{t("recordingInjection")}</p> : null}
              <h4>{t("recordingSteps")}</h4>
              <ol className="steps">
                {current.proposal.steps.map((step) => (
                  <li key={step.id} data-step={step.id} data-path={step.path}>
                    {step.intent}
                    {step.needsReview ? ` — ${t("recordingNeedsReview")}` : ""}
                  </li>
                ))}
              </ol>
              <p className="needs">
                {t("toolNeeds")}: {current.proposal.toolNeeds.join(", ") || t("none")} · {t("capabilityNeeds")}: {current.proposal.capabilityNeeds.join(", ") || t("none")}
              </p>
              {current.proposal.slots.length > 0 ? (
                <fieldset className="recording-slots">
                  <legend>{t("recordingSlots")}</legend>
                  <p className="hint">{t("recordingSlotHelp")}</p>
                  {current.proposal.slots.map((slot) => (
                    <div key={slot.slotId} className="slot" data-slot={slot.slotId} data-decision={decisions[slot.slotId] ?? "undecided"}>
                      <strong>{slot.name}</strong> · {slot.type}
                      <p className="example">{slot.example ?? ""}</p>
                      <label>
                        <input type="radio" name={`slot-${slot.slotId}`} checked={decisions[slot.slotId] === "input"} onChange={() => { decide(slot.slotId, "input"); }} /> {t("recordingInput")}
                      </label>
                      <label>
                        <input
                          type="radio"
                          name={`slot-${slot.slotId}`}
                          disabled={!canBeConstant(slot)}
                          checked={decisions[slot.slotId] === "constant"}
                          onChange={() => { decide(slot.slotId, "constant"); }}
                        />{" "}
                        {t("recordingConstant")}
                      </label>
                    </div>
                  ))}
                </fieldset>
              ) : null}
              <form
                className="recording-submit"
                onSubmit={(event) => {
                  event.preventDefault();
                  void submit(current);
                }}
              >
                <input aria-label={t("recordingSkillName")} placeholder={t("recordingSkillName")} value={name} onChange={(event) => { setName(event.target.value); }} />
                <input aria-label={t("recordingSkillVersion")} placeholder={t("recordingSkillVersion")} value={semver} onChange={(event) => { setSemver(event.target.value); }} />
                <button type="submit" disabled={current.status !== "compiled"}>{t("recordingSubmit")}</button>
              </form>
            </div>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
