/**
 * New-user onboarding (APP-002). No configuration files; the server provisions a
 * personal tenant and workspace. Secrets are `sec_` handles only.
 */

export interface OnboardingInput {
  readonly email: string;
  readonly displayName: string;
  /** Opaque secret handle from the OS keychain / secret broker. */
  readonly credentialHandle: string;
}

export interface OnboardingPlan {
  readonly steps: readonly string[];
  readonly requiresConfigFile: false;
  readonly authMethods: readonly string[];
}

/** The steps a fresh user walks. Nothing here reads a YAML/JSON config file. */
export function onboardingPlan(input: OnboardingInput): OnboardingPlan {
  if (!input.credentialHandle.startsWith("sec_")) {
    throw new Error("credentialHandle must be a sec_ handle");
  }
  return {
    steps: [
      "email-magic-link",
      "provision-personal-tenant",
      "create-default-workspace",
      "choose-execution-target",
      "apply-security-defaults",
    ],
    requiresConfigFile: false,
    authMethods: ["email_magic_link", "oauth_google", "oauth_microsoft", "oauth_github"],
  };
}
