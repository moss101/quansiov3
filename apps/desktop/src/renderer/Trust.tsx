/**
 * Trust surface (APP-006): pending approvals with their full preview and warnings, the
 * effect timeline with unsettled effects kept visibly open, and evidence shown only next
 * to a digest the page verified itself.
 *
 * A decision is bound to the preview: the page compares the preview it rendered with the
 * request as last loaded (`bindApproval`) before sending `ApproveEffect` with that digest,
 * and shows the server's `APPROVAL_SUPERSEDED` when the request changed underneath.
 */

import { useCallback, useEffect, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { bindApproval, decodeApprovals, isDecidable, warnings, type ApprovalPreview } from "../trust/approvals.js";
import { decodeEffects, resolution, timeline, unresolved, type EffectView } from "../trust/effects.js";
import { errorState, evidenceText, verifiedState, type EvidenceState } from "../trust/evidence.js";

interface TrustProps {
  readonly workspaceId: string;
  readonly runId?: string | null;
  readonly onInspectRun?: (runId: string) => void;
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: unknown;
  readonly error?: { readonly code: string; readonly message: string; readonly details: unknown };
}

function fromBase64(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

export function Trust({ workspaceId, runId = null, onInspectRun }: TrustProps): ReactElement {
  const [approvals, setApprovals] = useState<readonly ApprovalPreview[]>([]);
  const [effects, setEffects] = useState<readonly EffectView[]>([]);
  /** The preview the user is looking at, frozen when they opened it. */
  const [previewed, setPreviewed] = useState<ApprovalPreview | null>(null);
  const [evidence, setEvidence] = useState<EvidenceState | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [stale, setStale] = useState(true);

  const reload = useCallback(async () => {
    try {
      const scope = runId ? { runId } : {};
      const [approvalsPayload, effectsPayload] = await Promise.all([window.quansio.listApprovals(scope), window.quansio.listEffects(scope)]);
      setApprovals(decodeApprovals(approvalsPayload));
      setEffects(decodeEffects(effectsPayload));
      setStale(false);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, [runId]);

  useEffect(() => {
    if (stale) {
      void reload();
    }
  }, [stale, reload]);

  useEffect(
    () =>
      window.quansio.onStreamFrame((frame) => {
        const record = frame && typeof frame === "object" ? (frame as Record<string, unknown>) : {};
        const event = record["event"] && typeof record["event"] === "object" ? (record["event"] as Record<string, unknown>) : null;
        const type = event && typeof event["type"] === "string" ? event["type"] : "";
        if (type.startsWith("approval.") || type.startsWith("effect.") || type.startsWith("run.") || type.startsWith("evidence.")) {
          setStale(true);
        }
      }),
    [],
  );

  const decide = useCallback(
    async (decision: "approve" | "deny") => {
      if (!previewed) {
        return;
      }
      const current = decodeApprovals(await window.quansio.listApprovals(runId ? { runId } : {})).find((entry) => entry.id === previewed.id);
      if (!current) {
        setNotice(t("approvalGone"));
        setPreviewed(null);
        setStale(true);
        return;
      }
      const binding = bindApproval(previewed, current);
      if (!binding.ok) {
        // The page refuses before anything is sent: the request is not what was previewed.
        setNotice(`${t("approvalRefused")} ${binding.code}: ${binding.reason}`);
        setPreviewed(null);
        setStale(true);
        return;
      }
      const outcome = (decision === "approve"
        ? await window.quansio.approveEffect(binding.params.approval_request_id, binding.params.params_digest)
        : await window.quansio.denyEffect(binding.params.approval_request_id)) as SettledPayload;
      if (!outcome.ok) {
        setNotice(`${t("approvalRefused")} ${outcome.error?.code ?? "INTERNAL"}: ${outcome.error?.message ?? ""}`);
      } else {
        setNotice(decision === "approve" ? t("approved") : t("denied"));
      }
      setPreviewed(null);
      setStale(true);
    },
    [previewed, runId],
  );

  const openEvidence = useCallback(async (evidenceId: string) => {
    setEvidence({ kind: "loading", evidenceId });
    const outcome = (await window.quansio.evidenceContent(evidenceId)) as SettledPayload;
    if (!outcome.ok) {
      setEvidence(errorState(evidenceId, { details: outcome.error?.details, message: outcome.error?.message }));
      return;
    }
    const payload = outcome.result as { bytesBase64: string; digest: string };
    setEvidence(await verifiedState(evidenceId, fromBase64(payload.bytesBase64), payload.digest, evidenceText));
  }, []);

  const pending = approvals.filter((entry) => isDecidable(entry));
  const open = unresolved(effects);

  return (
    <section className="trust" aria-label={t("trustSurface")} data-workspace={workspaceId}>
      <div className="approvals">
        <h2>{t("approvals")} ({String(pending.length)})</h2>
        {notice ? <p className="notice">{notice}</p> : null}
        <ul>
          {approvals.map((entry) => (
            <li key={entry.id} className={`approval approval-${entry.status}`} data-approval={entry.id}>
              <button type="button" onClick={() => { setPreviewed(entry); }}>
                {entry.summary} <span className="status">{entry.status}</span>
              </button>
            </li>
          ))}
        </ul>
        {previewed ? (
          <div className="preview" role="dialog" data-approval={previewed.id} data-digest={previewed.digest}>
            <h3>{previewed.summary}</h3>
            <dl>
              <dt>{t("effect")}</dt>
              <dd>
                {previewed.effect?.effectClass ?? "?"} · {t("tier")} {previewed.effect?.tier ?? "?"}
              </dd>
              <dt>{t("resource")}</dt>
              <dd>{previewed.effect?.resource ? `${previewed.effect.resource.kind} ${previewed.effect.resource.selector}` : t("none")}</dd>
              <dt>{t("target")}</dt>
              <dd>{previewed.effect?.target ? `${previewed.effect.target.kind} ${previewed.effect.target.id ?? ""}` : t("none")}</dd>
              <dt>{t("tool")}</dt>
              <dd>
                {previewed.effect?.toolName ?? t("none")}
                {previewed.effect?.args ? <pre className="args">{JSON.stringify(previewed.effect.args, null, 2)}</pre> : null}
              </dd>
              <dt>{t("consequences")}</dt>
              <dd>
                {previewed.consequences.recipients.length > 0 ? `${t("recipients")}: ${previewed.consequences.recipients.join(", ")}; ` : ""}
                {previewed.consequences.amounts.length > 0 ? `${t("amounts")}: ${previewed.consequences.amounts.map((amount) => `${String(amount.minorUnits / 100)} ${amount.currency}`).join(", ")}; ` : ""}
                {previewed.consequences.targets.length > 0 ? `${t("targets")}: ${previewed.consequences.targets.join(", ")}; ` : ""}
                {previewed.consequences.dataClasses.length > 0 ? `${t("dataClasses")}: ${previewed.consequences.dataClasses.join(", ")}` : ""}
                {previewed.consequences.recipients.length + previewed.consequences.amounts.length + previewed.consequences.targets.length + previewed.consequences.dataClasses.length === 0 ? t("none") : ""}
              </dd>
              <dt>{t("expires")}</dt>
              <dd>{previewed.expiresAt}</dd>
              <dt>{t("digest")}</dt>
              <dd><code>{previewed.digest}</code></dd>
            </dl>
            {warnings(previewed).length > 0 ? (
              <ul className="warnings">
                {warnings(previewed).map((warning) => (
                  <li key={warning} className="warning">{warning}</li>
                ))}
              </ul>
            ) : null}
            {isDecidable(previewed) ? (
              <div className="decision">
                <button type="button" className="approve" onClick={() => void decide("approve")}>{t("approve")}</button>
                <button type="button" className="deny" onClick={() => void decide("deny")}>{t("deny")}</button>
                <button type="button" onClick={() => { setPreviewed(null); }}>{t("close")}</button>
              </div>
            ) : (
              <p className="decided">{t("notDecidable")}: {previewed.status}</p>
            )}
          </div>
        ) : null}
      </div>
      <div className="effects">
        <h2>{t("timeline")}</h2>
        {open.length > 0 ? (
          <div className="unresolved" role="status">
            <h3>{t("unresolved")} ({String(open.length)})</h3>
            <ul>
              {open.map((effect) => (
                <li key={effect.id} className={`effect effect-${resolution(effect)}`} data-effect={effect.id} data-resolution={resolution(effect)}>
                  {effect.effectClass} · {effect.status}
                  {effect.reconciliation ? ` · ${t("reconciliation")} ${effect.reconciliation.strategy} ×${String(effect.reconciliation.attempts)}` : ""}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
        <ol className="entries">
          {timeline(effects).map((entry) => (
            <li key={`${entry.kind}-${entry.id}`} className={`entry entry-${entry.kind}${entry.resolution ? ` entry-${entry.resolution}` : ""}`} data-resolution={entry.resolution ?? undefined}>
              <time>{entry.at}</time> {entry.label}
              {entry.kind === "evidence" ? (
                <button type="button" className="link" onClick={() => void openEvidence(entry.id)}>{t("viewEvidence")}</button>
              ) : null}
              {entry.runId && onInspectRun ? (
                <button type="button" className="link" onClick={() => { onInspectRun(entry.runId ?? ""); }}>{t("run")}</button>
              ) : null}
            </li>
          ))}
        </ol>
        {evidence ? (
          <div className={`evidence evidence-${evidence.kind}`} data-evidence={evidence.evidenceId} data-state={evidence.kind}>
            {evidence.kind === "loading" ? <p>{t("loading")}</p> : null}
            {evidence.kind === "verified" ? (
              <>
                <p><code>{evidence.digest}</code> · {t("verified")}</p>
                {evidence.text !== null ? <pre>{evidence.text}</pre> : <p>{String(evidence.bytes.byteLength)} B</p>}
              </>
            ) : null}
            {evidence.kind === "digest_error" ? (
              <p className="error" role="alert">
                {t("digestMismatch")} ({evidence.source}): {t("expected")} <code>{evidence.expected}</code>, {t("got")} <code>{evidence.actual}</code>
              </p>
            ) : null}
            {evidence.kind === "unavailable" ? <p className="error" role="alert">{t("evidenceUnavailable")}: {evidence.reason}</p> : null}
          </div>
        ) : null}
      </div>
    </section>
  );
}
