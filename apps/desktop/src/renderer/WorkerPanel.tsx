import { useCallback, useEffect, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";

interface WorkerView {
  readonly available: boolean;
  readonly reason: string | null;
  readonly state: "stopped" | "starting" | "running" | "exited";
  readonly root: string | null;
  readonly attached: boolean;
  readonly exitCode: number | null;
  readonly lastError: string | null;
}

interface Answer {
  readonly ok: boolean;
  readonly error?: { readonly code: string; readonly message: string };
}

/**
 * The worker on this computer (EXEC-002): where the agent's files, terminal and browser run when it is not on a cloud
 * machine. The person picks the folder in a native dialog; this panel names no path and no program.
 */
export function WorkerPanel({ onChanged }: { readonly onChanged?: () => void }): ReactElement {
  const [view, setView] = useState<WorkerView | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setView((await window.quansio.workerStatus()) as WorkerView);
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const act = useCallback(
    async (kind: "start" | "stop") => {
      setBusy(true);
      setNotice(null);
      try {
        const answer = (kind === "start" ? await window.quansio.startWorker() : await window.quansio.stopWorker()) as Answer;
        if (!answer.ok) {
          setNotice(`${t("refused")} ${answer.error?.code ?? "INTERNAL"}: ${answer.error?.message ?? ""}`);
        }
      } finally {
        setBusy(false);
        await refresh();
        onChanged?.();
      }
    },
    [refresh, onChanged],
  );

  if (!view) {
    return <section className="worker-panel" aria-label={t("workerHeading")} />;
  }
  const running = view.state === "running" || view.state === "starting";
  return (
    <section className="worker-panel" aria-label={t("workerHeading")}>
      <h3>{t("workerHeading")}</h3>
      {!view.available ? (
        <p className="stage worker-unavailable">{view.reason ?? t("workerUnavailable")}</p>
      ) : (
        <>
          <p className={`status worker-state worker-state-${view.state}`} role="status">
            {view.state === "running" ? (view.attached ? t("workerRunning") : t("workerNotAttached")) : view.state === "starting" ? t("workerStarting") : view.state === "exited" ? t("workerExited") : t("workerStopped")}
          </p>
          {view.root ? <p className="stage worker-root">{view.root}</p> : null}
          {view.state === "exited" && view.lastError ? <pre className="worker-error">{view.lastError}</pre> : null}
          {running ? (
            <button type="button" className="worker-stop" disabled={busy} onClick={() => void act("stop")}>
              {t("workerStop")}
            </button>
          ) : (
            <button type="button" className="worker-start" disabled={busy} onClick={() => void act("start")}>
              {t("workerStart")}
            </button>
          )}
        </>
      )}
      {notice ? <p className="notice">{notice}</p> : null}
    </section>
  );
}
