//! Action receipts (ADP-002, DOMAIN.md §8.6.1 A and C): the shape of a receipt, what "unknown" is allowed
//! to mean, how a background claim is re-derived, and what the dispatcher does with a receipt.
//!
//! Real PostgreSQL stands behind the dispatcher tests (`QUANSIO_TEST_POSTGRES_URL`, else
//! `BLOCKED_EXTERNAL`). The platform is scripted, so these tests prove the runtime's rules and nothing
//! about macOS; the native half is `native/macos` (`ReceiptTests`) and the two are held to one wire
//! shape by the fixtures under `native/macos/Tests/QuansioMacBridgeTests/Fixtures/`.

use quansio_machine::computer_use::bridge::{ComputerDispatchError, ComputerDispatcher};
use quansio_machine::computer_use::receipt::{
    ActionReceipt, BackgroundInvariants, BridgeReport, ComputerActionResult, ExecutionMethod,
    FrontmostApp, LedgerOutcome, ReceiptError, ReceiptMethod, ReceiptOutcome,
};
use quansio_machine::computer_use::store::ComputerControlStore;
use quansio_machine::computer_use::ComputerTier;
use serde_json::{json, Value};

mod common;
use common::computer::{
    applied_report, click, policy, prepare, read, ScriptedBridge, Step, CALL_A, CALL_B, TARGET,
    TENANT,
};

fn app(bundle: &str, pid: i32) -> FrontmostApp {
    FrontmostApp {
        bundle_id: bundle.to_string(),
        pid,
    }
}

fn clean() -> BackgroundInvariants {
    BackgroundInvariants {
        physical_input: false,
        moved_system_cursor: false,
        frontmost_before: app("com.apple.TextEdit", 4242),
        frontmost_after: app("com.apple.TextEdit", 4242),
        frontmost_changed: false,
    }
}

fn background(receipt: ActionReceipt) -> ActionReceipt {
    ActionReceipt {
        execution_method: ExecutionMethod::Background,
        ..receipt
    }
}

// ------------------------------------------------------------------ shape and unknown verification

#[test]
fn unknown_verification_is_neither_true_nor_false_on_the_wire() {
    let receipt = ActionReceipt::unverified(ReceiptMethod::HidEvent, ExecutionMethod::Foreground);
    let wire = receipt.to_json();
    // The key is present and null: absence would be a different statement.
    assert_eq!(wire["effect_verified"], Value::Null);
    assert!(wire
        .as_object()
        .expect("object")
        .contains_key("effect_verified"));
    assert_eq!(wire["dispatched"], json!(true));
    assert_eq!(wire["outcome"], json!("unverified"));
    assert_eq!(wire["method"], json!("hid_event"));
    assert_eq!(wire["execution_method"], json!("foreground"));

    // It survives a round trip as unknown, and normalization does not round it either way.
    let parsed = ActionReceipt::from_json(&wire).expect("parse");
    assert_eq!(parsed.effect_verified, None);
    assert_eq!(parsed.normalized().expect("coherent").effect_verified, None);

    // True and false stay what they are.
    let applied = ActionReceipt::applied(ReceiptMethod::Pasteboard, ExecutionMethod::Foreground);
    assert_eq!(applied.to_json()["effect_verified"], json!(true));
    let absent = ActionReceipt::not_applied(ReceiptMethod::Pasteboard, ExecutionMethod::Foreground);
    assert_eq!(absent.to_json()["effect_verified"], json!(false));
}

