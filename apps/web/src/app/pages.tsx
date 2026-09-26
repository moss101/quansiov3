/**
 * The review and administration pages (APP-012). Each page reads its projection through
 * the session's client, decodes it with the page module, and sends its commands through
 * the same client; a `SCOPE_FORBIDDEN` refusal renders as the refusal, never as an empty
 * page. Nothing here reaches a host API: there is none.
 */

import { useCallback, useEffect, useState, type ReactElement } from "react";

import { ApiError, type WebClient } from "../client.js";
import { type Roles, allows } from "../rbac.js";
import { DECISIONS, decodeAudit, decodeConnectors, decodeMembers, decodePolicies, decodeUsage, invite, removeMember, setPolicy, updateMembership, type MemberView, type PolicyRuleView, type PolicyView } from "../review/admin.js";
import { decide, decodeApprovals, warnings, type ApprovalView } from "../review/approvals.js";
import { contentPath, decodeArtifacts } from "../review/artifacts.js";
import { decodeWorkGraph, needsAttention } from "../review/work.js";
import { Face, initials } from "./faces.js";
import { CloseIcon, DownloadIcon, FileIcon, LockIcon, PlugIcon } from "./icons.js";
import { t } from "./strings.js";

/** A status as a pill: the word always says it; the colour only reinforces it. */
function Pill({ status }: { readonly status: string }): ReactElement {
  return <span className={`pill status-${status.toLowerCase().replace(/[^a-z_]+/g, "-")}`}>{status}</span>;
}

export interface PageProps {
  readonly client: WebClient;
  readonly workspaceId: string;
  readonly roles: Roles;
}

/** A refusal or failure, as the server said it. */
export function Refusal({ error }: { readonly error: unknown }): ReactElement {
  if (error instanceof ApiError && error.isRbacRefusal) {
    return (
      <div className="refusal" role="alert" data-code={error.code}>
        <span className="refusal-icon" aria-hidden="true"><LockIcon size={20} /></span>
        <h3>{t("refused")}</h3>
        <p>{t("refusedBody")}</p>
        <p className="detail">{error.message}</p>
      </div>
    );
  }
  return (
    <div className="refusal" role="alert" data-code={error instanceof ApiError ? error.code : "INTERNAL"}>
      <p className="detail">{error instanceof Error ? error.message : String(error)}</p>
    </div>
  );
}

