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
export { DesktopBridge, MAX_FILE_BYTES, MAX_PREVIEW_FETCH_BYTES, registerBridgeHandlers, suggestedFilename } from "./main/ipc.js";
export type { ActiveRunProjection, DesktopBridgeOptions, DesktopFiles, IpcMainLike, PushSender, Settled } from "./main/ipc.js";
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
export {
  ARTIFACT_KINDS,
  MAX_DIFF_LINES,
  artifactWorkspaceReducer,
  comparedVersion,
  decodeArtifacts,
  decodeEvidence,
  decodeHistory,
  diffApplicable,
  diffLines,
  initialArtifactWorkspace,
  selectedVersion,
} from "./artifacts/workspace.js";
export type {
  ArtifactHistory,
  ArtifactKind,
  ArtifactRow,
  ArtifactVersion,
  ArtifactWorkspaceAction,
  ArtifactWorkspaceState,
  DiffLine,
  EvidenceLink,
  LoadedPreview,
  ProducedBy,
  WorkspaceFilter,
} from "./artifacts/workspace.js";
export {
  MAX_IMAGE_PREVIEW_BYTES,
  MAX_TEXT_PREVIEW_BYTES,
  MAX_TEXT_PREVIEW_CHARS,
  imageDataUrl,
  isActiveContent,
  isExecutable,
  isTextLike,
  mediaKind,
  openAllowed,
  previewExecutesWithHostPrivileges,
  previewIsSafe,
  previewPlan,
  textPreview,
} from "./artifacts/preview.js";
export type { PreviewMode, PreviewPlan } from "./artifacts/preview.js";
export { MAX_QUERY_CHARS as MAX_SEARCH_QUERY_CHARS, MAX_RESULTS as MAX_SEARCH_RESULTS, SEARCH_CHANNELS, decodeSearch, searchQueryString, searchRequest } from "./artifacts/search.js";
export type { SearchChannel, SearchHit, SearchRequest, SearchResults } from "./artifacts/search.js";
export {
  DEFAULT_CONTRACT,
  MAX_TITLE_CHARS,
  OBJECTIVE_COMMAND,
  PROPOSE_COMMAND,
  childrenOf,
  decodeWorkGraph,
  describeIntent,
  handoffsFor,
  initialWorkState,
  matchesCanonical,
  needsTwoProposals,
  newDraft,
  prerequisitesOf,
  proposal,
  rebase,
  resolveLocalIds,
  withIntent,
  withoutIntent,
  workReducer,
} from "./work/monitor.js";
export type {
  AgentView,
  BlockerKind,
  BlockerView,
  CompletionContract,
  EdgeKind,
  EditDraft,
  EditIntent,
  HandoffView,
  NodeKind,
  NodeStatus,
  ProposalParams,
  RebaseOutcome,
  RunRef,
  WorkAction,
  WorkEdgeView,
  WorkGraphView,
  WorkNodeView,
  WorkState,
} from "./work/monitor.js";
export { bindApproval, decodeApprovals, isDecidable, warnings } from "./trust/approvals.js";
export type { ApprovalEffect, ApprovalPreview, ApprovalStatus, Binding, ConsequencePreview, Receipt, UntrustedOrigin } from "./trust/approvals.js";
export { EFFECT_STATUSES, decodeEffects, isUnsettled, resolution, timeline, unresolved } from "./trust/effects.js";
export type { EffectOutcome, EffectStatus, EffectView, Reconciliation, Resolution, TimelineEntry } from "./trust/effects.js";
export { errorState, evidenceText, sha256Hex, verifiedState } from "./trust/evidence.js";
export type { EvidenceState } from "./trust/evidence.js";
export {
  DELETE_KNOWLEDGE_COMMAND,
  DELETE_MEMORY_COMMAND,
  KNOWLEDGE_DELETE_FROM,
  KNOWLEDGE_STATUSES,
  KNOWLEDGE_SUPERSEDE_FROM,
  MEMORY_DELETE_FROM,
  MEMORY_FIELDS,
  MEMORY_STATUSES,
  SUPERSEDE_KNOWLEDGE_COMMAND,
  decodeKnowledge,
  decodeMemory,
  deleteKnowledge,
  deleteMemory,
  describeProvenance,
  isRecoveryField,
  retrievable,
  successorsFor,
  supersedeKnowledge,
  tombstones,
} from "./knowledge/entries.js";
export type { Control, DecodedMemory, KnowledgeStatus, KnowledgeView, MemoryStatus, MemoryView, Provenance } from "./knowledge/entries.js";
export { DISABLE_COMMAND, INSTALL_COMMAND, PROMOTE_COMMAND, SKILL_LADDER, activation, activeVersions, decodeSkills, disable, install, promotion } from "./knowledge/skills.js";
export type { CapabilityNeed, Disable, Install, InstallInput, Promotion, SkillManifest, SkillStatus, SkillVersionView, SkillView } from "./knowledge/skills.js";
export { LAYERS, decodeCapabilityResolution, isCurrent, layersInOrder, skillInputs } from "./knowledge/capabilities.js";
export type { CapabilityResolution, GrantView, Layer, LayerInput, LayerRow } from "./knowledge/capabilities.js";
export {
  ARCHIVE_COMMAND,
  AVAILABILITIES,
  CREATE_COMMAND,
  MAX_INSTRUCTIONS_CHARS,
  MAX_PERSONA_CHARS,
  MEMORY_SCOPES,
  UPDATE_COMMAND,
  archiveInRoster,
  archiveTeammate,
  createTeammate,
  decodeRoster,
  decodeTeammate,
  narrowing,
  needsAttention as teammatesNeedingAttention,
  updateTeammate,
  visibleRoster,
} from "./teammates/roster.js";
export type { Availability, CreateInput, EffectiveCapability, MemoryScope, Narrowing, Roster, TeammateStatus, TeammateView, TemplateView, UpdateInput } from "./teammates/roster.js";
