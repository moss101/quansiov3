/**
 * Knowledge, memory, skills and capability resolution (APP-011).
 *
 * Knowledge and semantic memory are two sections, each entry with its provenance, scope,
 * confidence and lifecycle state, and the controls the ladder allows — forget, or
 * supersede with an existing successor. Memory is labelled as what it is and is never
 * recovery: the decoder refuses a row that carries a recovery field and the page says so.
 * Skills show every version on the §11.5 ladder with only the next legal rung offered;
 * "Activate" is offered to an APPROVED version and refused for anything else before the
 * command is sent, and the server refuses it again. A run's capability resolution lays
 * out the ten layers in order with the inputs it had and the grants that resulted; the
 * active skills are visibly an input that contributes no grant.
 */

import { useCallback, useEffect, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { decodeCapabilityResolution, layersInOrder, skillInputs, type CapabilityResolution } from "../knowledge/capabilities.js";
import {
  KNOWLEDGE_SUPERSEDE_FROM,
  decodeKnowledge,
  decodeMemory,
  deleteKnowledge,
  deleteMemory,
  describeProvenance,
  successorsFor,
  supersedeKnowledge,
  type KnowledgeView,
  type MemoryView,
} from "../knowledge/entries.js";
import { activation, decodeSkills, disable, install, promotion, type SkillStatus, type SkillView } from "../knowledge/skills.js";

interface KnowledgeProps {
  readonly workspaceId: string;
  readonly runId?: string | null;
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: unknown;
  readonly error?: { readonly code: string; readonly message: string };
}

function refusal(settled: SettledPayload): string | null {
  return settled.ok ? null : `${t("refused")} ${settled.error?.code ?? "INTERNAL"}: ${settled.error?.message ?? ""}`;
}

export function Knowledge({ workspaceId, runId = null }: KnowledgeProps): ReactElement {
  const [knowledge, setKnowledge] = useState<readonly KnowledgeView[]>([]);
  const [memory, setMemory] = useState<readonly MemoryView[]>([]);
  const [refusedMemory, setRefusedMemory] = useState(0);
  const [skills, setSkills] = useState<readonly SkillView[]>([]);
  const [resolution, setResolution] = useState<CapabilityResolution | null>(null);
  const [showDeleted, setShowDeleted] = useState(false);
  const [superseding, setSuperseding] = useState<KnowledgeView | null>(null);
  const [draft, setDraft] = useState({ name: "", semver: "1.0.0", instructions: "" });
  const [notice, setNotice] = useState<string | null>(null);
  const [stale, setStale] = useState(true);

  const reload = useCallback(async () => {
    try {
      const [knowledgePayload, memoryPayload, skillsPayload] = await Promise.all([
        window.quansio.listKnowledge({ includeDeleted: showDeleted }),
        window.quansio.listMemory({ includeDeleted: showDeleted }),
        window.quansio.listSkills(),
      ]);
      setKnowledge(decodeKnowledge(knowledgePayload));
      const decoded = decodeMemory(memoryPayload);
      setMemory(decoded.entries);
      setRefusedMemory(decoded.refused.length);
      setSkills(decodeSkills(skillsPayload));
      setResolution(runId ? decodeCapabilityResolution(await window.quansio.runCapabilities(runId)) : null);
      setStale(false);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, [runId, showDeleted]);

  useEffect(() => {
    if (stale) {
      void reload();
    }
  }, [stale, reload]);

  useEffect(() => {
    setStale(true);
  }, [showDeleted, runId]);

  useEffect(
    () =>
      window.quansio.onStreamFrame((frame) => {
        const record = frame && typeof frame === "object" ? (frame as Record<string, unknown>) : {};
        const event = record["event"] && typeof record["event"] === "object" ? (record["event"] as Record<string, unknown>) : null;
        const type = event && typeof event["type"] === "string" ? event["type"] : "";
        if (type.startsWith("knowledge.") || type.startsWith("memory.") || type.startsWith("skill.") || type.startsWith("capability.")) {
          setStale(true);
        }
      }),
    [],
  );

  const forgetKnowledge = useCallback(async (entry: KnowledgeView) => {
    const control = deleteKnowledge(entry);
    if (!control.ok) {
      setNotice(`${t("refused")} ${control.code}: ${control.reason}`);
      return;
    }
    const settled = (await window.quansio.deleteKnowledge(entry.id)) as SettledPayload;
    setNotice(refusal(settled) ?? t("forgotten"));
    setStale(true);
  }, []);

  const supersede = useCallback(async (entry: KnowledgeView, successor: KnowledgeView) => {
    const control = supersedeKnowledge(entry, successor);
    if (!control.ok) {
      setNotice(`${t("refused")} ${control.code}: ${control.reason}`);
      return;
    }
    const settled = (await window.quansio.supersedeKnowledge(entry.id, successor.id)) as SettledPayload;
    setNotice(refusal(settled) ?? t("superseded"));
    setSuperseding(null);
    setStale(true);
  }, []);

  const forgetMemory = useCallback(async (entry: MemoryView) => {
    const control = deleteMemory(entry);
    if (!control.ok) {
      setNotice(`${t("refused")} ${control.code}: ${control.reason}`);
      return;
    }
    const settled = (await window.quansio.deleteMemory(entry.id)) as SettledPayload;
    setNotice(refusal(settled) ?? t("forgotten"));
    setStale(true);
  }, []);

  const promote = useCallback(async (skill: SkillView, versionId: string, to: SkillStatus) => {
    const version = skill.versions.find((candidate) => candidate.id === versionId);
    if (!version) {
      return;
    }
    const edge = to === "active" ? activation(version) : promotion(version, to);
    if (!edge.ok) {
      setNotice(`${t("refused")} ${edge.code}: ${edge.reason}`);
      return;
    }
    const settled = (await window.quansio.promoteSkillVersion(edge.params.skill_version_id, edge.params.to)) as SettledPayload;
    setNotice(refusal(settled));
    setStale(true);
  }, []);

  const disableSkill = useCallback(async (skill: SkillView) => {
    const control = disable(skill);
    if (!control.ok) {
      setNotice(`${t("refused")} ${control.code}: ${control.reason}`);
      return;
    }
    const settled = (await window.quansio.disableSkill(skill.id)) as SettledPayload;
    setNotice(refusal(settled));
    setStale(true);
  }, []);

  const installDraft = useCallback(async () => {
    const control = install({ scope: "workspace", name: draft.name, semver: draft.semver, instructions: draft.instructions });
    if (!control.ok) {
      setNotice(`${t("refused")} ${control.code}: ${control.reason}`);
      return;
    }
    const settled = (await window.quansio.installSkill(control.params)) as SettledPayload;
    setNotice(refusal(settled));
    setDraft({ name: "", semver: "1.0.0", instructions: "" });
    setStale(true);
  }, [draft]);

  return (
    <section className="knowledge" aria-label={t("knowledgeSurface")} data-workspace={workspaceId}>
      {notice ? <p className="notice" role="status">{notice}</p> : null}
      <div className="entries">
        <h2>{t("knowledge")}</h2>
        <label>
          <input type="checkbox" checked={showDeleted} onChange={(event) => { setShowDeleted(event.target.checked); }} /> {t("showDeleted")}
        </label>
        <ul>
          {knowledge.map((entry) => (
            <li key={entry.id} className={`entry entry-${entry.status}`} data-entry={entry.id} data-status={entry.status} data-retrievable={entry.retrievable ? "true" : "false"}>
              <strong>{entry.kind}</strong> · {entry.scope} · v{String(entry.version)} · {t("confidence")} {String(entry.confidence)}
              <span className={`status status-${entry.status}`}>{entry.status}{entry.status === "deleted" || entry.status === "superseded" ? ` (${t("tombstone")})` : ""}</span>
              <span className="retrieval">{entry.retrievable ? t("retrievableYes") : t("retrievableNo")}</span>
              <pre className="content">{entry.contentRef ?? JSON.stringify(entry.content)}</pre>
              <p className="provenance">{t("provenance")}: {describeProvenance(entry)}</p>
              {entry.supersededBy ? <p className="successor">{t("supersededBy")} <code>{entry.supersededBy}</code></p> : null}
              {deleteKnowledge(entry).ok ? <button type="button" className="forget" onClick={() => void forgetKnowledge(entry)}>{t("forget")}</button> : null}
              {KNOWLEDGE_SUPERSEDE_FROM.includes(entry.status) ? <button type="button" className="supersede" onClick={() => { setSuperseding(entry); }}>{t("supersede")}</button> : null}
              {superseding?.id === entry.id ? (
                <ul className="successors" role="listbox">
                  {successorsFor(entry, knowledge).map((successor) => (
                    <li key={successor.id}>
                      <button type="button" onClick={() => void supersede(entry, successor)}>{successor.id} · {successor.status}</button>
                    </li>
                  ))}
                </ul>
              ) : null}
            </li>
          ))}
        </ul>
      </div>
      <div className="memory">
        <h2>{t("memory")}</h2>
        <p className="hint">{t("memoryNotRecovery")}</p>
        {refusedMemory > 0 ? <p className="error" role="alert">{String(refusedMemory)} {t("memoryRefused")}</p> : null}
        <ul>
          {memory.map((entry) => (
            <li key={entry.id} className={`entry entry-${entry.status}`} data-entry={entry.id} data-status={entry.status} data-retrievable={entry.retrievable ? "true" : "false"}>
              <span className="scope">{t("scope")}: {entry.scope} · {entry.subjectRef}</span>
              <span className={`status status-${entry.status}`}>{entry.status}</span>
              <span className="retrieval">{entry.retrievable ? t("retrievableYes") : t("retrievableNo")}</span>
              <p className="content">{entry.content}</p>
              <p className="provenance">{t("provenance")}: {entry.provenance.kind}{entry.provenance.ref ? ` (${entry.provenance.ref})` : ""} · {t("confidence")} {String(entry.confidence)}</p>
              {deleteMemory(entry).ok ? <button type="button" className="forget" onClick={() => void forgetMemory(entry)}>{t("forget")}</button> : null}
            </li>
          ))}
        </ul>
      </div>
      <div className="skills">
        <h2>{t("skills")}</h2>
        <ul>
          {skills.map((skill) => (
            <li key={skill.id} className="skill" data-skill={skill.id} data-active-version={skill.currentActiveVersionId ?? undefined}>
              <strong>{skill.name}</strong> · {skill.scope} · {skill.owner}
              {disable(skill).ok ? <button type="button" className="disable" onClick={() => void disableSkill(skill)}>{t("disable")}</button> : null}
              <ol className="versions">
                {skill.versions.map((version) => (
                  <li key={version.id} className={`version version-${version.status}`} data-version={version.id} data-status={version.status} data-resolves={version.resolves ? "true" : "false"}>
                    {version.semver} · {version.status} · {version.resolves ? t("resolves") : t("notResolving")}
                    <p className="needs">
                      {t("toolNeeds")}: {version.manifest.toolNeeds.join(", ") || t("none")} · {t("capabilityNeeds")}: {version.manifest.capabilityNeeds.map((need) => need.effectClass).join(", ") || t("none")}
                    </p>
                    {version.promotableTo.map((to) => (
                      <button
                        key={to}
                        type="button"
                        className={to === "active" ? "activate" : "promote"}
                        disabled={to === "active" && !activation(version).ok}
                        onClick={() => void promote(skill, version.id, to)}
                      >
                        {to === "active" ? t("activate") : `${t("promoteTo")} ${to}`}
                      </button>
                    ))}
                  </li>
                ))}
              </ol>
            </li>
          ))}
        </ul>
        <form
          className="install"
          onSubmit={(event) => {
            event.preventDefault();
            void installDraft();
          }}
        >
          <h3>{t("installSkill")}</h3>
          <input aria-label={t("skillName")} placeholder={t("skillName")} value={draft.name} onChange={(event) => { setDraft({ ...draft, name: event.target.value }); }} />
          <input aria-label={t("skillVersion")} placeholder={t("skillVersion")} value={draft.semver} onChange={(event) => { setDraft({ ...draft, semver: event.target.value }); }} />
          <textarea aria-label={t("instructions")} placeholder={t("instructions")} value={draft.instructions} onChange={(event) => { setDraft({ ...draft, instructions: event.target.value }); }} />
          <button type="submit">{t("installSkill")}</button>
        </form>
      </div>
      <div className="capabilities">
        <h2>{t("capabilities")}</h2>
        {!runId ? <p className="hint">{t("runCapabilityHint")}</p> : null}
        {runId && !resolution ? <p className="hint">{t("noProjection")}</p> : null}
        {resolution ? (
          <div className="resolution" data-projection={resolution.id} data-digest={resolution.inputsDigest}>
            <table>
              <thead>
                <tr>
                  <th>{t("layer")}</th>
                  <th>{t("input")}</th>
                </tr>
              </thead>
              <tbody>
                {layersInOrder(resolution).map((row) => (
                  <tr key={row.layer} className={row.input ? "present" : "absent"} data-layer={row.layer}>
                    <td>{row.layer}</td>
                    <td>{row.input ? <code>{row.input.ref}</code> : t("absent")}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {skillInputs(resolution).length > 0 ? <p className="skill-inputs">{t("skills")}: {skillInputs(resolution).join(", ")} — {t("capabilityNeeds")}</p> : null}
            <h3>{t("grants")}</h3>
            <ul className="grants">
              {resolution.grants.map((grant, index) => (
                <li key={`${grant.effectClass}-${String(index)}`} data-grant={grant.effectClass}>
                  {grant.effectClass} · {grant.resource ? `${grant.resource.kind} ${grant.resource.selector}` : t("none")}
                </li>
              ))}
            </ul>
          </div>
        ) : null}
      </div>
    </section>
  );
}
