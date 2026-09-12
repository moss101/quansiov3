//! File-backed SearchIndex tests: channels, rebuild determinism, tenant isolation,
//! incremental maintenance, budgets and provenance.
//!
//! The index files live in a local temporary directory (`tempfile`). No external
//! boundary is required; the PostgreSQL-backed rebuild path is tested separately in
//! `postgres_rebuild.rs`.

use quansio_indexer::{
    extract_symbols, Channel, IndexError, SearchBudget, SearchFilters, SearchIndex, SearchProgram,
    Snapshot, SourceChange, SourceDocument,
};
use tempfile::TempDir;

const TENANT_A: &str = "tn_alpha";
const TENANT_B: &str = "tn_beta";
const WORKSPACE_A: &str = "ws_alpha";
const WORKSPACE_B: &str = "ws_other";

fn document(
    tenant: &str,
    workspace: &str,
    artifact: &str,
    version: &str,
    title: &str,
    body: &str,
) -> SourceDocument {
    SourceDocument::artifact(
        tenant,
        workspace,
        artifact,
        version,
        title,
        body,
        "text/markdown",
        format!("digest-{version}"),
    )
    .with_artifact_kind("document")
}

fn index() -> (TempDir, SearchIndex) {
    let directory = TempDir::new().expect("temp dir");
    let index = SearchIndex::open(directory.path()).expect("open index");
    (directory, index)
}

fn program(tenant: &str, query: &str) -> SearchProgram {
    SearchProgram::new(quansio_indexer::SearchScope::tenant(tenant), query)
}

#[test]
fn exact_lexical_and_symbol_channels_round_trip_real_content() {
    let (_directory, index) = index();
    let revenue = document(
        TENANT_A,
        WORKSPACE_A,
        "art_revenue",
        "artv_revenue1",
        "Quarterly revenue report",
        "The quarterly revenue grew twelve percent. Call compute_summary for the breakdown.",
    );
    let checklist = document(
        TENANT_A,
        WORKSPACE_A,
        "art_checklist",
        "artv_checklist1",
        "Accessibility checklist",
        "Keyboard navigation must work in every panel.",
    );
    let rebuild = index
        .rebuild(TENANT_A, &[revenue.clone(), checklist.clone()])
        .expect("rebuild");
    assert_eq!(rebuild.document_count, 2);

    let lexical = index
        .search(&program(TENANT_A, "keyboard").with_channels(vec![Channel::Lexical]))
        .expect("lexical search");
    assert_eq!(lexical.results.len(), 1);
    assert_eq!(lexical.results[0].source_id, "art_checklist");

    let exact = index
        .search(&program(TENANT_A, "revenue").with_channels(vec![Channel::Exact]))
        .expect("exact search");
    assert_eq!(exact.results.len(), 1);
    assert_eq!(exact.results[0].source_id, "art_revenue");

    let symbols = index
        .search(&program(TENANT_A, "compute_summary").with_channels(vec![Channel::Symbol]))
        .expect("symbol search");
    assert_eq!(symbols.results.len(), 1);
    assert_eq!(symbols.results[0].source_id, "art_revenue");

    // Exact matching is whole-token: a prefix is not a match.
    let prefix = index
        .search(&program(TENANT_A, "revenu").with_channels(vec![Channel::Exact]))
        .expect("exact prefix search");
    assert!(prefix.results.is_empty());

    assert!(extract_symbols("call compute_summary() now").contains(&"compute_summary".to_string()));
}

#[test]
fn provenance_carries_source_locator_and_snapshot() {
    let (_directory, index) = index();
    let seed = document(
        TENANT_A,
        WORKSPACE_A,
        "art_source",
        "artv_source1",
        "Source document",
        "The observable marker appears exactly once.",
    );
    let rebuild = index.rebuild(TENANT_A, &[seed]).expect("rebuild");
    let results = index
        .search(&program(TENANT_A, "observable"))
        .expect("search");
    let hit = results.results.first().expect("one hit");
    assert_eq!(hit.source_id, "art_source");
    assert_eq!(hit.locator, "artifact_version:artv_source1");
    assert_eq!(hit.snapshot, rebuild.snapshot.as_string());
    assert_eq!(hit.snapshot, results.snapshot.as_string());
    assert_eq!(hit.content_digest, "digest-artv_source1");
    assert_eq!(hit.media_type, "text/markdown");
    assert_eq!(
        Snapshot::parse(&hit.snapshot).expect("snapshot parses"),
        results.snapshot
    );
}

