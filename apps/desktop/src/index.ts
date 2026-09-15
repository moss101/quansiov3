/**
 * Desktop shell (APP-003). Canonical owner: `apps/desktop`.
 *
 * Electron main applies {@link RENDERER_WEB_PREFERENCES} and {@link RENDERER_CSP}.
 * The renderer uses {@link createPreloadApi} only. Session reconnect is
 * {@link DesktopSessionStore}.
 */
export type { SurfaceDescriptor, SurfaceId } from "./surfaces.js";
export { DESKTOP_SURFACES, surface } from "./surfaces.js";
export {
  DENIED_PERMISSIONS,
  RENDERER_CSP,
  RENDERER_WEB_PREFERENCES,
  permissionAllowed,
  rendererSecurityBaselineHolds,
} from "./main/security.js";
export { createDesktopShell } from "./main/shell.js";
export type { DesktopShell, ShellConfig } from "./main/shell.js";
export {
  FORBIDDEN_RENDERER_APIS,
  PRELOAD_ALLOWLIST,
  createPreloadApi,
  isForbiddenRendererApi,
  preloadAllows,
} from "./preload/bridge.js";
export type { BridgeCommand, PreloadChannel } from "./preload/bridge.js";
export { DesktopSessionStore } from "./renderer/session.js";
export type { ActiveRunSession } from "./renderer/session.js";
export { commandUrl, streamUrl, tenantHeaders } from "./client/server.js";
export { STRINGS } from "./i18n/strings.js";
export { onboardingPlan } from "./onboarding/flow.js";
export type { OnboardingInput, OnboardingPlan } from "./onboarding/flow.js";
export {
  ANSWER_COMMAND,
  CANCEL_COMMAND,
  POST_COMMAND,
  answerQuestion,
  attachArtifact,
  bindObjective,
  cancelRun,
  inspectTurn,
  newChatSession,
  postMessage,
  reload,
} from "./conversation/composer.js";
export type {
  AnsweredQuestion,
  ComposerAttachment,
  ConversationSession,
  TurnInspector,
} from "./conversation/composer.js";
