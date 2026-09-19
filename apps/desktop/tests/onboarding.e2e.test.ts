/**
 * New-user E2E (APP-002, acceptance 1): a fresh user completes onboarding without a
 * configuration file — against the real `quansio-server` binary in its deployable
 * session-only mode, on an empty scratch database with no seeded tenant.
 *
 * The desktop's own bridge, session manager, keychain store (a reversible cipher double
 * standing in for `safeStorage`) and clients drive it: magic link → the token as the
 * developer's console shows it → sign-in → the server-provisioned personal tenant and
 * default workspace → execution-target choice and security defaults through
 * `UpdateWorkspace` → a message posted with the bearer → its durable event on the bearer
 * stream → a desktop "restart" that resumes from the keychain alone.
 *
 * Environment: `QUANSIO_TEST_POSTGRES_URL`, `target/debug/quansio-server` and
 * `target/debug/quansio-dev-seed` (for the scratch database only; nothing is seeded).
 * Without them the suite skips with a `BLOCKED_EXTERNAL` marker.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { DesktopBridge, SessionFile, UpdateHooks, type SocketLike } from "../src/index.js";
import { TokenStore, type SecretCipher } from "../src/main/token-store.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

/** Reversible and visibly not plaintext: what `safeStorage` provides on a real desktop. */
const CIPHER: SecretCipher = {
  isEncryptionAvailable: () => true,
  encryptString: (plain) => Buffer.from(Buffer.from(plain, "utf8").map((byte) => byte ^ 0x3c)),
  decryptString: (encrypted) => Buffer.from(encrypted.map((byte) => byte ^ 0x3c)).toString("utf8"),
};

function scratchDatabase(database: string): string {
  // Seeding the tenant is deliberately not done here: onboarding must create everything.
  const seed = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", "tn_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ", "--user", "usr_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ", "--workspace", "ws_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ", "--node", "wn_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ"],
    { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL }, encoding: "utf8" },
  );
  const line = seed.split("\n").find((candidate) => candidate.startsWith("database_url="));
  if (!line) {
    throw new Error(`quansio-dev-seed printed no database_url: ${seed}`);
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
      const response = await fetch(`${origin}/v1/health`);
      if (response.ok) {
        return;
      }
    } catch {
      /* not up yet */
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`quansio-server did not become healthy: ${stderr.join("")}`);
}

function waitFor<T>(poll: () => T | undefined, what: string, timeoutMs = 15_000): Promise<T> {
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + timeoutMs;
    const tick = () => {
      const value = poll();
      if (value !== undefined) {
        resolve(value);
      } else if (Date.now() > deadline) {
        reject(new Error(`timed out waiting for ${what}`));
      } else {
        setTimeout(tick, 50);
      }
    };
    tick();
  });
}

/** The magic-link token exactly as the console delivery prints it for `email`. */
function consoleToken(stderr: string[], email: string): string | undefined {
  const line = stderr
    .join("")
    .split("\n")
    .reverse()
    .find((candidate) => candidate.includes(`magic-link for ${email}`) && candidate.includes("token="));
  return line?.split("token=")[1]?.trim();
}

const openSocket = (url: string, headers: Record<string, string>): SocketLike => new WebSocket(url, { headers });