#[test]
fn rebuild_is_deterministic_for_a_fixed_corpus() {
    let (_directory, index) = index();
    let corpus = vec![
        document(
            TENANT_A,
            WORKSPACE_A,
            "art_a",
            "artv_a1",
            "Alpha notes",
            "shared alpha body",
        ),
        document(
            TENANT_A,
            WORKSPACE_A,
            "art_b",
            "artv_b1",
            "Beta notes",
            "shared beta body",
        ),
        document(
            TENANT_A,
            WORKSPACE_A,
            "art_c",
            "artv_c1",
            "Gamma notes",
            "shared gamma body",
        ),
    ];
    let first = index.rebuild(TENANT_A, &corpus).expect("first rebuild");
    let before = index
        .search(&program(TENANT_A, "shared"))
        .expect("search before");

    let second = index.rebuild(TENANT_A, &corpus).expect("second rebuild");
    let after = index
        .search(&program(TENANT_A, "shared"))
        .expect("search after");

    assert_eq!(
        first.snapshot, second.snapshot,
        "identical corpus keeps the epoch"
    );
    assert_eq!(before.results, after.results, "same hits after rebuild");

    // A changed corpus advances the epoch and is reflected in the snapshot.
    let mut changed = corpus.clone();
    changed.push(document(
        TENANT_A,
        WORKSPACE_A,
        "art_d",
        "artv_d1",
        "Delta notes",
        "shared delta body",
    ));
    let third = index.rebuild(TENANT_A, &changed).expect("third rebuild");
    assert!(third.snapshot.epoch() > second.snapshot.epoch());
    let after_change = index
        .search(&program(TENANT_A, "shared"))
        .expect("search changed");
    assert_eq!(after_change.results.len(), 4);
}

#[test]
fn tenant_isolation_never_returns_another_tenants_documents() {
    let (_directory, index) = index();
    let alpha = document(
        TENANT_A,
        WORKSPACE_A,
        "art_alpha",
        "artv_alpha1",
        "Alpha orbital notes",
        "orbital mechanics for tenant alpha",
    );
    let beta = document(
        TENANT_B,
        WORKSPACE_A,
        "art_beta",
        "artv_beta1",
        "Beta orbital notes",
        "orbital mechanics for tenant beta",
    );
    index.rebuild(TENANT_A, &[alpha]).expect("rebuild alpha");
    index.rebuild(TENANT_B, &[beta]).expect("rebuild beta");

    let alpha_hits = index
        .search(&program(TENANT_A, "orbital"))
        .expect("alpha search");
    assert_eq!(alpha_hits.results.len(), 1);
    assert_eq!(alpha_hits.results[0].source_id, "art_alpha");

    let beta_hits = index
        .search(&program(TENANT_B, "orbital"))
        .expect("beta search");
    assert_eq!(beta_hits.results.len(), 1);
    assert_eq!(beta_hits.results[0].source_id, "art_beta");

    // Cross-tenant writes are refused outright.
    let foreign = document(
        TENANT_B,
        WORKSPACE_A,
        "art_foreign",
        "artv_foreign1",
        "foreign",
        "x",
    );
    let error = index
        .rebuild(TENANT_A, &[foreign])
        .expect_err("cross-tenant rebuild");
    assert!(matches!(error, IndexError::CrossTenantDocument { .. }));
}

