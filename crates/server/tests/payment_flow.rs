//! The one-time payment-input flow end to end (ADP-009, DOMAIN.md §13.9 C): a **real PostgreSQL** holds the sealed values (the
//! secret broker, with a real data-key envelope), a **real headless Chrome** drives a checkout page on loopback through the real
//! `BrowserDriver`, and the only card number typed is a payment provider's published test number.
//!
//! The person's supply and the worker's take are the runtime's two halves; the tests prove the properties that make "card data
//! never enters model context" true of the whole path, not of one half:
//!
//! * the value is sealed (the stored bytes are not the value) and found **nowhere** in the database: every table is scanned,
//!   as text, for the number and the security code, before, during and after;
//! * a take returns the value once and overwrites it in the same transaction (the sealed bytes now open to nothing, the handle is
//!   revoked); a second take, a take for another workspace and a take after the time ran out find nothing, and a lapsed value is
//!   cleared even though nobody asked for it;
//! * the worker types it into the field the page labels as a card number, masked, on the origin it was supplied for, and the
//!   reply, the journal and the session directory never hold it;
//! * a supply that breaks a rule seals nothing.
//!
//! Environment: `QUANSIO_TEST_POSTGRES_URL` and a Chrome; without either the tests print `BLOCKED_EXTERNAL`.

use std::collections::BTreeMap;

use quansio_machine::secrets::payment::{
    PaymentInputConfig, PaymentInputError, PaymentInputs, Supply, PAYMENT_FIELDS,
};
use quansio_machine::secrets::{open, LocalMasterKeyProvider, SecretBroker, SecretStatus};
use quansio_qworkerd::browser::secrets::{
    PaymentField, PaymentRequest, PaymentValue, SecretPort, SecretText,
};
use quansio_qworkerd::browser::{
    BrowserCallContext, BrowserDriver, BrowserFault, BrowserSession, DriverConfig, FaultCode,
};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

mod common;
use common::outputs::Chrome;
use common::{admin_url, blocked_marker, drop_pool, fresh_database, scratch_name, seed_tenant};

const TENANT: &str = "tn_01J8Z3K6F1N8VQ2X5W9YADP00B";
const USER: &str = "usr_01J8Z3K6F1N8VQ2X5W9YADP00B";
const WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP00B";
const OTHER_WORKSPACE: &str = "ws_01J8Z3K6F1N8VQ2X5W9YADP00C";
const WORK_NODE: &str = "wn_01J8Z3K6F1N8VQ2X5W9YADP00B";
const SESSION: &str = "bsn_01J8Z3K6F1N8VQ2X5W9YADP00B";
/// The instant of the test, canonical: the database's own clock bounds an expiry from below, so the instants are real ones.
fn now() -> &'static str {
    static INSTANT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANT.get_or_init(|| chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// Ten minutes after [`now`]: past a one-minute value's time.
fn later() -> &'static str {
    static INSTANT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTANT.get_or_init(|| quansio_machine::secrets::add_seconds(now(), 600).expect("canonical"))
}

/// A payment provider's published test card number and a test security code.
const TEST_CARD: &str = "4242424242424242";
const TEST_CVC: &str = "6803";

fn provider() -> LocalMasterKeyProvider {
    LocalMasterKeyProvider::from_master_key(&[7u8; 32], "local:payment-test").expect("a master key")
}

const PAGE: &str = r#"<html><head><title>Checkout</title></head><body>
<input id="num" name="n1" type="text" autocomplete="cc-number" aria-label="Card number">
<input id="cvc" name="n2" type="text" autocomplete="cc-csc" aria-label="Security code">
</body></html>"#;

/// A one-page HTTP server on loopback; its address is the origin the values are supplied for.
async fn serve() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{PAGE}",
                    PAGE.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (port, task)
}

struct Rig {
    name: String,
    pool: PgPool,
}

async fn rig(prefix: &str) -> Option<Rig> {
    let name = admin_url().map(|_| scratch_name(prefix))?;
    let pool = fresh_database(&name).await?;
    quansio_server::control::schema::migrate(&pool)
        .await
        .expect("migrate");
    seed_tenant(&pool, TENANT, USER, WORKSPACE, WORK_NODE).await;
    // A second workspace of the same tenant.
    let mut tx = pool.begin().await.expect("begin");
    quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
        .await
        .expect("tenant");
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'other')")
        .bind(OTHER_WORKSPACE)
        .bind(TENANT)
        .execute(&mut *tx)
        .await
        .expect("second workspace");
    tx.commit().await.expect("commit");
    Some(Rig { name, pool })
}

