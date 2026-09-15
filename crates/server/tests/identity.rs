//! APP-002 identity, onboarding and settings-merge tests.

use quansio_server::control::identity::{merge_settings, platform_defaults, IdentityStore};
use quansio_server::control::schema;
use serde_json::json;

mod common;

#[test]
fn security_settings_merge_is_deterministic_and_most_restrictive() {
    let platform = platform_defaults();
    let tenant = json!({ "telemetry": true, "retention_days": 90, "theme": "dark" });
    let workspace = json!({ "telemetry": false, "clipboard_to_model": true, "retention_days": 30 });
    let user = json!({ "telemetry": true, "theme": "light", "retention_days": 365 });
    let first = merge_settings(&[
        platform.clone(),
        tenant.clone(),
        workspace.clone(),
        user.clone(),
    ]);
    let second = merge_settings(&[platform, tenant, workspace, user]);
    assert_eq!(first, second);
    assert_eq!(first["telemetry"], false, "deny wins");
    assert_eq!(
        first["clipboard_to_model"], false,
        "platform deny plus workspace allow still deny"
    );
    assert_eq!(first["unsigned_updates"], false);
    assert_eq!(first["retention_days"], 30, "minimum retention wins");
    assert_eq!(first["theme"], "light", "non-security keys override");
}

#[tokio::test]
async fn a_fresh_user_is_onboarded_without_a_config_file() {
    let name = common::scratch_name("id_onboard");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    let store = IdentityStore::new(pool.clone());
    let onboarded = store
        .onboard_email("ada@example.com", "Ada", "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA")
        .await
        .expect("onboard");
    assert!(onboarded.user_id.starts_with("usr_"));
    assert!(onboarded.tenant_id.starts_with("tn_"));
    assert!(onboarded.workspace_id.starts_with("ws_"));

    let methods: serde_json::Value =
        sqlx::query_scalar("SELECT auth_methods FROM users WHERE id = $1")
            .bind(&onboarded.user_id)
            .fetch_one(&pool)
            .await
            .expect("auth methods");
    let dumped = methods.to_string();
    assert!(dumped.contains("email_magic_link"));
    assert!(dumped.contains("sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"));
    assert!(!dumped.contains("sk-"), "raw provider secret leaked");
    assert!(!dumped.to_lowercase().contains("password"));

    let personal: bool = sqlx::query_scalar("SELECT personal FROM tenants WHERE id = $1")
        .bind(&onboarded.tenant_id)
        .fetch_one(&pool)
        .await
        .expect("personal");
    assert!(personal);

    let effective = store
        .effective_for_workspace(
            &onboarded.tenant_id,
            &onboarded.workspace_id,
            &onboarded.user_id,
        )
        .await
        .expect("settings");
    assert_eq!(effective["unsigned_updates"], false);
    common::drop_pool(&pool, &name).await;
}

#[tokio::test]
async fn onboarding_refuses_a_raw_secret() {
    let name = common::scratch_name("id_secret");
    let Some(pool) = common::fresh_database(&name).await else {
        common::blocked_marker();
        return;
    };
    schema::migrate(&pool).await.expect("migrate");
    let store = IdentityStore::new(pool.clone());
    let error = store
        .onboard_email("x@example.com", "X", "sk-live-not-a-handle")
        .await
        .expect_err("raw secret");
    assert!(error.to_string().contains("sec_"), "{error}");
    common::drop_pool(&pool, &name).await;
}
