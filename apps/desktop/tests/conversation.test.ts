import { describe, expect, it } from "vitest";

import {
  answerQuestion,
  attachArtifact,
  bindObjective,
  cancelRun,
  inspectTurn,
  newChatSession,
  postMessage,
  reload,
} from "../src/conversation/composer.js";

describe("conversation composer", () => {
  it("maps objectives to WorkGraph/Run and survives reload", () => {
    let session = newChatSession("thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    session = postMessage(session, "plan the launch");
    session = bindObjective(
      session,
      "wn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    );
    session = attachArtifact(session, {
      artifactId: "art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      title: "brief",
    });
    session = answerQuestion(session, {
      questionId: "q_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
      answer: "ship it",
    });
    session = inspectTurn(session, {
      modelRoute: "chat/anthropic",
      contextSources: ["thread", "artifact"],
      trustLabels: ["TRUSTED_USER"],
      toolsUsed: ["fs.read"],
    });
    const restored = reload(session);
    expect(restored.threadId).toBe(session.threadId);
    expect(restored.runId).toBe("run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(restored.objectiveWorkNodeId).toBe("wn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(restored.attachments).toHaveLength(1);
    expect(restored.answeredQuestions).toHaveLength(1);
    expect(restored.inspector.toolsUsed).toEqual(["fs.read"]);
    expect(cancelRun(restored).cancelled).toBe(true);
  });

  it("refuses a UI-only objective without run identities", () => {
    const session = newChatSession("thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    expect(() => bindObjective(session, "local-objective", "ui-run")).toThrow(
      /WorkGraph\/Run/,
    );
  });
});
