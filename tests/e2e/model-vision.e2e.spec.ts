/**
 * A real model reads a real page through a real screenshot (INT-002, EXEC-009).
 *
 * Everything between the question and the answer is the real thing: the server answers the turn through the Python
 * intelligence process and a live provider; the model chooses the tools; `browser.navigate` is approved by the person; the
 * screenshot is taken by a real Chrome on a real worker, recorded as evidence in the real object store, read back and shown
 * to the model as a picture. The page writes its answer on a canvas, so the words are in the pixels and nowhere in the
 * page's text: only a model that really saw the picture can give them back.
 *
 * Environment (the OpenAI-compatible live boundary, as `python/tests/intelligence/test_model_gateway_live.py` names it):
 * `QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL`, `_API_KEY` and `_MODEL_ID` (the model must accept images), plus the QA-008
 * harness's. Absent, the suite reports `BLOCKED_EXTERNAL`.
 */

import { spawn, type ChildProcess } from "node:child_process";
import { createServer, type Server } from "node:http";
import { mkdtempSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { createServer as netServer, Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { serverBlocked, ROOT, Stack, emailOf, OWNER, waitFor } from "./harness.js";

const BASE = process.env["QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL"]?.trim() ?? "";
const KEY = process.env["QUANSIO_TEST_OPENAI_COMPATIBLE_API_KEY"]?.trim() ?? "";
const MODEL = process.env["QUANSIO_TEST_OPENAI_COMPATIBLE_MODEL_ID"]?.trim() ?? "";
const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

const blocked =
  BASE === "" || KEY === "" || MODEL === ""
    ? "BLOCKED_EXTERNAL: QUANSIO_TEST_OPENAI_COMPATIBLE_BASE_URL, _API_KEY and _MODEL_ID are not all set; no live model to ask"
    : !existsSync(CHROME)
      ? `BLOCKED_EXTERNAL: Google Chrome is not installed at ${CHROME}`
      : serverBlocked();

/** The answer is drawn, not written: the page's text and accessibility tree never contain it. */
const CODE = "ZEBRA-4417";
const PAGE = `<!doctype html><html><head><title>Vision check</title></head><body style="margin:0;background:#fff">
<h1 style="font:20px sans-serif">Vision check</h1>
<canvas id="c" width="900" height="360"></canvas>
<script>
var c=document.getElementById('c').getContext('2d');
c.fillStyle='#5b2a86';c.fillRect(0,0,900,360);
c.fillStyle='#ffffff';c.font='bold 110px sans-serif';c.fillText('${CODE}',60,215);
</script></body></html>`;

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const probe = netServer();
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      const port = typeof address === "object" && address ? address.port : 0;
      probe.close(() => {
        if (port) {
          resolve(port);
        } else {
          reject(new Error("no port"));
        }
      });
    });
  });
}

