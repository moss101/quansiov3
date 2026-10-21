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
//!       --workspace <ws_…> --node <wn_…> [--ask-writes] [--intelligence] [--live] [--worker] \
//!       [--member <usr_…>:<tenant role>:<workspace role>]…
//!   quansio-dev-seed drop --database <name>
//!
//! The seeded user owns the tenant and administers the workspace; `--member` adds users
//! with other roles (a viewer, an approver…) so a harness can prove the server's RBAC.
//!
//! `--intelligence` also seeds what a knowledge/memory harness deletes (APP-011): three
//! knowledge entries (active, verified, candidate), one active memory entry, and a derived
//! semantic index row for each entry in force, under the fixed ids in [`INTELLIGENCE_IDS`].
//!
//! `--live` also seeds what the live browser/computer/terminal surface observes and takes
//! over (APP-007): one READY execution target, a worker AgentThread and a RUNNING run on it,
//! the run's browser session (agent-held, active, on a page), one open terminal session
//! with a durable cursor, and the target's native-computer control row, under the fixed
//! ids in [`LIVE_IDS`].
//!
//! `--worker` also registers one READY `customer_private_worker` target, [`WORKER_TARGET`], which a `qworkerd` started
//! with `--target` set to it can attach to (EXEC-002): `fs.*`, `terminal.exec` and `process.spawn` then run on that
//! worker. The target is written through the machine authority, like the `--live` one.

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
    /// Also seed `ask` rules for writes, so an `fs.write` parks the run for approval.
    ask_writes: bool,
    /// Also seed knowledge, memory and their derived index rows.
    intelligence: bool,
    /// Also seed a target with a run, a browser session, a terminal session and a
    /// computer-control row.
    live: bool,
    /// Also register a READY `customer_private_worker` target a `qworkerd` can attach to.
    worker: bool,
    /// Also let agents open pages: `network.egress.new_destination` and `read.external` are allowed, so a
    /// `browser.navigate` runs on the worker without parking for an approval.
    browse: bool,
    /// Extra users with roles: `--member <usr_…>:<tenant role>:<workspace role>`.
    members: Vec<(String, String, String)>,
}

/// Fixed ids of the `--intelligence` rows: knowledge in three lifecycle states, active
/// memory, and one derived embedding per entry in force.
pub const INTELLIGENCE_IDS: [(&str, &str); 7] = [
    ("knowledge_active", "kn_01J8Z3K6F1N8VQ2X5W9Y0E2KN1"),
    ("knowledge_verified", "kn_01J8Z3K6F1N8VQ2X5W9Y0E2KN2"),
    ("knowledge_candidate", "kn_01J8Z3K6F1N8VQ2X5W9Y0E2KN3"),
    ("memory_active", "mem_01J8Z3K6F1N8VQ2X5W9Y0E2MM1"),
    (
        "embedding_knowledge_active",
        "emb_01J8Z3K6F1N8VQ2X5W9Y0E2EM1",
    ),
    (
        "embedding_knowledge_verified",
        "emb_01J8Z3K6F1N8VQ2X5W9Y0E2EM2",
    ),
    ("embedding_memory_active", "emb_01J8Z3K6F1N8VQ2X5W9Y0E2EM3"),
];

/// Fixed ids of the `--live` rows: the execution target, the worker thread and run on it,
/// the run's browser session, its terminal session and the terminal's last command.
pub const LIVE_IDS: [(&str, &str); 6] = [
    ("target", "tgt_01J8Z3K6F1N8VQ2X5W9Y0E2TG1"),
    ("agent_thread", "ath_01J8Z3K6F1N8VQ2X5W9Y0E2AT1"),
    ("run", "run_01J8Z3K6F1N8VQ2X5W9Y0E2RN1"),
    ("browser_session", "bsn_01J8Z3K6F1N8VQ2X5W9Y0E2BS1"),
    ("terminal_session", "tsn_01J8Z3K6F1N8VQ2X5W9Y0E2TS1"),
    ("terminal_last_command", "cmd_01J8Z3K6F1N8VQ2X5W9Y0E2CM1"),
];

/// The durable byte offset the `--live` terminal session has been consumed up to.
pub const LIVE_TERMINAL_CURSOR: u64 = 128;

