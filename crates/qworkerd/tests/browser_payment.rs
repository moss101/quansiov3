//! The one-time payment-input path against a real Google Chrome (ADP-009, DOMAIN.md §13.9 C).
//!
//! The pages are local: three HTTP servers on three loopback ports are three origins to Chrome, a checkout page on one embeds a
//! frame from another, and the card number is the **published test number** a payment provider documents for exactly this
//! (`4242 4242 4242 4242`), never a real card. What the page ends up holding is read back from the page, as a boolean, and the
//! value is searched for in everything the driver persists and returns.
//!
//! Proved here:
//! * the agent names a field and never writes a value; the runtime inserts what the person supplied, masks the field first,
//!   verifies by asking the page whether it holds what was supplied, and the value is gone afterwards (taken once, zeroed);
//! * the live `autocomplete` token must say it is that field: an unlabelled or mislabelled field gets nothing and the value is
//!   not even taken;
//! * the value is bound to the origin the person supplied it for, for the top frame and the field's frame alike;
//! * every failure names its stage; a failure after the take spends the value; a replayed call does not take a second one;
//! * the journal, the result and `Debug` hold the placeholder form, never the value.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::{
    blocked, call, driver_with_secrets, evaluate, observe, open_page, ref_named, run, Browser,
    Site, SESSION,
};
use quansio_qworkerd::browser::secrets::{
    PaymentField, PaymentRequest, PaymentValue, SecretPort, SecretText,
};
use quansio_qworkerd::browser::{BrowserCallContext, BrowserFault, FaultCode};
use serde_json::{json, Value};

/// A payment provider's published test card number; the one value this suite types.
const TEST_CARD: &str = "4242424242424242";
const TEST_CVC: &str = "123";

fn checkout(frame_port: u16) -> String {
    format!(
        r#"<html><head><title>Checkout</title></head><body>
<input id="num" name="n1" type="text" autocomplete="cc-number" aria-label="Card number">
<input id="cvc" name="n2" type="text" autocomplete="cc-csc" aria-label="Security code">
<input id="exp" name="n3" type="text" autocomplete="cc-exp" aria-label="Expiry">
<input id="holder" name="n4" type="text" autocomplete="cc-name" aria-label="Name on card">
<input id="zip" name="n5" type="text" autocomplete="postal-code" aria-label="Postal code">
<input id="bare" name="cardnumber" type="text" aria-label="Card number (unlabelled)">
<input id="wrong" name="cardnumber" type="text" autocomplete="cc-csc" aria-label="Looks like a card number">
<input id="eater" name="n6" type="text" autocomplete="cc-number" aria-label="Rewriting card number"
 oninput="this.value = 'xxxx'">
<input id="grouped" name="n7" type="text" autocomplete="cc-number" aria-label="Grouped card number"
 oninput="this.value = this.value.replace(/\D/g, '').replace(/(.{{4}})/g, '$1 ').trim()">
<iframe id="f" src="http://127.0.0.1:{frame_port}/frame" width="420" height="80"></iframe>
</body></html>"#
    )
}

const FRAME: &str = r#"<html><body><input id="fnum" name="f1" type="text" autocomplete="cc-number" aria-label="Frame card number"></body></html>"#;

struct Fixture {
    browser: Browser,
    top: Site,
    frame: Site,
    other: Site,
    dir: tempfile::TempDir,
}

async fn fixture() -> Option<Fixture> {
    let Some(browser) = Browser::launch() else {
        blocked("no Chrome on this host");
        return None;
    };
    let (top, frame, other) = (
        Site::start().await,
        Site::start().await,
        Site::start().await,
    );
    top.html("/checkout", &checkout(frame.port));
    frame.html("/frame", FRAME);
    other.html("/checkout", &checkout(frame.port));
    Some(Fixture {
        browser,
        top,
        frame,
        other,
        dir: tempfile::tempdir().expect("session dir"),
    })
}