#[test]
fn a_receipt_that_does_not_say_is_malformed_not_unknown() {
    let good =
        ActionReceipt::unverified(ReceiptMethod::HidEvent, ExecutionMethod::Foreground).to_json();
    // Dropping `effect_verified` must not be read as null.
    let mut without = good.clone();
    without
        .as_object_mut()
        .expect("object")
        .remove("effect_verified");
    assert!(matches!(
        ActionReceipt::from_json(&without),
        Err(ReceiptError::Malformed(_))
    ));
    // A string, a number or a nested object is not a verdict.
    for bad in [json!("true"), json!(1), json!("unknown"), json!({})] {
        let mut wire = good.clone();
        wire["effect_verified"] = bad;
        assert!(
            matches!(
                ActionReceipt::from_json(&wire),
                Err(ReceiptError::Malformed(_))
            ),
            "{wire}"
        );
    }
    // Unknown words and extra fields are refused.
    let mut extra = good.clone();
    extra["verified"] = json!(true);
    assert!(ActionReceipt::from_json(&extra).is_err());
    let mut method = good.clone();
    method["method"] = json!("telepathy");
    assert!(ActionReceipt::from_json(&method).is_err());
    let mut outcome = good;
    outcome["outcome"] = json!("probably_fine");
    assert!(ActionReceipt::from_json(&outcome).is_err());
}

#[test]
fn contradictory_receipts_are_incoherent() {
    let base = |dispatched: bool, verified: Option<bool>, outcome: ReceiptOutcome| ActionReceipt {
        method: ReceiptMethod::HidEvent,
        execution_method: ExecutionMethod::Foreground,
        dispatched,
        effect_verified: verified,
        outcome,
        invariant_violated: false,
        invariants: None,
    };
    let incoherent = [
        // Verified true although nothing was sent.
        base(false, Some(true), ReceiptOutcome::Applied),
        // Refused is the right word, but the verdict still has to be "known absent".
        base(false, Some(true), ReceiptOutcome::Refused),
        base(false, None, ReceiptOutcome::Refused),
        base(false, None, ReceiptOutcome::Unverified),
        base(false, Some(false), ReceiptOutcome::Unverified),
        // Verified true with a word that says otherwise.
        base(true, Some(true), ReceiptOutcome::Unverified),
        base(true, Some(true), ReceiptOutcome::NotApplied),
        // Unknown with a word that says known.
        base(true, None, ReceiptOutcome::Applied),
        base(true, None, ReceiptOutcome::NotApplied),
        // False with a word that says true or unknown.
        base(true, Some(false), ReceiptOutcome::Applied),
        base(true, Some(false), ReceiptOutcome::Unverified),
        // Refused but dispatched.
        base(true, Some(false), ReceiptOutcome::Refused),
        // An invariant outcome with no violation.
        base(true, Some(false), ReceiptOutcome::InvariantViolated),
    ];
    for receipt in incoherent {
        assert!(
            matches!(
                receipt.clone().normalized(),
                Err(ReceiptError::Incoherent(_))
            ),
            "{receipt:?} was accepted"
        );
    }
    // A foreground action cannot claim a background violation.
    let mut foreground = base(true, Some(false), ReceiptOutcome::InvariantViolated);
    foreground.invariant_violated = true;
    assert!(foreground.normalized().is_err());
    // The coherent ones pass.
    for receipt in [
        base(false, Some(false), ReceiptOutcome::Refused),
        base(true, Some(true), ReceiptOutcome::Applied),
        base(true, Some(false), ReceiptOutcome::NotApplied),
        base(true, None, ReceiptOutcome::Unverified),
        base(true, None, ReceiptOutcome::Observed),
    ] {
        assert!(receipt.clone().normalized().is_ok(), "{receipt:?}");
    }
}

#[test]
fn the_ledger_never_settles_an_unverified_input_as_success() {
    let method = ReceiptMethod::HidEvent;
    let fg = ExecutionMethod::Foreground;
    assert_eq!(
        ActionReceipt::applied(method, fg).ledger_outcome(),
        LedgerOutcome::SettledSuccess
    );
    assert_eq!(
        ActionReceipt::observed(ReceiptMethod::AccessibilityTree, fg).ledger_outcome(),
        LedgerOutcome::SettledSuccess
    );
    assert_eq!(
        ActionReceipt::refused(method, fg).ledger_outcome(),
        LedgerOutcome::SettledFailed
    );
    // Input reached the machine and is not established: reconcile, never retry blind.
    assert_eq!(
        ActionReceipt::unverified(method, fg).ledger_outcome(),
        LedgerOutcome::OutcomeUnknown
    );
    assert_eq!(
        ActionReceipt::not_applied(method, fg).ledger_outcome(),
        LedgerOutcome::OutcomeUnknown
    );
    assert_eq!(LedgerOutcome::OutcomeUnknown.as_str(), "OUTCOME_UNKNOWN");
}