fn intelligence_id(key: &str) -> &'static str {
    INTELLIGENCE_IDS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, id)| *id)
        .expect("a fixed intelligence id")
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
        ask_writes: false,
        intelligence: false,
        live: false,
        worker: false,
        browse: false,
        members: Vec::new(),
    };
    while let Some(flag) = args.next() {
        if flag == "--ask-writes" {
            parsed.ask_writes = true;
            continue;
        }
        if flag == "--intelligence" {
            parsed.intelligence = true;
            continue;
        }
        if flag == "--live" {
            parsed.live = true;
            continue;
        }
        if flag == "--worker" {
            parsed.worker = true;
            continue;
        }
        if flag == "--browse" {
            parsed.browse = true;
            continue;
        }
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--database" => parsed.database = value,
            "--tenant" => parsed.tenant = value,
            "--user" => parsed.user = value,
            "--workspace" => parsed.workspace = value,
            "--node" => parsed.node = value,
            "--member" => {
                let mut parts = value.splitn(3, ':');
                let (Some(user), Some(tenant_role), Some(workspace_role)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    return Err("--member needs <usr_…>:<tenant role>:<workspace role>".to_string());
                };
                parsed.members.push((
                    user.to_string(),
                    tenant_role.to_string(),
                    workspace_role.to_string(),
                ));
            }
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
        // Stored the way sign-in normalizes an address (lower case), so a seeded user can sign in
        // through the real magic-link flow instead of being onboarded a second time.
        .bind(format!("{}@example.com", args.user).to_ascii_lowercase())
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
    // Roles are memberships (DOMAIN.md §2): the seeded user owns the tenant and
    // administers the workspace, as onboarding would have made them.
    seed_membership(
        &mut tx,
        &args.tenant,
        &args.workspace,
        &args.user,
        "owner",
        "admin",
    )
    .await?;
    for (user, tenant_role, workspace_role) in &args.members {
        sqlx::query("INSERT INTO users (id, primary_email, display_name) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING")
            .bind(user)
            .bind(format!("{user}@example.com").to_ascii_lowercase())
            .bind(format!("e2e {tenant_role}/{workspace_role}"))
            .execute(&mut *tx)
            .await
            .map_err(|error| format!("member user {user}: {error}"))?;
        seed_membership(
            &mut tx,
            &args.tenant,
            &args.workspace,
            user,
            tenant_role,
            workspace_role,
        )
        .await?;
    }
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
    let mut rules = vec![
        json!({ "effect_class": "read.internal", "resource": { "kind": "fs", "selector": "**" }, "decision": "allow" }),
    ];
    if args.ask_writes {
        // Writes ask a human first (APP-006's approval loop); without an execution target
        // the runtime classifies fs.write as the host class, so both are governed.
        rules.push(json!({ "effect_class": "fs.write.workspace", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" }));
        rules.push(json!({ "effect_class": "fs.write.host", "resource": { "kind": "fs", "selector": "**" }, "decision": "ask" }));
    }
    if args.browse {
        rules.push(json!({ "effect_class": "network.egress.new_destination", "resource": { "kind": "domain", "selector": "*" }, "decision": "allow" }));
        rules.push(json!({ "effect_class": "read.external", "resource": { "kind": "domain", "selector": "*" }, "decision": "allow" }));
        rules.push(json!({ "effect_class": "read.internal", "resource": { "kind": "target", "selector": "browser" }, "decision": "allow" }));
    }
    let rules = json!(rules);
    sqlx::query(
        "INSERT INTO policies (id, tenant_id, workspace_id, scope, rules) \
         VALUES ('pol_01J8Z3K6F1N8VQ2X5W9Y0E2E01', $1, NULL, 'tenant', $2)",
    )
    .bind(&args.tenant)
    .bind(rules)
    .execute(&mut *tx)
    .await
    .map_err(|error| format!("policy: {error}"))?;
    if args.intelligence {
        seed_intelligence(&mut tx, &args).await?;
    }
    if args.live {
        seed_live(&mut tx, &args).await?;
    }
    if args.worker {
        seed_worker(&mut tx, &args).await?;
    }
    tx.commit()
        .await
        .map_err(|error| format!("commit: {error}"))?;
    pool.close().await;
    println!("database_url={database_url}");
    Ok(())
}

/// The target `--worker` registers, which a `qworkerd` names with `--target`.
pub const WORKER_TARGET: &str = "tgt_01J8Z3K6F1N8VQ2X5W9Y0E2WK1";

/// The `--worker` row (EXEC-002): one READY target on the substrate that dials in, registered and walked to `READY` through
/// the machine authority, the way a controller would.
async fn seed_worker(tx: &mut PgConnection, args: &Args) -> Result<(), String> {
    use quansio_machine::control::{
        MachineControl, NewTarget, Substrate, TargetClass, TargetFence, TargetStatus,
    };

    MachineControl::register(
        tx,
        &args.tenant,
        &NewTarget {
            id: WORKER_TARGET.to_string(),
            workspace_id: args.workspace.clone(),
            class: TargetClass::PersistentWorkspaceComputer,
            substrate: Substrate::CustomerPrivateWorker,
            image_digest: None,
            desired_state: Some("READY".to_string()),
        },
    )
    .await
    .map_err(|error| format!("worker target: {error}"))?;
    for to in [TargetStatus::Provisioning, TargetStatus::Ready] {
        let current = MachineControl::load(tx, &args.tenant, WORKER_TARGET)
            .await
            .map_err(|error| format!("worker target: {error}"))?;
        MachineControl::transition(
            tx,
            &args.tenant,
            WORKER_TARGET,
            &TargetFence {
                generation: current.generation,
            },
            to,
        )
        .await
        .map_err(|error| format!("worker target {to:?}: {error}"))?;
    }
    println!("worker_target={WORKER_TARGET}");
    Ok(())
}

/// The `--live` rows (APP-007), one RUNNING run's worth: the ids are fixed so a harness can
/// name the session it observes and prove it is the same one after a takeover. The target,
/// its browser and terminal sessions and its computer-control row are written through the
/// machine authority (`crates/machine`), which is the only writer of those tables; the
/// thread and run are seed rows like the tenant's.
async fn seed_live(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    args: &Args,
) -> Result<(), String> {
    use quansio_machine::computer_use::store::ComputerControlStore;
    use quansio_machine::control::browser::BrowserSessionStore;
    use quansio_machine::control::terminal::TerminalStore;
    use quansio_machine::control::{
        MachineControl, NewTarget, Substrate, TargetClass, TargetFence, TargetStatus,
    };

    let id = |name: &str| {
        LIVE_IDS
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value)
            .expect("live id")
    };
    let target = id("target");
    MachineControl::register(
        tx,
        &args.tenant,
        &NewTarget {
            id: target.to_string(),
            workspace_id: args.workspace.clone(),
            class: TargetClass::PersistentWorkspaceComputer,
            substrate: Substrate::LocalCapsuleMacos,
            image_digest: None,
            desired_state: Some("READY".to_string()),
        },
    )
    .await
    .map_err(|error| format!("live target: {error}"))?;
    for to in [TargetStatus::Provisioning, TargetStatus::Ready] {
        let current = MachineControl::load(tx, &args.tenant, target)
            .await
            .map_err(|error| format!("live target: {error}"))?;
        MachineControl::transition(
            tx,
            &args.tenant,
            target,
            &TargetFence {
                generation: current.generation,
            },
            to,
        )
        .await
        .map_err(|error| format!("live target {to:?}: {error}"))?;
    }
    sqlx::query(
        "INSERT INTO agent_threads (id, tenant_id, workspace_id, agent_kind, work_node_id, status, \
                                    execution_target_id) \
         VALUES ($1, $2, $3, 'worker', $4, 'ACTIVE', $5)",
    )
    .bind(id("agent_thread"))
    .bind(&args.tenant)
    .bind(&args.workspace)
    .bind(&args.node)
    .bind(target)
    .execute(&mut **tx)
    .await
    .map_err(|error| format!("live agent thread: {error}"))?;
    sqlx::query(
        "INSERT INTO runs (id, tenant_id, workspace_id, work_node_id, agent_thread_id, generation, \
                           status, trigger_kind, trigger_ref, execution_target_id, started_at) \
         VALUES ($1, $2, $3, $4, $5, 1, 'RUNNING', 'manual', 'e2e seed', $6, now())",
    )
    .bind(id("run"))
    .bind(&args.tenant)
    .bind(&args.workspace)
    .bind(&args.node)
    .bind(id("agent_thread"))
    .bind(target)
    .execute(&mut **tx)
    .await
    .map_err(|error| format!("live run: {error}"))?;
    let session = id("browser_session");
    BrowserSessionStore::open(
        tx,
        &args.tenant,
        session,
        target,
        Some(id("run")),
        Some("profile:e2e"),
    )
    .await
    .map_err(|error| format!("live browser session: {error}"))?;
    BrowserSessionStore::record_page(
        tx,
        &args.tenant,
        session,
        "cdp-tab-1",
        "https://example.com/",
        "Example Domain",
    )
    .await
    .map_err(|error| format!("live browser page: {error}"))?;
    let terminal = id("terminal_session");
    TerminalStore::open(
        tx,
        &args.tenant,
        terminal,
        target,
        Some(id("run")),
        Some("pty:e2e"),
    )
    .await
    .map_err(|error| format!("live terminal session: {error}"))?;
    TerminalStore::attach(tx, &args.tenant, terminal, id("terminal_last_command"))
        .await
        .map_err(|error| format!("live terminal command: {error}"))?;
    TerminalStore::advance(tx, &args.tenant, terminal, LIVE_TERMINAL_CURSOR)
        .await
        .map_err(|error| format!("live terminal cursor: {error}"))?;
    ComputerControlStore::open(tx, &args.tenant, target, Some(id("run")))
        .await
        .map_err(|error| format!("live computer control: {error}"))?;
    Ok(())
}

/// One tenant membership and one workspace membership, ids derived from the user id as
/// onboarding derives them.
async fn seed_membership(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: &str,
    workspace: &str,
    user: &str,
    tenant_role: &str,
    workspace_role: &str,
) -> Result<(), String> {
    let suffix = user.trim_start_matches("usr_");
    sqlx::query(
        "INSERT INTO tenant_memberships (id, tenant_id, user_id, role, status) VALUES ($1, $2, $3, $4, 'active')",
    )
    .bind(format!("tm_{suffix}"))
    .bind(tenant)
    .bind(user)
    .bind(tenant_role)
    .execute(&mut **tx)
    .await
    .map_err(|error| format!("tenant membership {user}: {error}"))?;
    sqlx::query(
        "INSERT INTO workspace_memberships (id, tenant_id, workspace_id, user_id, role, status) \
         VALUES ($1, $2, $3, $4, $5, 'active')",
    )
    .bind(format!("wm_{suffix}"))
    .bind(tenant)
    .bind(workspace)
    .bind(user)
    .bind(workspace_role)
    .execute(&mut **tx)
    .await
    .map_err(|error| format!("workspace membership {user}: {error}"))?;
    Ok(())
}

/// Knowledge, memory and derived index rows shaped exactly as the intelligence plane writes
/// them (`python/intelligence/knowledge/store.py`, `memory/store.py`, `embeddings/index.py`).
async fn seed_intelligence(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    args: &Args,
) -> Result<(), String> {
    let knowledge = [
        (intelligence_id("knowledge_active"), "active", "The release checklist requires a signed evidence bundle."),
        (intelligence_id("knowledge_verified"), "verified", "The release checklist requires a signed evidence bundle and a dossier consistency run."),
        (intelligence_id("knowledge_candidate"), "candidate", "The release checklist may require a signed evidence bundle, a dossier consistency run and a CI summary."),
    ];
    for (id, status, claim) in knowledge {
        sqlx::query(
            "INSERT INTO knowledge_entries (id, tenant_id, workspace_id, scope, kind, content, provenance, confidence, status) \
             VALUES ($1, $2, $3, 'workspace', 'fact', $4, $5, 0.9, $6)",
        )
        .bind(id)
        .bind(&args.tenant)
        .bind(&args.workspace)
        .bind(json!({ "claim": claim }))
        .bind(json!([{ "source_kind": "approved_source", "ref": "doc:release-checklist", "digest": "0".repeat(64), "retrieved_at": "2026-09-20T00:00:00Z" }]))
        .bind(status)
        .execute(&mut **tx)
        .await
        .map_err(|error| format!("knowledge {id}: {error}"))?;
    }
    sqlx::query(
        "INSERT INTO memory_entries (id, tenant_id, workspace_id, scope, subject_ref, content, provenance_kind, provenance_ref, confidence, status) \
         VALUES ($1, $2, $3, 'user', $4, 'Prefers evidence bundles summarised before the diff.', 'explicit_user', 'msg_seed', 0.8, 'active')",
    )
    .bind(intelligence_id("memory_active"))
    .bind(&args.tenant)
    .bind(&args.workspace)
    .bind(&args.user)
    .execute(&mut **tx)
    .await
    .map_err(|error| format!("memory: {error}"))?;
    let zero = format!("[{}]", vec!["0"; 1536].join(","));
    let embeddings = [
        (
            intelligence_id("embedding_knowledge_active"),
            "knowledge_entry",
            intelligence_id("knowledge_active"),
        ),
        (
            intelligence_id("embedding_knowledge_verified"),
            "knowledge_entry",
            intelligence_id("knowledge_verified"),
        ),
        (
            intelligence_id("embedding_memory_active"),
            "memory_entry",
            intelligence_id("memory_active"),
        ),
    ];
    for (id, source_kind, source_ref) in embeddings {
        sqlx::query(
            "INSERT INTO derived.embeddings (id, tenant_id, workspace_id, source_kind, source_ref, model_id, dimensions, embedding, snapshot, content_digest, chunk_index) \
             VALUES ($1, $2, $3, $4, $5, 'seed-embedding', 1536, $6::vector, 'seed', $7, 0)",
        )
        .bind(id)
        .bind(&args.tenant)
        .bind(&args.workspace)
        .bind(source_kind)
        .bind(source_ref)
        .bind(&zero)
        .bind("0".repeat(64))
        .execute(&mut **tx)
        .await
        .map_err(|error| format!("embedding {id}: {error}"))?;
    }
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
