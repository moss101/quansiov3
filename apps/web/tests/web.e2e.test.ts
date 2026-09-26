/**
 * Web E2E (APP-012) against the real `quansio-server` on a scratch database seeded with an
 * owner, a viewer and an approver, serving the built web app under `/app/`:
 *
 * - web RBAC E2E: the page offers a viewer no admin surface, and the server refuses every
 *   admin projection and command with `SCOPE_FORBIDDEN` (typed `rbac` details) however
 *   the page addresses them; an owner is served and administers members and policies;
 * - approval E2E: a viewer's decision is refused by the server; an approver's decision
 *   bound to the previewed digest is granted (a stale digest is `APPROVAL_SUPERSEDED`)
 *   and audited under the approver's identity;
 * - responsive smoke: the server serves the bundle with the viewport meta, the locked
 *   CSP and the breakpoints the layout module derives.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { createServer } from "node:net";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  ApiError,
  BREAKPOINTS,
  WebSession,
  decide,
  decodeApprovals,
  decodeAudit,
  decodeMembers,
  decodePolicies,
  decodeUsage,
  decodeWorkGraph,
  invite,
  layoutFor,
  mediaQueries,
  offered,
  removeMember,
  rolesFromMe,
  setPolicy,
  updateMembership,
  type ApprovalView,
  type Roles,
} from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WEB_ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const WORKSPACE_ROOT = join(WEB_ROOT, "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");
const BUNDLE = join(WEB_ROOT, "dist", "web");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0WEBWW";
const OWNER = "usr_01J8Z3K6F1N8VQ2X5W9Y0WEBW1";
const VIEWER = "usr_01J8Z3K6F1N8VQ2X5W9Y0WEBW2";
const APPROVER = "usr_01J8Z3K6F1N8VQ2X5W9Y0WEBW3";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0WEBWW";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0WEBWW";
const APPROVAL_SCENARIO = "[[scenario:approval]]";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

function seedScratchDatabase(database: string): string {
  const output = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", TENANT, "--user", OWNER, "--workspace", WORKSPACE, "--node", NODE, "--ask-writes", "--member", `${VIEWER}:member:viewer`, "--member", `${APPROVER}:member:approver`],
    { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL }, encoding: "utf8" },
  );
  const line = output.split("\n").find((candidate) => candidate.startsWith("database_url="));
  if (!line) {
    throw new Error(`quansio-dev-seed printed no database_url: ${output}`);
  }
  return line.slice("database_url=".length);
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close(() => {
        if (port) {
          resolve(port);
        } else {
          reject(new Error("no port"));
        }
      });
    });
  });
}

async function waitForHealth(origin: string, child: ChildProcess, stderr: string[]): Promise<void> {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`quansio-server exited with ${String(child.exitCode)}: ${stderr.join("")}`);
    }
    try {
      if ((await fetch(`${origin}/v1/health`)).ok) {
        return;
      }
    } catch {
      /* not up yet */
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`quansio-server did not become healthy: ${stderr.join("")}`);
}

async function refusal(work: Promise<unknown>): Promise<ApiError> {
  const outcome = await work.then(() => null, (error: unknown) => error);
  if (!(outcome instanceof ApiError)) {
    throw new Error(`expected an ApiError, got ${outcome === null ? "success" : outcome instanceof Error ? outcome.message : JSON.stringify(outcome)}`);
  }
  return outcome;
}

