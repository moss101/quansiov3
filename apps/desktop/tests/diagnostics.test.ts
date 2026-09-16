import { describe, expect, it } from "vitest";

import {
  CANARY_PLACEHOLDER,
  DEFAULT_SECRET_CANARY,
  exportBundle,
  redactText,
} from "../src/diagnostics/bundle.js";

describe("desktop diagnostics", () => {
  it("traces a run by correlation id and redacts canaries", () => {
    const raw = `Bearer abc sk-live-1 user@x.test ${DEFAULT_SECRET_CANARY}`;
    const redacted = redactText(raw);
    expect(redacted).not.toContain(DEFAULT_SECRET_CANARY);
    expect(redacted).not.toContain("Bearer abc");
    expect(redacted).toContain(CANARY_PLACEHOLDER);
    const bundle = exportBundle("corr_run_1", [
      { correlationId: "corr_run_1", service: "desktop", msg: raw },
      { correlationId: "corr_run_1", service: "runtime", msg: "ok" },
    ]);
    expect(bundle.correlationId).toBe("corr_run_1");
    expect(JSON.stringify(bundle)).not.toContain(DEFAULT_SECRET_CANARY);
    expect(bundle.frames[0]?.msg).toContain(CANARY_PLACEHOLDER);
  });

  it("bounds the diagnostic bundle", () => {
    const frames = Array.from({ length: 40 }, (_, i) => ({
      correlationId: "corr_b",
      service: "desktop" as const,
      msg: `line ${String(i)} ${DEFAULT_SECRET_CANARY}`,
    }));
    const bundle = exportBundle("corr_b", frames, 400);
    expect(bundle.truncated).toBe(true);
    expect(JSON.stringify(bundle).length).toBeLessThanOrEqual(480);
    expect(JSON.stringify(bundle)).not.toContain(DEFAULT_SECRET_CANARY);
  });
});
