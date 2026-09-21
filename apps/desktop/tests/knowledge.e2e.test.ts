/**
 * Knowledge E2E (APP-011) against the real `quansio-server` on a scratch database seeded
 * with knowledge, memory and their derived index rows: forgetting a memory or a knowledge
 * entry through the page removes it from every future retrieval (the projection, the typed
 * search, the derived semantic index) while its tombstone and provenance remain; a skill
 * installed through the page climbs the ladder one rung at a time, and an unapproved
 * version cannot be activated — refused by the page before the command and by the server
 * when sent anyway; a run's capability resolution records the active skill as an input
 * that grants nothing.
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import WebSocket from "ws";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

import {
  DesktopBridge,
  SessionFile,
  UpdateHooks,
  activation,
  decodeCapabilityResolution,
  decodeKnowledge,
  decodeMemory,
  decodeSearch,
  decodeSkills,
  deleteKnowledge,
  deleteMemory,
  install,
  layersInOrder,
  promotion,
  retrievable,
  skillInputs,
  supersedeKnowledge,
  tombstones,
  type KnowledgeView,
  type Settled,
  type SkillVersionView,
  type SocketLike,
} from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
// The fixed ids `quansio-dev-seed --intelligence` writes.
const KN_ACTIVE = "kn_01J8Z3K6F1N8VQ2X5W9Y0E2KN1";
const KN_VERIFIED = "kn_01J8Z3K6F1N8VQ2X5W9Y0E2KN2";
const KN_CANDIDATE = "kn_01J8Z3K6F1N8VQ2X5W9Y0E2KN3";
const MEM_ACTIVE = "mem_01J8Z3K6F1N8VQ2X5W9Y0E2MM1";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

function seedScratchDatabase(database: string): string {
  const output = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", TENANT, "--user", USER, "--workspace", WORKSPACE, "--node", NODE, "--intelligence"],
    { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL }, encoding: "utf8" },
  );
  const line = output.split("\n").find((candidate) => candidate.startsWith("database_url="));
  if (!line) {
    throw new Error(`quansio-dev-seed printed no database_url: ${output}`);
  }
  return line.slice("database_url=".length);
}

function dropScratchDatabase(database: string): void {
  execFileSync(SEED_BIN, ["drop", "--database", database], { env: { ...process.env, QUANSIO_DATABASE_URL: ADMIN_URL } });
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

const openSocket = (url: string, headers: Record<string, string>): SocketLike => new WebSocket(url, { headers });

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

describe.skipIf(blocked !== null)("desktop knowledge E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_knowledge_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    child = spawn(SERVER_BIN, [], {
      env: { ...process.env, QUANSIO_DATABASE_URL: databaseUrl, QUANSIO_BIND: `127.0.0.1:${String(port)}`, QUANSIO_TRUST_SCOPE_HEADERS: "1" },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-knowledge-"));
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      dropScratchDatabase(databaseName);
    } catch {
      /* named by pid; a leftover is harmless */
    }
    rmSync(userData, { recursive: true, force: true });
  }, 30_000);

  function page(consumer: string, pushed: { channel: string; payload: unknown }[]): DesktopBridge {
    return new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer,
      push: (channel, payload) => pushed.push({ channel, payload }),
    });
  }

  function eventSeen(pushed: readonly { channel: string; payload: unknown }[], type: string): boolean {
    return pushed.some((p) => p.channel === "stream.frame" && (p.payload as { kind?: string }).kind === "event" && ((p.payload as { event?: { type?: string } }).event?.type ?? "") === type);
  }

  it("forgetting memory or knowledge removes it from every future retrieval and leaves a tombstone with its provenance", async () => {
    const pushed: { channel: string; payload: unknown }[] = [];
    const bridge = page("desktop:knowledge", pushed);
    const handlers = bridge.handlers();
    await handlers["stream.subscribe"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "stream open");

    // 1. Before: knowledge with provenance and scope; memory with provenance and no
    //    recovery field; search answers with the knowledge; only ACTIVE is retrievable.
    const before = decodeKnowledge(await handlers["knowledge.list"]({}));
    expect(before.map((entry) => entry.id).sort()).toEqual([KN_ACTIVE, KN_VERIFIED, KN_CANDIDATE]);
    const active = before.find((entry) => entry.id === KN_ACTIVE) as KnowledgeView;
    expect(active.provenance[0]).toMatchObject({ sourceKind: "approved_source", ref: "doc:release-checklist" });
    expect(active.scope).toBe("workspace");
    expect(retrievable(before).map((entry) => entry.id)).toEqual([KN_ACTIVE]);
    const memoryBefore = decodeMemory(await handlers["memory.list"]({}));
    expect(memoryBefore.refused).toEqual([]);
    expect(memoryBefore.entries.map((entry) => entry.id)).toEqual([MEM_ACTIVE]);
    expect(memoryBefore.entries[0]).toMatchObject({ scope: "user", subjectRef: USER, provenance: { kind: "explicit_user", ref: "msg_seed" }, retrievable: true });
    const found = decodeSearch(await handlers["search.query"]({ q: "checklist", channels: ["knowledge"] }));
    expect(found.hits.map((hit) => hit.id).sort()).toEqual([KN_ACTIVE, KN_VERIFIED]);

    // 2. Forget the memory: the page offers the edge, the server tombstones the row and
    //    purges its derived index row, the event reaches the stream, and the memory is gone
    //    from the projection — but visible as a tombstone when asked for.
    const memory = memoryBefore.entries[0];
    if (!memory) {
      throw new Error("no memory");
    }
    expect(deleteMemory(memory).ok).toBe(true);
    const forgotten = (await handlers["memory.delete"]({ memoryId: MEM_ACTIVE })) as Settled<{ status: string; derived_rows_removed: number }>;
    expect(forgotten.ok, JSON.stringify(forgotten)).toBe(true);
    if (forgotten.ok) {
      expect(forgotten.result.status).toBe("deleted");
      expect(forgotten.result.derived_rows_removed).toBe(1);
    }
    await waitFor(() => (eventSeen(pushed, "memory.deleted") ? true : undefined), "memory.deleted event");
    expect(decodeMemory(await handlers["memory.list"]({})).entries).toEqual([]);
    const memoryTombstones = decodeMemory(await handlers["memory.list"]({ includeDeleted: true })).entries;
    expect(memoryTombstones.map((entry) => entry.id)).toEqual([MEM_ACTIVE]);
    expect(memoryTombstones[0]).toMatchObject({ status: "deleted", retrievable: false, provenance: { kind: "explicit_user", ref: "msg_seed" } });
    expect(retrievable(memoryTombstones)).toEqual([]);
    // The page refuses to forget a tombstone; the server reports the idempotent outcome.
    expect(deleteMemory(memoryTombstones[0] as never)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    const again = (await handlers["memory.delete"]({ memoryId: MEM_ACTIVE })) as Settled<{ already: boolean }>;
    expect(again.ok && again.result.already).toBe(true);

    // 3. Forget the active knowledge: gone from the projection and from search, its
    //    derived row purged, its tombstone keeps the provenance.
    expect(deleteKnowledge(active).ok).toBe(true);
    const deleted = (await handlers["knowledge.delete"]({ knowledgeId: KN_ACTIVE })) as Settled<{ status: string; derived_rows_removed: number }>;
    expect(deleted.ok).toBe(true);
    if (deleted.ok) {
      expect(deleted.result).toMatchObject({ status: "deleted", derived_rows_removed: 1 });
    }
    await waitFor(() => (eventSeen(pushed, "knowledge.deleted") ? true : undefined), "knowledge.deleted event");
    const after = decodeKnowledge(await handlers["knowledge.list"]({}));
    expect(after.map((entry) => entry.id).sort()).toEqual([KN_VERIFIED, KN_CANDIDATE]);
    expect(retrievable(after)).toEqual([]);
    const searched = decodeSearch(await handlers["search.query"]({ q: "checklist", channels: ["knowledge"] }));
    expect(searched.hits.map((hit) => hit.id)).toEqual([KN_VERIFIED]);
    const withTombstones = decodeKnowledge(await handlers["knowledge.list"]({ includeDeleted: true }));
    const tombstone = tombstones(withTombstones).find((entry) => entry.id === KN_ACTIVE) as KnowledgeView;
    expect(tombstone).toMatchObject({ status: "deleted", retrievable: false });
    expect(tombstone.provenance[0]?.ref).toBe("doc:release-checklist");
    expect(deleteKnowledge(tombstone)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });

    // 4. Supersede: the page refuses an entry that is not in force and a self-successor;
    //    the server refuses the same independently; a legal supersession points the old
    //    entry at its successor and search stops answering with it.
    const verified = after.find((entry) => entry.id === KN_VERIFIED) as KnowledgeView;
    const candidate = after.find((entry) => entry.id === KN_CANDIDATE) as KnowledgeView;
    expect(supersedeKnowledge(tombstone, verified)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    const refusedByServer = (await handlers["knowledge.supersede"]({ knowledgeId: KN_ACTIVE, supersededBy: KN_VERIFIED })) as Settled<unknown>;
    expect(!refusedByServer.ok && refusedByServer.error.code).toBe("CONFLICT_STATE");
    expect(supersedeKnowledge(verified, verified)).toMatchObject({ ok: false, code: "VALIDATION_SCHEMA" });
    const selfRefused = (await handlers["knowledge.supersede"]({ knowledgeId: KN_VERIFIED, supersededBy: KN_VERIFIED })) as Settled<unknown>;
    expect(!selfRefused.ok && selfRefused.error.code).toBe("VALIDATION_SCHEMA");
    expect(supersedeKnowledge(verified, tombstone)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(supersedeKnowledge(candidate, verified)).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    const candidateRefused = (await handlers["knowledge.supersede"]({ knowledgeId: KN_CANDIDATE, supersededBy: KN_VERIFIED })) as Settled<unknown>;
    expect(!candidateRefused.ok && candidateRefused.error.code).toBe("CONFLICT_STATE");
    expect(supersedeKnowledge(verified, candidate).ok).toBe(true);
    const superseded = (await handlers["knowledge.supersede"]({ knowledgeId: KN_VERIFIED, supersededBy: KN_CANDIDATE })) as Settled<{ status: string; superseded_by: string; derived_rows_removed: number }>;
    expect(superseded.ok, JSON.stringify(superseded)).toBe(true);
    if (superseded.ok) {
      expect(superseded.result).toMatchObject({ status: "superseded", superseded_by: KN_CANDIDATE, derived_rows_removed: 1 });
    }
    await waitFor(() => (eventSeen(pushed, "knowledge.superseded") ? true : undefined), "knowledge.superseded event");
    const finalList = decodeKnowledge(await handlers["knowledge.list"]({}));
    expect(finalList.find((entry) => entry.id === KN_VERIFIED)).toMatchObject({ status: "superseded", supersededBy: KN_CANDIDATE, retrievable: false });
    expect(tombstones(finalList).map((entry) => entry.id)).toEqual([KN_VERIFIED]);
    const searchedAgain = decodeSearch(await handlers["search.query"]({ q: "checklist", channels: ["knowledge"] }));
    expect(searchedAgain.hits).toEqual([]);
    bridge.close();
  }, 90_000);

  it("a skill climbs the ladder one rung at a time; an unapproved version cannot be activated; the run's resolution names it and grants nothing", async () => {
    const pushed: { channel: string; payload: unknown }[] = [];
    const bridge = page("desktop:skills", pushed);
    const handlers = bridge.handlers();
    await handlers["stream.subscribe"]({});
    await waitFor(() => (pushed.some((p) => p.channel === "stream.state" && p.payload === "open") ? true : undefined), "stream open");

    // 1. Install: a draft, never active.
    const draft = install({ scope: "workspace", name: "release-notes", semver: "1.0.0", instructions: "summarise the evidence bundle", toolNeeds: ["fs.read"], capabilityNeeds: [{ effectClass: "fs.write.workspace", resource: { kind: "fs", selector: "**" } }] });
    if (!draft.ok) {
      throw new Error(draft.reason);
    }
    const installed = (await handlers["skills.install"](draft.params)) as Settled<{ skill_id: string; version: { skill_version_id: string; status: string } }>;
    expect(installed.ok).toBe(true);
    if (!installed.ok) {
      return;
    }
    expect(installed.result.version.status).toBe("draft");
    const skillId = installed.result.skill_id;
    const versionId = installed.result.version.skill_version_id;
    const version = (): SkillVersionView => {
      const skills = decodeSkills(pushedSkills);
      const found = skills.find((skill) => skill.id === skillId)?.versions.find((candidate) => candidate.id === versionId);
      if (!found) {
        throw new Error("version missing from the projection");
      }
      return found;
    };
    let pushedSkills: unknown = await handlers["skills.list"]({});
    expect(version()).toMatchObject({ status: "draft", resolves: false, promotableTo: ["candidate"] });

    // 2. Activation is refused for a draft — by the page before any command, and by the
    //    server when the command is sent anyway. Nothing changes.
    expect(activation(version())).toMatchObject({ ok: false, code: "RUNTIME_ILLEGAL_TRANSITION" });
    const refused = (await handlers["skills.promote"]({ skillVersionId: versionId, to: "active" })) as Settled<unknown>;
    expect(!refused.ok && refused.error.code).toBe("RUNTIME_ILLEGAL_TRANSITION");
    pushedSkills = await handlers["skills.list"]({});
    expect(version().status).toBe("draft");
    expect(decodeSkills(pushedSkills).find((skill) => skill.id === skillId)?.currentActiveVersionId).toBeNull();

    // 3. One rung at a time; only APPROVED may be activated, and then it resolves.
    for (const to of ["candidate", "evaluating", "approved"] as const) {
      const edge = promotion(version(), to);
      expect(edge.ok).toBe(true);
      const promoted = (await handlers["skills.promote"]({ skillVersionId: versionId, to })) as Settled<{ status: string }>;
      expect(promoted.ok && promoted.result.status).toBe(to);
      pushedSkills = await handlers["skills.list"]({});
      expect(activation(version()).ok).toBe(to === "approved");
    }
    const activate = activation(version());
    expect(activate.ok).toBe(true);
    const activated = (await handlers["skills.promote"]({ skillVersionId: versionId, to: "active" })) as Settled<{ status: string; resolves: boolean }>;
    expect(activated.ok && activated.result).toMatchObject({ status: "active", resolves: true });
    // Skill events are tenant-level (a skill row has no workspace), so the workspace
    // stream does not carry them; the page re-reads the projection after every command.
    pushedSkills = await handlers["skills.list"]({});
    expect(version()).toMatchObject({ status: "active", resolves: true, promotableTo: ["deprecated"] });

    // 4. A run started now resolves its authority with the skill as an input — and the
    //    skill's declared need for fs.write grants nothing: the run may read only.
    const posted = (await handlers["messages.post"]({ threadId: null, content: "hello" })) as { run_id: string };
    const resolution = decodeCapabilityResolution(await handlers["runs.capabilities"]({ runId: posted.run_id }));
    expect(resolution).not.toBeNull();
    if (!resolution) {
      return;
    }
    expect(skillInputs(resolution)).toEqual([versionId]);
    const rows = layersInOrder(resolution);
    // The workspace's default teammate (APP-014) is an input too, between the policy and the skills.
    expect(rows.filter((row) => row.input).map((row) => row.layer)).toEqual(["tenant_policy", "agent_definition", "active_skills"]);
    expect(resolution.grants.map((grant) => grant.effectClass)).toEqual(["read.internal"]);

    // 5. Disable: the version is deprecated, nothing resolves, and it cannot be revived.
    const disabled = (await handlers["skills.disable"]({ skillId })) as Settled<{ status: string }>;
    expect(disabled.ok && disabled.result.status).toBe("deprecated");
    pushedSkills = await handlers["skills.list"]({});
    expect(version()).toMatchObject({ status: "deprecated", resolves: false });
    expect(activation(version())).toMatchObject({ ok: false });
    const revived = (await handlers["skills.promote"]({ skillVersionId: versionId, to: "active" })) as Settled<unknown>;
    expect(!revived.ok && revived.error.code).toBe("RUNTIME_ILLEGAL_TRANSITION");
    const next = (await handlers["messages.post"]({ threadId: null, content: "again" })) as { run_id: string };
    const later = decodeCapabilityResolution(await handlers["runs.capabilities"]({ runId: next.run_id }));
    expect(later ? skillInputs(later) : null).toEqual([]);
    bridge.close();
  }, 90_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
