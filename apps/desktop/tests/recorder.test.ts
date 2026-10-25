/**
 * The desktop's recorder host against the real `quansio-recorder` process (ADP-003, DOMAIN.md §11.7 B).
 *
 * Real: the executable (built by `swift build --package-path native/macos`), the bootstrap file and the
 * private socket, the one-time token handshake, the live permission answers and the recorder's own
 * archive. The server is a recording double (the real server side is proven by `skill_recording.rs` against
 * PostgreSQL and the real plane, with an archive this recorder produced). Without the executable the tests
 * print `BLOCKED_EXTERNAL` and skip; without Accessibility and Input Monitoring the start tests assert the
 * typed permission refusal instead and say which positive path was not run.
 */

import { existsSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { RecordingHost } from "../src/main/recorder.js";
import type { RecordingServerPort } from "../src/main/recorder.js";

const ROOT = join(__dirname, "..", "..", "..");
const BINARY = join(ROOT, "native", "macos", ".build", "debug", "quansio-recorder");
const AVAILABLE = process.platform === "darwin" && existsSync(BINARY);
if (!AVAILABLE) {
  console.log(`BLOCKED_EXTERNAL: ${BINARY} is not built (swift build --package-path native/macos --product quansio-recorder)`);
}

const TENANT = "tn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const USER = "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const REC = "rec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA";
const LIMITS = {
  max_actions: 2000,
  action_text_max_chars: 2000,
  attribute_max_chars: 200,
  screenshots: { enabled: false, max_count: 24, min_interval_ms: 5000, max_bytes: 262144, max_dimension_px: 1280, screen_text_max_chars: 2000 },
  transcript: { max_segments: 400, max_chars: 20000 },
  archive: { max_bytes: 8388608 },
};

interface Sent {
  readonly name: string;
  readonly params: Record<string, unknown>;
}

class RefusalError extends Error {
  constructor(readonly code: string) {
    super(`${code}: refused`);
  }
}

function serverDouble(options: { finishRefusals?: string[] } = {}): { port: RecordingServerPort; sent: Sent[] } {
  const sent: Sent[] = [];
  const refusals = [...(options.finishRefusals ?? [])];
  const port: RecordingServerPort = {
    invoke: (name, _commandId, params) => {
      sent.push({ name, params });
      if (name === "StartRecording") {
        return Promise.resolve({ result: { recording_id: REC, epoch: params["epoch"], limits: LIMITS, max_duration_ms: 1_800_000, rule_set: "v1" } });
      }
      if (name === "FinishRecording") {
        const refusal = refusals.shift();
        if (refusal) {
          return Promise.reject(new RefusalError(refusal));
        }
        return Promise.resolve({ result: { recording: { id: REC, status: "finished" } } });
      }
      return Promise.resolve({ result: { recording: { id: REC, status: "cancelled" } } });
    },
  };
  return { port, sent };
}

const hosts: RecordingHost[] = [];

function host(port: RecordingServerPort, overrides: { identity?: () => { tenantId: string; userId: string } | null; binary?: string | null; tempRoot?: string } = {}): RecordingHost {
  const created = new RecordingHost({
    server: port,
    binary: overrides.binary === undefined ? BINARY : overrides.binary,
    identity: overrides.identity ?? (() => ({ tenantId: TENANT, userId: USER })),
    ...(overrides.tempRoot ? { tempRoot: overrides.tempRoot } : {}),
    readyTimeoutMs: 150_000,
    pollMs: 200,
  });
  hosts.push(created);
  return created;
}

afterEach(() => {
  for (const created of hosts.splice(0)) {
    created.dispose();
  }
});

const LONG = 200_000;

/** The recorder's answer to "may I start": granted, or the typed refusal the page's guide shows. */
async function startOrExplain(instance: RecordingHost, sent: Sent[]): Promise<boolean> {
  const started = await instance.start("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
  if (started.ok) {
    return true;
  }
  // The live host did not grant a permission recording needs: the refusal is typed, names the permission,
  // and the server never heard of a recording.
  expect(started.code).toBe("RECORDING_PERMISSION_REQUIRED");
  expect(["accessibility", "input_monitoring"]).toContain(started.permission);
  expect(sent.find((call) => call.name === "StartRecording")).toBeUndefined();
  console.log(`BLOCKED_EXTERNAL: ${String(started.permission)} is ${String(started.status)} for this process; the recorder start path was not run`);
  return false;
}

describe("the recorder host without a recorder", () => {
  it("says so, and contacts no server, with no binary, no identity or a recording already in progress", async () => {
    const { port, sent } = serverDouble();
    expect(await host(port, { binary: null }).start("ws_x")).toMatchObject({ ok: false, code: "RECORDER_UNAVAILABLE" });
    expect(await host(port, { identity: () => null }).start("ws_x")).toMatchObject({ ok: false, code: "AUTH_REQUIRED" });
    expect(sent).toEqual([]);
  });

  it("refuses to finish without a click, and a finish or cancel with nothing recording", async () => {
    const { port, sent } = serverDouble();
    const instance = host(port);
    expect(await instance.finish({ gesture: false })).toMatchObject({ ok: false, code: "GESTURE_REQUIRED" });
    expect(await instance.finish({ gesture: true })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(await instance.cancel()).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
    expect(sent).toEqual([]);
  });

  it("advances the epoch on every account change", () => {
    const instance = host(serverDouble().port);
    expect(instance.epoch).toBe(1);
    instance.accountChanged();
    instance.accountChanged();
    expect(instance.epoch).toBe(3);
  });
});

describe.skipIf(!AVAILABLE)("the real recorder process", () => {
  it(
    "starts through a one-time token, is finished only by a click and posts the recorder's own archive with the owner fence",
    async () => {
      const { port, sent } = serverDouble();
      const instance = host(port);
      instance.accountChanged(); // epoch 2: the fence is whatever main holds
      if (!(await startOrExplain(instance, sent))) {
        return;
      }
      expect(instance.status()).toMatchObject({ state: "recording", recordingId: REC });
      const start = sent.find((call) => call.name === "StartRecording");
      expect(start?.params).toMatchObject({ workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", epoch: 2 });

      // Not finished by anything but the person's click.
      expect(await instance.finish({ gesture: false })).toMatchObject({ ok: false, code: "GESTURE_REQUIRED" });
      await new Promise((resolve) => setTimeout(resolve, 1500));
      expect(instance.status().state).toBe("recording");
      expect(sent.find((call) => call.name === "FinishRecording")).toBeUndefined();

      const finished = await instance.finish({ gesture: true });
      expect(finished).toMatchObject({ ok: true });
      const posted = sent.find((call) => call.name === "FinishRecording");
      expect(posted?.params["recording_id"]).toBe(REC);
      expect(posted?.params["epoch"]).toBe(2);
      const archive = posted?.params["archive"] as { content_base64: string; content_digest: string };
      const bytes = Buffer.from(archive.content_base64, "base64");
      const document = JSON.parse(bytes.toString("utf8")) as Record<string, unknown>;
      expect(document).toMatchObject({
        schema_version: "v1",
        recording_id: REC,
        platform: "macos",
        owner: { tenant_id: TENANT, user_id: USER, epoch: 2 },
        finish: { by: "user" },
        redaction: { rule_set: "v1" },
      });
      const { createHash } = await import("node:crypto");
      expect(createHash("sha256").update(bytes).digest("hex")).toBe(archive.content_digest);
      expect(instance.status().state).toBe("idle");
    },
    LONG,
  );

  it(
    "ends the local recording at once when the account changes, and a later finish is refused",
    async () => {
      const { port, sent } = serverDouble();
      const instance = host(port);
      if (!(await startOrExplain(instance, sent))) {
        return;
      }
      const before = instance.epoch;
      instance.accountChanged();
      expect(instance.epoch).toBe(before + 1);
      expect(instance.status()).toMatchObject({ state: "idle", notice: "account_changed" });
      expect(await instance.finish({ gesture: true })).toMatchObject({ ok: false, code: "CONFLICT_STATE" });
      expect(sent.find((call) => call.name === "FinishRecording")).toBeUndefined();
    },
    LONG,
  );

  it(
    "keeps the archive for a retry when the server refuses it, and cancels through the server",
    async () => {
      const { port, sent } = serverDouble({ finishRefusals: ["RECORDING_UNREDACTED"] });
      const instance = host(port);
      if (!(await startOrExplain(instance, sent))) {
        return;
      }
      const refused = await instance.finish({ gesture: true });
      expect(refused).toMatchObject({ ok: false, code: "RECORDING_UNREDACTED" });
      expect(instance.status()).toMatchObject({ state: "finishing", notice: "RECORDING_UNREDACTED" });
      const retried = await instance.finish({ gesture: true });
      expect(retried).toMatchObject({ ok: true });
      const posts = sent.filter((call) => call.name === "FinishRecording");
      expect(posts).toHaveLength(2);
      expect((posts[0]?.params["archive"] as { content_digest: string }).content_digest).toBe((posts[1]?.params["archive"] as { content_digest: string }).content_digest);

      const second = serverDouble();
      const another = host(second.port);
      if (await startOrExplain(another, second.sent)) {
        expect(await another.cancel()).toMatchObject({ ok: true });
        expect(second.sent.map((call) => call.name)).toEqual(["StartRecording", "CancelRecording"]);
        expect(another.status().state).toBe("idle");
      }
    },
    LONG,
  );

  it(
    "keeps its one-time token off the disk once the recorder has read it, in a private directory",
    async () => {
      const { port, sent } = serverDouble();
      const temp = join("/tmp", `qrec-test-${String(process.pid)}`);
      const { mkdirSync } = await import("node:fs");
      mkdirSync(temp, { recursive: true, mode: 0o700 });
      const instance = host(port, { tempRoot: temp });
      if (!(await startOrExplain(instance, sent))) {
        return;
      }
      const directories = readdirSync(temp).filter((name) => name.startsWith("qrec."));
      expect(directories).toHaveLength(1);
      const directory = join(temp, directories[0] ?? "");
      expect(readdirSync(directory)).not.toContain("token");
      expect(statSync(join(directory, "sock")).mode & 0o777).toBe(0o700);
      instance.dispose();
      expect(readdirSync(temp).filter((name) => name.startsWith("qrec."))).toHaveLength(0);
    },
    LONG,
  );
});

if (!AVAILABLE) {
  it.skip(`BLOCKED_EXTERNAL: ${BINARY} is not built (swift build --package-path native/macos --product quansio-recorder)`, () => undefined);
}
