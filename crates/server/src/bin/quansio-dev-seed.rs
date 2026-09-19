//! Scratch-tenant seeding for end-to-end harnesses (APP-001; development and qualification
//! only).
//!
//! Clients are never persistence authority (DOSSIER.md §4.1) and nothing but Rust writes a
//! canonical authority table (§3), so a desktop, web or CLI harness that needs a tenant to
//! talk to gets one from this binary rather than from its own SQL. It creates (or drops) a
//! scratch database on the superuser DSN in `QUANSIO_DATABASE_URL`, applies the schema the
//! server itself applies, and seeds one tenant, user, workspace, objective node and the
//! `fs.read` allow policy the walking skeleton uses, then prints the scratch DSN as
//! `database_url=<url>` for the harness to hand to `quansio-server`.
//!
//! Usage:
//!   quansio-dev-seed create --database <name> --tenant <tn_…> --user <usr_…> \
//!       --workspace <ws_…> --node <wn_…>
//!   quansio-dev-seed drop --database <name>

use std::env;
use std::process::exit;

use quansio_server::control::schema;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection};

struct Args {
    command: String,
    database: String,
    tenant: String,
    user: String,
    workspace: String,
    node: String,
}

fn parse_args() -> Result<Args, String> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or("missing command: create | drop")?;
    let mut parsed = Args {
        command,
        database: String::new(),
        tenant: String::new(),
        user: String::new(),
        workspace: String::new(),
        node: String::new(),
    };
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--database" => parsed.database = value,
            "--tenant" => parsed.tenant = value,
            "--user" => parsed.user = value,
            "--workspace" => parsed.workspace = value,
            "--node" => parsed.node = value,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if parsed.database.is_empty() || !is_identifier(&parsed.database) {
        return Err("--database must be a plain SQL identifier".to_string());
    }
    if parsed.command == "create"
        && (parsed.tenant.is_empty()
            || parsed.user.is_empty()
            || parsed.workspace.is_empty()
            || parsed.node.is_empty())
    {
        return Err("create needs --tenant, --user, --workspace and --node".to_string());
    }
    Ok(parsed)
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn scratch_url(admin: &str, database: &str) -> String {
    match admin.rsplit_once('/') {
        Some((prefix, _)) => format!("{prefix}/{database}"),
        None => format!("{admin}/{database}"),
    }
}

async fn run(args: Args) -> Result<(), String> {
    let admin_url = env::var("QUANSIO_DATABASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or("QUANSIO_DATABASE_URL (superuser DSN) is not set")?;
    let mut admin = PgConnection::connect(&admin_url)
        .await
        .map_err(|error| format!("cannot connect to the admin database: {error}"))?;
    let drop_sql = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", args.database);
    sqlx::query(&drop_sql)
        .execute(&mut admin)
        .await
        .map_err(|error| format!("cannot drop {}: {error}", args.database))?;
    if args.command == "drop" {
        return Ok(());
    }
    if args.command != "create" {
        return Err(format!("unknown command {}", args.command));
    }
    let create_sql = format!("CREATE DATABASE {}", args.database);
    sqlx::query(&create_sql)
        .execute(&mut admin)
        .await
        .map_err(|error| format!("cannot create {}: {error}", args.database))?;
    admin.close().await.ok();

    let database_url = scratch_url(&admin_url, &args.database);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .map_err(|error| format!("cannot connect to the scratch database: {error}"))?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .map_err(|error| format!("cannot apply the schema: {error}"))?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|error| format!("begin: {error}"))?;
    sqlx::query("INSERT INTO tenants (id, name) VALUES ($1, 'e2e seed')")
        .bind(&args.tenant)
        .execute(&mut *tx)
        .await
        .map_err(|error| format!("tenant: {error}"))?;
    sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, 'e2e seed')")
        .bind(&args.user)
        .bind(format!("{}@example.com", args.user))
        .execute(&mut *tx)
        .await
        .map_err(|error| format!("user: {error}"))?;
    sqlx::query("INSERT INTO workspaces (id, tenant_id, name) VALUES ($1, $2, 'e2e seed')")
        .bind(&args.workspace)
        .bind(&args.tenant)
        .execute(&mut *tx)
        .await
        .map_err(|error| format!("workspace: {error}"))?;
    schema::set_tenant_context(&mut tx, &args.tenant)
        .await
        .map_err(|error| format!("tenant context: {error}"))?;
    sqlx::query(
        "INSERT INTO work_nodes (id, tenant_id, workspace_id, kind, title, created_by) \
         VALUES ($1, $2, $3, 'objective', 'e2e seed', '{\"kind\":\"user\",\"id\":\"usr_seed\"}'::jsonb)",
    )
    .bind(&args.node)
    .bind(&args.tenant)
    .bind(&args.workspace)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("work node: {error}"))?;
    let rules = json!([
        { "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }
    ]);
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0E2E01', $1, NULL, 'tenant', $2)",
    )
    .bind(&args.tenant)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("policy: {error}"))?;
    tx.commit()
        .await
        .map_err(|error| format!("commit: {error}"))?;
    pool.close().await;
    println!("database_url={database_url}");
    Ok(())
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("quansio-dev-seed: {message}");
            exit(2);
        }
    };
    if let Err(message) = run(args).await {
        eprintln!("quansio-dev-seed: {message}");
        exit(1);
    }
}
