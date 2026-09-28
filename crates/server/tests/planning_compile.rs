//! RUN-003 coverage: the pure plan compiler's refusals and its deterministic mutation set
//! (DOMAIN.md §4.1, §4.2, §4.5).
//!
//! `compile` writes nothing and reads nothing, so these tests drive it directly with a raw
//! proposal, a graph snapshot and a proposer projection. The DB-backed ingestion and audit
//! trail are covered by `planning.rs` and `graph_planning.rs`.
//!
//! Real boundary: none; no environment is needed.

use quansio_core::Revision;
use quansio_server::runtime::planning::{
    compile, parse_raw, CompiledPlan, ContractSource, EdgeKind, NodeStatus, PlanError,
    PlanGraphSnapshot, PlanProposerProjection, SnapshotEdge, SnapshotNode,
};
use serde_json::{json, Value};

const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0P1";
const OBJECTIVE: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0P1";
const TASK: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0P2";
const REVIEW: &str = "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0P3";
const DEPENDS: &str = "we_01J8Z3K6F1N8VQ2X5W9Y0QA0P1";
const AGENT: &str = "ath_01J8Z3K6F1N8VQ2X5W9Y0QA0P1";
const LIMIT: usize = 25;

fn contract() -> Value {
    json!({ "deterministic_checks": [{ "kind": "artifact_exists", "artifact_role": "report" }] })
}

fn proposer() -> PlanProposerProjection {
    PlanProposerProjection {
        agent_thread_id: Some(AGENT.to_string()),
        grants: json!([{
            "effect_class": "fs.write.workspace",
            "resource": { "kind": "fs", "selector": "/work/**" },
            "constraints": { "max_tier": 1 }
        }]),
        created_by: json!({ "kind": "agent", "id": AGENT }),
    }
}

/// Three existing nodes (the task and the review under the objective, both with contracts)
/// and one `depends_on` edge from the review to the task, at graph revision 4.
fn graph() -> PlanGraphSnapshot {
    let mut snapshot = PlanGraphSnapshot::empty(WORKSPACE, Revision::new(4));
    assert!(snapshot.nodes.is_empty() && snapshot.edges.is_empty());
    for (id, parent, revision) in [
        (OBJECTIVE, None, 1),
        (TASK, Some(OBJECTIVE), 2),
        (REVIEW, Some(OBJECTIVE), 3),
    ] {
        snapshot.nodes.push(SnapshotNode {
            id: id.to_string(),
            parent_id: parent.map(str::to_string),
            revision,
            completion_contract: contract(),
        });
    }
    snapshot.edges.push(SnapshotEdge {
        id: DEPENDS.to_string(),
        from_node_id: REVIEW.to_string(),
        to_node_id: TASK.to_string(),
        kind: EdgeKind::DependsOn,
        revision: 5,
    });
    snapshot
}

/// A proposal at the graph's revision carrying `changes` (merged over empty lists).
fn proposal(changes: Value) -> Value {
    let mut base = json!({
        "proposal_id": "prop_qa001",
        "run_id": "run_01J8Z3K6F1N8VQ2X5W9Y0QA0P1",
        "workspace_id": WORKSPACE,
        "base_revision": 4,
        "nodes_add": [],
        "nodes_update": [],
        "edges_add": [],
        "edges_remove": [],
        "rationale": "restructure the review",
        "capability_needs": []
    });
    for (key, value) in changes.as_object().expect("changes").clone() {
        base[key] = value;
    }
    base
}

fn node(id: &str, extra: Value) -> Value {
    let mut node = json!({
        "id": id,
        "kind": "task",
        "title": format!("node {id}"),
        "completion_contract": contract()
    });
    for (key, value) in extra.as_object().expect("extra").clone() {
        node[key] = value;
    }
    node
}

fn compiled(raw: &Value) -> Result<CompiledPlan, PlanError> {
    compile(&parse_raw(raw)?, &graph(), &proposer(), LIMIT)
}

/// Compile and expect a refusal whose message contains `needle`.
fn refused(raw: &Value, code: &str, needle: &str) -> PlanError {
    let error = compiled(raw).expect_err("the proposal is refused");
    assert_eq!(error.code(), code, "{error}");
    assert!(
        error.to_string().contains(needle),
        "the refusal names {needle:?}: {error}"
    );
    error
}

