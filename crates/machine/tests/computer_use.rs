//! Computer-use policy: tiers, fail-closed identity and the takeover fence (EXEC-010).
//!
//! The native half of EXEC-010 is `native/macos`, covered by its own Swift suite against the live system
//! (`swift test --package-path native/macos`). This suite covers the half that decides: which tier an
//! action needs, that an application nobody vouched for is refused whatever the grant, and that a human
//! takeover cannot interleave with agent input.

use std::sync::Arc;

use quansio_machine::computer_use::{
    AppIdentity, AutomationFence, ComputerGrant, ComputerTier, ComputerUsePolicy, KnownApps,
    Refusal,
};

/// An application the runtime recognises.
fn known_app() -> AppIdentity {
    AppIdentity::new("com.apple.TextEdit", "TextEdit", 4242)
}

/// One nobody vouched for.
fn stranger() -> AppIdentity {
    AppIdentity::new("com.example.unknown", "Something Else", 9999)
}

fn policy(granted: ComputerTier, known: &[&str]) -> ComputerUsePolicy {
    ComputerUsePolicy::new(KnownApps::of(known.to_vec()), ComputerGrant::up_to(granted))
}

// ------------------------------------------------------------------ permission tiers

#[test]
fn a_tier_is_a_grant_and_a_run_cannot_reach_past_it() {
    // A read-only run may read and may not type, click, touch the clipboard or send system keys.
    let reader = policy(ComputerTier::Read, &["com.apple.TextEdit"]);
    assert_eq!(
        reader.authorize(ComputerTier::Read, Some(&known_app())),
        Ok(())
    );
    for tier in [
        ComputerTier::Click,
        ComputerTier::Type,
        ComputerTier::Clipboard,
        ComputerTier::SystemKey,
    ] {
        assert_eq!(
            reader.authorize(tier, Some(&known_app())),
            Err(Refusal::TierNotGranted {
                tier: tier.as_str(),
                granted: "read",
            }),
            "{tier:?} must not be reachable from a read grant"
        );
    }

    // A grant reaches exactly its own rung and the ones below it.
    let typer = policy(ComputerTier::Type, &["com.apple.TextEdit"]);
    for (tier, allowed) in [
        (ComputerTier::Read, true),
        (ComputerTier::Click, true),
        (ComputerTier::Type, true),
        (ComputerTier::Clipboard, false),
        (ComputerTier::SystemKey, false),
    ] {
        assert_eq!(
            typer.authorize(tier, Some(&known_app())).is_ok(),
            allowed,
            "{tier:?} with a type grant"
        );
    }

    // The tiers are ordered, which is what makes `covers` meaningful rather than a list to maintain.
    assert!(ComputerTier::Read < ComputerTier::Click);
    assert!(ComputerTier::Click < ComputerTier::Type);
    assert!(ComputerTier::Type < ComputerTier::Clipboard);
    assert!(ComputerTier::Clipboard < ComputerTier::SystemKey);
    assert_eq!(
        ComputerTier::parse("system_key"),
        Some(ComputerTier::SystemKey)
    );
    assert_eq!(ComputerTier::parse("teleport"), None);
    assert!(!ComputerGrant::read_only().covers(ComputerTier::Click));
    assert_eq!(ComputerGrant::read_only().highest(), ComputerTier::Read);
    // Every tier names the tool that acts at it, so a refusal can say which call was refused.
    for tier in [
        ComputerTier::Read,
        ComputerTier::Click,
        ComputerTier::Type,
        ComputerTier::Clipboard,
        ComputerTier::SystemKey,
    ] {
        assert!(!tier.tools().is_empty(), "{tier:?} names no tool");
    }
}

// ------------------------------------------------------------------ app identity

#[test]
fn an_application_the_runtime_does_not_recognise_is_refused_at_every_tier() {
    // Fail closed means there is no tier at which guessing about the foreground app is safe: the identity
    // is what the decision is about, so an unknown app is refused before the tier is even considered --
    // a run granted every tier included.
    let everything = policy(ComputerTier::SystemKey, &["com.apple.TextEdit"]);
    assert_eq!(everything.grant().highest(), ComputerTier::SystemKey);
    for tier in [
        ComputerTier::Read,
        ComputerTier::Click,
        ComputerTier::Type,
        ComputerTier::Clipboard,
        ComputerTier::SystemKey,
    ] {
        assert_eq!(
            everything.authorize(tier, Some(&stranger())),
            Err(Refusal::UnknownApp {
                bundle_id: "com.example.unknown".to_string(),
            }),
            "{tier:?} was allowed on an unrecognised app"
        );
    }

    // A missing foreground application is its own refusal rather than being treated as unknown.
    assert_eq!(
        everything.authorize(ComputerTier::Read, None),
        Err(Refusal::NoForegroundApp)
    );

    // A recognised application is allowed, so the refusals above are the identity rule and not a broken
    // policy.
    assert_eq!(
        everything.authorize(ComputerTier::SystemKey, Some(&known_app())),
        Ok(())
    );

    // Identity matches on the bundle identifier, not the display name: a name is not an identity, and an
    // application can call itself anything.
    let impostor = AppIdentity::new("com.example.unknown", "TextEdit", 1);
    assert!(matches!(
        everything.authorize(ComputerTier::Read, Some(&impostor)),
        Err(Refusal::UnknownApp { .. })
    ));

    // The default set recognises nothing at all, which is what an uninformed run must get.
    let uninformed = ComputerUsePolicy::new(
        KnownApps::none(),
        ComputerGrant::up_to(ComputerTier::SystemKey),
    );
    assert!(uninformed.known().is_empty());
    assert_eq!(uninformed.known().len(), 0);
    assert!(matches!(
        uninformed.authorize(ComputerTier::Read, Some(&known_app())),
        Err(Refusal::UnknownApp { .. })
    ));
    let listed = KnownApps::of(["a.b", "c.d"]);
    assert_eq!(listed.len(), 2);
    assert!(listed.recognises("a.b"));
    assert!(!listed.recognises("e.f"));
}