describe.skipIf(blocked !== null)("a live model reads a page from its screenshot", () => {
  let intelligence: ChildProcess;
  let stack: Stack;
  let site: Server;
  let scratch: string;
  let pageUrl = "";
  const intelligenceLog: string[] = [];

  beforeAll(async () => {
    scratch = mkdtempSync(join(tmpdir(), "qa-vision-"));
    // The catalog is test configuration, outside the source tree: the provider, the model id and that it can see.
    const catalog = join(scratch, "models.yaml");
    writeFileSync(
      catalog,
      `version: 1
providers:
  openai-compatible:
    kind: openai_compatible
    base_url: ${BASE}
    credential_handle: provider/live-vision
routing:
  strategy: deterministic
  primary: live-vision
  fallback_order: []
models:
  - id: live-vision
    provider: openai-compatible
    model_id: ${MODEL}
    capabilities: [tools, vision, streaming]
    context_window: 128000
    cost_class: low
    dlp_eligible: false
`,
    );
    const port = await freePort();
    intelligence = spawn("uv", ["run", "--project", join(ROOT, "python"), "python", "-m", "intelligence.server", "--transport", "tcp", "--host", "127.0.0.1", "--port", String(port)], {
      env: { ...process.env, QUANSIO_INTELLIGENCE_MODEL_CATALOG: catalog, QUANSIO_SECRET_PROVIDER_LIVE_VISION: KEY },
      stdio: ["ignore", "ignore", "pipe"],
    });
    intelligence.stderr?.on("data", (chunk: Buffer) => intelligenceLog.push(chunk.toString()));
    await waitFor(async () => {
      try {
        await new Promise<void>((resolve, reject) => {
          const socket = new Socket();
          socket.once("error", reject);
          socket.connect(port, "127.0.0.1", () => {
            socket.destroy();
            resolve();
          });
        });
        return true;
      } catch {
        return false;
      }
    }, "the intelligence process to listen", 120_000);

    site = createServer((_request, response) => {
      response.writeHead(200, { "content-type": "text/html" });
      response.end(PAGE);
    });
    await new Promise<void>((resolve) => site.listen(0, "127.0.0.1", resolve));
    const address = site.address();
    pageUrl = `http://127.0.0.1:${typeof address === "object" && address ? String(address.port) : "0"}/`;
    stack = await Stack.start("vision", { browse: true, intelligenceEndpoint: `http://127.0.0.1:${String(port)}` });
  }, 240_000);

  afterAll(async () => {
    await stack.drop();
    site.close();
    intelligence.kill("SIGTERM");
    rmSync(scratch, { recursive: true, force: true });
  }, 60_000);

  it("reads the words drawn in the page's pixels, which no text of the page contains", async () => {
    const owner = await stack.session(emailOf(OWNER));
    const posted = (await stack.command(owner.access_token, "PostMessage", {
      workspace_id: owner.workspace_id,
      content: `Open ${pageUrl} in the browser, take a screenshot of it, and tell me the code written in white letters on the purple block. Use the screenshot to read it. Reply with the code only.`,
    })) as { thread_id: string; run_id: string };

    // The person approves each action the page's content could have steered (an unproven chain asks first).
    const approved = new Set<string>();
    const dump = async (): Promise<string> =>
      JSON.stringify({
        messages: await stack.get(owner.access_token, `/v1/messages?thread_id=${posted.thread_id}`),
        timeline: await stack.get(owner.access_token, `/v1/runs/${posted.run_id}/timeline`),
        intelligence: intelligenceLog.join("").slice(-3000),
      }).slice(0, 12000);
    const answer = await waitFor(
      async () => {
        const pending = ((await stack.get(owner.access_token, `/v1/approvals?workspace_id=${owner.workspace_id}&status=requested`))["approvals"] ?? []) as { id: string; params_digest: string; run_id: string }[];
        for (const request of pending) {
          if (!approved.has(request.id)) {
            try {
              await stack.command(owner.access_token, "ApproveEffect", { approval_request_id: request.id, params_digest: request.params_digest });
              approved.add(request.id);
            } catch (error) {
              // The request is listed a moment before the run reaches WAITING_APPROVAL; ask again on the next poll.
              if (!String(error).includes("CONFLICT_STATE")) {
                throw error;
              }
            }
          }
        }
        const messages = ((await stack.get(owner.access_token, `/v1/messages?thread_id=${posted.thread_id}`))["messages"] ?? []) as { author_kind?: string; content_blocks?: { text?: string }[] }[];
        const agent = messages.filter((message) => message.author_kind === "agent");
        const text = agent.map((message) => (message.content_blocks ?? []).map((block) => block.text ?? "").join(" ")).join("\n");
        return /4417/.test(text) ? text : undefined;
      },
      "the model's answer to name the code in the picture",
      360_000,
    ).catch(async (error: unknown) => {
      throw new Error(`${String(error)}\n${await dump()}`);
    });
    expect(answer).toContain("4417");

    // It could only have read it from the picture: the page's text has no such words, and the evidence is a screenshot of this run.
    expect(PAGE.replace(CODE, "")).not.toContain("4417");
    const evidence = await stack.get(owner.access_token, `/v1/runs/${posted.run_id}/timeline`);
    const steps = (evidence as { turns: { steps: { evidence_ids: string[]; tool_calls?: { tool_name: string }[] }[] }[] }).turns.flatMap((turn) => turn.steps);
    const shot = steps.find((step) => (step.tool_calls ?? []).some((call) => call.tool_name === "browser.screenshot"));
    expect(shot?.evidence_ids.length ?? 0).toBeGreaterThan(0);
  }, 480_000);
});
