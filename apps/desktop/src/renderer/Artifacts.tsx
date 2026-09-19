/**
 * Artifact workspace surface (APP-008): browser with kind filter, version history, safe
 * preview (text node / in-process raster image / download only), save and open through
 * main, diff between text versions, source and evidence links, and global search.
 *
 * Nothing here renders artifact bytes as markup: text goes into a `<pre>` text node and
 * images into an `<img>` with a `data:` URL the CSP allows; HTML/SVG/JS arrive from main
 * with a text plan and stay text.
 */

import { useCallback, useEffect, useReducer, useState, type ReactElement } from "react";

import { imageDataUrl, isTextLike, openAllowed, textPreview, type PreviewPlan } from "../artifacts/preview.js";
import { decodeSearch, type SearchChannel, type SearchResults } from "../artifacts/search.js";
import {
  ARTIFACT_KINDS,
  artifactWorkspaceReducer,
  comparedVersion,
  decodeArtifacts,
  decodeEvidence,
  decodeHistory,
  diffApplicable,
  diffLines,
  initialArtifactWorkspace,
  selectedVersion,
  type ArtifactKind,
  type ArtifactVersion,
  type LoadedPreview,
} from "../artifacts/workspace.js";
import { t } from "../i18n/strings.js";

interface ArtifactsProps {
  readonly workspaceId: string;
  /** Jump to a run's timeline (the conversation surface owns it). */
  readonly onInspectRun?: (runId: string) => void;
}

interface ContentPayload {
  readonly bytesBase64: string;
  readonly mediaType: string;
  readonly sizeBytes: number;
  readonly plan: PreviewPlan;
}

function fromBase64(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : t("commandFailed");
}

/** Turn main's content payload into what the page shows. Text is a string; images a data URL. */
export function previewFromContent(versionId: string, content: ContentPayload): LoadedPreview {
  if (content.plan.mode === "download-only" || !content.bytesBase64) {
    return { versionId, plan: content.plan, text: null, imageUrl: null, truncated: false };
  }
  const bytes = fromBase64(content.bytesBase64);
  if (content.plan.mode === "image") {
    return { versionId, plan: content.plan, text: null, imageUrl: imageDataUrl(bytes, content.mediaType), truncated: false };
  }
  const { text, truncated } = textPreview(bytes);
  return { versionId, plan: content.plan, text, imageUrl: null, truncated };
}

