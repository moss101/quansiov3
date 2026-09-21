/**
 * Teammate roster (APP-014): create, edit and archive persistent teammates; see each
 * one's thread, active run, attention state and effective capability.
 *
 * Authority is the server's: the card shows the effective capability the roster
 * projection computed (the workspace policy narrowed by the definition's needs) and
 * explains the narrowing; persona and instructions are shown as text that never grants.
 * Every command is validated on the page first and decided by the server again; an
 * archive is offered as what it is — a suspended thread, nothing deleted.
 */

import { useCallback, useEffect, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import {
  AVAILABILITIES,
  MEMORY_SCOPES,
  archiveTeammate,
  createTeammate,
  decodeRoster,
  narrowing,
  needsAttention,
  updateTeammate,
  visibleRoster,
  type Availability,
  type MemoryScope,
  type Roster,
  type TeammateView,
} from "../teammates/roster.js";

interface TeammatesProps {
  readonly workspaceId: string;
  readonly onInspectRun?: (runId: string) => void;
}

interface SettledPayload {
  readonly ok: boolean;
  readonly result?: unknown;
  readonly error?: { readonly code: string; readonly message: string };
}

interface Draft {
  readonly name: string;
  readonly persona: string;
  readonly standingInstructions: string;
  readonly needs: string;
  readonly memoryScope: MemoryScope;
  readonly modelRoutePreference: string;
  readonly availability: Availability;
}

const EMPTY_DRAFT: Draft = { name: "", persona: "", standingInstructions: "", needs: "", memoryScope: "workspace", modelRoutePreference: "", availability: "available" };

function draftOf(teammate: TeammateView): Draft {
  return {
    name: teammate.name,
    persona: teammate.persona,
    standingInstructions: teammate.standingInstructions,
    needs: teammate.defaultCapabilityNeeds.join(", "),
    memoryScope: teammate.memoryScope,
    modelRoutePreference: teammate.modelRoutePreference ?? "",
    availability: teammate.availability,
  };
}

function needsOf(draft: Draft): string[] {
  return draft.needs.split(",").map((need) => need.trim()).filter((need) => need.length > 0);
}

function refusal(settled: SettledPayload): string | null {
  return settled.ok ? null : `${t("refused")} ${settled.error?.code ?? "INTERNAL"}: ${settled.error?.message ?? ""}`;
}

export function Teammates({ workspaceId, onInspectRun }: TeammatesProps): ReactElement {
  const [roster, setRoster] = useState<Roster | null>(null);
  const [editing, setEditing] = useState<TeammateView | null>(null);
  const [creating, setCreating] = useState(false);
  const [templateId, setTemplateId] = useState<string>("");
  const [draft, setDraft] = useState<Draft>(EMPTY_DRAFT);
  const [notice, setNotice] = useState<string | null>(null);
  const [stale, setStale] = useState(true);

  const reload = useCallback(async () => {
    try {
      setRoster(decodeRoster(await window.quansio.listTeammates()));
      setStale(false);
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t("commandFailed"));
    }
  }, []);

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
        if (type.startsWith("agent.") || type.startsWith("run.") || type.startsWith("question.") || type.startsWith("approval.")) {
          setStale(true);
        }
      }),
    [],
  );

  const create = useCallback(async () => {
    const prepared = createTeammate({
      workspaceId,
      templateId: templateId || null,
      ...(draft.name ? { name: draft.name } : {}),
      ...(draft.persona ? { persona: draft.persona } : {}),
      ...(draft.standingInstructions ? { standingInstructions: draft.standingInstructions } : {}),
      ...(draft.needs ? { defaultCapabilityNeeds: needsOf(draft) } : {}),
      memoryScope: draft.memoryScope,
      modelRoutePreference: draft.modelRoutePreference || null,
      availability: draft.availability,
    });
    if (!prepared.ok) {
      setNotice(`${t("refused")} ${prepared.code}: ${prepared.reason}`);
      return;
    }
    const settled = (await window.quansio.createTeammate(prepared.params)) as SettledPayload;
    setNotice(refusal(settled) ?? t("created"));
    if (settled.ok) {
      setCreating(false);
      setDraft(EMPTY_DRAFT);
      setTemplateId("");
    }
    setStale(true);
  }, [draft, templateId, workspaceId]);

  const save = useCallback(async () => {
    if (!editing) {
      return;
    }
    const prepared = updateTeammate(editing, {
      name: draft.name,
      persona: draft.persona,
      standingInstructions: draft.standingInstructions,
      defaultCapabilityNeeds: needsOf(draft),
      memoryScope: draft.memoryScope,
      modelRoutePreference: draft.modelRoutePreference || null,
      availability: draft.availability,
    });
    if (!prepared.ok) {
      setNotice(`${t("refused")} ${prepared.code}: ${prepared.reason}`);
      return;
    }
    const settled = (await window.quansio.updateTeammate(prepared.params)) as SettledPayload;
    setNotice(refusal(settled) ?? t("teammateSaved"));
    if (settled.ok) {
      setEditing(null);
    }
    setStale(true);
  }, [draft, editing]);

  const archive = useCallback(async (teammate: TeammateView) => {
    const prepared = archiveTeammate(teammate);
    if (!prepared.ok) {
      setNotice(`${t("refused")} ${prepared.code}: ${prepared.reason}`);
      return;
    }
    const settled = (await window.quansio.archiveTeammate(teammate.id)) as SettledPayload;
    setNotice(refusal(settled) ?? t("archived"));
    setStale(true);
  }, []);

  const attention = roster ? needsAttention(roster.teammates) : [];
  const form = (submit: () => Promise<void>, label: string): ReactElement => (
    <form
      className="editor"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      {creating && !editing ? (
        <label>
          {t("fromTemplate")}
          <select value={templateId} onChange={(event) => { setTemplateId(event.target.value); const template = roster?.templates.find((item) => item.id === event.target.value); if (template) { setDraft({ ...draft, name: template.name, persona: template.persona, standingInstructions: template.standingInstructions, needs: template.defaultCapabilityNeeds.join(", "), memoryScope: template.memoryScope }); } }}>
            <option value="">—</option>
            {(roster?.templates ?? []).map((template) => <option key={template.id} value={template.id}>{template.name}</option>)}
          </select>
        </label>
      ) : null}
      <input aria-label="name" placeholder="name" value={draft.name} onChange={(event) => { setDraft({ ...draft, name: event.target.value }); }} />
      <textarea aria-label={t("persona")} placeholder={t("persona")} value={draft.persona} onChange={(event) => { setDraft({ ...draft, persona: event.target.value }); }} />
      <textarea aria-label={t("standingInstructions")} placeholder={t("standingInstructions")} value={draft.standingInstructions} onChange={(event) => { setDraft({ ...draft, standingInstructions: event.target.value }); }} />
      <input aria-label={t("needsLabel")} placeholder="read.internal, fs.write.workspace" value={draft.needs} onChange={(event) => { setDraft({ ...draft, needs: event.target.value }); }} />
      <p className="hint">{t("instructionsNeverGrant")}</p>
      <label>
        {t("memoryScope")}
        <select value={draft.memoryScope} onChange={(event) => { setDraft({ ...draft, memoryScope: event.target.value as MemoryScope }); }}>
          {MEMORY_SCOPES.map((scope) => <option key={scope} value={scope}>{scope}</option>)}
        </select>
      </label>
      <input aria-label={t("routePreference")} placeholder={t("routePreference")} value={draft.modelRoutePreference} onChange={(event) => { setDraft({ ...draft, modelRoutePreference: event.target.value }); }} />
      <label>
        {t("availability")}
        <select value={draft.availability} onChange={(event) => { setDraft({ ...draft, availability: event.target.value as Availability }); }}>
          {AVAILABILITIES.map((availability) => <option key={availability} value={availability}>{availability}</option>)}
        </select>
      </label>
      <button type="submit">{label}</button>
      <button type="button" onClick={() => { setEditing(null); setCreating(false); setDraft(EMPTY_DRAFT); }}>{t("cancel")}</button>
    </form>
  );

  return (
    <section className="teammates" aria-label={t("teammatesSurface")} data-workspace={workspaceId}>
      {notice ? <p className="notice" role="status">{notice}</p> : null}
      <h2>{t("roster")}</h2>
      {attention.length > 0 ? (
        <div className="attention" role="status">
          <ul>{attention.map((teammate) => <li key={teammate.id}>{teammate.name} · {t("attentionNeeded")}</li>)}</ul>
        </div>
      ) : null}
      <ul className="cards">
        {visibleRoster(roster?.teammates ?? []).map((teammate) => {
          const explained = narrowing(teammate);
          return (
            <li key={teammate.id} className={`card teammate teammate-${teammate.status}`} data-teammate={teammate.id} data-status={teammate.status} data-thread-status={teammate.thread?.status ?? ""} data-version={String(teammate.version)}>
              <strong>{teammate.name}</strong> <span className="muted">{teammate.status} · {teammate.availability} · {t("version")} {String(teammate.version)}{teammate.templateId ? ` · ${teammate.templateId}` : ""}</span>
              <p className="persona">{teammate.persona}</p>
              <dl>
                <dt>{t("thread")}</dt>
                <dd>{teammate.thread ? `${teammate.thread.id} · ${teammate.thread.status}${teammate.thread.suspendedReason ? ` (${teammate.thread.suspendedReason})` : ""} · ${String(teammate.thread.runs)} run(s)` : t("none")}</dd>
                <dt>{t("activeRunLabel")}</dt>
                <dd>
                  {teammate.activeRun ? (
                    <button type="button" className="link" onClick={() => { onInspectRun?.(teammate.activeRun?.id ?? ""); }}>{teammate.activeRun.id} · {teammate.activeRun.status}</button>
                  ) : (
                    t("none")
                  )}
                </dd>
                <dt>{t("effectiveCapability")}</dt>
                <dd>
                  {teammate.effectiveCapability.grants.map((grant) => grant.effectClass).join(", ") || t("none")}
                  <br />
                  <span className="muted">{explained.summary}{explained.unmet.length > 0 ? ` — not granted: ${explained.unmet.join(", ")}` : ""}</span>
                </dd>
                <dt>{t("memoryScope")}</dt>
                <dd>{teammate.memoryScope}{teammate.modelRoutePreference ? ` · ${t("routePreference")}: ${teammate.modelRoutePreference}` : ""}</dd>
              </dl>
              {teammate.attention.needsHuman ? <p className="warning">{t("attentionNeeded")}: {String(teammate.attention.openQuestions)} question(s), {String(teammate.attention.pendingApprovals)} approval(s)</p> : null}
              {teammate.status === "active" ? (
                <div className="actions">
                  <button type="button" onClick={() => { setEditing(teammate); setCreating(false); setDraft(draftOf(teammate)); }}>{t("edit")}</button>
                  <button type="button" className="archive" title={t("archiveHint")} onClick={() => void archive(teammate)}>{t("archive")}</button>
                </div>
              ) : null}
              {editing?.id === teammate.id ? form(save, t("teammateSaved").split(".")[0] ?? "Save") : null}
            </li>
          );
        })}
      </ul>
      {!creating ? <button type="button" onClick={() => { setCreating(true); setEditing(null); setDraft(EMPTY_DRAFT); }}>{t("newTeammate")}</button> : form(create, t("newTeammate"))}
    </section>
  );
}