impl Rig {
    async fn finish(self) {
        drop_pool(&self.pool, &self.name).await;
    }

    async fn supply(
        &self,
        workspace: &str,
        origin: &str,
        fields: &[(&str, &str)],
        ttl: Option<i64>,
        at: &str,
    ) -> Result<usize, PaymentInputError> {
        let mut tx = self.pool.begin().await.expect("begin");
        quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
            .await
            .expect("tenant");
        let supplied = PaymentInputs::supply(
            &mut tx,
            &provider(),
            &PaymentInputConfig::builtin(),
            &Supply {
                tenant_id: TENANT,
                workspace_id: workspace,
                origin,
                fields: fields.iter().map(|(f, v)| (*f, v.as_bytes())).collect(),
                ttl_seconds: ttl,
                at,
            },
        )
        .await;
        match supplied {
            Ok(supplied) => {
                tx.commit().await.expect("commit");
                Ok(supplied.len())
            }
            Err(error) => {
                tx.rollback().await.expect("rollback");
                Err(error)
            }
        }
    }

    /// Every table, as text, searched for each needle: the places a value could leak to are all of them.
    async fn find_anywhere(&self, needles: &[&str]) -> Vec<(String, String)> {
        let tables: Vec<String> = sqlx::query_scalar("SELECT tablename FROM pg_tables WHERE schemaname = 'public' AND left(tablename, 1) <> '_'")
            .fetch_all(&self.pool)
            .await
            .expect("tables");
        let mut found = Vec::new();
        for table in tables {
            let rows = sqlx::query(&format!("SELECT t::text AS row FROM {table} t"))
                .fetch_all(&self.pool)
                .await
                .expect("scan");
            for row in rows {
                let text: String = row.try_get("row").unwrap_or_default();
                for needle in needles {
                    if text.contains(needle) {
                        found.push((table.clone(), (*needle).to_string()));
                    }
                }
            }
        }
        found
    }

    async fn ciphertexts(&self) -> Vec<(String, String, Option<Vec<u8>>)> {
        sqlx::query("SELECT id, status, ciphertext FROM secret_handles WHERE provider = 'payment_input' ORDER BY created_at, id")
            .fetch_all(&self.pool)
            .await
            .expect("handles")
            .iter()
            .map(|row| (row.get("id"), row.get("status"), row.get("ciphertext")))
            .collect()
    }
}

/// The worker's payment port over the runtime's take, in a real transaction.
fn port(pool: PgPool, workspace: &'static str) -> SecretPort {
    SecretPort::new(|_| {
        Box::pin(async {
            Err(BrowserFault::new(
                FaultCode::SecretUnavailable,
                "no placeholder in this suite",
            ))
        })
    })
    .with_payments(move |request: PaymentRequest| {
        let pool = pool.clone();
        Box::pin(async move {
            let mut tx = pool.begin().await.map_err(|_| {
                BrowserFault::new(FaultCode::SecretUnavailable, "the runtime is unavailable")
            })?;
            quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
                .await
                .map_err(|_| BrowserFault::new(FaultCode::SecretUnavailable, "tenant"))?;
            let taken = PaymentInputs::take(
                &mut tx,
                &provider(),
                TENANT,
                workspace,
                request.field.as_str(),
                &request.action_id,
                now(),
            )
            .await;
            match taken {
                Ok(taken) => {
                    tx.commit()
                        .await
                        .map_err(|_| BrowserFault::new(FaultCode::SecretUnavailable, "commit"))?;
                    let value = String::from_utf8(taken.materialization.material.expose().to_vec())
                        .map_err(|_| {
                            BrowserFault::new(FaultCode::SecretUnavailable, "the value is not text")
                        })?;
                    Ok(PaymentValue {
                        value: SecretText::new(value),
                        allowed_origin: taken.origin,
                    })
                }
                Err(_) => {
                    let _ = tx.rollback().await;
                    Err(BrowserFault::new(
                        FaultCode::SecretUnavailable,
                        "the person has supplied nothing usable for this field",
                    ))
                }
            }
        })
    })
}