#[test]
fn the_tool_result_carries_the_receipt_and_a_bridge_cannot_write_it() {
    let result = ComputerActionResult {
        output: json!({ "clicked": true }),
        receipt: ActionReceipt::unverified(ReceiptMethod::HidEvent, ExecutionMethod::Foreground),
    };
    let shown = result.tool_result().expect("tool result");
    assert_eq!(shown["clicked"], json!(true));
    assert_eq!(shown["receipt"]["effect_verified"], Value::Null);
    // An output that already has a `receipt` key would let a bridge speak in the runtime's voice.
    let forged = ComputerActionResult {
        output: json!({ "clicked": true, "receipt": { "effect_verified": true } }),
        receipt: result.receipt.clone(),
    };
    assert!(forged.tool_result().is_err());
    let not_object = ComputerActionResult {
        output: json!("ok"),
        receipt: result.receipt,
    };
    assert!(not_object.tool_result().is_err());
}

// ------------------------------------------------------------------ background invariants

#[test]
fn a_broken_background_claim_overrides_what_the_bridge_says_about_the_effect() {
    // Each broken invariant, alone, turns a bridge's `applied` into effect_verified=false.
    type Breaker = fn(&mut BackgroundInvariants);
    let breakers: [(&str, Breaker); 5] = [
        ("physical input", |m| m.physical_input = true),
        ("moved cursor", |m| m.moved_system_cursor = true),
        ("frontmost pid changed", |m| {
            m.frontmost_after = app("com.apple.TextEdit", 9999);
        }),
        ("frontmost bundle changed", |m| {
            m.frontmost_after = app("com.apple.Safari", 4242);
        }),
        (
            "bridge flags a frontmost change the apps do not show",
            |m| {
                m.frontmost_changed = true;
            },
        ),
    ];
    for (name, break_it) in breakers {
        let mut measured = clean();
        break_it(&mut measured);
        assert!(measured.violated(), "{name}");
        let claimed = background(ActionReceipt::applied(
            ReceiptMethod::ProcessEvent,
            ExecutionMethod::Background,
        ))
        .with_invariants(measured);
        let normalized = claimed.normalized().expect("normalizes");
        assert!(normalized.invariant_violated, "{name}");
        assert_eq!(normalized.effect_verified, Some(false), "{name}");
        assert_eq!(
            normalized.outcome,
            ReceiptOutcome::InvariantViolated,
            "{name}"
        );
        assert_eq!(
            normalized.ledger_outcome(),
            LedgerOutcome::OutcomeUnknown,
            "{name}: the input did reach the machine"
        );
    }
}

#[test]
fn a_clean_measurement_keeps_a_verified_background_action_verified() {
    let receipt = ActionReceipt::applied(
        ReceiptMethod::AccessibilityAction,
        ExecutionMethod::Background,
    )
    .with_invariants(clean())
    .normalized()
    .expect("coherent");
    assert_eq!(receipt.effect_verified, Some(true));
    assert!(!receipt.invariant_violated);
    assert_eq!(receipt.outcome, ReceiptOutcome::Applied);
}

#[test]
fn a_background_claim_without_a_measurement_cannot_be_verified() {
    // The bridge says applied; nothing was measured. The claim that nothing else was disturbed is
    // unproven, and so is the verification resting on it: unknown, not true.
    let lowered = ActionReceipt::applied(ReceiptMethod::ProcessEvent, ExecutionMethod::Background)
        .normalized()
        .expect("lowered");
    assert_eq!(lowered.effect_verified, None);
    assert_eq!(lowered.outcome, ReceiptOutcome::Unverified);
    assert!(!lowered.invariant_violated);
}

