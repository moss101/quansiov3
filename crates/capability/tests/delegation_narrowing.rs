//! Delegation narrowing tests (RUN-005, DOMAIN.md §4.3, §6.3; RUN-002 seam).
//!
//! A child projection that is narrower than its parent is accepted; a widening child is
//! rejected and nothing is written. The same predicate is the one skills, tools and
//! capability packs use.

use quansio_capability::error::CapabilityError;
use quansio_capability::{
    assemble, check_narrowing, check_narrowing_grants, narrowing_violation,
    stage_delegated_projection, Approval, Constraints, Grant, Layer, ProjectionRequest,
    ProjectionRow, ProjectionSubject, WideningReason,
};
use quansio_core::{CanonicalId, CorrelationId, Prefix, UlidGenerator};
use quansio_events::{Actor, EventBatch};

mod common;
use common::{constraints, fs, grant_with, source_with_platform};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9Y0CCCCC";

fn subject() -> ProjectionSubject {
    ProjectionSubject::run("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
}

fn parent_projection() -> quansio_capability::CapabilityProjection {
    let platform = vec![grant_with(
        "read.internal",
        fs("/root/**"),
        constraints(Some(2), Approval::Ask, None, None),
    )];
    let source = source_with_platform(platform, &[("read.internal", 0)]);
    assemble(&source, &ProjectionRequest::new(subject()))
        .expect("parent projection")
        .projection
}

fn child_projection(
    parent: &quansio_capability::CapabilityProjection,
    grants: Vec<Grant>,
) -> quansio_capability::CapabilityProjection {
    let mut generator = UlidGenerator::new();
    let id = CanonicalId::generate(Prefix::CapabilityProjection, &mut generator);
    quansio_capability::CapabilityProjection::from_parts(
        id,
        ProjectionSubject::agent_thread("ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"),
        parent.inputs.clone(),
        grants,
        parent.computed_at,
        parent.expires_at,
    )
    .expect("child projection")
}

fn narrower() -> Grant {
    grant_with(
        "read.internal",
        fs("/root/a"),
        constraints(Some(1), Approval::Ask, None, None),
    )
}

fn wider() -> Grant {
    grant_with("payment.execute", fs("/root/a"), Constraints::default())
}

#[test]
fn narrower_child_is_accepted_and_its_projection_is_staged() {
    let parent = parent_projection();
    let child = child_projection(&parent, vec![narrower()]);

    assert!(check_narrowing(&parent, &child).is_ok());
    assert!(narrowing_violation(&parent.grants, &child.grants).is_none());
    assert!(check_narrowing_grants(&parent.grants, &child.grants).is_ok());

    let row = ProjectionRow::from_projection(&child, TENANT, Some(WORKSPACE.to_string()));
    let mut generator = UlidGenerator::new();
    let correlation_id = CorrelationId::generate(&mut generator);
    let mut batch = EventBatch::default();
    let draft = stage_delegated_projection(
        &mut batch,
        &parent,
        &row,
        correlation_id,
        Actor::agent("ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"),
    )
    .expect("narrowing child is staged");
    assert_eq!(batch.len(), 1);
    assert_eq!(draft.event_type.to_string(), "capability.projected");
    assert_eq!(draft.aggregate_id, child.id.to_string());
}

#[test]
fn widening_child_is_rejected_with_nothing_written() {
    let parent = parent_projection();
    let child = child_projection(&parent, vec![narrower(), wider()]);

    let error = check_narrowing(&parent, &child).expect_err("widening must be rejected");
    match error {
        CapabilityError::WideningRejected(rejection) => {
            assert_eq!(rejection.layer, Layer::Delegation);
            assert_eq!(rejection.grant, wider());
            assert_eq!(rejection.reason, WideningReason::GrantNotHeldByParent);
        }
        other => panic!("expected a widening rejection, got {other:?}"),
    }

    let row = ProjectionRow::from_projection(&child, TENANT, Some(WORKSPACE.to_string()));
    let mut generator = UlidGenerator::new();
    let correlation_id = CorrelationId::generate(&mut generator);
    let mut batch = EventBatch::default();
    let result = stage_delegated_projection(
        &mut batch,
        &parent,
        &row,
        correlation_id,
        Actor::agent("ath_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"),
    );
    assert!(result.is_err());
    assert!(batch.is_empty(), "a rejected delegation writes nothing");
}

#[test]
fn narrowing_predicate_is_reused_for_skills_and_tools() {
    let parent = vec![grant_with(
        "fs.write.workspace",
        fs("/root/**"),
        constraints(Some(3), Approval::Ask, None, None),
    )];
    let tool = vec![grant_with(
        "fs.write.workspace",
        fs("/root/out/**"),
        constraints(Some(1), Approval::Ask, None, None),
    )];
    assert!(check_narrowing_grants(&parent, &tool).is_ok());

    let pack_attempt = vec![grant_with(
        "fs.write.host",
        fs("/etc"),
        Constraints::default(),
    )];
    assert!(check_narrowing_grants(&parent, &pack_attempt).is_err());
}
