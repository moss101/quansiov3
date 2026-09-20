/**
 * Plan / WorkGraph / agent control surface (APP-005): the plan tree with dependencies,
 * active agents and handoffs, blockers and completion contracts, all from the canonical
 * revision; user edits as a draft of typed intents sent as `ProposeGraphEdit`, and a
 * conflict/rebase dialog when the server's revision moved under the draft.
 */

import { useCallback, useEffect, useReducer, useState, type ReactElement } from "react";

import { ApiError } from "../client/server.js";
import { t } from "../i18n/strings.js";
import {
  childrenOf,
  decodeWorkGraph,
  describeIntent,
  handoffsFor,
  initialWorkState,
  needsTwoProposals,
  newDraft,
  prerequisitesOf,
  proposal,
  rebase,
  resolveLocalIds,
  withIntent,
  withoutIntent,
  workReducer,
  type EditDraft,
  type EditIntent,
  type NodeKind,
  type NodeStatus,
  type WorkGraphView,
  type WorkNodeView,
} from "../work/monitor.js";

interface WorkProps {
  readonly workspaceId: string;
  readonly onInspectRun?: (runId: string) => void;
}

interface Conflict {
  readonly draft: EditDraft;
  readonly server: WorkGraphView;
  readonly dropped: readonly { readonly intent: EditIntent; readonly reason: string }[];
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : t("commandFailed");
}

function isRevisionConflict(error: unknown): boolean {
  return error instanceof ApiError && error.code === "CONFLICT_REVISION";
}

/** Send a draft: nodes first when edges name new nodes, then the rest against the new revision. */
async function sendDraft(draft: EditDraft, before: WorkGraphView | null): Promise<WorkGraphView> {
  if (!needsTwoProposals(draft)) {
    await window.quansio.proposeGraphEdit(proposal(draft) as unknown as Record<string, unknown>);
    return decodeWorkGraph(await window.quansio.workGraph());
  }
  await window.quansio.proposeGraphEdit(proposal(draft, "nodes") as unknown as Record<string, unknown>);
  const after = decodeWorkGraph(await window.quansio.workGraph());
  const localIds = resolveLocalIds(draft, before, after);
  const second = { ...draft, baseRevision: after.revision };
  await window.quansio.proposeGraphEdit(proposal(second, "edges", localIds) as unknown as Record<string, unknown>);
  return decodeWorkGraph(await window.quansio.workGraph());
}

