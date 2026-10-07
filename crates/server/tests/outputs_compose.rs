//! The deployable composes the web-dev preview (ADP-006): with an object store configured, the composition root
//! starts a preview listener on loopback over the real database and mounts it in the output tools' host, so
//! `webdev.preview_open` is not a tool that exists only in tests.
//!
//! Its own test binary because it sets process environment variables (the composition root reads the
//! environment). Environment: `QUANSIO_TEST_POSTGRES_URL`, `QUANSIO_TEST_MINIO_*` (see `tests/artifacts.rs`).

mod common;

#[tokio::test]
async fn the_deployable_starts_a_loopback_preview_server_when_it_has_an_object_store() {
    let Some(dsn) = common::admin_url() else {
        common::blocked_marker();
        return;
    };
    std::env::set_var("QUANSIO_DATABASE_URL", &dsn);
    std::env::set_var("QUANSIO_BIND", "127.0.0.1:0");
    std::env::set_var("QUANSIO_SCHEDULER", "off");
    std::env::set_var("QUANSIO_OBJECT_STORE", "1");
    let config = quansio_server::composition::ServerConfig::from_env().expect("config");
    let composition = quansio_server::composition::Composition::new(config)
        .await
        .expect("compose");
    let preview = composition
        .api()
        .output_host()
        .and_then(quansio_server::artifacts::outputs::OutputToolHost::preview)
        .expect("the output host has a preview server");
    assert!(preview.addr().ip().is_loopback(), "{}", preview.addr());
    assert_ne!(preview.addr().port(), 0);
    // It answers, and answers a token it never issued as nothing.
    let url = format!("http://{}/p/{}/", preview.addr(), "ab".repeat(24));
    let status = reqwest::get(url).await.expect("a response").status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    // Without an object store there is nothing to serve from or stage suggestions in: no preview.
    std::env::remove_var("QUANSIO_OBJECT_STORE");
    let bare = quansio_server::composition::ServerConfig::from_env().expect("config");
    let bare = quansio_server::composition::Composition::new(bare)
        .await
        .expect("compose");
    assert!(bare.api().output_host().is_none());
}
