/**
 * Artifact workspace E2E (APP-008) against the real `quansio-server` on a scratch database
 * with the dev MinIO and a temporary search index: an artifact outlives the run that
 * accompanied it and survives a desktop reload; its version history carries produced-by
 * and evidence links and diffs; a malicious HTML upload is previewed as text and refused
 * for "open"; save writes the exact bytes; global search finds it by content.
 *
 * The page is the bridge's handlers plus the workspace reducer; file operations are
 * injected (a temp directory stands in for the save dialog and the OS handler).
 */

import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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
  artifactWorkspaceReducer,
  comparedVersion,
  decodeArtifacts,
  decodeEvidence,
  decodeHistory,
  decodeSearch,
  diffLines,
  editableAsText,
  formatCheckLabel,
  initialArtifactWorkspace,
  selectedVersion,
  textPreview,
  type ArtifactWorkspaceState,
  type DesktopFiles,
  type SocketLike,
} from "../src/index.js";
import { previewFromContent } from "../src/renderer/Artifacts.js";
import type { PreviewPlan } from "../src/index.js";

const ADMIN_URL = process.env["QUANSIO_TEST_POSTGRES_URL"]?.trim() ?? "";
const WORKSPACE_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const SERVER_BIN = process.env["QUANSIO_TEST_SERVER_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-server");
const SEED_BIN = process.env["QUANSIO_TEST_SEED_BIN"]?.trim() || join(WORKSPACE_ROOT, "target", "debug", "quansio-dev-seed");
const MINIO = process.env["QUANSIO_TEST_MINIO_ENDPOINT"]?.trim() || "http://127.0.0.1:59010";

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const WORKSPACE = "ws_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";
const NODE = "wn_01J8Z3K6F1N8VQ2X5W9Y0KKKKK";

const blocked = !ADMIN_URL
  ? "BLOCKED_EXTERNAL: QUANSIO_TEST_POSTGRES_URL is not set; the dev stack is not running"
  : !existsSync(SERVER_BIN) || !existsSync(SEED_BIN)
    ? `BLOCKED_EXTERNAL: ${SERVER_BIN} and ${SEED_BIN} are needed; build them with cargo build -p quansio-server --bins`
    : null;