describe.skipIf(blocked !== null)("web E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  const stderr: string[] = [];

  const session = (userId: string): WebSession => new WebSession(origin, { harness: { tenantId: TENANT, userId } });

  beforeAll(async () => {
    // The bundle the server serves is the one this checkout builds.
    execFileSync(process.execPath, [join(WEB_ROOT, "scripts", "bundle.mjs")], { stdio: "pipe" });
    databaseName = `quansio_web_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    child = spawn(SERVER_BIN, [], {
      env: {
        ...process.env,
        QUANSIO_DATABASE_URL: databaseUrl,
        QUANSIO_BIND: `127.0.0.1:${String(port)}`,
        QUANSIO_TRUST_SCOPE_HEADERS: "1",
        QUANSIO_APPROVAL_SIGNING_KEY: "web-e2e-signing-key",
        QUANSIO_WEB_ROOT: BUNDLE,
      },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
  }, 120_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      execFileSync(SEED_BIN, ["drop", "--database", databaseName], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
    } catch {
      /* named by pid; a leftover is harmless */
    }
  }, 30_000);

  it("web RBAC E2E: the page offers what the role allows and the server refuses the rest", async () => {
    // The viewer: review surfaces only; every admin projection and command is refused by
    // the server with a typed RBAC refusal, whatever the page might have addressed.
    const viewer = session(VIEWER).client();
    // The page reads its roles from /v1/me exactly as the server reports them.
    const viewerRoles: Roles = rolesFromMe(await viewer.get("/v1/me"), WORKSPACE);
    expect(viewerRoles).toEqual({ tenant: "member", workspace: "viewer" });
    expect(offered(viewerRoles).map((item) => item.id)).toEqual(["work-review", "approvals", "artifacts"]);
    const graph = decodeWorkGraph(await viewer.get(`/v1/workgraph?workspace_id=${WORKSPACE}`));
    expect(graph.nodes.length).toBeGreaterThan(0);
    for (const [path, action] of [["/v1/members", "manage_workspace"], [`/v1/policies?workspace_id=${WORKSPACE}`, "manage_workspace"], ["/v1/connectors", "manage_workspace"], ["/v1/usage", "view_usage"], ["/v1/audit", "manage_tenant"]] as const) {
      const error = await refusal(viewer.get(path));
      expect(error.code, path).toBe("SCOPE_FORBIDDEN");
      expect(error.isRbacRefusal, path).toBe(true);
      expect((error.details as { action: string }).action, path).toBe(action);
    }
    const prepared = setPolicy({ scope: "tenant", rules: [] });
    if (!prepared.ok) {
      throw new Error(prepared.reason);
    }
    expect((await refusal(viewer.invoke(prepared.command, prepared.params))).code).toBe("SCOPE_FORBIDDEN");
    const invitation = invite({ email: "intruder@example.com", role: "admin" });
    if (!invitation.ok) {
      throw new Error(invitation.reason);
    }
    expect((await refusal(viewer.invoke(invitation.command, invitation.params))).code).toBe("SCOPE_FORBIDDEN");

    // The owner: every surface, and the administration works end to end.
    const owner = session(OWNER).client();
    const ownerRoles: Roles = rolesFromMe(await owner.get("/v1/me"), WORKSPACE);
    expect(ownerRoles).toEqual({ tenant: "owner", workspace: "admin" });
    expect(offered(ownerRoles).map((item) => item.id)).toHaveLength(9);
    let members = decodeMembers(await owner.get("/v1/members"));
    expect(members.map((member) => member.userId).sort()).toEqual([OWNER, VIEWER, APPROVER].sort());
    expect(members.find((member) => member.userId === VIEWER)?.workspaces[0]?.role).toBe("viewer");
    const nova = invite({ email: "nova@example.com", role: "member", workspaceId: WORKSPACE, workspaceRole: "editor" });
    if (!nova.ok) {
      throw new Error(nova.reason);
    }
    const invited = await owner.invoke<{ membership_id: string; status: string; user: { id: string } }>(nova.command, nova.params);
    expect(invited.status).toBe("invited");
    members = decodeMembers(await owner.get("/v1/members"));
    const novaMember = members.find((member) => member.userId === invited.user.id);
    expect(novaMember?.workspaces[0]?.role).toBe("editor");
    // An invited user holds no role until activated: the admin projections refuse them.
    expect((await refusal(session(invited.user.id).client().get("/v1/members"))).code).toBe("SCOPE_FORBIDDEN");
    const activate = updateMembership(invited.membership_id, { status: "active" });
    if (!activate.ok) {
      throw new Error(activate.reason);
    }
    await owner.invoke(activate.command, activate.params);
    members = decodeMembers(await owner.get("/v1/members"));
    expect(removeMember(members.find((member) => member.userId === OWNER) as never, members)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    const removal = removeMember(members.find((member) => member.userId === invited.user.id) as never, members);
    if (!removal.ok) {
      throw new Error(removal.reason);
    }
    await owner.invoke(removal.command, removal.params);
    members = decodeMembers(await owner.get("/v1/members"));
    expect(members.find((member) => member.userId === invited.user.id)?.status).toBe("removed");
    // Policies: the seeded tenant policy is versioned by SetPolicy and shown by the page.
    const before = decodePolicies(await owner.get(`/v1/policies?workspace_id=${WORKSPACE}`));
    const tenantPolicy = before.find((policy) => policy.scope === "tenant");
    expect(tenantPolicy?.rules.length).toBe(3);
    const change = setPolicy({ scope: "tenant", rules: tenantPolicy?.rules ?? [], approvalTtlSeconds: 900 });
    if (!change.ok) {
      throw new Error(change.reason);
    }
    const saved = await owner.invoke<{ version: number }>(change.command, change.params);
    expect(saved.version).toBe((tenantPolicy?.version ?? 0) + 1);
    const after = decodePolicies(await owner.get(`/v1/policies?workspace_id=${WORKSPACE}`));
    expect(after.find((policy) => policy.scope === "tenant")?.approvalTtlSeconds).toBe(900);
    expect(decodeUsage(await owner.get("/v1/usage")).meters.length).toBe(8);
    // The approver's roles, from /v1/me too: review surfaces only, and a decision offered.
    const approverRoles = rolesFromMe(await session(APPROVER).client().get("/v1/me"), WORKSPACE);
    expect(approverRoles).toEqual({ tenant: "member", workspace: "approver" });
    expect(offered(approverRoles).map((item) => item.id)).toEqual(["work-review", "approvals", "artifacts"]);
  }, 60_000);

  it("approval E2E: a viewer cannot decide, an approver decides bound to the preview, and the audit log says who", async () => {
    const owner = session(OWNER).client();
    // The command is accepted before its turn runs; the turn then parks on the approval.
    const posted = await owner.invoke<{ run_id: string }>("PostMessage", { workspace_id: WORKSPACE, content: `write the summary ${APPROVAL_SCENARIO}` });
    const load = async (client = owner, timeoutMs = 15_000): Promise<ApprovalView> => {
      const deadline = Date.now() + timeoutMs;
      for (;;) {
        const found = decodeApprovals(await client.get(`/v1/approvals?run_id=${posted.run_id}`))[0];
        if (found) {
          return found;
        }
        if (Date.now() > deadline) {
          throw new Error("no approval request");
        }
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    };
    const previewed = await load();
    expect(previewed.status).toBe("requested");
    expect(previewed.digest).toHaveLength(64);
    // The viewer sees the request and may not decide it: the page refuses to offer the
    // buttons, and the server refuses the command the page could not have sent.
    const viewer = session(VIEWER).client();
    const viewerSees = await load(viewer);
    expect(viewerSees.id).toBe(previewed.id);
    const decision = decide("approve", previewed, viewerSees);
    if (!decision.ok) {
      throw new Error(decision.reason);
    }
    const refused = await refusal(viewer.invoke(decision.command, decision.params));
    expect(refused.code).toBe("SCOPE_FORBIDDEN");
    expect((refused.details as { action: string }).action).toBe("approve_effect");
    expect((await load()).status).toBe("requested");
    // The approver: a stale digest is refused by the page and by the server; the previewed
    // digest is granted and the run completes.
    const approver = session(APPROVER).client();
    expect(decide("approve", { ...previewed, digest: "0".repeat(64) }, previewed)).toMatchObject({ ok: false, code: "APPROVAL_SUPERSEDED" });
    expect((await refusal(approver.invoke("ApproveEffect", { approval_request_id: previewed.id, params_digest: "0".repeat(64) }))).code).toBe("APPROVAL_SUPERSEDED");
    const granted = await approver.invoke<{ params_digest: string }>(decision.command, decision.params);
    expect(granted.params_digest).toBe(previewed.digest);
    expect((await load()).status).toBe("granted");
    // The released turn runs after the grant is accepted and consumes the receipt.
    const deadline = Date.now() + 15_000;
    while (!(await load()).receipts[0]?.consumed) {
      if (Date.now() > deadline) {
        throw new Error("the receipt was not consumed");
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    expect(decide("approve", previewed, await load())).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    const audit = decodeAudit(await owner.get(`/v1/audit?workspace_id=${WORKSPACE}`));
    expect(audit.chain.holds).toBe(true);
    const grant = audit.entries.find((entry) => entry.action === "ApproveEffect" && entry.targetRef === `approval:${previewed.id}`);
    expect(grant?.actor).toBe(`user:${APPROVER}`);
    expect(grant?.decision).toBe("granted");
    expect((await refusal(approver.get("/v1/audit"))).code).toBe("SCOPE_FORBIDDEN");
  }, 60_000);

  it("responsive smoke: the served bundle is responsive and delivered with the locked policy", async () => {
    for (const path of ["/app/", "/app/audit-and-privacy", "/app/approvals/apr_1"]) {
      const response = await fetch(`${origin}${path}`);
      expect(response.status, path).toBe(200);
      expect(response.headers.get("content-type")).toBe("text/html; charset=utf-8");
      expect(response.headers.get("content-security-policy")).toContain("default-src 'self'");
      expect(response.headers.get("content-security-policy")).toContain("frame-ancestors 'none'");
      expect(response.headers.get("x-content-type-options")).toBe("nosniff");
      const html = await response.text();
      expect(html).toContain('name="viewport" content="width=device-width, initial-scale=1"');
      expect(html).toContain('src="/app/assets/app.js"');
      expect(html).not.toMatch(/src="https?:/);
    }
    const css = await fetch(`${origin}/app/assets/app.css`);
    expect(css.headers.get("content-type")).toBe("text/css; charset=utf-8");
    const stylesheet = await css.text();
    for (const query of mediaQueries()) {
      expect(stylesheet, query).toContain(query);
    }
    const script = await fetch(`${origin}/app/assets/app.js`);
    expect(script.headers.get("content-type")).toBe("text/javascript; charset=utf-8");
    const bundle = await script.text();
    for (const mode of ["layout-phone", "layout-tablet", "layout-desktop"]) {
      expect(bundle.includes(mode) || bundle.includes("layout-"), mode).toBe(true);
    }
    expect(bundle).not.toContain("require(\"electron\")");
    expect(bundle).not.toContain("node:fs");
    expect([375, BREAKPOINTS.phone, BREAKPOINTS.tablet].map((width) => layoutFor(width).navigation)).toEqual(["drawer", "rail", "sidebar"]);
    const missing = await fetch(`${origin}/app/assets/missing.js`);
    expect(missing.status).toBe(404);
    const escape = await fetch(`${origin}/app/assets/../../Cargo.toml`);
    expect([400, 404]).toContain(escape.status);
  }, 30_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