#[test]
fn a_proposal_without_identity_or_with_malformed_nodes_is_a_schema_refusal() {
    refused(
        &proposal(json!({ "proposal_id": "  ", "nodes_add": [node("n1", json!({}))] })),
        "VALIDATION_SCHEMA",
        "proposal_id",
    );
    refused(
        &proposal(json!({ "workspace_id": "", "nodes_add": [node("n1", json!({}))] })),
        "VALIDATION_SCHEMA",
        "workspace_id",
    );
    refused(
        &proposal(json!({ "nodes_add": [node("n1", json!({ "status": "sleeping" }))] })),
        "VALIDATION_SCHEMA",
        "sleeping",
    );
    refused(
        &proposal(json!({ "nodes_add": [node(" ", json!({}))] })),
        "VALIDATION_SCHEMA",
        "node id must not be empty",
    );
    refused(
        &proposal(json!({ "nodes_add": [node("n1", json!({ "title": " " }))] })),
        "VALIDATION_SCHEMA",
        "must have a title",
    );
    refused(
        &proposal(json!({ "nodes_add": [node("n1", json!({ "priority": 7 }))] })),
        "VALIDATION_SCHEMA",
        "outside 0..=3",
    );
    refused(
        &proposal(json!({
            "nodes_update": [{ "node_id": TASK, "expected_revision": 2, "to": "sleeping" }]
        })),
        "VALIDATION_SCHEMA",
        "sleeping",
    );
}

#[test]
fn a_proposal_for_another_workspace_or_with_no_change_is_refused() {
    let error = refused(
        &proposal(json!({
            "workspace_id": "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0P9",
            "nodes_add": [node("n1", json!({}))]
        })),
        "INTERNAL",
        "ws_01J8Z3K6F1N8VQ2X5W9Y0QA0P9",
    );
    assert!(
        !error.is_rejection(),
        "a mismatched snapshot is an infrastructure fault, not a plan verdict"
    );
    let empty = refused(&proposal(json!({})), "VALIDATION_SCHEMA", "no change");
    assert_eq!(empty, PlanError::EmptyPlan);
}

#[test]
fn node_and_edge_identities_must_be_unique_and_new() {
    refused(
        &proposal(json!({ "nodes_add": [node(TASK, json!({}))] })),
        "VALIDATION_SCHEMA",
        "already exists",
    );
    let edge = |id: &str| json!({ "id": id, "from": TASK, "to": OBJECTIVE, "kind": "blocked_by" });
    refused(
        &proposal(json!({ "edges_add": [edge("e1"), edge("e1")] })),
        "VALIDATION_SCHEMA",
        "duplicate edge id",
    );
    refused(
        &proposal(json!({ "edges_add": [edge(DEPENDS)] })),
        "VALIDATION_SCHEMA",
        "already exists",
    );
}

#[test]
fn updates_and_removals_must_name_existing_rows_at_the_observed_revision() {
    let update = |id: &str, revision: u64| json!({ "node_id": id, "expected_revision": revision, "to": "ready" });
    refused(
        &proposal(json!({ "nodes_update": [update(TASK, 2), update(TASK, 2)] })),
        "VALIDATION_SCHEMA",
        "updated more than once",
    );
    refused(
        &proposal(json!({ "nodes_update": [update("wn_01J8Z3K6F1N8VQ2X5W9Y0QA0PZ", 1)] })),
        "VALIDATION_SCHEMA",
        "unknown node",
    );
    let stale = refused(
        &proposal(json!({ "nodes_update": [update(TASK, 1)] })),
        "CONFLICT_REVISION",
        "revision",
    );
    assert_eq!(
        stale,
        PlanError::StaleBaseRevision {
            expected: 1,
            current: 2
        }
    );

    let removal = |id: &str, revision: u64| json!({ "edge_id": id, "expected_revision": revision });
    refused(
        &proposal(json!({ "edges_remove": [removal(DEPENDS, 5), removal(DEPENDS, 5)] })),
        "VALIDATION_SCHEMA",
        "removed more than once",
    );
    refused(
        &proposal(json!({ "edges_remove": [removal("we_01J8Z3K6F1N8VQ2X5W9Y0QA0PZ", 1)] })),
        "VALIDATION_SCHEMA",
        "unknown edge",
    );
    let stale = refused(
        &proposal(json!({ "edges_remove": [removal(DEPENDS, 4)] })),
        "CONFLICT_REVISION",
        "revision",
    );
    assert_eq!(
        stale,
        PlanError::StaleBaseRevision {
            expected: 4,
            current: 5
        }
    );
}

