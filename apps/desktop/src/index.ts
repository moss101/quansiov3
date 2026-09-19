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
  RENDERER_ENTRY_URL,
  RENDERER_WEB_PREFERENCES,
  navigationAllowed,
  permissionAllowed,
  permissionRequestDecision,
  preloadPath,
  rendererSecurityBaselineHolds,
  windowOpenDecision,
  withContentSecurityPolicy,
} from "./main/security.js";
export { createDesktopShell } from "./main/shell.js";
export type { DesktopShell, ShellConfig } from "./main/shell.js";
export { DesktopBridge, registerBridgeHandlers } from "./main/ipc.js";
export type { ActiveRunProjection, DesktopBridgeOptions, IpcMainLike, PushSender } from "./main/ipc.js";
export { SessionFile, parseSession, serializeSession } from "./main/session-file.js";
export type { ActiveRunSession, DesktopSession } from "./main/session-file.js";
export { DEEP_LINK_SCHEME, deepLinkFromArgv, parseDeepLink } from "./main/deeplinks.js";
export type { DeepLinkIntent } from "./main/deeplinks.js";
export { UpdateHooks, parseUpdateChannel, updateFeedUrl } from "./main/updates.js";
export type { AutoUpdaterLike, UpdateChannel, UpdateStatus } from "./main/updates.js";
export {
  FORBIDDEN_RENDERER_APIS,
  PRELOAD_ALLOWLIST,
  PRELOAD_PUSH_CHANNELS,
  createPreloadApi,
  isForbiddenRendererApi,
  preloadAllows,
  pushAllows,
} from "./preload/bridge.js";
export type { BridgeCommand, PreloadApi, PreloadChannel, PushChannel } from "./preload/bridge.js";
export { ApiError, ServerClient, assertCommandName, commandUrl, scopeHeaders, streamUrl } from "./client/server.js";
export type { CommandResponse, FetchLike, HealthReport, PostMessageResult, SessionScope } from "./client/server.js";
export { StreamClient, isErrorFrame, parseStreamFrame, reconnectDelayMs, subscribeRequest } from "./client/stream.js";
export type { EventFrame, LiveFrame, SocketFactory, SocketLike, StreamErrorFrame, StreamFrame, StreamState } from "./client/stream.js";
export { EXECUTION_COMMANDS, LocalRuntimeClient, offeredSubstrates } from "./client/local-runtime.js";
export type { LocalSubstrate, ProvisionTargetParams, TargetClass, TargetProjection } from "./client/local-runtime.js";
export { STRINGS, t } from "./i18n/strings.js";
export type { StringKey } from "./i18n/strings.js";
export { onboardingPlan } from "./onboarding/flow.js";
export type { OnboardingInput, OnboardingPlan } from "./onboarding/flow.js";
export {
  ANSWER_COMMAND,
  CANCEL_COMMAND,
  MAX_UPLOAD_BYTES,
  POST_COMMAND,
  UPLOAD_COMMAND,
  answerParams,
  attachmentFromUpload,
  cancelParams,
  postParams,
  postedIdentities,
  uploadParams,
} from "./conversation/composer.js";
export type { UploadInput } from "./conversation/composer.js";
export {
  cancellableRun,
  conversationReducer,
  decodeMessages,
  decodeQuestions,
  initialConversation,
  openQuestions,
  ownsRun,
} from "./conversation/state.js";
export type {
  ActivityCard,
  AuthorKind,
  ComposerAttachment,
  ComposerState,
  ConversationAction,
  ConversationMessage,
  ConversationQuestion,
  ConversationState,
  QuestionStatus,
  RunActivity,
} from "./conversation/state.js";
export { decodeTimeline } from "./conversation/inspector.js";
export type { InspectedQuestion, InspectedSource, InspectedTool, InspectedTurn, RunInspection } from "./conversation/inspector.js";
