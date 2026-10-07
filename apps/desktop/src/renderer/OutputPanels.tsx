/**
 * The output generators' panels in the artifact browser (ADP-006): the quality record a version
 * carries, and a summary of a deck's pages, a transcript or a video timeline.
 *
 * A projection only. Every string from a record or a summary goes into a text node; nothing here
 * renders a page, a fragment or a transcript as markup, and nothing here decides whether a deck may be
 * delivered. Content that came from outside the trust boundary is labelled as such next to its text.
 */

import type { ReactElement } from "react";

import {
  clockTime,
  isUntrusted,
  orderedChecks,
  qualityLabel,
  type OutputSummary,
  type PageState,
  type QualityRecord,
} from "../artifacts/outputs.js";
import { t } from "../i18n/strings.js";

interface QualityPanelProps {
  readonly quality: QualityRecord | null;
  /** What the evidence rows the record names say, when they were loaded. */
  readonly evidenceSummary: (evidenceId: string) => string;
}

export function QualityPanel({ quality, evidenceSummary }: QualityPanelProps): ReactElement {
  if (!quality) {
    return (
      <div className="quality" data-ok="none">
        <h3>{t("qualityRecord")}</h3>
        <p className="empty">{t("noQualityRecord")}</p>
      </div>
    );
  }
  const verdict = quality.ok ? t("qualityOk") : quality.checks.length === 0 ? t("qualityNothing") : t("qualityFailed");
  return (
    <div className="quality" data-ok={String(quality.ok)} data-adapter={quality.adapter}>
      <h3>{t("qualityRecord")}</h3>
      <p className="verdict" title={qualityLabel(quality)}>{verdict}</p>
      <p className="made-by">
        {t("qualityMadeBy")} <code>{quality.adapter}</code> {quality.adapterVersion}
        {Object.entries(quality.environment).map(([key, value]) => (
          <small key={key} className="environment"> · {key}: {value}</small>
        ))}
      </p>
      {isUntrusted(quality) ? <p className="untrusted" role="note">{t("untrustedExternal")}</p> : null}
      <ul className="checks">
        {orderedChecks(quality).map((check, index) => (
          <li key={`${check.rule}-${String(index)}`} data-ok={String(check.ok)}>
            <code>{check.rule}</code>
            {check.subject ? <small> · {check.subject}</small> : null}
            <span className="detail"> {check.detail}</span>
          </li>
        ))}
      </ul>
      {quality.inputs.length > 0 ? (
        <p className="inputs">
          {t("qualityFrom")}{" "}
          {quality.inputs.map((input) => (
            <code key={input.versionId} title={input.contentDigest}>{input.artifactId}</code>
          ))}
        </p>
      ) : null}
      {quality.evidenceIds.length > 0 ? (
        <ul className="quality-evidence">
          {quality.evidenceIds.map((evidenceId) => (
            <li key={evidenceId} data-evidence={evidenceId}>{evidenceSummary(evidenceId)}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

const STATE_LABEL: Readonly<Record<PageState, string>> = {
  ready: "ready",
  pending: "not rendered",
  edited: "edited since render",
  failed_quality: "failed quality",
};

interface OutputSummaryProps {
  readonly summary: OutputSummary | null;
}

export function OutputSummaryPanel({ summary }: OutputSummaryProps): ReactElement | null {
  if (!summary) {
    return null;
  }
  if (summary.kind === "deck_pages") {
    const { slideCount, states, blockers } = summary.summary;
    return (
      <div className="output-summary deck-pages" data-blocked={String(blockers.length > 0)}>
        <h3>{t("deckPages")}</h3>
        <p className="states">
          {(Object.keys(states) as PageState[]).map((state) => (
            <span key={state} className={`state state-${state}`} data-count={String(states[state])}>
              {STATE_LABEL[state]}: {String(states[state])}
            </span>
          ))}
          <small> of {String(slideCount)}</small>
        </p>
        {blockers.length === 0 ? (
          <p className="all-ready">{t("deckAllReady")}</p>
        ) : (
          <>
            <h4>{t("deckBlocked")}</h4>
            <ul className="blockers">
              {blockers.slice(0, 10).map((blocker) => (
                <li key={blocker.index} data-state={blocker.state}>
                  #{String(blocker.index + 1)} · {STATE_LABEL[blocker.state]}
                  {blocker.detail ? <span className="detail"> {blocker.detail}</span> : null}
                </li>
              ))}
            </ul>
          </>
        )}
        <small className="note">{t("deckPagesNote")}</small>
      </div>
    );
  }
  if (summary.kind === "transcript") {
    const { language, durationMs, segmentCount, trust, firstSegments } = summary.summary;
    return (
      <div className="output-summary transcript" data-trust={trust}>
        <h3>{t("transcript")}</h3>
        <p>
          {language} · {clockTime(durationMs)} · {String(segmentCount)} segments
        </p>
        {trust === "UNTRUSTED_EXTERNAL" ? <p className="untrusted" role="note">{t("untrustedExternal")}</p> : null}
        <ol className="segments">
          {firstSegments.map((segment) => (
            <li key={`${String(segment.startMs)}-${segment.text.slice(0, 16)}`}>
              <code>{clockTime(segment.startMs)}</code> {segment.text}
            </li>
          ))}
        </ol>
      </div>
    );
  }
  const timeline = summary.summary;
  return (
    <div className="output-summary timeline" data-revision={String(timeline.revision)}>
      <h3>{t("videoTimeline")}</h3>
      <p>
        revision {String(timeline.revision)} · {String(timeline.width)}×{String(timeline.height)} at {String(timeline.fps)} fps ·{" "}
        {clockTime(timeline.lengthMs)}
      </p>
      <p>
        {String(timeline.mediaCount)} media · {String(timeline.trackCount)} tracks · {String(timeline.clipCount)} clips ·{" "}
        {String(timeline.subtitleCount)} subtitles
      </p>
    </div>
  );
}

/** The deck-pages pointer a presentation shows. */
export function DeckPagesLink({ onOpen }: { readonly onOpen: () => void }): ReactElement {
  return (
    <button type="button" className="link deck-pages-link" onClick={onOpen}>
      {t("openDeckPages")}
    </button>
  );
}