function seedScratchDatabase(database: string): string {
  const output = execFileSync(
    SEED_BIN,
    ["create", "--database", database, "--tenant", TENANT, "--user", USER, "--workspace", WORKSPACE, "--node", NODE],
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

async function minioReachable(): Promise<boolean> {
  try {
    return (await fetch(`${MINIO}/minio/health/live`)).ok;
  } catch {
    return false;
  }
}

const openSocket = (url: string, headers: Record<string, string>): SocketLike => new WebSocket(url, { headers });

interface ContentPayload {
  readonly bytesBase64: string;
  readonly mediaType: string;
  readonly sizeBytes: number;
  readonly plan: PreviewPlan;
}

/** A page over the bridge with injected file operations and the workspace reducer. */
class Page {
  readonly bridge: DesktopBridge;
  readonly handlers: ReturnType<DesktopBridge["handlers"]>;
  state: ArtifactWorkspaceState;

  constructor(origin: string, userData: string, files: DesktopFiles | null) {
    this.bridge = new DesktopBridge({
      serverOrigin: origin,
      auth: { kind: "trusted-headers", scope: { tenantId: TENANT, userId: USER }, workspaceId: WORKSPACE },
      session: new SessionFile(userData),
      openSocket,
      updates: new UpdateHooks("stable", null, `${origin}/feed`, false),
      consumer: "desktop:artifacts",
      push: () => undefined,
      ...(files ? { files } : {}),
    });
    this.handlers = this.bridge.handlers();
    this.state = initialArtifactWorkspace(WORKSPACE);
  }

  async refreshList(): Promise<void> {
    this.state = artifactWorkspaceReducer(this.state, { type: "list.loaded", artifacts: decodeArtifacts(await this.handlers["artifacts.list"]({ kind: this.state.filter.kind, runId: this.state.filter.runId })) });
  }

  async select(artifactId: string): Promise<void> {
    this.state = artifactWorkspaceReducer(this.state, { type: "select", artifactId });
    this.state = artifactWorkspaceReducer(this.state, { type: "history.loaded", history: decodeHistory(await this.handlers["artifacts.versions"]({ artifactId })) });
  }

  async loadPreview(): Promise<void> {
    const version = selectedVersion(this.state);
    if (!this.state.selectedId || !version) {
      throw new Error("nothing selected");
    }
    const payload = (await this.handlers["artifacts.content"]({ artifactId: this.state.selectedId, versionId: version.id })) as ContentPayload;
    this.state = artifactWorkspaceReducer(this.state, { type: "preview.loaded", preview: previewFromContent(version.id, payload) });
  }

  async upload(name: string, mediaType: string, bytes: Uint8Array, artifactId: string | null = null): Promise<{ artifact_id: string; version_id: string; seq: number; content_digest: string }> {
    return (await this.handlers["artifacts.upload"]({ name, mediaType, bytesBase64: Buffer.from(bytes).toString("base64"), artifactId })) as { artifact_id: string; version_id: string; seq: number; content_digest: string };
  }

  close(): void {
    this.bridge.close();
  }
}

describe.skipIf(blocked !== null)("desktop artifact workspace E2E against quansio-server", () => {
  let databaseName: string;
  let child: ChildProcess;
  let origin: string;
  let userData: string;
  let indexRoot: string;
  let filesDir: string;
  let minio = false;
  const stderr: string[] = [];

  beforeAll(async () => {
    databaseName = `quansio_desktop_art_${String(process.pid)}`;
    const databaseUrl = seedScratchDatabase(databaseName);
    const port = await freePort();
    origin = `http://127.0.0.1:${String(port)}`;
    minio = await minioReachable();
    indexRoot = mkdtempSync(join(tmpdir(), "quansio-search-index-"));
    filesDir = mkdtempSync(join(tmpdir(), "quansio-artifact-files-"));
    child = spawn(SERVER_BIN, [], {
      env: {
        ...process.env,
        QUANSIO_DATABASE_URL: databaseUrl,
        QUANSIO_BIND: `127.0.0.1:${String(port)}`,
        QUANSIO_TRUST_SCOPE_HEADERS: "1",
        QUANSIO_SEARCH_INDEX_ROOT: indexRoot,
        ...(minio ? { QUANSIO_OBJECT_STORE: "1", QUANSIO_TEST_MINIO_ENDPOINT: MINIO } : {}),
      },
      stdio: ["ignore", "ignore", "pipe"],
    });
    child.stderr?.on("data", (chunk: Buffer) => stderr.push(chunk.toString()));
    await waitForHealth(origin, child, stderr);
    userData = mkdtempSync(join(tmpdir(), "quansio-desktop-art-"));
  }, 90_000);

  afterAll(async () => {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 200));
    try {
      dropScratchDatabase(databaseName);
    } catch {
      /* named by pid; a leftover is harmless */
    }
    for (const directory of [userData, indexRoot, filesDir]) {
      rmSync(directory, { recursive: true, force: true });
    }
  }, 30_000);

  it("keeps artifacts after the run closes, shows history with links and diffs, survives reload, and search finds them by content", async () => {
    if (!minio) {
      console.error(`BLOCKED_EXTERNAL: dev MinIO at ${MINIO} is not reachable; the artifact workspace needs the object store`);
      return;
    }
    const files: DesktopFiles = {
      saveAs: (name, bytes) => {
        const path = join(filesDir, name);
        writeFileSync(path, bytes);
        return Promise.resolve(path);
      },
      openWithOs: (name, _bytes, mediaType) => Promise.resolve(join(filesDir, `opened-${name}-${mediaType.replace(/\W/g, "_")}`)),
    };
    const page = new Page(origin, userData, files);

    // A run starts and ends; the artifact produced alongside it stays first-class.
    const posted = (await page.handlers["messages.post"]({ threadId: null, content: "draft the field notes [[scenario:question]]" })) as { run_id: string; thread_id: string };
    const v1Bytes = new TextEncoder().encode("# Field notes\n\nThe quokka population on the island doubled.\nSecond line stays.\n");
    const first = await page.upload("field-notes.md", "text/markdown", v1Bytes);
    await page.handlers["turns.cancel"]({ runId: posted.run_id });
    expect((await page.bridge.server.run(posted.run_id))["status"]).toBe("CANCELLED");
    await page.refreshList();
    const listed = page.state.artifacts.find((artifact) => artifact.id === first.artifact_id);
    expect(listed?.kind).toBe("document");
    expect(listed?.versionCount).toBe(1);
    expect(listed?.producedBy.userId).toBe(USER);

    // A second version; history, default comparison with the parent, and the diff.
    const v2Bytes = new TextEncoder().encode("# Field notes\n\nThe quokka population on the island tripled.\nSecond line stays.\nA third line.\n");
    const second = await page.upload("field-notes.md", "text/markdown", v2Bytes, first.artifact_id);
    expect(second.seq).toBe(2);
    await page.select(first.artifact_id);
    expect(page.state.history?.versions.map((version) => version.seq)).toEqual([1, 2]);
    expect(selectedVersion(page.state)?.id).toBe(second.version_id);
    expect(comparedVersion(page.state)?.id).toBe(first.version_id);
    await page.loadPreview();
    expect(page.state.preview?.plan.mode).toBe("text");
    expect(page.state.preview?.text).toBe(new TextDecoder().decode(v2Bytes));
    const otherContent = (await page.handlers["artifacts.content"]({ artifactId: first.artifact_id, versionId: first.version_id })) as ContentPayload;
    const diff = diffLines(textPreview(Buffer.from(otherContent.bytesBase64, "base64")).text, page.state.preview?.text ?? "");
    expect(diff?.filter((line) => line.kind !== "same").map((line) => `${line.kind}:${line.text}`)).toEqual([
      "removed:The quokka population on the island doubled.",
      "added:The quokka population on the island tripled.",
      "added:A third line.",
    ]);

    // Save writes the exact bytes; open is offered for markdown and goes to the OS handler.
    const saved = (await page.handlers["artifacts.save"]({ artifactId: first.artifact_id, versionId: second.version_id })) as { path: string | null; sizeBytes: number };
    expect(saved.path).not.toBeNull();
    expect(new Uint8Array(readFileSync(saved.path ?? ""))).toEqual(v2Bytes);
    expect(saved.sizeBytes).toBe(v2Bytes.length);
    const opened = (await page.handlers["artifacts.open"]({ artifactId: first.artifact_id, versionId: second.version_id })) as { path: string; mediaType: string };
    expect(opened.mediaType).toBe("text/markdown");

    // Malicious preview: an HTML upload with a script is shown as text and never opened.
    const html = new TextEncoder().encode("<!doctype html><html><body><script>fetch('http://evil.example')</script>hi</body></html>");
    const hostile = await page.upload("payload.html", "text/html", html);
    await page.select(hostile.artifact_id);
    await page.loadPreview();
    expect(page.state.preview?.plan.mode).toBe("text");
    expect(page.state.preview?.text).toBe(new TextDecoder().decode(html));
    expect(page.state.preview?.imageUrl).toBeNull();
    await expect(page.handlers["artifacts.open"]({ artifactId: hostile.artifact_id, versionId: hostile.version_id })).rejects.toThrow(/not opened with the OS/);
    const svg = await page.upload("evil.svg", "image/svg+xml", new TextEncoder().encode("<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'/>"));
    await page.select(svg.artifact_id);
    await page.loadPreview();
    expect(page.state.preview?.plan.mode).toBe("text");
    expect(page.state.preview?.imageUrl).toBeNull();
    await expect(page.handlers["artifacts.open"]({ artifactId: svg.artifact_id, versionId: svg.version_id })).rejects.toThrow(/not opened/);
    // A raster image is decoded in-process from a data URL, and may be opened.
    const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, ...Array.from({ length: 64 }, (_, index) => index)]);
    const image = await page.upload("chart.png", "image/png", png);
    await page.select(image.artifact_id);
    await page.loadPreview();
    expect(page.state.preview?.plan.mode).toBe("image");
    expect(page.state.preview?.imageUrl).toBe(`data:image/png;base64,${Buffer.from(png).toString("base64")}`);
    expect(((await page.handlers["artifacts.open"]({ artifactId: image.artifact_id, versionId: image.version_id })) as { mediaType: string }).mediaType).toBe("image/png");

    // Evidence links: a version produced by a step links to what the step captured. The
    // upload path records none, so the link list is empty and the evidence projection
    // still answers for an id the search finds.
    expect(selectedVersion(page.state)?.evidenceIds).toEqual([]);

    // Global search: content through the index, the message through the thread channel.
    const results = decodeSearch(await page.handlers["search.query"]({ q: "quokka", channels: ["artifacts", "threads"] }));
    expect(results.engines.some((engine) => engine.engine === "quansio-indexer")).toBe(true);
    const contentHit = results.hits.find((hit) => hit.channel === "artifacts" && hit.id === first.artifact_id);
    expect(contentHit?.matched).toBe("content");
    expect(contentHit?.provenance.locator).toBe(`artifact_version:${second.version_id}`);
    expect(results.hits.some((hit) => hit.channel === "threads" && hit.ref["thread_id"] === posted.thread_id)).toBe(false);
    const messages = decodeSearch(await page.handlers["search.query"]({ q: "field notes", channels: ["threads", "artifacts"] }));
    expect(messages.hits.some((hit) => hit.channel === "threads" && hit.ref["thread_id"] === posted.thread_id)).toBe(true);
    await expect(page.handlers["search.query"]({ q: "quokka", channels: ["memory"] })).rejects.toThrow(/not a search channel/);
    page.close();

    // Reload: a new page over the same server lists the same artifacts with the same history.
    const reloaded = new Page(origin, userData, null);
    await reloaded.refreshList();
    expect(reloaded.state.artifacts.map((artifact) => artifact.id).sort()).toEqual([first.artifact_id, hostile.artifact_id, svg.artifact_id, image.artifact_id].sort());
    await reloaded.select(first.artifact_id);
    expect(reloaded.state.history?.versions.map((version) => [version.seq, version.contentDigest])).toEqual([[1, first.content_digest], [2, second.content_digest]]);
    await reloaded.loadPreview();
    expect(reloaded.state.preview?.text).toBe(new TextDecoder().decode(v2Bytes));
    // Without file operations the page can still preview; save and open are refused, not faked.
    await expect(reloaded.handlers["artifacts.save"]({ artifactId: first.artifact_id, versionId: second.version_id })).rejects.toThrow(/not available/);
    // Evidence lookups decode when the id exists (none here) and refuse bad ids at the bridge.
    await expect(reloaded.handlers["evidence.get"]({ evidenceId: "nope" })).rejects.toThrow(/evd_/);
    expect(() => decodeEvidence({})).toThrow(/evidence/);
    reloaded.close();
  }, 90_000);

  it("shows each version's source check and saves an edit as the next version (CAP-003)", async () => {
    if (!minio) {
      console.error(`BLOCKED_EXTERNAL: dev MinIO at ${MINIO} is not reachable; the artifact workspace needs the object store`);
      return;
    }
    const page = new Page(origin, userData, null);
    // A malformed spreadsheet is stored, and its version says why it is not a valid source.
    const ragged = await page.upload("census.csv", "text/csv", new TextEncoder().encode("year,count,site\n2025,12\n"));
    await page.select(ragged.artifact_id);
    expect(page.state.history?.kind).toBe("spreadsheet");
    const first = selectedVersion(page.state);
    expect(first?.formatCheck?.ok).toBe(false);
    expect(formatCheckLabel(first?.formatCheck ?? null)).toMatch(/^fails artifact\.format: spreadsheet row 2 has 2 columns/);
    expect(editableAsText(page.state.history?.kind ?? "", first?.mediaType ?? "")).toBe(true);

    // The edit is saved the way the page saves it: the same channel, the artifact named,
    // a new immutable version on top of the old one.
    await page.loadPreview();
    const edited = (page.state.preview?.text ?? "").replace("2025,12\n", "2025,12,north\n");
    const saved = await page.upload("census.csv", first?.mediaType ?? "text/csv", new TextEncoder().encode(edited), ragged.artifact_id);
    expect(saved.artifact_id).toBe(ragged.artifact_id);
    expect(saved.seq).toBe(2);
    // The page selects what it saved and reloads the history.
    page.state = artifactWorkspaceReducer(page.state, { type: "version.created", artifactId: ragged.artifact_id, versionId: saved.version_id });
    page.state = artifactWorkspaceReducer(page.state, { type: "history.loaded", history: decodeHistory(await page.handlers["artifacts.versions"]({ artifactId: ragged.artifact_id })) });
    expect(selectedVersion(page.state)?.id).toBe(saved.version_id);
    const versions = page.state.history?.versions ?? [];
    expect(versions.map((version) => version.seq)).toEqual([1, 2]);
    expect(versions[1]?.parentVersionId).toBe(versions[0]?.id);
    expect(versions[1]?.formatCheck?.ok).toBe(true);
    expect(formatCheckLabel(versions[1]?.formatCheck ?? null)).toBe("valid source");
    await page.loadPreview();
    expect(page.state.preview?.text).toBe("year,count,site\n2025,12,north\n");
    page.close();
  }, 60_000);
});

if (blocked) {
  it.skip(blocked, () => undefined);
}