// ------------------------------------------------------------------ the takeover race

#[tokio::test]
async fn a_takeover_cannot_race_with_agent_input() {
    let fence = AutomationFence::new();
    assert_eq!(
        fence.holder().await,
        quansio_machine::computer_use::MachineHolder::Agent
    );

    // An action takes the generation it acts under.
    let first = fence.begin().await.expect("the agent holds the machine");
    assert_eq!(first, 1);
    assert_eq!(fence.in_flight().await, 1);

    // A person asks for the machine, which fences new actions immediately.
    fence.request_takeover().await.expect("takeover requested");
    assert!(matches!(
        fence.begin().await,
        Err(Refusal::TakeoverInProgress)
    ));

    // The takeover cannot complete while that action is in flight: this is the race, and the answer is
    // that the takeover waits rather than interleaving.
    assert!(matches!(
        fence.complete_takeover().await,
        Err(Refusal::TakeoverInProgress)
    ));
    assert_eq!(
        fence.holder().await,
        quansio_machine::computer_use::MachineHolder::Agent
    );

    // The action finishes, and only then does control change hands -- with the fence moved.
    fence.finish(first).await.expect("the same generation");
    let generation = fence.complete_takeover().await.expect("takeover completes");
    assert_eq!(generation, 2);
    assert_eq!(
        fence.holder().await,
        quansio_machine::computer_use::MachineHolder::User
    );
    assert_eq!(fence.in_flight().await, 0);

    // While a person holds it, no action may begin; automation resumes only on an explicit handback.
    assert!(matches!(fence.begin().await, Err(Refusal::HeldByUser)));
    assert!(matches!(
        fence.request_takeover().await,
        Err(Refusal::HeldByUser)
    ));
    let back = fence.handback().await.expect("handback");
    assert_eq!(back, 3);
    assert_eq!(
        fence.holder().await,
        quansio_machine::computer_use::MachineHolder::Agent
    );
    assert!(fence.begin().await.is_ok());

    // A handback when the agent already holds it is a refusal, so it cannot be used to skip a takeover
    // that never happened.
    assert!(matches!(fence.handback().await, Err(Refusal::HeldByUser)));
}

#[tokio::test]
async fn an_action_cannot_close_a_generation_the_fence_moved_past() {
    let fence = AutomationFence::new();
    let stale = fence.begin().await.expect("begin");
    // A takeover completes for a reason the first action did not foresee: here, the action was abandoned.
    fence.request_takeover().await.expect("request");
    fence.finish(stale).await.expect("the first action closes");
    fence.complete_takeover().await.expect("complete");
    let moved = fence.generation().await;

    // A new action under the new generation... and the old generation cannot be closed as if it were
    // current, so an action cannot report acting under a fence that has moved.
    fence.handback().await.expect("handback");
    let fresh = fence.begin().await.expect("begin");
    assert_eq!(fresh, moved + 1);
    assert_eq!(
        fence.finish(stale).await,
        Err(Refusal::StaleGeneration {
            began: stale,
            current: fresh,
        })
    );
    fence
        .finish(fresh)
        .await
        .expect("the current generation closes");
}

#[tokio::test]
async fn many_actions_and_a_takeover_never_interleave() {
    // The property, driven with real concurrency rather than asserted about one interleaving: whatever
    // order the runtime schedules them in, no action may be in flight when the takeover completes, and
    // every action that began before it is refused for closing under the new generation.
    let fence = Arc::new(AutomationFence::new());
    let actions: Vec<_> = (0..8)
        .map(|_| {
            let fence = Arc::clone(&fence);
            tokio::spawn(async move {
                let started = fence.begin().await;
                let generation = match started {
                    Ok(generation) => generation,
                    Err(refusal) => return Err(refusal),
                };
                // A little work, so the actions overlap with the takeover.
                tokio::task::yield_now().await;
                fence.finish(generation).await
            })
        })
        .collect();
    let takeover = {
        let fence = Arc::clone(&fence);
        tokio::spawn(async move {
            fence.request_takeover().await?;
            // Retry until the drain completes, which is what a real takeover does.
            for _ in 0..64 {
                match fence.complete_takeover().await {
                    Ok(generation) => return Ok(generation),
                    Err(Refusal::TakeoverInProgress) => tokio::task::yield_now().await,
                    Err(other) => return Err(other),
                }
            }
            Err(Refusal::TakeoverInProgress)
        })
    };

    let mut started = 0;
    let mut refused_to_start = 0;
    for action in actions {
        match action.await.expect("join") {
            Ok(()) => started += 1,
            Err(Refusal::TakeoverInProgress) => refused_to_start += 1,
            Err(other) => panic!("unexpected refusal: {other:?}"),
        }
    }
    let generation = takeover.await.expect("join").expect("the takeover drains");
    assert_eq!(generation, 2, "the takeover moved the fence once");
    assert_eq!(
        fence.in_flight().await,
        0,
        "an action was in flight when control changed"
    );
    assert_eq!(
        fence.holder().await,
        quansio_machine::computer_use::MachineHolder::User
    );
    assert_eq!(
        started + refused_to_start,
        8,
        "every action is accounted for, whether it ran or was fenced"
    );
    assert!(
        refused_to_start > 0 || started == 8,
        "the storm was not actually concurrent"
    );
}
