//! Resource selector parsing and containment for every kind (DOMAIN.md §6.1).
//!
//! Containment is conservative: `covers` answers `true` only when every resource the narrower
//! selector matches is provably matched by the wider one, because a `false` is treated as a
//! widening attempt and an unproven `true` would grant more than was held.

use quansio_capability::ResourceSelector;

fn sel(kind: &str, selector: &str) -> ResourceSelector {
    ResourceSelector::from_parts(kind, selector, &[]).expect("valid selector")
}

fn domain(host: &str, ports: &[u16]) -> ResourceSelector {
    ResourceSelector::from_parts("domain", host, ports).expect("valid domain")
}

#[test]
fn every_kind_round_trips_its_wire_form() {
    for (kind, selector) in [
        ("fs", "/root/**"),
        ("domain", "*.example.com"),
        ("connector", "gmail/inbox/*"),
        ("app", "com.example.Mail"),
        ("artifact", "report*"),
        ("model", "fast/*"),
        ("work", "wn_1/**"),
        ("secret", "sec_01J8Z3K6F1N8VQ2X5W9Y0CCCCC"),
        ("target", "tgt_1/**"),
    ] {
        let parsed = sel(kind, selector);
        assert_eq!(parsed.kind(), kind);
        assert_eq!(parsed.selector(), selector);
    }
}

#[test]
fn malformed_selectors_are_refused() {
    for (kind, selector) in [
        ("fs", ""),
        ("domain", "https://example.com"),
        ("domain", "example.com/path"),
        ("gopher", "anything"),
    ] {
        let error = ResourceSelector::from_parts(kind, selector, &[]).expect_err(selector);
        assert_eq!(error.code(), "VALIDATION_SCHEMA", "{kind}:{selector}");
    }
}

#[test]
fn different_kinds_never_contain_each_other() {
    assert!(!sel("fs", "**").covers(&sel("artifact", "report")));
    assert!(!sel("secret", "*").covers(&sel("target", "tgt_1")));
    assert!(!sel("app", "*").overlaps(&sel("model", "fast")));
}

#[test]
fn apps_and_secrets_are_exact_or_everything() {
    assert!(sel("app", "com.example.Mail").covers(&sel("app", "com.example.Mail")));
    assert!(sel("app", "*").covers(&sel("app", "com.example.Mail")));
    assert!(!sel("app", "com.example.Mail").covers(&sel("app", "com.example.Notes")));
    assert!(
        !sel("app", "com.example.*").covers(&sel("app", "com.example.Mail")),
        "no globbing"
    );
    assert!(sel("secret", "*").covers(&sel("secret", "sec_a")));
    assert!(!sel("secret", "sec_a").covers(&sel("secret", "sec_b")));
    assert!(
        !sel("secret", "sec_a").covers(&sel("secret", "*")),
        "narrower may not be wider"
    );
}

#[test]
fn artifacts_and_models_contain_by_glob() {
    assert!(sel("artifact", "report*").covers(&sel("artifact", "report-q3")));
    assert!(!sel("artifact", "report*").covers(&sel("artifact", "notes")));
    assert!(sel("model", "fast/*").covers(&sel("model", "fast/haiku")));
    assert!(!sel("model", "fast/*").covers(&sel("model", "deep/opus")));
    assert!(sel("model", "*/?aiku").covers(&sel("model", "fast/haiku")));
}

#[test]
fn work_and_target_selectors_contain_subtrees() {
    assert!(sel("work", "*").covers(&sel("work", "wn_1/child")));
    assert!(sel("work", "wn_1/**").covers(&sel("work", "wn_1")));
    assert!(sel("work", "wn_1/**").covers(&sel("work", "wn_1/a/b")));
    assert!(
        !sel("work", "wn_1/**").covers(&sel("work", "wn_10")),
        "a sibling sharing a prefix"
    );
    assert!(sel("work", "wn_1").covers(&sel("work", "wn_1/a")));
    assert!(!sel("work", "wn_1").covers(&sel("work", "wn_2/a")));
    assert!(sel("target", "tgt_1").covers(&sel("target", "tgt_1")));
    assert!(sel("target", "*").covers(&sel("target", "tgt_9")));
    assert!(sel("target", "tgt_1/**").covers(&sel("target", "tgt_1/desktop")));
    assert!(!sel("target", "tgt_1/**").covers(&sel("target", "tgt_2")));
}

#[test]
fn domain_ports_an_empty_set_means_every_port() {
    assert!(domain("api.example.com", &[]).covers(&domain("api.example.com", &[443])));
    assert!(domain("api.example.com", &[]).covers(&domain("api.example.com", &[])));
    assert!(
        !domain("api.example.com", &[443]).covers(&domain("api.example.com", &[])),
        "a port list never contains every port"
    );
    assert!(domain("api.example.com", &[80, 443]).covers(&domain("api.example.com", &[443])));
    assert!(!domain("api.example.com", &[443]).covers(&domain("api.example.com", &[443, 8443])));
}

#[test]
fn wildcard_segments_nest_only_when_provable() {
    let fs = |glob: &str| sel("fs", glob);
    assert!(fs("/root/*.md").covers(&fs("/root/notes.md")));
    assert!(fs("/root/a?c").covers(&fs("/root/abc")));
    assert!(!fs("/root/a?c").covers(&fs("/root/abbc")));
    assert!(
        fs("/root/*x*y").covers(&fs("/root/axbxcy")),
        "backtracks past a false start"
    );
    assert!(!fs("/root/*x*y").covers(&fs("/root/axbxc")));
    assert!(
        fs("/root/**").covers(&fs("/root/a/*.md")),
        "** covers any deeper pattern"
    );
    assert!(
        !fs("/root/*.md").covers(&fs("/root/*.txt")),
        "two different wildcard patterns are never proven to nest"
    );
    assert!(
        !fs("/root/a").covers(&fs("/root/*")),
        "a literal never covers a wildcard"
    );
    assert!(fs("/root/a").overlaps(&fs("/root/*")), "but they overlap");
    assert!(
        !fs("/root/a/b").covers(&fs("/root/a")),
        "a deeper path does not cover its parent"
    );
}
