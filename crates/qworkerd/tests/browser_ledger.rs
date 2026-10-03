//! Browser action idempotency and the in-doubt outcome, against a real Google Chrome (ADP-001,
//! DOMAIN.md §8.4.1 B).
//!
//! The acceptance statement is that an in-doubt browser action is not retried and is surfaced as in
//! doubt. "Not retried" is proved the only way that cannot be faked: the page's button asks a real HTTP
//! server for `/hit` through a synchronous request, so the server's count is how many times the click
//! really happened, and the tests read it after a replay, after a restart and after a reconciliation.
//!
//! The doubt itself is real too. The button's handler keeps the renderer busy for seconds, so Chrome
//! never acknowledges the mouse event inside the action timeout — the driver genuinely cannot say
//! whether the click happened. (It did: the server saw it.)

mod common;

use std::time::Duration;

use common::{blocked, call, driver, observe, open_page, ref_named, run, Browser, Route, Site};
use quansio_qworkerd::browser::journal::{ActionJournal, ActionKey, ActionState};
use serde_json::{json, Value};

const PAGE: &str = r#"<html><head><title>Ledger</title></head><body>
<button id="count" onclick="var x = new XMLHttpRequest(); x.open('GET', '/hit', false); x.send(); document.title = 'counted';">Count</button>
<button id="busy" onclick="var x = new XMLHttpRequest(); x.open('GET', '/hit', false); x.send(); var t = Date.now(); while (Date.now() - t < 5000) {}">Busy</button>
<button id="slow" onclick="var x = new XMLHttpRequest(); x.open('GET', '/slow', false); x.send();">Slow</button>
<button id="buy" onclick="var x = new XMLHttpRequest(); x.open('GET', '/buy', false); x.send();">Buy now</button>
<button id="plain">Plain</button>
</body></html>"#;

async fn fixture() -> Option<(Browser, Site, tempfile::TempDir)> {
    let Some(browser) = Browser::launch() else {
        blocked("no Chrome on this host");
        return None;
    };
    let site = Site::start().await;
    site.html("/", PAGE);
    site.route("/hit", Route::Count);
    site.route("/buy", Route::Count);
    site.route(
        "/slow",
        Route::Slow {
            headers: Vec::new(),
            total: 2,
            first: b"o".to_vec(),
            delay: Duration::from_millis(700),
            rest: b"k".to_vec(),
        },
    );
    Some((browser, site, tempfile::tempdir().expect("session dir")))
}

fn click_args(tab: &str, observation: &Value, name: &str) -> Value {
    json!({
        "tab_id": tab,
        "expected_document_revision": observation["document_revision"],
        "snapshot_id": observation["snapshot_id"],
        "ref": ref_named(observation, name),
    })
}

