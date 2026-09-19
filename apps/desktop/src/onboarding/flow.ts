/**
 * New-user onboarding (APP-002): sign-in to a usable governed workspace with no
 * configuration file. The flow is a pure state machine over the session projection the
 * bridge exposes; the renderer drives it and every transition is a bridge call.
 *
 * Steps: sign in (magic link or OAuth) → the server provisions a personal tenant and a
 * default workspace → choose the execution target → confirm the security defaults → ready.
 * Provider credentials are entered as `sec_` handles only; the desktop never sees a secret.
 */

export type OnboardingStep =
  | "sign-in"
  | "check-inbox"
  | "choose-execution-target"
  | "confirm-defaults"
  | "ready";

export interface OnboardingState {
  readonly step: OnboardingStep;
  readonly email: string | null;
  readonly workspaceId: string | null;
  readonly executionTarget: ExecutionTarget | null;
  readonly error: string | null;
}

export type ExecutionTarget = "local_capsule" | "cloud" | "private_worker";

export const EXECUTION_TARGETS: readonly ExecutionTarget[] = ["local_capsule", "cloud", "private_worker"];

/** The security defaults a fresh workspace confirms; the server's own defaults, restated for the user. */
export const SECURITY_DEFAULTS = {
  telemetry: false,
  clipboard_to_model: false,
  unsigned_updates: false,
  share_analytics: false,
} as const;

export type OnboardingEvent =
  | { readonly kind: "session"; readonly state: SessionProjection }
  | { readonly kind: "link-sent"; readonly email: string }
  | { readonly kind: "target-chosen"; readonly target: ExecutionTarget }
  | { readonly kind: "defaults-confirmed" }
  | { readonly kind: "failed"; readonly message: string }
  | { readonly kind: "signed-out" };

/** The session projection the bridge pushes (`session.state`). */
export type SessionProjection =
  | { readonly kind: "signed-out"; readonly persistent: boolean }
  | { readonly kind: "link-sent"; readonly email: string; readonly expiresAt: string; readonly delivery: string }
  | { readonly kind: "signed-in"; readonly userId: string; readonly tenantId: string; readonly workspaceId: string | null; readonly onboarded: boolean };

export const INITIAL_ONBOARDING: OnboardingState = {
  step: "sign-in",
  email: null,
  workspaceId: null,
  executionTarget: null,
  error: null,
};

/** Pure transition; unknown combinations leave the state unchanged. */
export function reduceOnboarding(state: OnboardingState, event: OnboardingEvent): OnboardingState {
  switch (event.kind) {
    case "session": {
      const session = event.state;
      if (session.kind === "signed-out") {
        return { ...INITIAL_ONBOARDING };
      }
      if (session.kind === "link-sent") {
        return { ...state, step: "check-inbox", email: session.email, error: null };
      }
      // Signed in: a returning user skips straight to ready; a new one chooses a target.
      if (!session.onboarded && state.step !== "choose-execution-target" && state.step !== "confirm-defaults") {
        return { ...state, step: "ready", workspaceId: session.workspaceId, error: null };
      }
      if (state.step === "sign-in" || state.step === "check-inbox") {
        return { ...state, step: "choose-execution-target", workspaceId: session.workspaceId, error: null };
      }
      return { ...state, workspaceId: session.workspaceId, error: null };
    }
    case "link-sent":
      return { ...state, step: "check-inbox", email: event.email, error: null };
    case "target-chosen":
      return state.step === "choose-execution-target"
        ? { ...state, step: "confirm-defaults", executionTarget: event.target, error: null }
        : state;
    case "defaults-confirmed":
      return state.step === "confirm-defaults" ? { ...state, step: "ready", error: null } : state;
    case "failed":
      return { ...state, error: event.message };
    case "signed-out":
      return { ...INITIAL_ONBOARDING };
    default:
      return state;
  }
}

/** The workspace settings the execution-target step writes through `UpdateWorkspace`. */
export function executionTargetSettings(target: ExecutionTarget): Record<string, unknown> {
  return { execution_target: target };
}

/** The settings the defaults step confirms; the merge is most-restrictive, so this can only narrow. */
export function securityDefaultSettings(): Record<string, unknown> {
  return { ...SECURITY_DEFAULTS };
}

/** Which execution targets this host can offer first-class (others still work through cloud). */
export function recommendedExecutionTarget(platform: string): ExecutionTarget {
  return platform === "darwin" || platform === "win32" ? "local_capsule" : "cloud";
}

// ---- kept for the surfaces catalog and older callers ----

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
    steps: ["sign-in", "provision-personal-tenant", "create-default-workspace", "choose-execution-target", "confirm-defaults"],
    requiresConfigFile: false,
    authMethods: ["email_magic_link", "oauth_google", "oauth_microsoft", "oauth_github"],
  };
}