export function Work({ workspaceId, onInspectRun }: WorkProps): ReactElement {
  const [state, dispatch] = useReducer(workReducer, initialWorkState(workspaceId));
  const [draft, setDraft] = useState<EditDraft | null>(null);
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [title, setTitle] = useState("");
  const [kind, setKind] = useState<NodeKind>("task");
  const [dependsOn, setDependsOn] = useState("");
  const [objectiveTitle, setObjectiveTitle] = useState("");

  useEffect(
    () =>
      window.quansio.onStreamFrame((frame) => {
        if (frame && typeof frame === "object" && "kind" in frame) {
          dispatch({ type: "frame", frame: frame as never });
        }
      }),
    [],
  );

  useEffect(() => {
    if (!state.stale) {
      return;
    }
    let cancelled = false;
    void window.quansio
      .workGraph()
      .then((payload) => {
        if (!cancelled) {
          dispatch({ type: "graph.loaded", graph: decodeWorkGraph(payload) });
        }
      })
      .catch((error: unknown) => {
        dispatch({ type: "notice", notice: errorText(error) });
      });
    return () => {
      cancelled = true;
    };
  }, [state.stale]);

  const graph = state.graph;
  const selected = graph?.nodes.find((node) => node.id === state.selectedNodeId) ?? null;

  const addIntent = useCallback(
    (intent: EditIntent) => {
      if (!graph) {
        return;
      }
      try {
        setDraft((current) => withIntent(current ?? newDraft(workspaceId, graph.revision), intent));
      } catch (error) {
        dispatch({ type: "notice", notice: errorText(error) });
      }
    },
    [graph, workspaceId],
  );

  const submit = useCallback(async () => {
    if (!draft || draft.intents.length === 0) {
      return;
    }
    try {
      const after = await sendDraft(draft, graph);
      dispatch({ type: "graph.loaded", graph: after });
      setDraft(null);
      setConflict(null);
    } catch (error) {
      if (isRevisionConflict(error)) {
        // The server moved on: show what changed and let the user rebase or discard.
        const server = decodeWorkGraph(await window.quansio.workGraph());
        const outcome = rebase(draft, server);
        setConflict({ draft: outcome.draft, server, dropped: outcome.dropped });
        dispatch({ type: "graph.loaded", graph: server });
      } else {
        dispatch({ type: "notice", notice: errorText(error) });
      }
    }
  }, [draft, graph]);

  const applyRebase = useCallback(() => {
    if (!conflict) {
      return;
    }
    setDraft(conflict.draft.intents.length > 0 ? conflict.draft : null);
    setConflict(null);
  }, [conflict]);

  const createObjective = useCallback(async () => {
    if (!objectiveTitle.trim()) {
      return;
    }
    try {
      await window.quansio.createObjective({ title: objectiveTitle.trim() });
      setObjectiveTitle("");
      dispatch({ type: "stale" });
    } catch (error) {
      dispatch({ type: "notice", notice: errorText(error) });
    }
  }, [objectiveTitle]);

  const transition = useCallback(
    (node: WorkNodeView, to: NodeStatus) => {
      addIntent({ kind: "transition", nodeId: node.id, to, expectedRevision: node.revision });
    },
    [addIntent],
  );

  if (!graph) {
    return (
      <section className="work" aria-label={t("work")}>
        <p>{state.notice ?? t("loading")}</p>
      </section>
    );
  }

  const roots = childrenOf(graph, null);
  const agentById = new Map(graph.agents.map((agent) => [agent.id, agent]));

  const renderNode = (node: WorkNodeView, depth: number): ReactElement => {
    const blockers = graph.blockers.filter((blocker) => blocker.nodeId === node.id);
    const prerequisites = prerequisitesOf(graph, node.id);
    return (
      <li key={node.id} className={`node node-${node.status}${node.id === state.selectedNodeId ? " selected" : ""}`} data-node={node.id} style={{ marginLeft: depth * 16 }}>
        <button type="button" onClick={() => { dispatch({ type: "select", nodeId: node.id }); }}>
          <span className="kind">{node.kind}</span> {node.title} <span className="status">{node.status}</span>
        </button>
        {node.run ? (
          <button type="button" className="link" onClick={() => onInspectRun?.(node.run?.id ?? "")}>
            {t("run")} {node.run.status}
          </button>
        ) : null}
        {prerequisites.length > 0 ? <small className="deps">{t("dependsOn")}: {prerequisites.map((p) => p.title).join(", ")}</small> : null}
        {blockers.map((blocker) => (
          <span key={`${blocker.kind}-${blocker.ref ?? ""}`} className={`blocker blocker-${blocker.kind}`}>
            {t("blocked")}: {blocker.kind === "prerequisite" ? `${t("waitingFor")} ${graph.nodes.find((n) => n.id === blocker.ref)?.title ?? blocker.ref ?? ""}` : blocker.kind === "run" ? `${blocker.status ?? ""}${blocker.question ? ` · ${blocker.question.prompt}` : ""}` : blocker.status ?? ""}
          </span>
        ))}
        <ul>{childrenOf(graph, node.id).map((child) => renderNode(child, depth + 1))}</ul>
      </li>
    );
  };

  return (
    <section className="work" aria-label={t("work")} data-revision={graph.revision}>
      <div className="plan">
        <header>
          <h2>{t("plan")}</h2>
          <span className="revision">{t("revision")} {graph.revision}</span>
          {state.stale ? <span className="stale">{t("refreshing")}</span> : null}
        </header>
        {state.notice ? <p className="notice">{state.notice}</p> : null}
        <form
          className="objective"
          onSubmit={(event) => {
            event.preventDefault();
            void createObjective();
          }}
        >
          <input type="text" value={objectiveTitle} placeholder={t("objectivePlaceholder")} onChange={(event) => { setObjectiveTitle(event.target.value); }} />
          <button type="submit" disabled={!objectiveTitle.trim()}>{t("createObjective")}</button>
        </form>
        <ul className="tree">{roots.map((node) => renderNode(node, 0))}</ul>
      </div>
      <aside className="control">
        <h3>{t("agents")}</h3>
        <ul className="agents">
          {graph.agents.map((agent) => (
            <li key={agent.id} className={`agent agent-${agent.kind}`} data-agent={agent.id}>
              {agent.kind} · {agent.status}
              {agent.workNodeId ? ` · ${graph.nodes.find((n) => n.id === agent.workNodeId)?.title ?? agent.workNodeId}` : ""}
              {agent.parentId ? <small> ← {agent.parentId}</small> : null}
            </li>
          ))}
        </ul>
        <h3>{t("handoffs")}</h3>
        <ul className="handoffs">
          {graph.handoffs.length === 0 ? <li className="empty">{t("none")}</li> : null}
          {graph.handoffs.map((handoff) => (
            <li key={handoff.id} data-handoff={handoff.id}>
              {agentById.get(handoff.parentAgentThreadId)?.kind ?? handoff.parentAgentThreadId} → {agentById.get(handoff.childAgentThreadId)?.kind ?? handoff.childAgentThreadId}
              {handoff.workNodeId ? ` · ${graph.nodes.find((n) => n.id === handoff.workNodeId)?.title ?? ""}` : ""}
              {" · "}
              {handoff.joinedAt ? t("joined") : t("inFlight")}
            </li>
          ))}
        </ul>
        {selected ? (
          <div className="detail" data-node={selected.id}>
            <h3>{selected.title}</h3>
            <dl>
              <dt>{t("status")}</dt>
              <dd>{selected.status}</dd>
              <dt>{t("contract")}</dt>
              <dd>
                {selected.contract.deterministicChecks.length} {t("checks")}
                {selected.contract.semanticVerification.required ? ` · ${t("semantic")}` : ""}
                {selected.contract.humanSignoffRequired ? ` · ${t("humanSignoff")}` : ""}
              </dd>
              {handoffsFor(graph, selected.id).length > 0 ? (
                <>
                  <dt>{t("handoffs")}</dt>
                  <dd>{handoffsFor(graph, selected.id).length}</dd>
                </>
              ) : null}
            </dl>
            <div className="transitions">
              {(["ready", "cancelled"] as const).filter((to) => to !== selected.status).map((to) => (
                <button key={to} type="button" onClick={() => { transition(selected, to); }}>
                  → {to}
                </button>
              ))}
            </div>
            <form
              className="add-node"
              onSubmit={(event) => {
                event.preventDefault();
                addIntent({ kind: "add_node", localId: `new-${String(Date.now())}`, nodeKind: kind, title, description: null, parentId: selected.id, priority: 1, contract: null });
                setTitle("");
              }}
            >
              <select value={kind} onChange={(event) => { setKind(event.target.value as NodeKind); }}>
                {(["task", "subtask", "milestone", "wait"] as const).map((option) => (
                  <option key={option} value={option}>{option}</option>
                ))}
              </select>
              <input type="text" value={title} placeholder={t("nodeTitlePlaceholder")} onChange={(event) => { setTitle(event.target.value); }} />
              <button type="submit" disabled={!title.trim()}>{t("addUnder")}</button>
            </form>
            <form
              className="add-edge"
              onSubmit={(event) => {
                event.preventDefault();
                if (dependsOn) {
                  addIntent({ kind: "add_edge", localId: `edge-${String(Date.now())}`, fromNodeId: selected.id, toNodeId: dependsOn, edgeKind: "depends_on" });
                  setDependsOn("");
                }
              }}
            >
              <select value={dependsOn} onChange={(event) => { setDependsOn(event.target.value); }}>
                <option value="">{t("dependsOn")}…</option>
                {graph.nodes.filter((node) => node.id !== selected.id).map((node) => (
                  <option key={node.id} value={node.id}>{node.title}</option>
                ))}
              </select>
              <button type="submit" disabled={!dependsOn}>{t("addDependency")}</button>
            </form>
          </div>
        ) : null}
        {draft && draft.intents.length > 0 ? (
          <div className="draft" data-base-revision={draft.baseRevision}>
            <h3>{t("draft")} ({t("revision")} {draft.baseRevision})</h3>
            <ol>
              {draft.intents.map((intent, index) => (
                <li key={`${intent.kind}-${String(index)}`}>
                  {describeIntent(intent)}
                  <button type="button" aria-label={t("remove")} onClick={() => { setDraft(withoutIntent(draft, index)); }}>×</button>
                </li>
              ))}
            </ol>
            <button type="button" onClick={() => void submit()}>{t("propose")}</button>
            <button type="button" onClick={() => { setDraft(null); }}>{t("discard")}</button>
          </div>
        ) : null}
        {conflict ? (
          <div className="conflict" role="alertdialog" data-server-revision={conflict.server.revision}>
            <h3>{t("conflictTitle")}</h3>
            <p>{t("conflictBody")} {conflict.server.revision}.</p>
            {conflict.dropped.length > 0 ? (
              <ul>
                {conflict.dropped.map((item, index) => (
                  <li key={`${item.intent.kind}-${String(index)}`}>{describeIntent(item.intent)} — {item.reason}</li>
                ))}
              </ul>
            ) : null}
            <button type="button" onClick={applyRebase}>{t("rebase")} ({conflict.draft.intents.length})</button>
            <button type="button" onClick={() => { setConflict(null); setDraft(null); }}>{t("discard")}</button>
          </div>
        ) : null}
      </aside>
    </section>
  );
}