#[test]
fn a_bridge_flag_can_add_a_violation_but_never_clear_one() {
    // The bridge flags a violation its numbers do not show: the runtime keeps the flag.
    let mut flagged =
        ActionReceipt::applied(ReceiptMethod::ProcessEvent, ExecutionMethod::Background)
            .with_invariants(clean());
    flagged.invariant_violated = true;
    let normalized = flagged.normalized().expect("normalizes");
    assert!(normalized.invariant_violated);
    assert_eq!(normalized.effect_verified, Some(false));

    // The bridge clears the flag while its own numbers show a violation: the numbers win.
    let mut measured = clean();
    measured.moved_system_cursor = true;
    let mut cleared =
        ActionReceipt::applied(ReceiptMethod::ProcessEvent, ExecutionMethod::Background)
            .with_invariants(measured);
    cleared.invariant_violated = false;
    let normalized = cleared.normalized().expect("normalizes");
    assert!(normalized.invariant_violated);
}

#[test]
fn a_receipt_with_invariants_round_trips_on_the_wire() {
    let mut measured = clean();
    measured.physical_input = true;
    let receipt = ActionReceipt::applied(ReceiptMethod::ProcessEvent, ExecutionMethod::Background)
        .with_invariants(measured)
        .normalized()
        .expect("normalizes");
    let wire = receipt.to_json();
    assert_eq!(wire["invariant_violated"], json!(true));
    assert_eq!(wire["invariants"]["physical_input"], json!(true));
    assert_eq!(
        wire["invariants"]["frontmost_before"]["bundle_id"],
        json!("com.apple.TextEdit")
    );
    assert_eq!(ActionReceipt::from_json(&wire).expect("parse"), receipt);
}

// ------------------------------------------------------------------ the native wire fixtures

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../../native/macos/Tests/QuansioMacBridgeTests/Fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_str(&text).expect("fixture is JSON")
}

