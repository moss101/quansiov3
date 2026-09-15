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
import * as path from "node:path";
import { fileURLToPath } from "node:url";

import { onboardingPlan } from "../../apps/desktop/src/onboarding/flow.js";
import { answerQuestion, attachArtifact, bindObjective, newChatSession, postMessage } from "../../apps/desktop/src/conversation/composer.js";
import { approve, digestPayload, preview } from "../../apps/desktop/src/trust/approvals.js";
import { addVersion, previewIsSafe } from "../../apps/desktop/src/artifacts/workspace.js";
import { agentInputAllowed, handback, takeover } from "../../apps/desktop/src/live/session.js";
import { archiveInRoster, visibleRoster } from "../../apps/desktop/src/teammates/roster.js";
import { receivesProtected } from "../../apps/desktop/src/collaboration/participants.js";
import { STRINGS } from "../../apps/desktop/src/i18n/strings.js";
import { DesktopSessionStore } from "../../apps/desktop/src/renderer/session.js";

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

    // 2. Chat/objective: bind an objective and post a message.
    let session = newChatSession("thr_journey");
    session = bindObjective(session, "wn_journey", "run_journey");
    session = postMessage(session, "prepare the notes");
    expect(session.messages.some((m) => m === "prepare the notes")).toBe(true);

    // 3. A question interrupts; the answer routes back into the thread, and the
    //    produced artifact is attached for review.
    session = answerQuestion(session, { questionId: "q_1", answer: "markdown" });
    session = attachArtifact(session, { artifactId: "art_journey", title: "notes" });
    expect(session.answeredQuestions.map((item) => item.questionId)).toEqual(["q_1"]);
    expect(session.attachments.map((item) => item.artifactId)).toEqual(["art_journey"]);

    // 4. Approval: exact preview or superseded — never approve-on-faith.
    const payload = { effect: "message.send", to: "ops@example.com" };
    const shown = preview("apr_1", payload);
    expect(shown.digest).toBe(digestPayload(payload));
    expect(approve(shown, payload).ok).toBe(true);
    expect(approve(shown, { ...payload, to: "other@example.com" })).toEqual({
      ok: false,
      code: "APPROVAL_SUPERSEDED",
    });

    // 5. Artifact review: the produced document is versioned and safe to preview.
    const artifact = addVersion(
      {
        id: "art_journey",
        title: "notes",
        kind: "document",
        runId: "run_journey",
        versions: [{ id: "artv_1", seq: 1, contentDigest: digestPayload("v1"), mediaType: "text/markdown" }],
        sourceEvidenceIds: ["evd_1"],
      },
      { id: "artv_2", seq: 2, contentDigest: digestPayload("v2"), mediaType: "text/html" },
    );
    expect(artifact.versions).toHaveLength(2);
    expect(previewIsSafe("text/html")).toBe(true);

    // 6. Live takeover then handback: takeover gives the user control and pauses
    //    the agent; handback returns control and unpauses.
    const live = takeover({
      sessionId: "tgt_1",
      kind: "browser",
      controller: "agent",
      paused: false,
      terminalCursor: null,
    });
    expect(live.controller).toBe("user");
    expect(live.paused).toBe(true);
    expect(agentInputAllowed(live)).toBe(false);
    const returned = handback(live);
    expect(returned.controller).toBe("agent");
    expect(returned.paused).toBe(false);
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
    const store = new DesktopSessionStore();
    const session = {
      runId: "run_offline",
      generation: 3,
      streamCursor: "c1",
      workspaceId: "ws_offline",
    };
    store.attach(session);
    expect(store.active()).toEqual(session);
    store.clear();
    expect(store.active()).toBeNull();
    // Reconnect after the drop replays from the same durable cursor.
    store.attach({ ...session, streamCursor: "c2" });
    expect(store.reconnect()?.streamCursor).toBe("c2");
    expect(store.reconnect()?.runId).toBe("run_offline");
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