#[test]
fn removing_an_edge_lets_the_same_plan_add_its_reverse_and_the_summary_is_deterministic() {
    let reverse = json!({ "id": "e_reverse", "from": TASK, "to": REVIEW, "kind": "depends_on" });

    // Without the removal the reverse edge closes a cycle through the existing one.
    refused(
        &proposal(json!({ "edges_add": [reverse.clone()] })),
        "VALIDATION_SCHEMA",
        "cycle",
    );

    let raw = proposal(json!({
        "nodes_update": [{ "node_id": TASK, "expected_revision": 2, "to": "ready" }],
        "edges_remove": [{ "edge_id": DEPENDS, "expected_revision": 5 }],
        "edges_add": [
            reverse,
            { "id": "e_art", "from": TASK, "to": REVIEW, "kind": "produces_artifact" },
            { "id": "e_ver", "from": TASK, "to": REVIEW, "kind": "verified_by" },
            { "id": "e_blk", "from": REVIEW, "to": OBJECTIVE, "kind": "blocked_by" },
            { "id": "e_par", "from": TASK, "to": REVIEW, "kind": "parent_of" }
        ]
    }));
    let plan = compiled(&raw).expect("the plan compiles");
    assert_eq!(plan.change_count(), 7);
    assert_eq!(plan.nodes_update[0].to, NodeStatus::Ready);
    assert_eq!(plan.edges_remove[0].edge_id, DEPENDS);
    assert_eq!(
        plan.mutation_summary(),
        vec![
            format!("transition {TASK} expected=2 to=ready"),
            format!("edge {TASK} -> {REVIEW} kind=depends_on"),
            format!("edge {TASK} -> {REVIEW} kind=produces_artifact"),
            format!("edge {TASK} -> {REVIEW} kind=verified_by"),
            format!("edge {REVIEW} -> {OBJECTIVE} kind=blocked_by"),
            format!("edge {TASK} -> {REVIEW} kind=parent_of"),
            format!("remove {DEPENDS} expected=5"),
        ]
    );
    assert_eq!(
        compiled(&raw).expect("again").mutation_summary(),
        plan.mutation_summary(),
        "the same proposal over the same snapshot compiles identically"
    );
}

#[test]
fn a_parent_must_exist_and_a_planned_parent_must_carry_the_contract_it_would_hand_down() {
    refused(
        &proposal(json!({
            "nodes_add": [node("n1", json!({ "parent_id": "wn_01J8Z3K6F1N8VQ2X5W9Y0QA0PZ" }))]
        })),
        "VALIDATION_SCHEMA",
        "unknown parent",
    );
    // The planned parent inherits its own contract from an existing node, but inheritance is
    // one level: its child has nothing to inherit.
    let error = refused(
        &proposal(json!({
            "nodes_add": [
                node("parent", json!({ "parent_id": OBJECTIVE, "completion_contract": {} })),
                node("child", json!({ "parent_id": "parent", "completion_contract": null }))
            ]
        })),
        "VALIDATION_SCHEMA",
        "no CompletionContract",
    );
    assert_eq!(
        error,
        PlanError::MissingCompletionContract {
            node: "node child".to_string()
        }
    );
}

#[test]
fn any_non_empty_contract_value_is_the_nodes_own_and_an_empty_one_is_not() {
    for own in [
        json!("see the rubric"),
        json!(3),
        json!(true),
        json!(["check"]),
    ] {
        let plan = compiled(&proposal(json!({
            "nodes_add": [node("n1", json!({ "completion_contract": own }))]
        })))
        .unwrap_or_else(|error| panic!("{own} is a contract: {error}"));
        assert_eq!(plan.nodes_add[0].contract_source, ContractSource::Own);
    }
    for empty in [json!(""), json!([]), json!(null)] {
        refused(
            &proposal(json!({
                "nodes_add": [node("n1", json!({ "completion_contract": empty }))]
            })),
            "VALIDATION_SCHEMA",
            "no CompletionContract",
        );
    }
}