#[test]
fn the_receipts_the_native_bridge_writes_parse_and_re_serialize_identically() {
    // The Swift suite encodes its receipts and compares them to these same files, so the two sides
    // cannot drift apart on a field name or on how an unknown verdict is written.
    for name in [
        "receipt_unverified.json",
        "receipt_applied_background.json",
        "receipt_invariant_violated.json",
        "receipt_refused.json",
    ] {
        let wire = fixture(name);
        let parsed = ActionReceipt::from_json(&wire).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(parsed.to_json(), wire, "{name}");
        parsed
            .normalized()
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let unverified = ActionReceipt::from_json(&fixture("receipt_unverified.json")).expect("parse");
    assert_eq!(unverified.effect_verified, None);
    let violated =
        ActionReceipt::from_json(&fixture("receipt_invariant_violated.json")).expect("parse");
    assert!(violated.invariant_violated);
    assert_eq!(violated.effect_verified, Some(false));
}

// ------------------------------------------------------------------ the dispatcher

#[tokio::test]
async fn a_bridge_that_cannot_verify_gets_an_unknown_receipt_not_a_verified_one() {
    let Some((name, pool)) = prepare("receipt_unknown").await else {
        common::blocked_marker();
        return;
    };
    let mut conn = pool.acquire().await.expect("acquire");
    // The scripted bridge answers with no script: input sent, effect not observed.
    let bridge = ScriptedBridge::new();
    let dispatcher = ComputerDispatcher::new(bridge.clone(), policy(ComputerTier::Click));
    let result = dispatcher
        .execute(&mut conn, TENANT, TARGET, CALL_A, 1, &click())
        .await
        .expect("dispatch");
    assert_eq!(result.receipt.effect_verified, None);
    assert_eq!(result.receipt.outcome, ReceiptOutcome::Unverified);
    assert!(result.receipt.dispatched);
    assert_eq!(result.ledger_outcome(), LedgerOutcome::OutcomeUnknown);
    assert_eq!(
        result.tool_result().expect("result")["receipt"]["effect_verified"],
        Value::Null
    );
    // The bridge answered, so the slot is free for the next action; the unknown is the ledger's to hold.
    assert!(ComputerControlStore::load(&mut conn, TENANT, TARGET)
        .await
        .expect("load")
        .active_tool_call_id
        .is_none());

    // A read has no effect to verify: observed, success.
    let read_result = dispatcher
        .execute(&mut conn, TENANT, TARGET, CALL_B, 1, &read(false))
        .await
        .expect("read");
    assert_eq!(read_result.receipt.outcome, ReceiptOutcome::Observed);
    assert_eq!(read_result.ledger_outcome(), LedgerOutcome::SettledSuccess);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn the_dispatcher_re_derives_a_background_receipt_the_bridge_over_claimed() {
    let Some((name, pool)) = prepare("receipt_rederive").await else {
        common::blocked_marker();
        return;
    };
    let mut conn = pool.acquire().await.expect("acquire");
    let bridge = ScriptedBridge::new();
    let mut moved = clean();
    moved.moved_system_cursor = true;
    // The bridge says "applied, no violation" while its own measurement shows the cursor moved.
    let mut over_claim =
        ActionReceipt::applied(ReceiptMethod::ProcessEvent, ExecutionMethod::Background)
            .with_invariants(moved);
    over_claim.invariant_violated = false;
    bridge.push(Step::Report(BridgeReport {
        output: json!({ "clicked": true }),
        receipt: over_claim,
    }));
    let dispatcher = ComputerDispatcher::new(bridge, policy(ComputerTier::Click));
    let result = dispatcher
        .execute(&mut conn, TENANT, TARGET, CALL_A, 1, &click())
        .await
        .expect("dispatch");
    assert!(result.receipt.invariant_violated);
    assert_eq!(result.receipt.effect_verified, Some(false));
    assert_eq!(result.receipt.outcome, ReceiptOutcome::InvariantViolated);
    assert_eq!(result.ledger_outcome(), LedgerOutcome::OutcomeUnknown);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn an_incoherent_receipt_is_a_bridge_fault_handled_like_a_failed_call() {
    let Some((name, pool)) = prepare("receipt_incoherent").await else {
        common::blocked_marker();
        return;
    };
    let mut conn = pool.acquire().await.expect("acquire");
    let bridge = ScriptedBridge::new();
    // "Verified" with nothing dispatched: the bridge is lying or broken.
    let nonsense = ActionReceipt {
        method: ReceiptMethod::HidEvent,
        execution_method: ExecutionMethod::Foreground,
        dispatched: false,
        effect_verified: Some(true),
        outcome: ReceiptOutcome::Applied,
        invariant_violated: false,
        invariants: None,
    };
    bridge.push(Step::Report(BridgeReport {
        output: json!({}),
        receipt: nonsense.clone(),
    }));
    let dispatcher = ComputerDispatcher::new(bridge.clone(), policy(ComputerTier::Click));
    let error = dispatcher
        .execute(&mut conn, TENANT, TARGET, CALL_A, 1, &click())
        .await
        .expect_err("incoherent");
    // Input may have landed: the call stays active for reconciliation, as for a disconnect.
    assert!(
        matches!(&error, ComputerDispatchError::OutcomeUnknown { code, .. } if code == "receipt_incoherent"),
        "{error:?}"
    );
    assert_eq!(
        ComputerControlStore::load(&mut conn, TENANT, TARGET)
            .await
            .expect("load")
            .active_tool_call_id
            .as_deref(),
        Some(CALL_A)
    );
    ComputerControlStore::finish_action(&mut conn, TENANT, TARGET, CALL_A, 1)
        .await
        .expect("reconcile");

    // For a read the same fault is a failed read and the slot is released.
    bridge.push(Step::Report(BridgeReport {
        output: json!({}),
        receipt: nonsense,
    }));
    let read_error = dispatcher
        .execute(&mut conn, TENANT, TARGET, CALL_B, 1, &read(false))
        .await
        .expect_err("incoherent read");
    assert!(matches!(read_error, ComputerDispatchError::ReadFailed(_)));
    assert!(ComputerControlStore::load(&mut conn, TENANT, TARGET)
        .await
        .expect("load")
        .active_tool_call_id
        .is_none());
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn a_verified_receipt_and_a_refused_receipt_settle_as_the_bridge_said() {
    let Some((name, pool)) = prepare("receipt_known").await else {
        common::blocked_marker();
        return;
    };
    let mut conn = pool.acquire().await.expect("acquire");
    let bridge = ScriptedBridge::new();
    bridge.push(Step::Report(applied_report()));
    bridge.push(Step::Report(BridgeReport {
        output: json!({ "clicked": false }),
        receipt: ActionReceipt::refused(ReceiptMethod::HidEvent, ExecutionMethod::Foreground),
    }));
    let dispatcher = ComputerDispatcher::new(bridge, policy(ComputerTier::Click));
    let applied = dispatcher
        .execute(&mut conn, TENANT, TARGET, CALL_A, 1, &click())
        .await
        .expect("applied");
    assert_eq!(applied.receipt.effect_verified, Some(true));
    assert_eq!(applied.ledger_outcome(), LedgerOutcome::SettledSuccess);
    let refused = dispatcher
        .execute(&mut conn, TENANT, TARGET, CALL_B, 1, &click())
        .await
        .expect("refused");
    assert!(!refused.receipt.dispatched);
    assert_eq!(refused.receipt.effect_verified, Some(false));
    assert_eq!(refused.ledger_outcome(), LedgerOutcome::SettledFailed);
    drop(conn);
    common::drop_pool(&pool, &name).await;
}

// ------------------------------------------------------------------ one definition of "coherent"

#[test]
fn the_machine_and_the_tool_registry_agree_on_what_a_receipt_is() {
    use quansio_tools::receipt::validate_result;
    let declaration = quansio_tools::ToolRegistry::builtin()
        .get("computer.click")
        .expect("computer.click");
    let as_result =
        |receipt: &ActionReceipt| json!({ "clicked": true, "receipt": receipt.to_json() });
    let method = ReceiptMethod::ProcessEvent;
    let bg = ExecutionMethod::Background;
    let fg = ExecutionMethod::Foreground;

    // Everything the runtime produces after normalization is a valid Tool result.
    let mut violated = clean();
    violated.moved_system_cursor = true;
    let produced = [
        ActionReceipt::unverified(method, fg),
        ActionReceipt::applied(method, fg),
        ActionReceipt::not_applied(method, fg),
        ActionReceipt::refused(method, fg),
        ActionReceipt::observed(ReceiptMethod::AccessibilityTree, fg),
        ActionReceipt::applied(method, bg).with_invariants(clean()),
        ActionReceipt::applied(method, bg).with_invariants(violated),
        ActionReceipt::applied(method, bg),
    ];
    for receipt in produced {
        let normalized = receipt
            .clone()
            .normalized()
            .expect("coherent after normalization");
        validate_result(declaration, &as_result(&normalized))
            .unwrap_or_else(|error| panic!("{receipt:?}: {error}"));
    }

    // Everything the runtime calls incoherent the registry refuses too.
    let nonsense = |dispatched, verified, outcome, violated, execution| ActionReceipt {
        method,
        execution_method: execution,
        dispatched,
        effect_verified: verified,
        outcome,
        invariant_violated: violated,
        invariants: None,
    };
    for receipt in [
        nonsense(false, Some(true), ReceiptOutcome::Applied, false, fg),
        nonsense(true, Some(true), ReceiptOutcome::Unverified, false, fg),
        nonsense(true, None, ReceiptOutcome::Applied, false, fg),
        nonsense(true, Some(false), ReceiptOutcome::Refused, false, fg),
        nonsense(
            true,
            Some(false),
            ReceiptOutcome::InvariantViolated,
            true,
            fg,
        ),
        nonsense(true, Some(true), ReceiptOutcome::Applied, false, bg),
    ] {
        assert!(receipt.check_coherence().is_err(), "{receipt:?}");
        assert!(
            validate_result(declaration, &as_result(&receipt)).is_err(),
            "the registry accepted what the runtime refuses: {receipt:?}"
        );
    }
}