fn journal_ops(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("actions.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|record| record["op"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_replayed_action_returns_the_stored_result_and_never_touches_the_page() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, _) = open_page(&driver, 1, &site.url("/")).await;
    let observation = observe(&driver, 2, &tab).await;
    let args = click_args(&tab, &observation, "Count");

    let first = run(&driver, &call(3), "browser.click", args.clone()).await;
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(first["replayed"], false);
    assert_eq!(site.hits("/hit"), 1);

    // The same call delivered again — a reconnect replay — answers from the journal. The page has since
    // changed, so this click would be stale; the replay does not even look.
    let replay = run(&driver, &call(3), "browser.click", args.clone()).await;
    assert_eq!(replay["status"], "ok", "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["output"], first["output"]);
    assert_eq!(site.hits("/hit"), 1, "the replay clicked the page again");

    // One action id is one action: the same key with different arguments is refused, and the page is
    // not clicked for it either.
    let mut other = args;
    other["ref"] = json!(ref_named(&observation, "Plain"));
    let conflict = run(&driver, &call(3), "browser.click", other).await;
    assert_eq!(conflict["status"], "error", "{conflict}");
    assert_eq!(conflict["error"]["code"], "BROWSER_ACTION_REPLAY_CONFLICT");
    assert_eq!(conflict["error"]["retryable"], false);
    assert_eq!(conflict["error"]["requires_new_observation"], false);
    assert_eq!(site.hits("/hit"), 1);

    // Reads are not journaled: only the click, and its completion, are on the record.
    assert_eq!(
        journal_ops(dir.path()),
        vec!["start", "complete", "start", "complete"],
        "the navigation and the click are on the record, and nothing else"
    );
}

#[tokio::test]
async fn an_unacknowledged_click_is_in_doubt_surfaced_and_never_retried() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let driver = driver(&browser, dir.path(), |config| {
        config.action_timeout = Duration::from_millis(1200);
    });
    let (tab, _) = open_page(&driver, 1, &site.url("/")).await;
    let observation = observe(&driver, 2, &tab).await;
    let args = click_args(&tab, &observation, "Busy");

    let doubtful = run(&driver, &call(3), "browser.click", args.clone()).await;
    assert_eq!(doubtful["status"], "in_doubt", "{doubtful}");
    assert_eq!(doubtful["ledger_outcome"], "OUTCOME_UNKNOWN");
    assert_eq!(doubtful["error"]["code"], "BROWSER_ACTION_IN_DOUBT");
    assert_eq!(doubtful["error"]["retryable"], false);
    assert_eq!(doubtful["error"]["requires_new_observation"], true);
    assert_eq!(doubtful["output"]["in_doubt"], true);
    assert_eq!(doubtful["trust"], "UNTRUSTED_EXTERNAL");
    // It did happen — the server saw it — which is exactly why it must not be repeated.
    assert_eq!(site.hits("/hit"), 1);

    // The runtime redelivers the call. It is answered as in doubt again, without touching the page.
    let again = run(&driver, &call(3), "browser.click", args.clone()).await;
    assert_eq!(again["status"], "in_doubt", "{again}");
    assert_eq!(again["replayed"], true);
    assert_eq!(site.hits("/hit"), 1, "an in-doubt action was retried");
    assert_eq!(driver.journal().in_doubt().len(), 1);
    let report = driver.guard_report();
    assert_eq!(report["in_doubt"][0]["action_id"], "act_0003");
    // The report the server stores accepts what the driver says about itself.
    quansio_machine::control::browser::GuardReport::from_json(&report)
        .expect("a valid guard report");

    // A different argument set under the same key is still a conflict, not a retry in disguise.
    let mut other = args;
    other["ref"] = json!(ref_named(&observation, "Plain"));
    let conflict = run(&driver, &call(3), "browser.click", other).await;
    assert_eq!(
        conflict["error"]["code"], "BROWSER_ACTION_REPLAY_CONFLICT",
        "{conflict}"
    );

    // Reconciliation closes the doubt from a fresh observation; the key stays spent.
    driver
        .reconcile(&call(3), "observed the page: the click had happened once")
        .expect("reconcile");
    let spent = run(
        &driver,
        &call(3),
        "browser.click",
        click_args(&tab, &observation, "Busy"),
    )
    .await;
    assert_eq!(spent["status"], "ok", "{spent}");
    assert_eq!(spent["output"]["reconciled"], true);
    assert_eq!(spent["replayed"], true);
    assert_eq!(site.hits("/hit"), 1);
    assert!(driver.journal().in_doubt().is_empty());
    assert!(driver
        .journal()
        .state(&ActionKey::new("tc_call0003", "act_0003"))
        .is_some_and(|state| matches!(state, ActionState::Reconciled { .. })));
}

#[tokio::test]
async fn a_restart_in_the_middle_of_an_action_leaves_it_in_doubt_and_unrepeated() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let first = driver(&browser, dir.path(), |config| {
        config.action_timeout = Duration::from_secs(20);
    });
    let (tab, _) = open_page(&first, 1, &site.url("/")).await;
    let observation = observe(&first, 2, &tab).await;
    let args = click_args(&tab, &observation, "Busy");

    // The worker dies while the click is in flight: the future is dropped with no outcome recorded,
    // which is what a crash leaves behind in the journal.
    let worker = {
        let driver = first.clone();
        let args = args.clone();
        tokio::spawn(async move { run(&driver, &call(3), "browser.click", args).await })
    };
    let started = common::eventually(Duration::from_secs(5), || async {
        journal_ops(dir.path()) == vec!["start", "complete", "start"]
    })
    .await;
    assert!(
        started,
        "the action never started: {:?}",
        journal_ops(dir.path())
    );
    worker.abort();
    let _ = worker.await;
    drop(first);
    assert!(
        common::eventually(Duration::from_secs(3), || async { site.hits("/hit") == 1 }).await,
        "the click did not reach the page before the crash"
    );

    // A new worker process opens the same session directory. Nothing in memory carries over.
    let second = driver(&browser, dir.path(), |_| {});
    assert_eq!(
        second.journal().in_doubt().len(),
        1,
        "the unfinished action is not in doubt"
    );
    // The recovery is recorded, so a second restart finds the same doubt rather than re-deriving it.
    assert_eq!(
        journal_ops(dir.path()),
        vec!["start", "complete", "start", "in_doubt"]
    );

    let redelivered = run(&second, &call(3), "browser.click", args).await;
    assert_eq!(redelivered["status"], "in_doubt", "{redelivered}");
    assert_eq!(redelivered["ledger_outcome"], "OUTCOME_UNKNOWN");
    assert_eq!(redelivered["error"]["retryable"], false);
    assert_eq!(redelivered["replayed"], true);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(site.hits("/hit"), 1, "the restarted worker clicked again");

    // And a third process agrees.
    drop(second);
    let third = driver(&browser, dir.path(), |_| {});
    assert_eq!(third.journal().in_doubt().len(), 1);
}

