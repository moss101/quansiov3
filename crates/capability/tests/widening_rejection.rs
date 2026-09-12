//! Widening-rejection tests (RUN-005, DOMAIN.md §6.3).
//!
//! A layer may only remove or narrow. A lower layer that adds a grant the layer above
//! does not hold is ignored and reported exactly once as a `capability.widening_rejected`
//! event naming the offending layer.

use quansio_capability::{
    assemble, stage_assembly_rejections, stage_widening_rejected_event, Approval, Layer,
    LayerInput, ProjectionRequest, ProjectionSubject, WideningReason,
};
use quansio_core::{CorrelationId, UlidGenerator};
use quansio_events::{Actor, EventBatch};

mod common;
use common::{connector, constraints, fs, grant, grant_with, source_with_platform};

fn subject() -> ProjectionSubject {
    ProjectionSubject::run("run_01J8Z3K6F1N8VQ2X5W9Y0CCCCC")
}

#[test]
fn lower_layer_adding_a_grant_is_ignored_and_recorded_once() {
    let platform = vec![grant_with(
        "message.send",
        connector("cnx_01J8Z3K6F1N8VQ2X5W9Y0CCCCC", "channel:#eng-*"),
        constraints(Some(3), Approval::Ask, None, None),
    )];
    let added = grant_with(
        "payment.execute",
        fs("/root/a"),
        constraints(Some(4), Approval::Ask, None, None),
    );
    let mut lower = platform.clone();
    lower.push(added.clone());

    let source = source_with_platform(
        platform.clone(),
        &[("message.send", 3), ("payment.execute", 4)],
    )
    .with(
        Layer::AgentDefinition,
        LayerInput::grants("teammate/agt_01J8Z3K6F1N8VQ2X5W9Y0CCCCC/1", lower),
    );

    let assembly = assemble(&source, &ProjectionRequest::new(subject())).expect("projection");

    assert_eq!(
        assembly.projection.grants, platform,
        "the attempted addition must be ignored"
    );
    assert_eq!(assembly.rejections.len(), 1);
    let rejection = &assembly.rejections[0];
    assert_eq!(rejection.layer, Layer::AgentDefinition);
    assert_eq!(rejection.grant, added);
    assert_eq!(rejection.reason, WideningReason::NotNarrowerThanAnyInput);

    let mut generator = UlidGenerator::new();
    let correlation_id = CorrelationId::generate(&mut generator);
    let actor = Actor::system("capability_test");
    let mut batch = EventBatch::default();
    let recorded = stage_assembly_rejections(&mut batch, &assembly, correlation_id, actor.clone());
    assert_eq!(recorded, 1, "one event per rejected widening attempt");
    assert_eq!(
        batch.len(),
        1,
        "exactly one capability.widening_rejected event"
    );

    let mut inspection = EventBatch::default();
    let draft = stage_widening_rejected_event(
        &mut inspection,
        &assembly.projection.id,
        rejection,
        correlation_id,
        actor,
    );
    assert_eq!(draft.event_type.to_string(), "capability.widening_rejected");
    assert_eq!(draft.payload["layer"], "agent_definition");
    assert_eq!(draft.payload["grant"]["effect_class"], "payment.execute");
    assert_eq!(
        draft.payload["projection_id"],
        assembly.projection.id.to_string()
    );
}

#[test]
fn lower_layer_trying_a_wider_selector_never_widens() {
    let platform = vec![grant("read.internal", fs("/root/a"))];
    let wider = grant("read.internal", fs("/root/**"));

    let source = source_with_platform(platform.clone(), &[("read.internal", 0)]).with(
        Layer::ActiveSkills,
        LayerInput::grants(
            "skill/skl_01J8Z3K6F1N8VQ2X5W9Y0CCCCC/1",
            vec![wider.clone()],
        ),
    );

    let assembly = assemble(&source, &ProjectionRequest::new(subject())).expect("projection");

    assert_eq!(assembly.rejections.len(), 1);
    assert_eq!(assembly.rejections[0].layer, Layer::ActiveSkills);
    assert_eq!(assembly.rejections[0].grant, wider);
    for grant in &assembly.projection.grants {
        assert!(
            platform.iter().any(|held| grant.is_narrowing_of(held)),
            "a projected grant must not exceed the platform baseline"
        );
    }
}

#[test]
fn lower_layer_narrowing_is_accepted() {
    let platform = vec![grant("read.internal", fs("/root/**"))];
    let narrower = grant("read.internal", fs("/root/a"));

    let source = source_with_platform(platform, &[("read.internal", 0)]).with(
        Layer::ToolDeclaration,
        LayerInput::grants("tool/fs.read/1", vec![narrower.clone()]),
    );

    let assembly = assemble(&source, &ProjectionRequest::new(subject())).expect("projection");

    assert!(assembly.rejections.is_empty());
    assert_eq!(assembly.projection.grants, vec![narrower]);
}