fn driver(chrome: &Chrome, dir: &std::path::Path, port: SecretPort) -> BrowserDriver {
    let mut config = DriverConfig::new(chrome.port, dir);
    config.action_timeout = std::time::Duration::from_millis(2500);
    config.settle_timeout = std::time::Duration::from_millis(2000);
    BrowserDriver::open(
        config,
        BrowserSession::open(SESSION, TENANT, "tgt_01J8Z3K6F1N8VQ2X5W9YADP00B"),
        Some(port),
    )
    .expect("driver")
}

fn ctx(n: u32) -> BrowserCallContext {
    BrowserCallContext {
        run_id: "run_01J8Z3K6F1N8VQ2X5W9YADP00B".to_string(),
        tool_call_id: format!("tc_pay{n:04}"),
        action_id: format!("act_pay{n:04}"),
        approved_tier: Some(4),
    }
}

async fn tool(driver: &BrowserDriver, n: u32, name: &str, mut args: Value) -> Value {
    args["session_id"] = json!(SESSION);
    driver.dispatch(name, &args, &ctx(n)).await.to_json()
}

async fn page_value(chrome: &Chrome, id: &str) -> Option<String> {
    let targets: Value = reqwest_get(chrome.port, "/json/list").await;
    let socket = targets.as_array()?.iter().find(|t| t["type"] == "page")?["webSocketDebuggerUrl"]
        .as_str()?
        .to_string();
    let (mut ws, _) = tokio_tungstenite::connect_async(&socket).await.ok()?;
    use futures_util::{SinkExt, StreamExt};
    let request = json!({"id": 1, "method": "Runtime.evaluate", "params": {"expression": format!("document.getElementById({id:?}).value"), "returnByValue": true}});
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        request.to_string(),
    ))
    .await
    .ok()?;
    while let Some(Ok(message)) = ws.next().await {
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            let value: Value = serde_json::from_str(&text).ok()?;
            if value["id"] == 1 {
                return value["result"]["result"]["value"]
                    .as_str()
                    .map(str::to_string);
            }
        }
    }
    None
}

