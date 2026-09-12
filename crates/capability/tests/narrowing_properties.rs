//! Narrowing property tests (RUN-005, DOMAIN.md §6.3).
//!
//! Composition is intersection with most-restrictive constraints: composing any two
//! grant sets never yields a grant that is not a narrowing of a grant present in each
//! input, and the constraint laws are asserted over a table of cases. The generator is a
//! fixed-seed linear congruential sequence, so the properties are deterministic.

use chrono::Duration;
use quansio_capability::{
    narrow, Approval, BudgetRef, Constraints, Grant, Layer, ResourceSelector,
};

mod common;
use common::{constraints, fs, grant_with};

const CLASSES: [&str; 3] = ["read.internal", "fs.write.workspace", "message.send"];
const GLOBS: [&str; 6] = [
    "/root",
    "/root/**",
    "/root/a",
    "/root/a/b",
    "/root/*",
    "/srv",
];
const TIERS: [Option<u8>; 5] = [None, Some(0), Some(1), Some(3), Some(4)];
const APPROVALS: [Approval; 3] = [Approval::Ask, Approval::Always, Approval::Never];

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() >> 33) as usize % bound
    }

    fn grant(&mut self) -> Grant {
        let class = CLASSES[self.below(CLASSES.len())];
        let resource = fs(GLOBS[self.below(GLOBS.len())]);
        let max_tier = TIERS[self.below(TIERS.len())];
        let approval = APPROVALS[self.below(APPROVALS.len())];
        let expires_at = match self.below(3) {
            0 => None,
            1 => Some(common::at("2026-06-01T00:00:00Z") + Duration::hours(1)),
            _ => Some(common::at("2026-06-01T00:00:00Z") + Duration::hours(2)),
        };
        let budget_ref = match self.below(3) {
            0 => None,
            1 => Some(BudgetRef::new("bud_01J8Z3K6F1N8VQ2X5W9Y0AAAAA")),
            _ => Some(BudgetRef {
                reference: "bud_01J8Z3K6F1N8VQ2X5W9Y0BBBBB".to_string(),
                cap_units: Some(50),
            }),
        };
        let constraints = constraints(max_tier, approval, expires_at, budget_ref);
        grant_with(class, resource, constraints)
    }

    fn grant_set(&mut self, max_len: usize) -> Vec<Grant> {
        let len = 1 + self.below(max_len);
        let mut set: Vec<Grant> = Vec::new();
        for _ in 0..len {
            let candidate = self.grant();
            // One grant per (effect class, resource); duplicate regions with different
            // constraints are what `push_most_restrictive` folds, which is exercised by
            // the composition properties below rather than by this generator.
            if !set.iter().any(|grant| {
                grant.effect_class == candidate.effect_class && grant.resource == candidate.resource
            }) {
                set.push(candidate);
            }
        }
        set
    }
}

#[test]
fn composed_grants_are_narrowings_of_both_inputs() {
    let mut rng = Lcg::new(0x5AA5_1234_ABCD_0001);
    for _ in 0..500 {
        let held = rng.grant_set(4);
        let candidates = rng.grant_set(4);
        let (composed, _) = narrow(&held, &candidates, Layer::Delegation);
        for grant in &composed {
            assert!(
                candidates
                    .iter()
                    .any(|candidate| grant.is_narrowing_of(candidate)),
                "composed grant {grant} is absent from the candidate input"
            );
            assert!(
                held.iter().any(|existing| grant.is_narrowing_of(existing)),
                "composed grant {grant} is absent from the held input"
            );
        }
    }
}

#[test]
fn narrow_never_produces_a_grant_outside_its_held_input() {
    let mut rng = Lcg::new(0x5AA5_1234_ABCD_0002);
    for _ in 0..200 {
        let held = rng.grant_set(4);
        let (composed, rejections) = narrow(&held, &held, Layer::Delegation);
        assert!(rejections.is_empty());
        for grant in &composed {
            assert!(
                held.iter().any(|existing| grant.is_narrowing_of(existing)),
                "intersecting a set with itself must not produce new authority"
            );
        }
    }
}

#[test]
fn narrow_is_exactly_idempotent_for_disjoint_regions() {
    let held = vec![
        grant_with("read.internal", fs("/srv/a"), Constraints::default()),
        grant_with("read.internal", fs("/opt/b"), Constraints::default()),
        grant_with("message.send", fs("/srv/c"), Constraints::default()),
    ];
    let (composed, rejections) = narrow(&held, &held, Layer::Delegation);
    assert_eq!(composed, held);
    assert!(rejections.is_empty());
}