describe.skipIf(blocked !== null)("new-user onboarding E2E against quansio-server (session-only)", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_onboard_${String(process.pid)}`;
    const databaseUrl = scratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    // The deployable's defaults: bearer sessions only, magic links on the console.
    child = spawn(SERVER_BIN, [], {
      env: { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}` },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-onboard-"));
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      execFileSync(SEED_BIN, ["drop", "--database", databaseName], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
    } catch {
      /* a leftover scratch database named by pid is harmless */
    }
    rmSync(userData, { recursive: true, force: true });
  }, 30_000);

  it("takes a fresh user from a magic link to a governed workspace with a running stream, then resumes from the keychain", async () => {
    const pushed: { channel: string; payload: unknown }[] = [];
    const bridge = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "session", tokens: new TokenStore(userData, CIPHER) },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:onboarding-e2e",
      push: (channel, payload) => pushed.push({ channel, payload }),
    });
    const handlers = bridge.handlers();
    expect(await bridge.resume()).toBe(false);
    expect(await handlers["session.state"]({})).toEqual({ kind: "signed-out", persistent: true });

    // The server trusts nothing but a session: a header-scoped command is refused outright.
    const refused = await fetch(`${origin}/v1/commands/PostMessage`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-quansio-tenant": "tn_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ", "x-quansio-user": "usr_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ" },
      body: JSON.stringify({ command_id: "cmd_01J8Z3K6F1N8VQ2X5W9Y0ZZZZ1", params: { workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0ZZZZZ", content: "x" } }),
    });
    expect(refused.status).toBe(401);
    expect(((await refused.json()) as { code: string }).code).toBe("AUTH_REQUIRED");

    // 1. Sign in: the link is delivered on the console; the desktop pastes the token.
    const email = "fresh.user@example.com";
    await handlers["auth.magicLink.start"]({ email: "Fresh.User@Example.com", displayName: "Fresh User" });
    expect(await handlers["session.state"]({})).toMatchObject({ kind: "link-sent", email, delivery: "console" });
    const token = await waitFor(() => consoleToken(stderr, email), "the magic link on the console");
    expect(token.startsWith("qml_")).toBe(true);
    const signedIn = (await handlers["auth.magicLink.complete"]({ token })) as { kind: string; workspaceId: string | null; onboarded: boolean; tenantId: string; userId: string };
    expect(signedIn).toMatchObject({ kind: "signed-in", onboarded: true });
    expect(signedIn.workspaceId).toMatch(/^ws_/);
    expect(JSON.stringify(signedIn)).not.toMatch(/eyJ|qrt_|qml_/);

    // 2. The server provisioned a personal tenant and a default workspace; no file was read.
    const me = (await handlers["session.me"]({})) as { user: { primary_email: string; auth_methods: string[] }; tenants: { personal: boolean; role: string }[]; workspaces: { id: string; role: string; effective_settings: Record<string, unknown> }[] };
    expect(me.user.primary_email).toBe(email);
    expect(me.tenants[0]).toMatchObject({ personal: true, role: "owner" });
    expect(me.workspaces[0]).toMatchObject({ id: signedIn.workspaceId, role: "admin" });
    expect(me.workspaces[0]?.effective_settings["unsigned_updates"]).toBe(false);
    expect(me.user.auth_methods).toContain("email_magic_link");

    // 3. Execution target and security defaults, with the restart effect reported.
    const chosen = (await handlers["settings.updateWorkspace"]({ workspaceId: signedIn.workspaceId, settings: { execution_target: "local_capsule" } })) as {
      effective_settings: Record<string, unknown>;
      effects: { kind: string; reason: string }[];
    };
    expect(chosen.effective_settings["execution_target"]).toBe("local_capsule");
    expect(chosen.effects.some((effect) => effect.kind === "restart")).toBe(true);
    const defaults = (await handlers["settings.updateWorkspace"]({ workspaceId: signedIn.workspaceId, settings: { telemetry: false, clipboard_to_model: false, unsigned_updates: false, share_analytics: false } })) as {
      effective_settings: Record<string, unknown>;
    };
    expect(defaults.effective_settings["telemetry"]).toBe(false);
    await expect(handlers["settings.updateWorkspace"]({ workspaceId: signedIn.workspaceId, settings: { providers: { openai: { credential_handle: "sk-live-raw" } } } })).rejects.toMatchObject({ code: "VALIDATION_SCHEMA" });

    // 4. The bearer session drives the product: a message, and its durable event on the stream.
    await handlers["stream.subscribe"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "stream open with the bearer");
    const posted = (await handlers["commands.invoke"]({ name: "PostMessage", commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0ZZZZ2", params: { workspace_id: signedIn.workspaceId, content: "hello from onboarding" } })) as {
      result: { run_id: string; thread_id: string };
    };
    expect(posted.result.run_id).toMatch(/^run_/);
    await waitFor(() => pushed.find((p) => p.channel === "stream.frame" && (p.payload as { kind: string }).kind === "event"), "a durable event over the bearer stream");
    const projection = (await handlers["session.activeRun"]({})) as { activeRun: { runId: string } | null };
    expect(projection.activeRun?.runId).toBe(posted.result.run_id);
    bridge.close();

    // 5. Restart: nothing but the keychain. The refresh token rotates into a live session and
    //    the workspace comes back from the server, not from a file.
    const restarted = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "session", tokens: new TokenStore(userData, CIPHER) },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:onboarding-e2e-restart",
      push: () => undefined,
    });
    expect(await restarted.resume()).toBe(true);
    const resumed = (await restarted.handlers()["session.state"]({})) as { kind: string; userId: string; workspaceId: string | null; onboarded: boolean };
    expect(resumed).toMatchObject({ kind: "signed-in", userId: signedIn.userId, workspaceId: signedIn.workspaceId, onboarded: false });
    const again = (await restarted.handlers()["session.me"]({})) as { user: { primary_email: string } };
    expect(again.user.primary_email).toBe(email);

    // Sign-out revokes the family and clears custody: a third start is signed out again.
    await restarted.handlers()["auth.signOut"]({});
    expect(new TokenStore(userData, CIPHER).load()).toBeNull();
    const third = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "session", tokens: new TokenStore(userData, CIPHER) },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:onboarding-e2e-third",
      push: () => undefined,
    });
    expect(await third.resume()).toBe(false);
  }, 60_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
