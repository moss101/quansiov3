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
  MEDIA_RESPONSE_CSP,
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
export { DesktopBridge, MAX_FILE_BYTES, MAX_PREVIEW_FETCH_BYTES, settled, suggestedFilename } from "./main/ipc.js";
export type { ActiveRunProjection, DesktopBridgeOptions, DesktopFiles, IpcMainLike, PushSender, Settled } from "./main/ipc.js";
export { SessionFile, parseSession, serializeSession } from "./main/session-file.js";
export type { ActiveRunSession, DesktopSession } from "./main/session-file.js";
export {
  PAUSE_IDLE_SECONDS,
  PERMISSION_NAMES,
  PERMISSION_POLL_MS,
  SETTINGS_PANES,
  UNPROBED_PERMISSIONS,
  buildSnapshot,
  guideAction,
  mapMediaAccess,
  parseOpenSettings,
  parsePermissionRequest,
  parsePermissionStatus,
  readSnapshot,
  settingsUrl,
} from "./main/permissions.js";
export type { PermissionSource, PermissionStatus } from "./main/permissions.js";
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
  postedRunStatus,
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
  editableAsText,
  formatCheckLabel,
  initialArtifactWorkspace,
  selectedVersion,
} from "./artifacts/workspace.js";
export {
  DECK_PAGES_MEDIA_TYPE,
  TIMELINE_MEDIA_TYPE,
  TRANSCRIPT_MEDIA_TYPE,
  clockTime,
  decodeQualityRecord,
  deckPagesRole,
  isUntrusted,
  orderedChecks,
  qualityLabel,
  summarizeOutput,
} from "./artifacts/outputs.js";
export type {
  DeckPageSummary,
  OutputSummary,
  PageState,
  QualityCheck,
  QualityInput,
  QualityRecord,
  TimelineSummary,
  TranscriptSummary,
} from "./artifacts/outputs.js";
export type {
  ArtifactHistory,
  ArtifactKind,
  ArtifactRow,
  ArtifactVersion,
  ArtifactWorkspaceAction,
  ArtifactWorkspaceState,
  DiffLine,
  EvidenceLink,
  FormatCheck,
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
export {
  CANCEL_COMMAND as CANCEL_RECORDING_COMMAND,
  COMPILE_COMMAND as COMPILE_RECORDING_COMMAND,
  FINISH_COMMAND as FINISH_RECORDING_COMMAND,
  IDLE_RECORDER,
  RECORDING_COMMANDS,
  REDACTION_CATEGORIES,
  START_COMMAND as START_RECORDING_COMMAND,
  SUBMIT_COMMAND as SUBMIT_RECORDING_COMMAND,
  canBeConstant,
  canCompile,
  canFinish,
  decodeProposal,
  decodeRecorder,
  decodeRecording,
  decodeRecordings,
  decodeRedaction,
  finishRequest,
  isCapturing,
  submission,
  undecided,
} from "./recording/state.js";
export type { Decisions, ProposalView, RecorderState, RecorderView, RecordingStatus, RecordingView, RedactionCategory, RedactionSummary, SlotClassification, SlotView, StepView } from "./recording/state.js";
export { RecorderChannel, RecordingHost } from "./main/recorder.js";
export type { Outcome as RecorderOutcome, Refusal as RecorderRefusal, RecorderHostOptions, RecorderIdentity, RecordingServerPort } from "./main/recorder.js";
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
export {
  agentInputAllowed,
  applyEvent as applyLiveEvent,
  attentionNeeded,
  computerCommand,
  decodeComputerControl,
  decodeGuard,
  decodeLiveSession,
  decodeTargets,
  decodeTerminal,
  handback,
  handbackAllowed,
  isSameSession,
  pauseReasonOf,
  requestTakeover,
  takeoverAllowed,
} from "./live/session.js";
export type {
  Attention,
  AttentionKind,
  ComputerControlView,
  Controller,
  GuardCertificateView,
  GuardDialogView,
  GuardDownloadView,
  GuardTabView,
  GuardView,
  InDoubtView,
  LiveSession,
  PauseReason,
  RunView,
  SessionStatus,
  Tab,
  TargetView,
  TerminalSessionView,
} from "./live/session.js";
export {
  PermissionPoller,
  attentionFromEvent,
  decodePermissionSnapshot,
  guideView,
  parseComputerAttention,
  requestFor,
  resumeCommand,
} from "./live/permissions.js";
export type { ComputerAttention, GuideView, PermissionName, PermissionRow, PermissionSnapshot, PermissionState } from "./live/permissions.js";
export { TerminalReplay, decodeTerminalBytes, replayPosition } from "./live/terminal.js";
export type { Accepted, ReplayPosition, TerminalBytesFrame } from "./live/terminal.js";
export { decodeBrowserFrame, latestFrame } from "./live/frames.js";
export type { AgentCursor, BrowserFrame, CursorAction } from "./live/frames.js";
export {
  activeParticipants,
  applyThreadEvent,
  attentionFor,
  createThreadParams,
  decodeThreadList,
  decodeThreadMessages,
  decodeThreadView,
  handOffParams,
  handoffTargets,
  hasGap,
  mayLeave,
  mayManage,
  mentionCandidates,
  mentionsIn,
  mergeMessages,
  participantParams,
  reactionToggle,
  summarizeReactions,
  viewerFrom,
} from "./collaboration/thread.js";
export type { AttentionView, Mention, Mentionable, ParticipantView, Reaction, ReactionSummary, ThreadListEntry, ThreadMessage, ThreadState, ThreadView } from "./collaboration/thread.js";
export { createRoutineParams, decodeRoutines, draftProblem, routineControls, summarizeFires } from "./automation/routines.js";
export type { AbsencePolicy, FireSummary, FireView, RoutineControl, RoutineDraft, RoutineStatus, RoutineTrigger, RoutineView, TriggerKind } from "./automation/routines.js";
export {
  NOTIFICATION_CHANNELS,
  NOTIFICATION_KINDS,
  applyNotificationEvent,
  decodeNotifications,
  desktopNotice,
  inAppItems,
  preferenceParams,
  toggleChannel,
} from "./automation/notifications.js";
export type { Channel, DesktopNotice, NotificationCenter, NotificationKind, NotificationView, Preference } from "./automation/notifications.js";

// The desktop shell (ADP-004).
export { SchemaError, safeParse } from "./main/schema.js";
export * as schema from "./main/schema.js";
export { loadShellConfig, parseFlatYaml, parseShellConfig, shellConfigFrom, NUMERIC_BOUNDS } from "./main/shell-config.js";
export type { DesktopShellConfig } from "./main/shell-config.js";
export { KEY_NAMES, bindingFromKeyEvent, bindingProblem, displayBinding, parseBinding, toAccelerator } from "./shortcut-grammar.js";
export type { Binding as ShortcutBinding, BindingProblem } from "./shortcut-grammar.js";
export { FIXED_BINDINGS, GLOBAL_SHORTCUTS, ShortcutRegistry, WINDOW_DEFAULTS, WINDOW_SHORTCUTS } from "./main/shortcuts.js";
export type { GlobalShortcutAdapter, SetResult, ShortcutSnapshot, ShortcutView } from "./main/shortcuts.js";
export { DEFAULT_PREFERENCES, PreferencesFile, parsePreferences } from "./main/preferences.js";
export type { ShellPreferences } from "./main/preferences.js";
export { KeepAwake, shouldHold } from "./main/keep-awake.js";
export { ConnectivityMonitor } from "./main/connectivity.js";
export { QuitGate } from "./main/quit.js";
export { UpdateGate } from "./main/update-gate.js";
export { describeWork, decodeMaintenance, workStatus } from "./main/work-status.js";
export type { MaintenanceStatus, WorkStatus } from "./main/work-status.js";
export { ActivityTracker, compareActivity } from "./main/activity.js";
export type { ActivityItem, ActivityKind } from "./main/activity.js";
export { WindowManager, PendingQueue, ManagedWindow, windowSpec, specMeetsBaseline, WINDOW_TYPES, clampOrigin, defaultIndicatorOrigin, indicatorOrigin, indicatorPosition } from "./main/windows.js";
export type { DisplayAdapter, DisplayInfo, WindowFactory, WindowHandle, WindowType, WindowSpec } from "./main/windows.js";
export { IpcGuard, POLICY, registerGuardedHandlers, senderOf } from "./main/ipc-trust.js";
export type { WindowHandler } from "./main/ipc-trust.js";
export { ShellHost, commandForItem, commandForNotice, shortcutResult } from "./main/shell-host.js";
export type { ShellBridge } from "./main/shell-host.js";
export { buildMenu, acceleratorsOf } from "./main/menu.js";
export type { MenuItem, ShellCommand } from "./main/menu.js";
export { closeBehaviour, trayIconPng, trayMenu, wantsTray } from "./main/tray.js";
export { quitChosen, quitDialog, updateChoiceOf, updateDialog } from "./main/dialogs.js";
