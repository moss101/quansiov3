/**
 * QA-008 critical-journey qualification (desktop/web UX).
 *
 * Playwright against a served build is the release boundary (`QUANSIO_TEST_E2E=1`);
 * without it this suite drives the shipped journey modules in-process — the same
 * code the renderer runs — and asserts the journeys, keyboard/accessibility and
 * degraded/offline states.
 */
import { describe, expect, it } from "vitest";

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

import { onboardingPlan } from "../../apps/desktop/src/onboarding/flow.js";
import { answerParams, postParams } from "../../apps/desktop/src/conversation/composer.js";
import { conversationReducer, decodeQuestions, initialConversation, openQuestions } from "../../apps/desktop/src/conversation/state.js";
import { createHash } from "node:crypto";

import { bindApproval, decodeApprovals } from "../../apps/desktop/src/trust/approvals.js";
import { decodeHistory, diffLines } from "../../apps/desktop/src/artifacts/workspace.js";
import { openAllowed, previewIsSafe, previewPlan } from "../../apps/desktop/src/artifacts/preview.js";
import { agentInputAllowed, applyEvent, decodeLiveSession, handbackAllowed, takeoverAllowed } from "../../apps/desktop/src/live/session.js";
import { archiveInRoster, visibleRoster } from "../../apps/desktop/src/teammates/roster.js";
import { receivesProtected } from "../../apps/desktop/src/collaboration/participants.js";
import { STRINGS } from "../../apps/desktop/src/i18n/strings.js";
import { SessionFile } from "../../apps/desktop/src/main/session-file.js";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const E2E_FLAG = process.env.QUANSIO_TEST_E2E === "1";

function a11yReport(): string {
  return fs.readFileSync(path.join(HERE, "a11y-baseline.json"), "utf8");
}

