/**
 * The always-visible "recording" indicator (ADP-003, DOMAIN.md §11.7 F).
 *
 * It sits at the top of every page while the person's recorder is starting, recording or saving, and follows
 * the recorder process itself, so it cannot show "not recording" while something is watching. It carries the
 * two controls that end a recording, both the person's own clicks: Finish (the click's `isTrusted` is what
 * allows it) and Cancel. Nothing in the page, a message or a model output can press them.
 */

import { useCallback, useEffect, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { IDLE_RECORDER, canFinish, decodeRecorder, finishRequest, isCapturing, type RecorderView } from "../recording/state.js";

/** How often the recorder's counts are read. */
export const INDICATOR_POLL_MS = 1000;

interface Props {
  /** Called after a recording ended (finished or cancelled), so a list on the page re-reads. */
  readonly onEnded?: () => void;
}

export function RecordingIndicator({ onEnded }: Props): ReactElement | null {
  const [recorder, setRecorder] = useState<RecorderView>(IDLE_RECORDER);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    // Each read waits for the one before it, so a slow main never stacks requests.
    const read = (): void => {
      void window.quansio
        .recordingStatus()
        .then((raw) => {
          if (alive) {
            setRecorder(decodeRecorder(raw));
          }
        })
        .catch(() => undefined)
        .finally(() => {
          if (alive) {
            timer = setTimeout(read, INDICATOR_POLL_MS);
          }
        });
    };
    read();
    return () => {
      alive = false;
      if (timer) {
        clearTimeout(timer);
      }
    };
  }, []);

  const finish = useCallback(
    async (event: { readonly isTrusted: boolean }) => {
      const refused = await window.quansio.finishRecording(finishRequest(event));
      const outcome = refused && typeof refused === "object" ? (refused as { ok?: unknown; code?: unknown; message?: unknown }) : {};
      setNotice(outcome.ok === true ? null : `${typeof outcome.code === "string" ? outcome.code : "INTERNAL"}: ${typeof outcome.message === "string" ? outcome.message : ""}`);
      setRecorder(decodeRecorder(await window.quansio.recordingStatus()));
      onEnded?.();
    },
    [onEnded],
  );

  const cancel = useCallback(async () => {
    await window.quansio.cancelRecording();
    setNotice(null);
    setRecorder(decodeRecorder(await window.quansio.recordingStatus()));
    onEnded?.();
  }, [onEnded]);

  if (!isCapturing(recorder)) {
    return recorder.notice ? (
      <p className="recording-notice" role="status" data-notice={recorder.notice}>
        {recorder.notice === "account_changed" ? t("recordingAccountChanged") : t("recordingRecorderStopped")}
      </p>
    ) : null;
  }
  const label =
    recorder.state === "starting" ? t("recordingStarting") : recorder.state === "finishing" ? t("recordingFinishing") : t("recordingIndicatorHint");
  return (
    <div className="recording-indicator" role="status" aria-live="polite" data-recording={recorder.state} data-recording-id={recorder.recordingId ?? undefined}>
      <span className="recording-dot" aria-hidden="true" />
      <strong>{t("recordingIndicator")}</strong>
      <span className="recording-detail">
        {label} · {String(recorder.actions)} {t("recordingActions")} · {String(recorder.screenshots)} {t("recordingScreenshots")}
        {recorder.paused ? ` · ${t("recordingPaused")}` : ""}
      </span>
      {canFinish(recorder) || recorder.state === "finishing" ? (
        <button
          type="button"
          className="recording-finish"
          onClick={(event) => {
            void finish(event.nativeEvent);
          }}
        >
          {t("recordingFinish")}
        </button>
      ) : null}
      <button type="button" className="recording-cancel ghost" onClick={() => { void cancel(); }}>
        {t("recordingCancel")}
      </button>
      {notice ? <span className="recording-error" role="alert">{notice}</span> : null}
    </div>
  );
}