#[test]
fn a_created_node_starts_draft_and_may_not_hang_under_a_node_created_in_the_same_plan() {
    refused(
        &proposal(json!({ "nodes_add": [node("n1", json!({ "status": "ready" }))] })),
        "VALIDATION_SCHEMA",
        "starts draft",
    );
    let draft = compiled(&proposal(json!({
        "nodes_add": [node("n1", json!({ "status": "draft", "parent_id": OBJECTIVE }))]
    })))
    .expect("an explicit draft under an existing parent compiles");
    assert_eq!(draft.nodes_add[0].status, NodeStatus::Draft);
    assert_eq!(draft.nodes_add[0].parent_id.as_deref(), Some(OBJECTIVE));

    refused(
        &proposal(json!({
            "nodes_add": [node("parent", json!({})), node("child", json!({ "parent_id": "parent" }))]
        })),
        "VALIDATION_SCHEMA",
        "planned parent",
    );
}

#[test]
fn capability_needs_must_be_canonical_grants_that_narrow_the_proposer() {
    // A single grant object is read as a one-element list; null means no needs.
    let narrow = json!({
        "effect_class": "fs.write.workspace",
        "resource": { "kind": "fs", "selector": "/work/reports/**" },
        "constraints": { "max_tier": 1 }
    });
    let plan = compiled(&proposal(json!({
        "nodes_add": [node("n1", json!({ "capability_needs": narrow.clone() }))],
        "capability_needs": null
    })))
    .expect("a narrower need compiles");
    assert_eq!(plan.nodes_add[0].capability_needs, json!([narrow]));
    assert_eq!(plan.capability_needs, json!([]));

    let wide = json!({
        "effect_class": "fs.write.workspace",
        "resource": { "kind": "fs", "selector": "/etc/**" },
        "constraints": {}
    });
    refused(
        &proposal(json!({ "nodes_add": [node("n1", json!({ "capability_needs": [wide] }))] })),
        "CAPABILITY_DENIED",
        "fs.write.workspace",
    );
    refused(
        &proposal(json!({
            "nodes_add": [node("n1", json!({}))],
            "capability_needs": [{ "effect_class": 5 }]
        })),
        "VALIDATION_SCHEMA",
        "non-canonical grant",
    );

    let mut broken = proposer();
    broken.grants = json!([{ "effect_class": "fs.write.workspace" }]);
    let raw = parse_raw(&proposal(json!({ "nodes_add": [node("n1", json!({}))] }))).expect("raw");
    let error = compile(&raw, &graph(), &broken, LIMIT).expect_err("a broken projection");
    assert_eq!(error.code(), "INTERNAL");
    assert!(error.to_string().contains("not canonical"), "{error}");
}

/// `parent_of` runs parent -> child, the direction `work_nodes.parent_id` means (a parent
/// waits on its children): restating an existing parent is no cycle, inverting it is.
#[test]
fn parent_of_and_parent_id_share_one_direction() {
    let restated = proposal(json!({
        "edges_add": [{ "id": "e_par", "from": OBJECTIVE, "to": TASK, "kind": "parent_of" }]
    }));
    compiled(&restated).expect("the objective already is the task's parent");

    refused(
        &proposal(json!({
            "edges_add": [{ "id": "e_inv", "from": TASK, "to": OBJECTIVE, "kind": "parent_of" }]
        })),
        "VALIDATION_SCHEMA",
        "cycle",
    );
    refused(
        &proposal(json!({
            "edges_add": [{ "id": "e_up", "from": TASK, "to": OBJECTIVE, "kind": "depends_on" }]
        })),
        "VALIDATION_SCHEMA",
        "cycle",
    );
    compiled(&proposal(json!({
        "edges_add": [{ "id": "e_down", "from": OBJECTIVE, "to": TASK, "kind": "depends_on" }]
    })))
    .expect("a parent may depend on its own child");
}
