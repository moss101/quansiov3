//! AgentGraph store: delegation lineage and the RUN-005 narrowing hook (CORE-004).
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL`; absent → `BLOCKED_EXTERNAL` marker.

mod common;

use common::{blocked_marker, drop_pool, prepare, WORKSPACE, WORKSPACE_B};
use quansio_core::{CanonicalId, Prefix, UlidGenerator};
use quansio_graph::{
    AgentKind, AgentThread, AgentThreadStatus, DelegationNarrowingCheck, DelegationRequest,
    GraphError, GraphStore, NewAgentThread,
};

/// A RUN-005 stand-in: the real Capability Projection algebra runs here.
struct RejectingCheck;

impl DelegationNarrowingCheck for RejectingCheck {
    fn check(
        &self,
        _parent: &AgentThread,
        _child: &NewAgentThread,
        _delegation_capability_id: Option<&CanonicalId>,
    ) -> Result<(), GraphError> {
        Err(GraphError::NarrowingRejected(
            "run-005 narrowing rejected this delegation".to_string(),
        ))
    }
}

fn capability_id() -> CanonicalId {
    let mut generator = UlidGenerator::new();
    CanonicalId::generate(quansio_core::Prefix::CapabilityProjection, &mut generator)
}

fn agent_thread_id() -> CanonicalId {
    let mut generator = UlidGenerator::new();
    CanonicalId::generate(Prefix::AgentThread, &mut generator)
}

#[tokio::test]
async fn delegation_records_the_parent_capability_and_requires_the_parent() {
    let Some((name, pool)) = prepare("delegation").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");

    let mut parent_spec = NewAgentThread::new(WORKSPACE, AgentKind::Teammate);
    parent_spec.capability_projection_id = Some(capability_id());
    let parent = store
        .create_agent_thread(parent_spec)
        .await
        .expect("parent");

    let delegated = store
        .delegate(
            &parent.id,
            DelegationRequest::new(NewAgentThread::new(WORKSPACE, AgentKind::Worker)),
        )
        .await
        .expect("delegate");
    assert_eq!(delegated.child.parent_id, Some(parent.id));
    assert_eq!(delegated.child.status, AgentThreadStatus::Provisioned);
    assert_eq!(delegated.edge.parent_agent_thread_id, parent.id);
    assert_eq!(delegated.edge.child_agent_thread_id, delegated.child.id);
    assert_eq!(
        delegated.edge.delegation_capability_id, parent.capability_projection_id,
        "the child records the parent's delegation capability id"
    );
    assert_eq!(
        store
            .list_delegations(WORKSPACE)
            .await
            .expect("edges")
            .len(),
        1
    );

    // An unknown parent is rejected, and nothing is written.
    let error = store
        .delegate(
            &agent_thread_id(),
            DelegationRequest::new(NewAgentThread::new(WORKSPACE, AgentKind::Worker)),
        )
        .await
        .expect_err("unknown parent");
    assert!(
        matches!(error, GraphError::ParentNotFound { .. }),
        "{error:?}"
    );
    assert_eq!(error.code(), "NOT_FOUND");

    // A child in another workspace is rejected even when the parent exists.
    let error = store
        .delegate(
            &parent.id,
            DelegationRequest::new(NewAgentThread::new(WORKSPACE_B, AgentKind::Worker)),
        )
        .await
        .expect_err("workspace mismatch");
    assert!(
        matches!(error, GraphError::WorkspaceMismatch { .. }),
        "{error:?}"
    );

    // The structural rule refuses a delegation capability the parent does not hold.
    let mut request = DelegationRequest::new(NewAgentThread::new(WORKSPACE, AgentKind::Worker));
    request.delegation_capability_id = Some(capability_id());
    let error = store
        .delegate(&parent.id, request)
        .await
        .expect_err("widening rejected");
    assert!(
        matches!(error, GraphError::NarrowingRejected(_)),
        "{error:?}"
    );
    assert_eq!(
        store
            .list_delegations(WORKSPACE)
            .await
            .expect("edges")
            .len(),
        1
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn the_narrowing_hook_is_invoked_and_rejection_writes_nothing() {
    let Some((name, pool)) = prepare("narrowing").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");
    let parent = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Teammate))
        .await
        .expect("parent");
    let head_before = store.graph_revision(WORKSPACE).await.expect("head");

    let error = store
        .delegate_with(
            &RejectingCheck,
            &parent.id,
            DelegationRequest::new(NewAgentThread::new(WORKSPACE, AgentKind::Worker)),
        )
        .await
        .expect_err("hook rejects");
    match error {
        GraphError::NarrowingRejected(message) => {
            assert!(message.contains("run-005"), "hook message: {message}");
        }
        other => panic!("unexpected error {other:?}"),
    }

    assert_eq!(
        store
            .list_agent_threads(WORKSPACE)
            .await
            .expect("threads")
            .len(),
        1,
        "the rejected child must not exist"
    );
    assert!(store
        .list_delegations(WORKSPACE)
        .await
        .expect("edges")
        .is_empty());
    assert_eq!(
        store.graph_revision(WORKSPACE).await.expect("head"),
        head_before,
        "a rejected delegation does not advance the graph revision"
    );

    drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn teammates_never_join_and_workers_do() {
    let Some((name, pool)) = prepare("joining").await else {
        blocked_marker();
        return;
    };
    let store = GraphStore::new(pool.clone(), common::TENANT).expect("store");

    let teammate = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Teammate))
        .await
        .expect("teammate");
    let active = store
        .transition_agent_thread(&teammate.id, AgentThreadStatus::Active)
        .await
        .expect("active");
    let joining = store
        .transition_agent_thread(&active.id, AgentThreadStatus::Joining)
        .await
        .expect("joining");
    let error = store
        .transition_agent_thread(&joining.id, AgentThreadStatus::Joined)
        .await
        .expect_err("teammate must not join");
    assert!(
        matches!(error, GraphError::IllegalTransition { .. }),
        "{error:?}"
    );
    assert_eq!(
        store
            .get_agent_thread(&joining.id)
            .await
            .expect("thread")
            .status,
        AgentThreadStatus::Joining
    );

    let worker = store
        .create_agent_thread(NewAgentThread::new(WORKSPACE, AgentKind::Worker))
        .await
        .expect("worker");
    let active = store
        .transition_agent_thread(&worker.id, AgentThreadStatus::Active)
        .await
        .expect("active");
    let joining = store
        .transition_agent_thread(&active.id, AgentThreadStatus::Joining)
        .await
        .expect("joining");
    let joined = store
        .transition_agent_thread(&joining.id, AgentThreadStatus::Joined)
        .await
        .expect("worker joins");
    assert_eq!(joined.status, AgentThreadStatus::Joined);

    // A teammate definition id must carry the `agt_` prefix.
    let mut bad = NewAgentThread::new(WORKSPACE, AgentKind::Teammate);
    bad.definition_id = Some(capability_id());
    let error = store
        .create_agent_thread(bad)
        .await
        .expect_err("bad prefix");
    assert!(matches!(error, GraphError::InvalidId { .. }), "{error:?}");
    assert_eq!(Prefix::Teammate.as_str(), "agt_");

    drop_pool(&pool, &name).await;
}