#[tokio::test]
async fn the_journal_survives_a_torn_final_line() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("actions.jsonl");
    let key = ActionKey::new("tc_a", "act_a");
    {
        let journal = ActionJournal::open(&path).expect("open");
        assert_eq!(
            journal.check(&key, "digest-1").expect("check"),
            quansio_qworkerd::browser::journal::Begin::Fresh
        );
        journal.start(&key, "digest-1").expect("start");
    }
    // A crash mid-write leaves half a line; the `start` before it is what matters.
    let mut text = std::fs::read_to_string(&path).expect("read");
    text.push_str("{\"op\":\"complete\",\"call_id\":\"tc_a\",\"act");
    std::fs::write(&path, text).expect("write");
    let reopened = ActionJournal::open(&path).expect("reopen");
    assert!(matches!(
        reopened.state(&key),
        Some(ActionState::InDoubt { .. })
    ));
    let conflict = reopened
        .check(&key, "digest-2")
        .expect_err("a different digest conflicts");
    assert_eq!(conflict.code.as_str(), "BROWSER_ACTION_REPLAY_CONFLICT");
}

#[tokio::test]
async fn side_effecting_actions_are_serialized_in_one_queue() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, _) = open_page(&driver, 1, &site.url("/")).await;
    let observation = observe(&driver, 2, &tab).await;
    // Two actions are submitted at the same moment. The first holds the renderer for most of a second.
    let slow = click_args(&tab, &observation, "Slow");
    let count = click_args(&tab, &observation, "Count");
    let (first_call, second_call) = (call(3), call(4));
    let (a, b) = tokio::join!(run(&driver, &first_call, "browser.click", slow), async {
        // Submitted a beat later, so the order is not an accident of scheduling.
        tokio::time::sleep(Duration::from_millis(100)).await;
        run(&driver, &second_call, "browser.click", count).await
    });
    assert_eq!(a["status"], "ok", "{a}");
    // The second sees the page the first left behind: its snapshot was invalidated by the first.
    assert_eq!(b["status"], "error", "{b}");
    assert_eq!(b["error"]["code"], "BROWSER_STALE_PAGE");
    // And their journal lines never interleave: the second did not begin until the first was done.
    assert_eq!(
        journal_ops(dir.path()),
        vec!["start", "complete", "start", "complete"]
    );
    assert_eq!(site.hits("/slow"), 1);
    assert_eq!(site.hits("/hit"), 0);
}

#[tokio::test]
async fn a_consequence_above_the_approved_tier_is_refused_before_dispatch() {
    let Some((browser, site, dir)) = fixture().await else {
        return;
    };
    let driver = driver(&browser, dir.path(), |_| {});
    let (tab, _) = open_page(&driver, 1, &site.url("/")).await;
    let observation = observe(&driver, 2, &tab).await;
    let args = click_args(&tab, &observation, "Buy now");

    // The runtime approved a tier-2 click. The button is a purchase (tier 4): the page's own semantics
    // classify it, and the click is not made.
    let mut ctx = call(3);
    ctx.approved_tier = Some(2);
    let reply = common::run(&driver, &ctx, "browser.click", args.clone()).await;
    assert_eq!(
        reply["error"]["code"], "BROWSER_EFFECT_TIER_EXCEEDED",
        "{reply}"
    );
    assert_eq!(reply["error"]["details"]["effect_class"], "payment.execute");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(site.hits("/buy"), 0);
    assert_eq!(
        journal_ops(dir.path()),
        vec!["start", "complete"],
        "a refusal before dispatch must write nothing beyond the navigation"
    );

    // Approved at tier 4, the same click goes through and says what it settles.
    let mut approved = call(4);
    approved.approved_tier = Some(4);
    let reply = common::run(&driver, &approved, "browser.click", args).await;
    assert_eq!(reply["status"], "ok", "{reply}");
    assert_eq!(
        reply["output"]["classified"]["effect_class"],
        "payment.execute"
    );
    assert_eq!(reply["output"]["classified"]["tier"], 4);
    assert!(common::eventually(Duration::from_secs(3), || async { site.hits("/buy") == 1 }).await);
}