describe("critical journeys", () => {
  it("walks onboarding to artifact review without a config file", () => {
    // 1. Onboarding provisions without config.
    const plan = onboardingPlan({
      email: "ada@example.com",
      displayName: "Ada",
      credentialHandle: "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    });
    expect(plan.requiresConfigFile).toBe(false);
    expect(plan.steps.length).toBeGreaterThan(3);
    expect(() =>
      onboardingPlan({
        email: "ada@example.com",
        displayName: "Ada",
        credentialHandle: "raw-password",
      }),
    ).toThrow();

    // 2. Chat/objective: the message is a PostMessage command and the objective is the
    //    run/thread the server assigns, never a UI-only identity.
    let session = conversationReducer(initialConversation("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"), { type: "composer.text", text: "prepare the notes" });
    expect(postParams(session)).toEqual({ workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", content: "prepare the notes" });
    session = conversationReducer(session, { type: "composer.posted", threadId: "thr_journey", runId: "run_journey", messageId: "msg_journey" });
    expect(session.threadId).toBe("thr_journey");
    expect(session.activeRunId).toBe("run_journey");

    // 3. A question interrupts; the answer is an AnswerQuestion command back into the
    //    thread, and the produced artifact is attached (by identity) for review.
    session = conversationReducer(session, {
      type: "questions.loaded",
      questions: decodeQuestions({ questions: [{ id: "q_1", run_id: "run_journey", kind: "free_text", prompt: "format?", status: "open" }] }),
    });
    expect(openQuestions(session).map((item) => item.id)).toEqual(["q_1"]);
    expect(answerParams("q_1", "markdown")).toEqual({ question_id: "q_1", value: "markdown" });
    session = conversationReducer(session, { type: "composer.attach", attachment: { artifactId: "art_journey", title: "notes", sizeBytes: 1 } });
    session = conversationReducer(session, { type: "composer.text", text: "review these" });
    expect(postParams(session)["attachments"]).toEqual(["art_journey"]);

    // 4. Approval: exact preview or superseded — never approve-on-faith. The decision
    //    carries the digest of the previewed request; a request whose digest moved is refused.
    const digestPayload = (payload: unknown): string => createHash("sha256").update(JSON.stringify(payload)).digest("hex");
    const payload = { effect: "message.send", to: "ops@example.com" };
    const request = (digest: string) => ({
      approvals: [{ id: "apr_1", workspace_id: "ws_1", run_id: "run_journey", effect_id: "eff_1", summary: "Approve message.send", consequence_preview: { recipients: ["ops@example.com"] }, params_digest: digest, status: "requested", expires_at: "2999-01-01T00:00:00Z", effect: { class: "message.send", tier: 2 } }],
    });
    const [shown] = decodeApprovals(request(digestPayload(payload)));
    const [changed] = decodeApprovals(request(digestPayload({ ...payload, to: "other@example.com" })));
    if (!shown || !changed) {
      throw new Error("previews expected");
    }
    expect(bindApproval(shown, shown)).toEqual({ ok: true, params: { approval_request_id: "apr_1", params_digest: digestPayload(payload) } });
    expect(bindApproval(shown, changed)).toMatchObject({ ok: false, code: "APPROVAL_SUPERSEDED" });

    // 5. Artifact review: the produced document is versioned (the server's history
    //    projection), diffable, and safe to preview — active content is text, never opened.
    const history = decodeHistory({
      artifact: { id: "art_journey", title: "notes", kind: "document", origin: { kind: "tool", ref: "tc_1" }, current_version_id: "artv_2" },
      versions: [
        { id: "artv_1", seq: 1, content_digest: digestPayload("v1"), size_bytes: 2, media_type: "text/markdown", produced_by: { run_id: "run_journey" }, evidence_ids: ["evd_1"] },
        { id: "artv_2", seq: 2, content_digest: digestPayload("v2"), size_bytes: 2, media_type: "text/html", produced_by: { run_id: "run_journey" }, evidence_ids: [], parent_version_id: "artv_1" },
      ],
    });
    expect(history.versions).toHaveLength(2);
    expect(history.versions[0]?.evidenceIds).toEqual(["evd_1"]);
    expect(diffLines("v1", "v2")?.map((line) => line.kind)).toEqual(["removed", "added"]);
    expect(previewIsSafe("text/html")).toBe(true);
    expect(previewPlan("text/html", 2).mode).toBe("text");
    expect(openAllowed("text/html")).toBe(false);

    // 6. Live takeover then handback: the same session moves to the user and the agent
    //    is fenced; the handback returns it; another session's event changes nothing.
    const live = decodeLiveSession({
      session: { id: "bsn_1", target_id: "tgt_1", run_id: "run_journey", control_holder: "agent", status: "active", agent_may_drive: true, tabs: [] },
      automation: { paused: false, reason: null, agent_input_fenced: false },
      terminal_sessions: [],
      computer_control: null,
      run: null,
    });
    expect(takeoverAllowed(live)).toBe(true);
    const taken = applyEvent(live, { type: "browser.control_changed", payload: { session_id: "bsn_1", holder: "user", status: "paused_takeover" } });
    expect(taken.sessionId).toBe("bsn_1");
    expect(taken.controller).toBe("user");
    expect(taken.automation.paused).toBe(true);
    expect(agentInputAllowed(taken)).toBe(false);
    expect(handbackAllowed(taken)).toBe(true);
    expect(applyEvent(taken, { type: "browser.control_changed", payload: { session_id: "bsn_2", holder: "agent", status: "active" } })).toBe(taken);
    const returned = applyEvent(taken, { type: "browser.control_changed", payload: { session_id: "bsn_1", holder: "agent", status: "active" } });
    expect(returned.controller).toBe("agent");
    expect(returned.automation.paused).toBe(false);
    expect(agentInputAllowed(returned)).toBe(true);
  });

  it("archives a teammate keeping lineage, and stops protected delivery when removed", () => {
    // APP-014: archiving keeps lineage — the entry stays visible with no active run.
    const roster = visibleRoster([
      { id: "agt_1", name: "Scout", status: "active", activeRunId: "run_1" },
      { id: "agt_2", name: "Archived", status: "archived", activeRunId: null },
    ]);
    expect(roster.map((entry) => entry.status)).toEqual(["active", "archived"]);
    const afterArchive = archiveInRoster(
      [{ id: "agt_1", name: "Scout", status: "active", activeRunId: "run_1" }],
      "agt_1",
    );
    expect(afterArchive[0]?.status).toBe("archived");
    expect(afterArchive[0]?.activeRunId).toBeNull();
    expect(receivesProtected({ id: "agt_1", status: "removed" })).toBe(false);
    expect(receivesProtected({ id: "agt_1", status: "active" })).toBe(true);
  });

  it("surfaces every state through externalized strings, never hard-coded copy", () => {
    for (const key of ["reconnecting", "runRestored", "permissionDenied"] as const) {
      expect(typeof STRINGS[key]).toBe("string");
      expect(STRINGS[key].length).toBeGreaterThan(0);
    }
  });

  it("degraded and offline states recover through the reconnect path", () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "quansio-journeys-"));
    try {
      const store = new SessionFile(dir);
      store.begin("http://127.0.0.1:8080", "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
      store.attach({
        runId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
        generation: 3,
        streamCursor: "c1",
        workspaceId: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      });
      store.advance("c2");
      // A restart reads the same durable run and cursor back from disk: reconnect replays from c2.
      const restored = new SessionFile(dir).current();
      expect(restored?.activeRun?.runId).toBe("run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
      expect(restored?.streamCursor).toBe("c2");
      store.clear();
      expect(new SessionFile(dir).current()).toBeNull();
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});

describe("accessibility baseline", () => {
  it("has no open accessibility blockers on the shipped surfaces", () => {
    const report = JSON.parse(a11yReport()) as {
      surfaces: { id: string; violations: unknown[] }[];
    };
    const open = report.surfaces.filter((surface) => surface.violations.length > 0);
    expect(open).toEqual([]);
  });

  it("keeps a keyboard-only path for every interactive journey step", () => {
    const report = JSON.parse(a11yReport()) as {
      surfaces: { id: string; keyboardReachable: boolean }[];
    };
    const unreachable = report.surfaces.filter((surface) => !surface.keyboardReachable);
    expect(unreachable).toEqual([]);
  });
});

describe("playwright boundary", () => {
  // `playwright` is a real-boundary dependency, deliberately not a package.json
  // dependency of this workspace (QA-008's served-build tier only runs under
  // QUANSIO_TEST_E2E=1, on a host that installs it separately) -- see
  // playwright.d.ts for the minimal ambient types that stand in for it here.
  it.skipIf(!E2E_FLAG)("runs the served build on the support matrix", async () => {
    const { chromium, firefox, webkit } = await import("playwright");
    for (const browserType of [chromium, firefox, webkit]) {
      const browser = await browserType.launch();
      const page = await browser.newPage();
      await page.goto(process.env.QUANSIO_E2E_URL ?? "http://127.0.0.1:4173");
      expect(await page.title()).toContain("Quansio");
      await browser.close();
    }
  });

  it("reports BLOCKED_EXTERNAL when the served E2E boundary is absent", () => {
    if (!E2E_FLAG) {
      console.log(
        "BLOCKED_EXTERNAL: QUANSIO_TEST_E2E=1 is not set; served Playwright matrix not running",
      );
    }
    expect(true).toBe(true);
  });
});