/** Load a projection once per dependency change; the page shows loading, refusal or data. */
function useProjection<T>(load: () => Promise<T>, deps: readonly unknown[]): { readonly data: T | null; readonly error: unknown; readonly reload: () => void } {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setError(null);
    load().then(
      (loaded) => {
        if (!cancelled) {
          setData(loaded);
        }
      },
      (failure: unknown) => {
        if (!cancelled) {
          setError(failure);
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [...deps, tick]);
  return { data, error, reload: () => { setTick((n) => n + 1); } };
}

export function WorkReview({ client, workspaceId }: PageProps): ReactElement {
  const { data, error } = useProjection(async () => decodeWorkGraph(await client.get(`/v1/workgraph?workspace_id=${encodeURIComponent(workspaceId)}`)), [client, workspaceId]);
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  const attention = needsAttention(data);
  return (
    <section className="page work" aria-label={t("workReview")}>
      <h2>{t("workReview")} <small>rev {String(data.revision)}</small></h2>
      {attention.length > 0 ? (
        <div className="attention" role="status">
          <h3>{t("attention")} ({String(attention.length)})</h3>
          <ul>{attention.map((node) => <li key={node.id}>{node.title} · {node.status}</li>)}</ul>
        </div>
      ) : null}
      <ul className="cards">
        {data.nodes.map((node) => (
          <li key={node.id} className={`card node-${node.status}`} data-node={node.id}>
            <div className="card-head">
              <strong>{node.title}</strong>
              <Pill status={node.status} />
            </div>
            <p className="meta">{node.kind}{node.runStatus ? ` · run ${node.runStatus}` : ""}{node.openQuestions > 0 ? ` · ${String(node.openQuestions)} open question(s)` : ""}</p>
          </li>
        ))}
      </ul>
      {data.nodes.length === 0 ? <p className="muted">{t("empty")}</p> : null}
    </section>
  );
}

export function Approvals({ client, workspaceId, roles }: PageProps): ReactElement {
  const { data, error, reload } = useProjection(async () => decodeApprovals(await client.get(`/v1/approvals?workspace_id=${encodeURIComponent(workspaceId)}`)), [client, workspaceId]);
  const [previewed, setPreviewed] = useState<ApprovalView | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const mayDecide = allows(roles, "approve_effect");
  const act = useCallback(
    async (kind: "approve" | "deny") => {
      if (!previewed) {
        return;
      }
      const current = decodeApprovals(await client.get(`/v1/approvals?workspace_id=${encodeURIComponent(workspaceId)}`)).find((item) => item.id === previewed.id);
      const decision = current ? decide(kind, previewed, current) : ({ ok: false, code: "CONFLICT_STATE", reason: "the request no longer exists" } as const);
      if (!decision.ok) {
        setNotice(`${decision.code}: ${decision.reason}`);
        return;
      }
      try {
        await client.invoke(decision.command, decision.params);
        setNotice(t("decided"));
        setPreviewed(null);
        reload();
      } catch (failure) {
        setNotice(failure instanceof Error ? failure.message : String(failure));
      }
    },
    [client, previewed, reload, workspaceId],
  );
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  return (
    <section className="page approvals" aria-label={t("approvals")}>
      <h2>{t("approvals")}</h2>
      {notice ? <p className="notice" role="status">{notice}</p> : null}
      <ul className="cards">
        {data.map((view) => (
          <li key={view.id} className={`card approval-${view.status}`} data-approval={view.id}>
            <div className="card-head">
              <strong>{view.summary}</strong>
              <Pill status={view.status} />
            </div>
            <p className="meta">{view.effect ? `${view.effect.effectClass} · tier ${String(view.effect.tier ?? "?")} · ${view.effect.toolName ?? ""}` : ""}</p>
            {view.status === "requested" ? <div className="card-actions"><button type="button" className="primary" onClick={() => { setPreviewed(view); }}>{t("review")}</button></div> : null}
          </li>
        ))}
      </ul>
      {data.length === 0 ? <p className="muted">{t("empty")}</p> : null}
      {previewed ? <div className="scrim" aria-hidden="true" onClick={() => { setPreviewed(null); }} /> : null}
      {previewed ? (
        <div className="preview" role="dialog" aria-modal="true" aria-label={previewed.summary} data-digest={previewed.digest}>
          <header>
            <h3>{previewed.summary}</h3>
            <button type="button" className="icon-button" aria-label={t("close")} onClick={() => { setPreviewed(null); }}>
              <CloseIcon size={14} />
            </button>
          </header>
          <dl>
            <dt>effect</dt><dd>{previewed.effect?.effectClass ?? "?"} · {previewed.effect?.resource ?? ""}</dd>
            <dt>expires</dt><dd>{previewed.expiresAt}</dd>
            <dt>{t("boundTo")}</dt><dd><code>{previewed.digest}</code></dd>
          </dl>
          {warnings(previewed).length > 0 ? <ul className="warnings">{warnings(previewed).map((warning) => <li key={warning}>{warning}</li>)}</ul> : null}
          {mayDecide ? null : <p className="muted">{t("refusedBody")}</p>}
          <div className="decision">
            <button type="button" className="quiet" onClick={() => { setPreviewed(null); }}>{t("close")}</button>
            {mayDecide ? <button type="button" className="deny" onClick={() => void act("deny")}>{t("deny")}</button> : null}
            {mayDecide ? <button type="button" className="approve" onClick={() => void act("approve")}>{t("approve")}</button> : null}
          </div>
        </div>
      ) : null}
    </section>
  );
}

export function Artifacts({ client, workspaceId }: PageProps): ReactElement {
  const { data, error } = useProjection(async () => decodeArtifacts(await client.get(`/v1/artifacts?workspace_id=${encodeURIComponent(workspaceId)}`)), [client, workspaceId]);
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  return (
    <section className="page artifacts" aria-label={t("artifacts")}>
      <h2>{t("artifacts")}</h2>
      <ul className="cards">
        {data.map((artifact) => (
          <li key={artifact.id} className="card" data-artifact={artifact.id}>
            <div className="card-head">
              <span className="card-icon" aria-hidden="true"><FileIcon /></span>
              <strong>{artifact.title}</strong>
            </div>
            <p className="meta">{artifact.kind} · {String(artifact.versionCount)} {t("versions")} · {artifact.mediaType ?? ""} {artifact.sizeBytes !== null ? `· ${String(artifact.sizeBytes)} B` : ""}</p>
            {artifact.currentVersionId ? <div className="card-actions"><a className="button-link" href={`${client.origin}${contentPath(artifact.id, artifact.currentVersionId)}`} download><DownloadIcon size={14} />{t("download")}</a></div> : null}
          </li>
        ))}
      </ul>
      {data.length === 0 ? <p className="muted">{t("empty")}</p> : null}
    </section>
  );
}

const EMPTY_RULE: PolicyRuleView = { effectClass: "read.internal", resource: { kind: "fs", selector: "**" }, decision: "allow" };

export function Policies({ client, workspaceId }: PageProps): ReactElement {
  const { data, error, reload } = useProjection(async () => decodePolicies(await client.get(`/v1/policies?workspace_id=${encodeURIComponent(workspaceId)}`)), [client, workspaceId]);
  const [editing, setEditing] = useState<PolicyView | null>(null);
  const [rules, setRules] = useState<readonly PolicyRuleView[]>([]);
  const [notice, setNotice] = useState<string | null>(null);
  const save = useCallback(async () => {
    if (!editing) {
      return;
    }
    const prepared = setPolicy({ scope: editing.scope, workspaceId: editing.workspaceId, rules, questionTtlSeconds: editing.questionTtlSeconds, approvalTtlSeconds: editing.approvalTtlSeconds, maxPlanNodes: editing.maxPlanNodes });
    if (!prepared.ok) {
      setNotice(`${prepared.code}: ${prepared.reason}`);
      return;
    }
    try {
      await client.invoke(prepared.command, prepared.params);
      setEditing(null);
      setNotice(null);
      reload();
    } catch (failure) {
      setNotice(failure instanceof Error ? failure.message : String(failure));
    }
  }, [client, editing, reload, rules]);
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  return (
    <section className="page policies" aria-label={t("policies")}>
      <h2>{t("policies")}</h2>
      {notice ? <p className="notice" role="status">{notice}</p> : null}
      <ul className="cards">
        {data.map((policy) => (
          <li key={policy.id} className="card" data-policy={policy.id}>
            <div className="card-head">
              <strong>{policy.scope} policy</strong>
              <span className="meta">v{String(policy.version)} · {String(policy.rules.length)} rules</span>
            </div>
            <ul className="rules">{policy.rules.map((rule, index) => <li key={`${rule.effectClass}-${String(index)}`}><code>{rule.effectClass}</code> {rule.resource.kind} {rule.resource.selector} → <Pill status={rule.decision} /></li>)}</ul>
            <div className="card-actions"><button type="button" onClick={() => { setEditing(policy); setRules(policy.rules); }}>{t("edit")}</button></div>
          </li>
        ))}
      </ul>
      {editing ? (
        <form className="editor" onSubmit={(event) => { event.preventDefault(); void save(); }}>
          <h3>{editing.scope} policy</h3>
          {rules.map((rule, index) => (
            <div className="rule" key={String(index)}>
              <input aria-label="effect class" value={rule.effectClass} onChange={(event) => { setRules(rules.map((item, at) => (at === index ? { ...item, effectClass: event.target.value } : item))); }} />
              <input aria-label="resource kind" value={rule.resource.kind} onChange={(event) => { setRules(rules.map((item, at) => (at === index ? { ...item, resource: { ...item.resource, kind: event.target.value } } : item))); }} />
              <input aria-label="resource selector" value={rule.resource.selector} onChange={(event) => { setRules(rules.map((item, at) => (at === index ? { ...item, resource: { ...item.resource, selector: event.target.value } } : item))); }} />
              <select aria-label="decision" value={rule.decision} onChange={(event) => { setRules(rules.map((item, at) => (at === index ? { ...item, decision: event.target.value } : item))); }}>
                {DECISIONS.map((decision) => <option key={decision} value={decision}>{decision}</option>)}
              </select>
              <button type="button" className="icon-button" aria-label={t("remove")} onClick={() => { setRules(rules.filter((_, at) => at !== index)); }}><CloseIcon size={14} /></button>
            </div>
          ))}
          <div className="card-actions">
            <button type="button" onClick={() => { setRules([...rules, EMPTY_RULE]); }}>{t("addRule")}</button>
            <button type="submit">{t("save")}</button>
          </div>
        </form>
      ) : null}
    </section>
  );
}

export function Connectors({ client, workspaceId }: PageProps): ReactElement {
  const { data, error } = useProjection(async () => decodeConnectors(await client.get(`/v1/connectors?workspace_id=${encodeURIComponent(workspaceId)}`)), [client, workspaceId]);
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  return (
    <section className="page connectors" aria-label={t("connectors")}>
      <h2>{t("connectors")}</h2>
      <ul className="cards">
        {data.map((connector) => (
          <li key={connector.id} className={`card connector-${connector.status}`}>
            <div className="card-head">
              <span className="card-icon" aria-hidden="true"><PlugIcon /></span>
              <strong>{connector.displayName}</strong>
              <Pill status={connector.status} />
            </div>
            <p className="meta">{connector.connectorId} · {connector.hasCredential ? "credential held as a handle" : "no credential"}{connector.lastHealthAt ? ` · health ${connector.lastHealthAt}` : ""}</p>
          </li>
        ))}
      </ul>
      {data.length === 0 ? <p className="muted">{t("empty")}</p> : null}
    </section>
  );
}

export function Users({ client, workspaceId, roles }: PageProps): ReactElement {
  const { data, error, reload } = useProjection(async () => decodeMembers(await client.get("/v1/members")), [client]);
  const [email, setEmail] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const send = useCallback(
    async (prepared: ReturnType<typeof invite>) => {
      if (!prepared.ok) {
        setNotice(`${prepared.code}: ${prepared.reason}`);
        return;
      }
      try {
        await client.invoke(prepared.command, prepared.params);
        setNotice(null);
        reload();
      } catch (failure) {
        setNotice(failure instanceof Error ? failure.message : String(failure));
      }
    },
    [client, reload],
  );
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  const tenantAdmin = allows(roles, "manage_tenant");
  return (
    <section className="page users" aria-label={t("users")}>
      <h2>{t("users")}</h2>
      {notice ? <p className="notice" role="status">{notice}</p> : null}
      <form className="invite" onSubmit={(event) => { event.preventDefault(); void send(invite({ email, role: "member", workspaceId, workspaceRole: "viewer" })); setEmail(""); }}>
        <input aria-label={t("email")} placeholder={t("email")} value={email} onChange={(event) => { setEmail(event.target.value); }} />
        <button type="submit">{t("invite")}</button>
      </form>
      <div className="table-wrap">
      <table>
        <thead><tr><th>user</th><th>{t("role")}</th><th>{t("status")}</th><th>workspaces</th><th><span className="sr-only">{t("actions")}</span></th></tr></thead>
        <tbody>
          {data.map((member: MemberView) => (
            <tr key={member.membershipId} data-member={member.userId} className={`member-${member.status}`}>
              <td>
                <span className="person">
                  <Face id={member.userId} text={initials(member.displayName, member.email)} size="sm" />
                  <span className="person-text">{member.displayName} <span className="muted">{member.email}</span></span>
                </span>
              </td>
              <td>{member.role}</td>
              <td><Pill status={member.status} /></td>
              <td>{member.workspaces.map((membership) => `${membership.role}${membership.status === "active" ? "" : ` (${membership.status})`}`).join(", ")}</td>
              <td className="row-actions">
                {tenantAdmin && member.status === "active" ? <button type="button" onClick={() => void send(updateMembership(member.membershipId, { status: "suspended" }))}>{t("suspend")}</button> : null}
                {tenantAdmin && (member.status === "suspended" || member.status === "invited") ? <button type="button" onClick={() => void send(updateMembership(member.membershipId, { status: "active" }))}>{t("activate")}</button> : null}
                {tenantAdmin && removeMember(member, data).ok ? <button type="button" className="remove" onClick={() => void send(removeMember(member, data))}>{t("remove")}</button> : null}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      </div>
    </section>
  );
}

export function Capabilities({ client, workspaceId }: PageProps): ReactElement {
  const { data, error } = useProjection(async () => {
    const [policies, skills] = await Promise.all([client.get(`/v1/policies?workspace_id=${encodeURIComponent(workspaceId)}`), client.get<{ skills?: unknown[] }>("/v1/skills")]);
    return { policies: decodePolicies(policies), skills: Array.isArray(skills.skills) ? skills.skills : [] };
  }, [client, workspaceId]);
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  const classes = new Map<string, string[]>();
  for (const policy of data.policies) {
    for (const rule of policy.rules) {
      classes.set(rule.effectClass, [...(classes.get(rule.effectClass) ?? []), `${policy.scope}: ${rule.decision}`]);
    }
  }
  return (
    <section className="page capabilities" aria-label={t("capabilities")}>
      <h2>{t("capabilities")}</h2>
      <div className="table-wrap">
        <table>
          <thead><tr><th>effect class</th><th>decisions by policy</th></tr></thead>
          <tbody>{[...classes.entries()].map(([effectClass, decisions]) => <tr key={effectClass}><td><code>{effectClass}</code></td><td>{decisions.join(" · ")}</td></tr>)}</tbody>
        </table>
      </div>
      <p className="muted">{String(data.skills.length)} skill(s) registered; skills never grant a capability.</p>
    </section>
  );
}

export function Audit({ client, workspaceId }: PageProps): ReactElement {
  const { data, error } = useProjection(async () => decodeAudit(await client.get(`/v1/audit?workspace_id=${encodeURIComponent(workspaceId)}&limit=200`)), [client, workspaceId]);
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  return (
    <section className="page audit" aria-label={t("audit")}>
      <h2>{t("audit")}</h2>
      <p className={data.chain.holds ? "chain ok" : "chain broken"} role="status" data-holds={data.chain.holds ? "true" : "false"}>
        {data.chain.holds ? `${t("chainHolds")} (${String(data.chain.entries)})` : `${t("chainBroken")} ${data.chain.brokenAt ?? "?"}`}
      </p>
      <div className="table-wrap">
      <table>
        <thead><tr><th>when</th><th>actor</th><th>action</th><th>target</th><th>decision</th><th>reason</th></tr></thead>
        <tbody>
          {data.entries.map((entry) => (
            <tr key={entry.id} data-audit={entry.id}>
              <td><time>{entry.occurredAt}</time></td><td>{entry.actor}</td><td>{entry.action}</td><td>{entry.targetRef}</td><td>{entry.decision}</td><td>{entry.reason}</td>
            </tr>
          ))}
        </tbody>
      </table>
      </div>
    </section>
  );
}

export function Usage({ client, workspaceId }: PageProps): ReactElement {
  const { data, error } = useProjection(async () => decodeUsage(await client.get(`/v1/usage?workspace_id=${encodeURIComponent(workspaceId)}`)), [client, workspaceId]);
  if (error) {
    return <Refusal error={error} />;
  }
  if (!data) {
    return <p className="loading">{t("loading")}</p>;
  }
  return (
    <section className="page usage" aria-label={t("usage")}>
      <h2>{t("usage")}</h2>
      <div className="table-wrap">
        <table>
          <thead><tr><th>meter</th><th className="num">total</th><th>unit</th></tr></thead>
          <tbody>{data.meters.map((meter) => <tr key={meter.meter}><td>{meter.meter}</td><td className="num">{String(meter.total)}</td><td>{meter.unit ?? ""}</td></tr>)}</tbody>
        </table>
      </div>
      <p className="muted">{String(data.records)} record(s) from {String(data.eventsRead)} event(s)</p>
    </section>
  );
}
