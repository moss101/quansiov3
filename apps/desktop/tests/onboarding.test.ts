import { describe, expect, it } from "vitest";

import { onboardingPlan } from "../src/onboarding/flow.js";

describe("onboarding", () => {
  it("completes without a configuration file and keeps secrets as handles", () => {
    const plan = onboardingPlan({
      email: "ada@example.com",
      displayName: "Ada",
      credentialHandle: "sec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA",
    });
    expect(plan.requiresConfigFile).toBe(false);
    expect(plan.steps).toContain("provision-personal-tenant");
    expect(plan.authMethods).toContain("email_magic_link");
    expect(() =>
      onboardingPlan({
        email: "ada@example.com",
        displayName: "Ada",
        credentialHandle: "sk-not-a-handle",
      }),
    ).toThrow(/sec_/);
  });
});