export function Artifacts({ workspaceId, onInspectRun }: ArtifactsProps): ReactElement {
  const [state, dispatch] = useReducer(artifactWorkspaceReducer, initialArtifactWorkspace(workspaceId));
  const [query, setQuery] = useState("");
  const [channels, setChannels] = useState<readonly SearchChannel[]>(["artifacts", "threads", "knowledge", "evidence"]);
  const [results, setResults] = useState<SearchResults | null>(null);
  const [compareText, setCompareText] = useState<{ versionId: string; text: string } | null>(null);

  useEffect(() => {
    if (!state.stale.list) {
      return;
    }
    let cancelled = false;
    void window.quansio
      .listArtifacts({ kind: state.filter.kind, runId: state.filter.runId })
      .then((payload) => {
        if (!cancelled) {
          dispatch({ type: "list.loaded", artifacts: decodeArtifacts(payload) });
        }
      })
      .catch((error: unknown) => {
        dispatch({ type: "notice", notice: errorText(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [state.stale.list, state.filter.kind, state.filter.runId]);

  // An artifact event (a new version, an upload from the composer) refreshes the list.
  useEffect(
    () =>
      window.quansio.onStreamFrame((frame) => {
        const record = frame && typeof frame === "object" ? (frame as Record<string, unknown>) : {};
        const event = record["event"] && typeof record["event"] === "object" ? (record["event"] as Record<string, unknown>) : null;
        const type = event && typeof event["type"] === "string" ? event["type"] : "";
        if (type.startsWith("artifact.")) {
          const aggregate = typeof event?.["aggregate_id"] === "string" ? event["aggregate_id"] : "";
          dispatch({ type: "artifact.changed", artifactId: aggregate });
        }
      }),
    [],
  );

  useEffect(() => {
    const artifactId = state.selectedId;
    if (!artifactId || !state.stale.history) {
      return;
    }
    let cancelled = false;
    void window.quansio
      .artifactVersions(artifactId)
      .then((payload) => {
        if (!cancelled) {
          dispatch({ type: "history.loaded", history: decodeHistory(payload) });
        }
      })
      .catch((error: unknown) => {
        dispatch({ type: "notice", notice: errorText(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [state.selectedId, state.stale.history]);

  const version = selectedVersion(state);
  const other = comparedVersion(state);

  useEffect(() => {
    const artifactId = state.selectedId;
    if (!artifactId || !version || state.preview?.versionId === version.id) {
      return;
    }
    let cancelled = false;
    void window.quansio
      .artifactContent(artifactId, version.id)
      .then((payload) => {
        if (!cancelled) {
          dispatch({ type: "preview.loaded", preview: previewFromContent(version.id, payload as ContentPayload) });
        }
      })
      .catch((error: unknown) => {
        dispatch({ type: "notice", notice: errorText(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [state.selectedId, version, state.preview?.versionId]);

  // The diff needs the other side's text; fetched only when both sides are text.
  useEffect(() => {
    const artifactId = state.selectedId;
    if (!artifactId || !version || !other || !diffApplicable(version, other, isTextLike) || compareText?.versionId === other.id) {
      return;
    }
    let cancelled = false;
    void window.quansio
      .artifactContent(artifactId, other.id)
      .then((payload) => {
        const content = payload as ContentPayload;
        if (!cancelled && content.plan.mode === "text" && content.bytesBase64) {
          setCompareText({ versionId: other.id, text: textPreview(fromBase64(content.bytesBase64)).text });
        }
      })
      .catch((error: unknown) => {
        dispatch({ type: "notice", notice: errorText(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [state.selectedId, version, other, compareText?.versionId]);

  useEffect(() => {
    if (!version) {
      return;
    }
    for (const evidenceId of version.evidenceIds) {
      if (evidenceId in state.evidence) {
        continue;
      }
      void window.quansio
        .getEvidence(evidenceId)
        .then((payload) => {
          dispatch({ type: "evidence.loaded", evidence: decodeEvidence(payload) });
        })
        .catch((error: unknown) => {
          dispatch({ type: "notice", notice: errorText(error) });
        });
    }
  }, [version, state.evidence]);

  const runSearch = useCallback(async () => {
    try {
      setResults(decodeSearch(await window.quansio.search({ q: query, channels })));
    } catch (error) {
      dispatch({ type: "notice", notice: errorText(error) });
    }
  }, [query, channels]);

  const save = useCallback(async () => {
    if (!state.selectedId || !version) {
      return;
    }
    try {
      const saved = (await window.quansio.saveArtifact(state.selectedId, version.id)) as { path: string | null };
      dispatch({ type: "notice", notice: saved.path ? `${t("saved")} ${saved.path}` : t("saveCancelled") });
    } catch (error) {
      dispatch({ type: "notice", notice: errorText(error) });
    }
  }, [state.selectedId, version]);

  const open = useCallback(async () => {
    if (!state.selectedId || !version) {
      return;
    }
    try {
      await window.quansio.openArtifact(state.selectedId, version.id);
    } catch (error) {
      dispatch({ type: "notice", notice: errorText(error) });
    }
  }, [state.selectedId, version]);

  const diff = version && other && state.preview?.text !== null && state.preview?.text !== undefined && compareText?.versionId === other.id
    ? diffLines(compareText.text, state.preview.text)
    : null;

  return (
    <section className="artifacts" aria-label={t("artifacts")}>
      <aside className="browser">
        <h2>{t("artifacts")}</h2>
        <form
          className="search"
          onSubmit={(event) => {
            event.preventDefault();
            void runSearch();
          }}
        >
          <input type="search" value={query} placeholder={t("searchPlaceholder")} onChange={(event) => { setQuery(event.target.value); }} />
          <div className="channels">
            {(["artifacts", "threads", "knowledge", "evidence"] as const).map((channel) => (
              <label key={channel}>
                <input
                  type="checkbox"
                  checked={channels.includes(channel)}
                  onChange={() => {
                    setChannels(channels.includes(channel) ? channels.filter((item) => item !== channel) : [...channels, channel]);
                  }}
                />
                {channel}
              </label>
            ))}
          </div>
          <button type="submit" disabled={!query.trim() || channels.length === 0}>{t("search")}</button>
        </form>
        {results ? (
          <div className="results" data-truncated={results.truncated ? "true" : "false"}>
            <h3>{t("results")} ({String(results.hits.length)}{results.truncated ? "+" : ""})</h3>
            <ul>
              {results.hits.map((hit) => (
                <li key={`${hit.channel}:${hit.id}`} className={`hit hit-${hit.channel}`}>
                  <span className="channel">{hit.channel}</span>
                  {hit.channel === "artifacts" ? (
                    <button type="button" onClick={() => { dispatch({ type: "select", artifactId: hit.id }); }}>{hit.title}</button>
                  ) : hit.ref["run_id"] && onInspectRun ? (
                    <button type="button" onClick={() => { onInspectRun(hit.ref["run_id"] ?? ""); }}>{hit.title}</button>
                  ) : (
                    <span>{hit.title}</span>
                  )}
                  <p className="snippet">{hit.snippet}</p>
                  <small className="provenance">{hit.provenance.locator}</small>
                </li>
              ))}
            </ul>
            <button type="button" onClick={() => { setResults(null); }}>{t("clear")}</button>
          </div>
        ) : null}
        <label>
          {t("kind")}
          <select
            value={state.filter.kind ?? ""}
            onChange={(event) => {
              dispatch({ type: "filter", filter: { kind: (event.target.value || null) as ArtifactKind | null } });
            }}
          >
            <option value="">{t("allKinds")}</option>
            {ARTIFACT_KINDS.map((kind) => (
              <option key={kind} value={kind}>{kind}</option>
            ))}
          </select>
        </label>
        <ul className="artifact-list">
          {state.artifacts.map((artifact) => (
            <li key={artifact.id} className={artifact.id === state.selectedId ? "selected" : undefined}>
              <button type="button" onClick={() => { dispatch({ type: "select", artifactId: artifact.id }); }}>
                <span className="kind">{artifact.kind}</span> {artifact.title}
                <small> v{String(artifact.versionCount)}</small>
              </button>
            </li>
          ))}
          {state.artifacts.length === 0 ? <li className="empty">{t("noArtifacts")}</li> : null}
        </ul>
      </aside>
      <div className="detail">
        {state.notice ? <p className="notice">{state.notice}</p> : null}
        {state.history && version ? (
          <>
            <header>
              <h2>{state.history.title}</h2>
              <span className="kind">{state.history.kind}</span>
              <span className="origin">{t("source")}: {state.history.origin.kind} {state.history.origin.ref}</span>
            </header>
            <div className="versions">
              <h3>{t("versions")}</h3>
              <ol>
                {state.history.versions.map((candidate: ArtifactVersion) => (
                  <li key={candidate.id} className={candidate.id === version.id ? "selected" : undefined} data-version={candidate.id}>
                    <button type="button" onClick={() => { dispatch({ type: "version.select", versionId: candidate.id }); }}>
                      v{String(candidate.seq)} · {candidate.mediaType} · {String(candidate.sizeBytes)} B
                    </button>
                    {candidate.producedBy.runId ? (
                      <button type="button" className="link" onClick={() => onInspectRun?.(candidate.producedBy.runId ?? "")}>
                        {t("run")} {candidate.producedBy.runId}
                      </button>
                    ) : candidate.producedBy.userId ? (
                      <small>{t("uploadedBy")} {candidate.producedBy.userId}</small>
                    ) : null}
                    {candidate.id !== version.id ? (
                      <button type="button" className="link" onClick={() => { dispatch({ type: "version.compare", versionId: candidate.id }); }}>
                        {t("compare")}
                      </button>
                    ) : null}
                  </li>
                ))}
              </ol>
            </div>
            <div className="actions">
              <button type="button" onClick={() => void save()}>{t("save")}</button>
              {openAllowed(version.mediaType) ? <button type="button" onClick={() => void open()}>{t("openWithOs")}</button> : null}
              <code className="digest">{version.contentDigest}</code>
            </div>
            {version.evidenceIds.length > 0 ? (
              <div className="evidence">
                <h3>{t("evidence")}</h3>
                <ul>
                  {version.evidenceIds.map((evidenceId) => {
                    const link = state.evidence[evidenceId];
                    return (
                      <li key={evidenceId} data-evidence={evidenceId}>
                        {link ? `${link.kind}: ${link.summary ?? evidenceId}` : evidenceId}
                        {link?.runId && onInspectRun ? (
                          <button type="button" className="link" onClick={() => { onInspectRun(link.runId ?? ""); }}>{t("run")}</button>
                        ) : null}
                      </li>
                    );
                  })}
                </ul>
              </div>
            ) : null}
            <div className="preview" data-mode={state.preview?.plan.mode ?? "loading"}>
              {state.preview ? (
                state.preview.plan.mode === "image" && state.preview.imageUrl ? (
                  <img alt={state.history.title} src={state.preview.imageUrl} />
                ) : state.preview.plan.mode === "text" && state.preview.text !== null ? (
                  diff && other ? (
                    <div className="diff" data-compare={other.id}>
                      <h3>{t("diff")} v{String(other.seq)} → v{String(version.seq)}</h3>
                      <pre>
                        {diff.map((line, index) => (
                          <span key={`${String(index)}-${line.kind}`} className={`line line-${line.kind}`}>
                            {line.kind === "added" ? "+ " : line.kind === "removed" ? "- " : "  "}
                            {line.text}
                            {"\n"}
                          </span>
                        ))}
                      </pre>
                      <button type="button" onClick={() => { dispatch({ type: "version.compare", versionId: null }); setCompareText(null); }}>
                        {t("clear")}
                      </button>
                    </div>
                  ) : (
                    <pre className="text">{state.preview.text}</pre>
                  )
                ) : (
                  <p className="download-only">{t("downloadOnly")}: {state.preview.plan.reason}</p>
                )
              ) : (
                <p>{t("loading")}</p>
              )}
            </div>
          </>
        ) : (
          <p>{t("pickArtifact")}</p>
        )}
      </div>
    </section>
  );
}