#[test]
fn narrow_rejects_candidates_absent_from_the_input() {
    let mut rng = Lcg::new(0x5AA5_1234_ABCD_0003);
    for _ in 0..200 {
        let held = rng.grant_set(4);
        // `payment.execute` is outside the generated universe, so no held grant can
        // contain it: it is a pure widening attempt.
        let extra = grant_with("payment.execute", fs("/root/a"), Constraints::default());
        let mut candidates = held.clone();
        candidates.push(extra.clone());
        let (composed, rejections) = narrow(&held, &candidates, Layer::AgentDefinition);
        assert!(
            composed
                .iter()
                .all(|grant| grant.effect_class != extra.effect_class),
            "the widening attempt must not appear in the result"
        );
        assert!(
            composed
                .iter()
                .all(|grant| held.iter().any(|existing| grant.is_narrowing_of(existing))),
            "every composed grant must remain within the held authority"
        );
        assert_eq!(rejections.len(), 1);
        assert_eq!(rejections[0].layer, Layer::AgentDefinition);
        assert_eq!(rejections[0].grant, extra);
        assert_eq!(
            rejections[0].reason,
            quansio_capability::WideningReason::NotNarrowerThanAnyInput
        );
    }
}

#[test]
fn constraints_combine_most_restrictively() {
    let early = common::at("2026-01-01T00:00:00Z");
    let late = common::at("2027-01-01T00:00:00Z");
    let small = BudgetRef {
        reference: "bud_small".to_string(),
        cap_units: Some(10),
    };
    let large = BudgetRef {
        reference: "bud_large".to_string(),
        cap_units: Some(1000),
    };

    let cases = [
        (
            constraints(Some(3), Approval::Always, Some(late), Some(large.clone())),
            constraints(Some(1), Approval::Ask, Some(early), Some(small.clone())),
            constraints(Some(1), Approval::Ask, Some(early), Some(small.clone())),
        ),
        (
            constraints(None, Approval::Always, None, None),
            constraints(Some(2), Approval::Never, Some(late), Some(small.clone())),
            constraints(Some(2), Approval::Never, Some(late), Some(small.clone())),
        ),
        (
            constraints(Some(4), Approval::Ask, Some(early), None),
            constraints(Some(4), Approval::Always, Some(late), None),
            constraints(Some(4), Approval::Ask, Some(early), None),
        ),
        (
            constraints(None, Approval::Never, None, None),
            constraints(None, Approval::Always, None, Some(small.clone())),
            constraints(None, Approval::Never, None, Some(small.clone())),
        ),
    ];

    for (first, second, expected) in cases {
        assert_eq!(Constraints::combine(&first, &second), expected);
        assert_eq!(Constraints::combine(&second, &first), expected);
    }
}

#[test]
fn approval_only_narrows_toward_ask_or_never() {
    assert_eq!(
        Approval::Always.most_restrictive(Approval::Ask),
        Approval::Ask
    );
    assert_eq!(
        Approval::Always.most_restrictive(Approval::Never),
        Approval::Never
    );
    assert_eq!(Approval::Ask.most_restrictive(Approval::Ask), Approval::Ask);
    assert_eq!(
        Approval::Ask.most_restrictive(Approval::Always),
        Approval::Ask
    );
    assert_eq!(
        Approval::Never.most_restrictive(Approval::Ask),
        Approval::Never
    );
}

#[test]
fn grant_intersection_is_commutative_and_monotone() {
    let first = grant_with(
        "fs.write.workspace",
        fs("/root/**"),
        constraints(Some(3), Approval::Ask, None, None),
    );
    let second = grant_with(
        "fs.write.workspace",
        fs("/root/a/b"),
        constraints(Some(2), Approval::Always, None, None),
    );
    let intersection = first.intersect(&second).expect("selectors nest");
    assert_eq!(intersection, second.intersect(&first).expect("symmetric"));
    assert!(intersection.is_narrowing_of(&first));
    assert!(intersection.is_narrowing_of(&second));
    assert_eq!(intersection.resource, fs("/root/a/b"));

    let disjoint = grant_with("fs.write.workspace", fs("/srv"), Constraints::default());
    assert_eq!(first.intersect(&disjoint), None);
    let other_class = grant_with("read.internal", fs("/root/a"), Constraints::default());
    assert_eq!(first.intersect(&other_class), None);
}

#[test]
fn selector_containment_is_conservative() {
    let cases = [
        ("/root/**", "/root/a/b", true),
        ("/root/**", "/root", true),
        ("/root", "/root/a", false),
        ("/root/*", "/root/a", true),
        ("/root/a", "/root/**", false),
        ("/root/a", "/root/a", true),
    ];
    for (wider, narrower, expected) in cases {
        let wider = ResourceSelector::from_parts("fs", wider, &[]).expect("selector");
        let narrower = ResourceSelector::from_parts("fs", narrower, &[]).expect("selector");
        assert_eq!(
            wider.covers(&narrower),
            expected,
            "{} covers {}",
            wider.selector(),
            narrower.selector()
        );
    }
}