#[test]
fn incremental_add_update_delete_is_idempotent() {
    let (_directory, index) = index();
    let first = document(
        TENANT_A,
        WORKSPACE_A,
        "art_i",
        "artv_i1",
        "First",
        "alpha content",
    );
    index.rebuild(TENANT_A, &[first]).expect("rebuild");

    // Update the same version to new text (a changed digest marks a real change).
    let updated = SourceDocument::artifact(
        TENANT_A,
        WORKSPACE_A,
        "art_i",
        "artv_i1",
        "First",
        "beta content",
        "text/markdown",
        "digest-artv_i1-v2",
    )
    .with_artifact_kind("document");
    let outcome = index
        .apply(SourceChange::upsert(updated.clone()))
        .expect("update");
    assert!(outcome.applied);
    let updated_snapshot = outcome.snapshot;
    assert_eq!(
        index
            .search(&program(TENANT_A, "beta"))
            .expect("search beta")
            .results
            .len(),
        1
    );
    assert!(index
        .search(&program(TENANT_A, "alpha"))
        .expect("search alpha")
        .results
        .is_empty());

    // Re-applying the identical change is a no-op and does not advance the epoch.
    let replay = index
        .apply(SourceChange::upsert(updated))
        .expect("idempotent update");
    assert!(!replay.applied);
    assert_eq!(replay.snapshot, updated_snapshot);

    // Add a new version.
    let added = document(
        TENANT_A,
        WORKSPACE_A,
        "art_j",
        "artv_j1",
        "Second",
        "gamma content",
    );
    assert!(
        index
            .apply(SourceChange::upsert(added))
            .expect("add")
            .applied
    );
    assert_eq!(
        index
            .search(&program(TENANT_A, "gamma"))
            .expect("search gamma")
            .results
            .len(),
        1
    );

    // Delete the version and prove the terms stop matching.
    let deleted = index
        .apply(SourceChange::delete(TENANT_A, "artv_i1"))
        .expect("delete");
    assert!(deleted.applied);
    assert!(index
        .search(&program(TENANT_A, "beta"))
        .expect("search beta after delete")
        .results
        .is_empty());
    assert_eq!(index.document_count(TENANT_A).expect("count"), 1);

    // Deleting an absent version is a no-op.
    let replay_delete = index
        .apply(SourceChange::delete(TENANT_A, "artv_i1"))
        .expect("idempotent delete");
    assert!(!replay_delete.applied);
    assert_eq!(replay_delete.snapshot, deleted.snapshot);
}

#[test]
fn budget_truncation_is_reported_and_oversized_requests_fail_closed() {
    let (_directory, index) = index();
    let corpus: Vec<SourceDocument> = (0..5)
        .map(|number| {
            document(
                TENANT_A,
                WORKSPACE_A,
                &format!("art_b{number}"),
                &format!("artv_b{number}"),
                &format!("Budget document {number}"),
                "shared term in every document of this corpus",
            )
        })
        .collect();
    index.rebuild(TENANT_A, &corpus).expect("rebuild");

    let limited = index
        .search(&program(TENANT_A, "shared").with_budget(SearchBudget::new(2, 4_096)))
        .expect("limited search");
    assert_eq!(limited.results.len(), 2);
    assert_eq!(limited.total_matches, 5);
    assert!(limited.truncation.results_truncated);
    assert_eq!(limited.truncation.dropped_results, 3);

    let token_limited = index
        .search(&program(TENANT_A, "shared").with_budget(SearchBudget::new(10, 2)))
        .expect("token-limited search");
    assert!(token_limited.results.is_empty());
    assert!(token_limited.truncation.tokens_truncated);

    let too_many =
        index.search(&program(TENANT_A, "shared").with_budget(SearchBudget::new(0, 4_096)));
    assert!(matches!(
        too_many,
        Err(IndexError::BoundsExceeded {
            field: "max_results",
            ..
        })
    ));
    let over_results = index.search(
        &program(TENANT_A, "shared")
            .with_budget(SearchBudget::new(SearchBudget::MAX_RESULTS + 1, 4_096)),
    );
    assert!(matches!(
        over_results,
        Err(IndexError::BoundsExceeded {
            field: "max_results",
            ..
        })
    ));
    let over_tokens = index.search(
        &program(TENANT_A, "shared")
            .with_budget(SearchBudget::new(10, SearchBudget::MAX_TOKENS + 1)),
    );
    assert!(matches!(
        over_tokens,
        Err(IndexError::BoundsExceeded {
            field: "max_tokens",
            ..
        })
    ));
}