/// `GET path` from Chrome's DevTools HTTP endpoint. The server may keep the connection open, so the read ends at the timeout
/// with whatever the answer was by then.
async fn reqwest_get(port: u16, path: &str) -> Value {
    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("timeout");
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .expect("write");
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap_or("[]")).unwrap_or(Value::Null)
    })
    .await
    .expect("join")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_supplied_value_is_sealed_found_nowhere_typed_once_and_gone() {
    let Some(rig) = rig("adp009_pay").await else {
        blocked_marker();
        return;
    };
    let Some(chrome) = Chrome::launch() else {
        eprintln!("BLOCKED_EXTERNAL: no Chrome on this host");
        rig.finish().await;
        return;
    };
    let (site, server) = serve().await;
    let origin = format!("http://127.0.0.1:{site}");

    // The person supplies the number and the security code for this checkout's origin.
    rig.supply(
        WORKSPACE,
        &origin,
        &[("card_number", TEST_CARD), ("cvc", TEST_CVC)],
        None,
        now(),
    )
    .await
    .expect("supplied");
    let sealed = rig.ciphertexts().await;
    assert_eq!(sealed.len(), 2);
    for (_, status, cipher) in &sealed {
        assert_eq!(status, "active");
        let cipher = cipher.as_ref().expect("sealed");
        assert!(
            !cipher
                .windows(TEST_CARD.len())
                .any(|w| w == TEST_CARD.as_bytes()),
            "the stored bytes are not the value"
        );
        assert!(!cipher
            .windows(TEST_CVC.len())
            .any(|w| w == TEST_CVC.as_bytes()));
    }
    assert_eq!(
        rig.find_anywhere(&[TEST_CARD, TEST_CVC]).await,
        vec![],
        "no table holds a value"
    );

    // The worker fills the card number into the field the page labels as one.
    let dir = tempfile::tempdir().expect("session dir");
    let driver = driver(&chrome, dir.path(), port(rig.pool.clone(), WORKSPACE));
    let opened = tool(
        &driver,
        1,
        "browser.navigate",
        json!({"url": format!("{origin}/")}),
    )
    .await;
    assert_eq!(opened["status"], "ok", "{opened}");
    let tab = opened["output"]["tab_id"]
        .as_str()
        .expect("tab")
        .to_string();
    let observed =
        tool(&driver, 2, "browser.observe", json!({"tab_id": tab})).await["output"].clone();
    let element = observed["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["name"] == "Card number")
        .expect("the field")["ref"]
        .clone();
    let filled = tool(
        &driver,
        3,
        "browser.fill_payment_field",
        json!({"tab_id": tab, "expected_document_revision": observed["document_revision"], "snapshot_id": observed["snapshot_id"], "ref": element, "field": "card_number"}),
    )
    .await;
    assert_eq!(filled["status"], "ok", "{filled}");
    assert_eq!(filled["output"]["masked"], true);
    assert_eq!(filled["output"]["verified"], true);
    assert_eq!(
        page_value(&chrome, "num").await.as_deref(),
        Some(TEST_CARD),
        "the page holds what the person supplied"
    );
    assert_eq!(
        page_value(&chrome, "cvc").await.as_deref(),
        Some(""),
        "the security code was not asked for"
    );
    assert!(!filled.to_string().contains(TEST_CARD));

    // The take deleted it: the sealed bytes open to nothing and the handle is revoked, and the number is nowhere.
    let after = rig.ciphertexts().await;
    let revoked: Vec<_> = after
        .iter()
        .filter(|(_, status, _)| status == "revoked")
        .collect();
    assert_eq!(
        revoked.len(),
        1,
        "the card number's handle is revoked and the security code's still waits"
    );
    let cleared = open(
        &provider(),
        revoked[0].2.as_ref().expect("a seal of nothing"),
    )
    .expect("opens");
    assert!(cleared.is_empty(), "overwritten with nothing");
    assert_eq!(
        rig.find_anywhere(&[TEST_CARD]).await,
        vec![],
        "the number is nowhere in the database"
    );
    for path in walk(dir.path()) {
        let bytes = std::fs::read(&path).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(TEST_CARD),
            "{} holds the number",
            path.display()
        );
    }

    // A second fill of the same field finds nothing: the person supplies it again.
    let observed =
        tool(&driver, 4, "browser.observe", json!({"tab_id": tab})).await["output"].clone();
    let element = observed["elements"]
        .as_array()
        .expect("elements")
        .iter()
        .find(|e| e["name"] == "Card number")
        .expect("the field")["ref"]
        .clone();
    let again = tool(
        &driver,
        5,
        "browser.fill_payment_field",
        json!({"tab_id": tab, "expected_document_revision": observed["document_revision"], "snapshot_id": observed["snapshot_id"], "ref": element, "field": "card_number"}),
    )
    .await;
    assert_eq!(
        again["error"]["code"], "BROWSER_SECRET_UNAVAILABLE",
        "{again}"
    );
    assert_eq!(again["error"]["details"]["stage"], "take");

    server.abort();
    rig.finish().await;
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
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
async fn a_value_is_taken_once_for_its_workspace_and_a_lapsed_one_is_cleared_unasked() {
    let Some(rig) = rig("adp009_paytake").await else {
        blocked_marker();
        return;
    };
    let origin = "https://shop.example";
    rig.supply(
        WORKSPACE,
        origin,
        &[("card_number", TEST_CARD)],
        Some(60),
        now(),
    )
    .await
    .expect("supplied");

    let take = |workspace: &'static str, at: &'static str| {
        let pool = rig.pool.clone();
        async move {
            let mut tx = pool.begin().await.expect("begin");
            quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
                .await
                .expect("tenant");
            let taken = PaymentInputs::take(
                &mut tx,
                &provider(),
                TENANT,
                workspace,
                "card_number",
                "eff_x",
                at,
            )
            .await;
            match taken {
                Ok(taken) => {
                    let value = String::from_utf8(taken.materialization.material.expose().to_vec())
                        .expect("text");
                    tx.commit().await.expect("commit");
                    Ok((value, taken.origin))
                }
                Err(error) => {
                    tx.commit().await.expect("commit");
                    Err(error)
                }
            }
        }
    };
    // Another workspace of the same tenant finds nothing; the value is for the workspace that received it.
    assert!(matches!(
        take(OTHER_WORKSPACE, now()).await,
        Err(PaymentInputError::Unavailable { .. })
    ));
    // The right workspace gets it, with the origin it was supplied for.
    let (value, from) = take(WORKSPACE, now()).await.expect("taken");
    assert_eq!((value.as_str(), from.as_str()), (TEST_CARD, origin));
    // Once.
    assert!(matches!(
        take(WORKSPACE, now()).await,
        Err(PaymentInputError::Unavailable { .. })
    ));

    // A value that waits too long is cleared when anything next touches the workspace, whether or not anyone asked for it.
    rig.supply(WORKSPACE, origin, &[("cvc", TEST_CVC)], Some(60), now())
        .await
        .expect("supplied");
    assert!(matches!(
        {
            let mut tx = rig.pool.begin().await.expect("begin");
            quansio_server::control::schema::set_tenant_context(&mut tx, TENANT)
                .await
                .expect("tenant");
            let result = PaymentInputs::take(
                &mut tx,
                &provider(),
                TENANT,
                WORKSPACE,
                "cvc",
                "eff_y",
                later(),
            )
            .await;
            tx.commit().await.expect("commit");
            result
        },
        Err(PaymentInputError::Unavailable { .. })
    ));
    let rows = rig.ciphertexts().await;
    assert!(
        rows.iter().all(|(_, status, _)| status == "revoked"),
        "{rows:?}"
    );
    for (_, _, cipher) in rows {
        assert!(
            open(&provider(), cipher.as_ref().expect("a seal"))
                .expect("opens")
                .is_empty(),
            "nothing is left sealed"
        );
    }
    assert_eq!(rig.find_anywhere(&[TEST_CARD, TEST_CVC]).await, vec![]);
    // The handle is the broker's: its status is the broker's.
    let mut conn = rig.pool.acquire().await.expect("conn");
    let handles = SecretBroker::list(&mut conn, TENANT).await.expect("list");
    assert!(handles.iter().all(|h| h.status == SecretStatus::Revoked));
    rig.finish().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn supplying_a_field_again_overwrites_the_older_value_and_a_bad_supply_seals_nothing() {
    let Some(rig) = rig("adp009_paysupply").await else {
        blocked_marker();
        return;
    };
    let origin = "https://shop.example";
    rig.supply(
        WORKSPACE,
        origin,
        &[("card_number", "4000056655665556")],
        None,
        now(),
    )
    .await
    .expect("first");
    rig.supply(
        WORKSPACE,
        origin,
        &[("card_number", TEST_CARD)],
        None,
        now(),
    )
    .await
    .expect("second");
    let rows = rig.ciphertexts().await;
    assert_eq!(
        rows.iter()
            .filter(|(_, status, _)| status == "active")
            .count(),
        1,
        "latest wins"
    );
    assert_eq!(
        rig.find_anywhere(&["4000056655665556"]).await,
        vec![],
        "the older value is overwritten, not kept"
    );

    let sealed_before = rig.ciphertexts().await.len();
    let long = "9".repeat(129);
    let rules: BTreeMap<&str, Result<usize, PaymentInputError>> = BTreeMap::from([
        (
            "an unknown field",
            rig.supply(WORKSPACE, origin, &[("ssn", "123")], None, now())
                .await,
        ),
        (
            "a duplicate",
            rig.supply(
                WORKSPACE,
                origin,
                &[("cvc", "1"), ("cvc", "2")],
                None,
                now(),
            )
            .await,
        ),
        (
            "an empty value",
            rig.supply(WORKSPACE, origin, &[("cvc", "")], None, now())
                .await,
        ),
        (
            "an over-long value",
            rig.supply(WORKSPACE, origin, &[("cvc", long.as_str())], None, now())
                .await,
        ),
        (
            "an origin with a path",
            rig.supply(
                WORKSPACE,
                "https://shop.example/checkout",
                &[("cvc", "1")],
                None,
                now(),
            )
            .await,
        ),
        (
            "an origin that is not one",
            rig.supply(WORKSPACE, "shop.example", &[("cvc", "1")], None, now())
                .await,
        ),
        (
            "credentials in the origin",
            rig.supply(
                WORKSPACE,
                "https://u:p@shop.example",
                &[("cvc", "1")],
                None,
                now(),
            )
            .await,
        ),
        (
            "a value that waits too long",
            rig.supply(WORKSPACE, origin, &[("cvc", "1")], Some(100_000), now())
                .await,
        ),
        (
            "nothing at all",
            rig.supply(WORKSPACE, origin, &[], None, now()).await,
        ),
    ]);
    for (what, result) in rules {
        assert!(
            matches!(result, Err(PaymentInputError::Invalid(_))),
            "{what}: {result:?}"
        );
    }
    assert_eq!(
        rig.ciphertexts().await.len(),
        sealed_before,
        "a refused supply seals nothing"
    );

    // The fields a person may supply are the fields the tool may fill.
    let worker: Vec<&str> = PaymentField::ALL.iter().map(|f| f.as_str()).collect();
    assert_eq!(worker, PAYMENT_FIELDS.to_vec());
    rig.finish().await;
}