/// One supplied value: its field, its text and the origin it was supplied for.
type Waiting = (PaymentField, &'static str, String);

/// What the person supplied: values by field, each usable once, for one origin.
#[derive(Clone)]
struct Supplied {
    values: Arc<Mutex<Vec<Waiting>>>,
    taken: Arc<AtomicUsize>,
}

impl Supplied {
    fn new(values: &[(PaymentField, &'static str)], origin: &str) -> Self {
        Self {
            values: Arc::new(Mutex::new(
                values
                    .iter()
                    .map(|(f, v)| (*f, *v, origin.to_string()))
                    .collect(),
            )),
            taken: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn port(&self) -> SecretPort {
        let supplied = self.clone();
        // The secret broker half is unused by these tests: no placeholder is typed.
        SecretPort::new(|_| {
            Box::pin(async {
                Err(BrowserFault::new(
                    FaultCode::SecretUnavailable,
                    "this suite types no placeholder",
                ))
            })
        })
        .with_payments(move |request: PaymentRequest| {
            let supplied = supplied.clone();
            Box::pin(async move {
                supplied.taken.fetch_add(1, Ordering::SeqCst);
                let mut values = supplied.values.lock().expect("values");
                match values
                    .iter()
                    .position(|(field, _, _)| *field == request.field)
                {
                    // A take consumes: the entry is removed as it is handed over.
                    Some(index) => {
                        let (_, value, origin) = values.remove(index);
                        Ok(PaymentValue {
                            value: SecretText::new(value.to_string()),
                            allowed_origin: origin,
                        })
                    }
                    None => Err(BrowserFault::new(
                        FaultCode::SecretUnavailable,
                        "the person has supplied nothing for this field",
                    )),
                }
            })
        })
    }

    fn remaining(&self) -> usize {
        self.values.lock().expect("values").len()
    }
}

fn fill_args(tab: &str, observation: &Value, name: &str, field: &str) -> Value {
    json!({
        "tab_id": tab,
        "expected_document_revision": observation["document_revision"],
        "snapshot_id": observation["snapshot_id"],
        "ref": ref_named(observation, name),
        "field": field,
    })
}

fn tier(approved: u8, n: u32) -> BrowserCallContext {
    BrowserCallContext {
        approved_tier: Some(approved),
        ..call(n)
    }
}

async fn run_as(
    driver: &quansio_qworkerd::browser::BrowserDriver,
    ctx: &BrowserCallContext,
    args: Value,
) -> Value {
    run(driver, ctx, "browser.fill_payment_field", args).await
}

async fn field_holds(browser: &Browser, tab: &str, id: &str, expected: &str) -> bool {
    evaluate(
        browser,
        tab,
        &format!(
            "document.getElementById({id:?}).value === {}",
            Value::String(expected.to_string())
        ),
    )
    .await
        == json!(true)
}

async fn field_masked(browser: &Browser, tab: &str, id: &str) -> bool {
    evaluate(
        browser,
        tab,
        &format!("getComputedStyle(document.getElementById({id:?})).getPropertyValue('-webkit-text-security') === 'disc'"),
    )
    .await
        == json!(true)
}

fn every_file(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path)
            } else {
                found.push(path)
            }
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_names_a_field_and_the_runtime_masks_inserts_verifies_and_forgets() {
    let Some(f) = fixture().await else { return };
    let supplied = Supplied::new(
        &[
            (PaymentField::CardNumber, TEST_CARD),
            (PaymentField::Cvc, TEST_CVC),
        ],
        &f.top.origin(),
    );
    let driver = driver_with_secrets(&f.browser, f.dir.path(), supplied.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.top.url("/checkout")).await;
    let observation = observe(&driver, 2, &tab).await;

    let reply = run_as(
        &driver,
        &tier(4, 3),
        fill_args(&tab, &observation, "Card number", "card_number"),
    )
    .await;
    assert_eq!(reply["status"], "ok", "{reply}");
    assert_eq!(reply["trust"], "UNTRUSTED_EXTERNAL");
    assert_eq!(reply["output"]["filled"], true);
    assert_eq!(reply["output"]["field"], "card_number");
    assert_eq!(reply["output"]["stage"], "done");
    assert_eq!(
        reply["output"]["masked"], true,
        "a card number is masked before it is typed"
    );
    assert_eq!(reply["output"]["verified"], true);
    assert!(
        field_holds(&f.browser, &tab, "num", TEST_CARD).await,
        "the page holds the supplied number"
    );
    assert!(
        field_masked(&f.browser, &tab, "num").await,
        "and shows it masked"
    );
    assert!(
        !field_masked(&f.browser, &tab, "zip").await,
        "other fields are untouched"
    );
    assert_eq!(
        supplied.remaining(),
        1,
        "the card number was taken; the security code waits"
    );

    // Nothing the model would see, and nothing at rest, holds the value.
    let model_sees = reply.to_string();
    assert!(!model_sees.contains(TEST_CARD), "{model_sees}");
    for path in every_file(f.dir.path()) {
        let bytes = std::fs::read(&path).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(TEST_CARD),
            "{} holds the card number",
            path.display()
        );
    }
    let after = observe(&driver, 4, &tab).await;
    assert!(
        !after.to_string().contains(TEST_CARD),
        "a following observation does not carry it either"
    );
    assert_eq!(reply["output"]["completed"], true);
    let _ = SESSION;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_value_can_be_used_once_and_a_replayed_call_does_not_take_another() {
    let Some(f) = fixture().await else { return };
    let supplied = Supplied::new(&[(PaymentField::CardNumber, TEST_CARD)], &f.top.origin());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), supplied.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.top.url("/checkout")).await;
    let observation = observe(&driver, 2, &tab).await;
    let ctx = tier(4, 3);
    let first = run_as(
        &driver,
        &ctx,
        fill_args(&tab, &observation, "Card number", "card_number"),
    )
    .await;
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(supplied.taken.load(Ordering::SeqCst), 1);

    // The same call delivered again returns the stored result and does not touch the port or the page.
    let replay = run_as(
        &driver,
        &ctx,
        fill_args(&tab, &observation, "Card number", "card_number"),
    )
    .await;
    assert_eq!(replay["status"], "ok", "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(
        supplied.taken.load(Ordering::SeqCst),
        1,
        "a replay takes nothing"
    );

    // A new call for the same field finds nothing: the person has to supply it again.
    let observation = observe(&driver, 4, &tab).await;
    let again = run_as(
        &driver,
        &tier(4, 5),
        fill_args(&tab, &observation, "Card number", "card_number"),
    )
    .await;
    assert_eq!(again["status"], "error", "{again}");
    assert_eq!(again["error"]["code"], "BROWSER_SECRET_UNAVAILABLE");
    assert_eq!(again["error"]["details"]["stage"], "take");
    assert_eq!(supplied.remaining(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_live_autocomplete_token_decides_and_a_field_the_page_does_not_label_gets_nothing() {
    let Some(f) = fixture().await else { return };
    let supplied = Supplied::new(
        &[
            (PaymentField::CardNumber, TEST_CARD),
            (PaymentField::Cvc, TEST_CVC),
        ],
        &f.top.origin(),
    );
    let driver = driver_with_secrets(&f.browser, f.dir.path(), supplied.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.top.url("/checkout")).await;
    let observation = observe(&driver, 2, &tab).await;

    // No autocomplete token: the name says "cardnumber" and the runtime does not guess.
    let bare = run_as(
        &driver,
        &tier(4, 3),
        fill_args(
            &tab,
            &observation,
            "Card number (unlabelled)",
            "card_number",
        ),
    )
    .await;
    assert_eq!(bare["error"]["code"], "BROWSER_INVALID_REQUEST", "{bare}");
    assert_eq!(bare["error"]["details"]["stage"], "match");
    assert_eq!(bare["error"]["details"]["field"], "card_number");

    // A token for another field: the page says this is a security code, not a number.
    let observation = observe(&driver, 4, &tab).await;
    let wrong = run_as(
        &driver,
        &tier(4, 5),
        fill_args(
            &tab,
            &observation,
            "Looks like a card number",
            "card_number",
        ),
    )
    .await;
    assert_eq!(wrong["error"]["details"]["stage"], "match", "{wrong}");
    assert_eq!(wrong["error"]["details"]["found_tokens"], json!(["cc-csc"]));

    // Neither the value nor the port was touched, and nothing was typed.
    assert_eq!(
        supplied.taken.load(Ordering::SeqCst),
        0,
        "a mismatch does not even take the value"
    );
    assert_eq!(supplied.remaining(), 2);
    assert!(
        field_holds(&f.browser, &tab, "bare", "").await
            && field_holds(&f.browser, &tab, "wrong", "").await
    );

    // The name of a field outside the list is not a field.
    let observation = observe(&driver, 6, &tab).await;
    let unknown = run_as(
        &driver,
        &tier(4, 7),
        fill_args(&tab, &observation, "Card number", "ssn"),
    )
    .await;
    assert_eq!(
        unknown["error"]["code"], "BROWSER_INVALID_REQUEST",
        "{unknown}"
    );
    // A value to type is not an argument at all.
    let mut smuggled = fill_args(&tab, &observation, "Card number", "card_number");
    smuggled["value"] = json!(TEST_CARD);
    let refused = run_as(&driver, &tier(4, 8), smuggled).await;
    assert_eq!(
        refused["error"]["code"], "BROWSER_INVALID_REQUEST",
        "{refused}"
    );
    assert_eq!(supplied.taken.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_value_is_bound_to_the_origin_the_person_supplied_it_for() {
    let Some(f) = fixture().await else { return };
    // Supplied for the top site, but the agent's page is the lookalike on another origin.
    let supplied = Supplied::new(&[(PaymentField::CardNumber, TEST_CARD)], &f.top.origin());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), supplied.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.other.url("/checkout")).await;
    let observation = observe(&driver, 2, &tab).await;
    let refused = run_as(
        &driver,
        &tier(4, 3),
        fill_args(&tab, &observation, "Card number", "card_number"),
    )
    .await;
    assert_eq!(
        refused["error"]["code"], "BROWSER_SECRET_ORIGIN_MISMATCH",
        "{refused}"
    );
    assert_eq!(refused["error"]["details"]["stage"], "origin");
    assert_eq!(refused["error"]["details"]["which"], "top");
    assert!(
        field_holds(&f.browser, &tab, "num", "").await,
        "nothing was typed"
    );
    assert!(!refused.to_string().contains(TEST_CARD));
    assert_eq!(
        supplied.remaining(),
        0,
        "the take spent the value: the person supplies it again for the right site"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_field_in_a_frame_of_another_origin_is_refused_even_when_the_top_page_is_the_right_one() {
    let Some(f) = fixture().await else { return };
    let supplied = Supplied::new(&[(PaymentField::CardNumber, TEST_CARD)], &f.top.origin());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), supplied.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.top.url("/checkout")).await;
    let observation = observe(&driver, 2, &tab).await;
    // The top page is the right one; the field is in a frame of another origin, which is refused at the `origin` stage.
    let args = fill_args(&tab, &observation, "Frame card number", "card_number");
    let reply = run_as(&driver, &tier(4, 3), args).await;
    assert_eq!(reply["status"], "error", "{reply}");
    assert_eq!(
        reply["error"]["code"], "BROWSER_SECRET_ORIGIN_MISMATCH",
        "{reply}"
    );
    assert_eq!(reply["error"]["details"]["stage"], "origin");
    assert_eq!(reply["error"]["details"]["which"], "target_frame");
    assert!(!reply.to_string().contains(TEST_CARD));
    assert_eq!(f.frame.hits("/frame"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn verification_asks_the_page_and_a_field_that_does_not_hold_the_value_is_emptied() {
    let Some(f) = fixture().await else { return };
    let supplied = Supplied::new(
        &[
            (PaymentField::CardNumber, TEST_CARD),
            (PaymentField::CardNumber, TEST_CARD),
        ],
        &f.top.origin(),
    );
    let driver = driver_with_secrets(&f.browser, f.dir.path(), supplied.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.top.url("/checkout")).await;
    let observation = observe(&driver, 2, &tab).await;

    // A page that rewrites what it is given: the field does not hold what was supplied, so the stage is `verify`.
    let eaten = run_as(
        &driver,
        &tier(4, 3),
        fill_args(&tab, &observation, "Rewriting card number", "card_number"),
    )
    .await;
    assert_eq!(eaten["error"]["code"], "BROWSER_PAGE_FAILURE", "{eaten}");
    assert_eq!(eaten["error"]["details"]["stage"], "verify");
    assert!(
        field_holds(&f.browser, &tab, "eater", "").await,
        "the wrong content was emptied"
    );
    assert!(!eaten.to_string().contains(TEST_CARD));

    // A page that formats (groups of four) still verifies: spaces are not a difference for a masked numeric field.
    let observation = observe(&driver, 4, &tab).await;
    let grouped = run_as(
        &driver,
        &tier(4, 5),
        fill_args(&tab, &observation, "Grouped card number", "card_number"),
    )
    .await;
    assert_eq!(grouped["status"], "ok", "{grouped}");
    assert_eq!(grouped["output"]["verified"], true);
    assert!(field_holds(&f.browser, &tab, "grouped", "4242 4242 4242 4242").await);
}

#[tokio::test(flavor = "multi_thread")]
async fn inserting_a_payment_value_is_a_tier_four_effect_and_the_journal_holds_the_placeholder() {
    let Some(f) = fixture().await else { return };
    let supplied = Supplied::new(&[(PaymentField::CardNumber, TEST_CARD)], &f.top.origin());
    let driver = driver_with_secrets(&f.browser, f.dir.path(), supplied.port(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.top.url("/checkout")).await;
    let observation = observe(&driver, 2, &tab).await;

    // Approved only as far as tier 3, the call is refused before the value is taken or anything is typed.
    let low = run_as(
        &driver,
        &tier(3, 3),
        fill_args(&tab, &observation, "Card number", "card_number"),
    )
    .await;
    assert_eq!(
        low["error"]["code"], "BROWSER_EFFECT_TIER_EXCEEDED",
        "{low}"
    );
    assert_eq!(low["error"]["details"]["effect_class"], "credential.access");
    assert_eq!(supplied.taken.load(Ordering::SeqCst), 0);

    let ok = run_as(
        &driver,
        &tier(4, 4),
        fill_args(&tab, &observation, "Card number", "card_number"),
    )
    .await;
    assert_eq!(ok["status"], "ok", "{ok}");
    // The journal line for the action records a digest of the placeholder form, never the value or a digest of it.
    let journal = std::fs::read_to_string(f.dir.path().join("actions.jsonl")).expect("journal");
    assert!(journal.contains("act_0004"));
    assert!(!journal.contains(TEST_CARD));
    let value_digest =
        quansio_core::Digest::of_canonical_json(&quansio_tools::canonical_json(&json!(TEST_CARD)));
    assert!(
        !journal.contains(value_digest.as_str()),
        "no digest of the value either"
    );
    // A debug rendering of the value has no characters in it.
    let rendered = format!(
        "{:?}",
        PaymentValue {
            value: SecretText::new(TEST_CARD.to_string()),
            allowed_origin: "https://x".to_string()
        }
    );
    assert!(!rendered.contains("4242"), "{rendered}");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_payment_source_the_tool_fails_closed_and_the_session_without_a_port_too() {
    let Some(f) = fixture().await else { return };
    let driver = common::driver(&f.browser, f.dir.path(), |_| {});
    let (tab, _) = open_page(&driver, 1, &f.top.url("/checkout")).await;
    let observation = observe(&driver, 2, &tab).await;
    let reply = run_as(
        &driver,
        &tier(4, 3),
        fill_args(&tab, &observation, "Card number", "card_number"),
    )
    .await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_SECRET_UNAVAILABLE",
        "{reply}"
    );
    assert_eq!(reply["error"]["details"]["stage"], "take");
    assert!(field_holds(&f.browser, &tab, "num", "").await);
}