#[test]
fn semantic_and_unowned_channels_are_rejected_with_a_typed_error() {
    let (_directory, index) = index();
    index
        .rebuild(
            TENANT_A,
            &[document(
                TENANT_A,
                WORKSPACE_A,
                "art_a",
                "artv_a1",
                "A",
                "body",
            )],
        )
        .expect("rebuild");
    for channel in [
        Channel::Semantic,
        Channel::Graph,
        Channel::History,
        Channel::Memory,
    ] {
        let expected = channel.as_str();
        let error = index
            .search(&program(TENANT_A, "body").with_channels(vec![channel]))
            .expect_err("unowned channel");
        assert!(matches!(
            error,
            IndexError::ChannelNotAvailable { ref channel, .. } if channel.as_str() == expected
        ));
    }
    let empty = index
        .search(&program(TENANT_A, "body").with_channels(vec![]))
        .expect_err("empty channels");
    assert!(matches!(empty, IndexError::NoChannels));
}

#[test]
fn typed_filters_and_workspace_scope_narrow_results() {
    let (_directory, index) = index();
    let markdown = document(
        TENANT_A,
        WORKSPACE_A,
        "art_md",
        "artv_md1",
        "Markdown source",
        "shared marker in markdown",
    );
    let json = SourceDocument::artifact(
        TENANT_A,
        WORKSPACE_A,
        "art_json",
        "artv_json1",
        "JSON source",
        "shared marker in json",
        "application/json",
        "digest-artv_json1",
    )
    .with_artifact_kind("data");
    let other_workspace = document(
        TENANT_A,
        WORKSPACE_B,
        "art_other",
        "artv_other1",
        "Other workspace",
        "shared marker in other workspace",
    );
    index
        .rebuild(TENANT_A, &[markdown, json, other_workspace])
        .expect("rebuild");

    let filtered = index
        .search(
            &program(TENANT_A, "shared")
                .with_filters(SearchFilters::none().media_type("application/json")),
        )
        .expect("filtered search");
    assert_eq!(filtered.results.len(), 1);
    assert_eq!(filtered.results[0].source_id, "art_json");

    let mut scoped = SearchProgram::new(
        quansio_indexer::SearchScope::workspace(TENANT_A, WORKSPACE_B),
        "shared",
    );
    scoped = scoped.with_channels(vec![Channel::Lexical]);
    let workspace_hits = index.search(&scoped).expect("workspace search");
    assert_eq!(workspace_hits.results.len(), 1);
    assert_eq!(workspace_hits.results[0].source_id, "art_other");
}

#[test]
fn stale_snapshot_is_detected() {
    let (_directory, index) = index();
    let first = index
        .rebuild(
            TENANT_A,
            &[document(
                TENANT_A,
                WORKSPACE_A,
                "art_a",
                "artv_a1",
                "A",
                "first body",
            )],
        )
        .expect("first rebuild");
    let stale = index
        .rebuild(
            TENANT_A,
            &[document(
                TENANT_A,
                WORKSPACE_A,
                "art_b",
                "artv_b1",
                "B",
                "second body",
            )],
        )
        .expect("second rebuild");
    let error = index
        .search(&program(TENANT_A, "second").with_snapshot(Some(first.snapshot)))
        .expect_err("stale snapshot");
    assert!(matches!(error, IndexError::StaleSnapshot { .. }));
    let current = index
        .search(&program(TENANT_A, "second").with_snapshot(Some(stale.snapshot)))
        .expect("current snapshot");
    assert_eq!(current.results.len(), 1);
}

#[test]
fn searching_an_unrebuilt_tenant_returns_an_empty_set() {
    let (_directory, index) = index();
    let results = index
        .search(&program("tn_missing", "anything"))
        .expect("empty search");
    assert!(results.results.is_empty());
    assert_eq!(results.total_matches, 0);
    assert!(!results.truncation.is_truncated());
}
