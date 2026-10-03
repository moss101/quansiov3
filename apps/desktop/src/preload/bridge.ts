/**
 * Secure preload bridge (APP-003).
 *
 * The renderer may call only these names. There is no `fs`, `child_process`, `net` or
 * `eval` channel. Main implements the other side against the Rust server (`main/ipc.ts`).
 * `preload/index.ts` is the script Electron loads; it exposes exactly `createPreloadApi`
 * on `window.quansio` through `contextBridge`.
 */

/** Channels the renderer may invoke (renderer → main, request/response). */
export const PRELOAD_ALLOWLIST = [
  "commands.invoke",
  "stream.subscribe",
  "stream.reconnect",
  "session.activeRun",
  "session.state",
  "session.me",
  "auth.magicLink.start",
  "auth.magicLink.complete",
  "auth.oauth.start",
  "auth.signOut",
  "settings.updateWorkspace",
  "settings.updateUser",
  "deepLink.open",
  "update.status",
  "threads.list",
  "threads.messages",
  "questions.list",
  "runs.timeline",
  "messages.post",
  "questions.answer",
  "turns.cancel",
  "artifacts.upload",
  "artifacts.list",
  "artifacts.versions",
  "artifacts.content",
  "artifacts.save",
  "artifacts.open",
  "evidence.get",
  "search.query",
  "work.graph",
  "work.propose",
  "work.objective",
  "approvals.list",
  "approvals.approve",
  "approvals.deny",
  "effects.list",
  "evidence.content",
  "knowledge.list",
  "knowledge.delete",
  "knowledge.supersede",
  "memory.list",
  "memory.delete",
  "skills.list",
  "skills.install",
  "skills.promote",
  "skills.disable",
  "runs.capabilities",
  "teammates.list",
  "teammates.create",
  "teammates.update",
  "teammates.archive",
  "live.targets",
  "live.session",
  "live.takeover",
  "live.handback",
  "live.attach",
  "computer.permissions",
  "computer.openSettings",
  "live.detach",
  "threads.view",
  "threads.create",
  "participants.add",
  "participants.remove",
  "messages.react",
  "threads.handoff",
  "routines.list",
  "routines.command",
  "notifications.list",
  "notifications.read",
  "notifications.preference",
  "diagnostics.export",
  "appearance.set",
] as const;

export type PreloadChannel = (typeof PRELOAD_ALLOWLIST)[number];

/** Channels main may push on (main → renderer, fire and forget). */
export const PRELOAD_PUSH_CHANNELS = [
  "stream.frame",
  "stream.state",
  "stream.error",
  "session.state",
  "deepLink.intent",
  "update.status",
  "live.frame",
] as const;

export type PushChannel = (typeof PRELOAD_PUSH_CHANNELS)[number];

/** A command envelope sent through the bridge to the server. */
export interface BridgeCommand {
  readonly name: string;
  readonly commandId: string;
  readonly params: Record<string, unknown>;
}

/** Result of asking whether a renderer request is allowed. */
export function preloadAllows(channel: string): boolean {
  return (PRELOAD_ALLOWLIST as readonly string[]).includes(channel);
}

/** True when main may push on `channel`. */
export function pushAllows(channel: string): boolean {
  return (PRELOAD_PUSH_CHANNELS as readonly string[]).includes(channel);
}

/** Host APIs the renderer must never receive. */
export const FORBIDDEN_RENDERER_APIS = [
  "fs",
  "child_process",
  "net",
  "os",
  "process.binding",
  "eval",
  "Function",
] as const;

/** True when a candidate renderer API name is forbidden. */
export function isForbiddenRendererApi(name: string): boolean {
  return (FORBIDDEN_RENDERER_APIS as readonly string[]).includes(name);
}

export type BridgeSend = (channel: PreloadChannel, payload: unknown) => Promise<unknown>;
export type BridgeSubscribe = (channel: PushChannel, listener: (payload: unknown) => void) => () => void;

/**
 * The object exposed on `window.quansio`. Invoke keys map one-to-one onto the allowlist
 * and subscription keys onto the push channels; `send`/`subscribe` are the IPC wrappers
 * supplied by the preload script. Tests freeze this shape.
 */
export function createPreloadApi(send: BridgeSend, subscribe: BridgeSubscribe = () => () => undefined) {
  return {
    invokeCommand: (command: BridgeCommand) => send("commands.invoke", command),
    subscribeStream: (cursor?: string) => send("stream.subscribe", { cursor }),
    reconnectStream: (cursor: string) => send("stream.reconnect", { cursor }),
    activeRun: () => send("session.activeRun", {}),
    sessionState: () => send("session.state", {}),
    me: () => send("session.me", {}),
    startMagicLink: (email: string, displayName: string) => send("auth.magicLink.start", { email, displayName }),
    completeMagicLink: (token: string) => send("auth.magicLink.complete", { token }),
    startOAuth: (provider: string) => send("auth.oauth.start", { provider }),
    signOut: () => send("auth.signOut", {}),
    updateWorkspace: (workspaceId: string, changes: { name?: string; settings?: Record<string, unknown> }) =>
      send("settings.updateWorkspace", { workspaceId, ...changes }),
    updateUserSettings: (workspaceId: string, settings: Record<string, unknown>) =>
      send("settings.updateUser", { workspaceId, settings }),
    openDeepLink: (url: string) => send("deepLink.open", { url }),
    updateStatus: () => send("update.status", {}),
    // The conversation surface (APP-004): projections and commands, never bytes to disk.
    listThreads: () => send("threads.list", {}),
    threadMessages: (threadId: string) => send("threads.messages", { threadId }),
    listQuestions: (scope: { threadId: string } | { runId: string }) => send("questions.list", scope),
    runTimeline: (runId: string) => send("runs.timeline", { runId }),
    postMessage: (message: {
      threadId?: string | null;
      content: string;
      attachments?: readonly string[];
      teammateId?: string | null;
      mentions?: readonly { kind: "user" | "teammate"; id: string }[];
    }) => send("messages.post", message),
    answerQuestion: (questionId: string, value: unknown) => send("questions.answer", { questionId, value }),
    cancelTurn: (runId: string, generation?: number) => send("turns.cancel", { runId, generation }),
    uploadAttachment: (file: { name: string; mediaType: string; bytesBase64: string; threadId?: string | null; artifactId?: string | null }) =>
      send("artifacts.upload", file),
    // The artifact workspace (APP-008): projections, bounded bytes for an in-process
    // preview, and file operations main performs only after the user asked.
    listArtifacts: (filter: { kind?: string | null; runId?: string | null } = {}) => send("artifacts.list", filter),
    artifactVersions: (artifactId: string) => send("artifacts.versions", { artifactId }),
    artifactContent: (artifactId: string, versionId: string) => send("artifacts.content", { artifactId, versionId }),
    saveArtifact: (artifactId: string, versionId: string) => send("artifacts.save", { artifactId, versionId }),
    openArtifact: (artifactId: string, versionId: string) => send("artifacts.open", { artifactId, versionId }),
    getEvidence: (evidenceId: string) => send("evidence.get", { evidenceId }),
    search: (request: { q: string; channels?: readonly string[]; maxResults?: number }) => send("search.query", request),
    // The plan / WorkGraph surface (APP-005): the projection and the two validated edits.
    workGraph: () => send("work.graph", {}),
    proposeGraphEdit: (proposal: Record<string, unknown>) => send("work.propose", proposal),
    createObjective: (objective: { title: string; description?: string | null; completionContract?: Record<string, unknown> | null }) =>
      send("work.objective", objective),
    // Approvals, effects and evidence (APP-006): a decision carries the previewed digest.
    listApprovals: (scope: { runId?: string; status?: string } = {}) => send("approvals.list", scope),
    approveEffect: (approvalRequestId: string, paramsDigest: string) => send("approvals.approve", { approvalRequestId, paramsDigest }),
    denyEffect: (approvalRequestId: string) => send("approvals.deny", { approvalRequestId }),
    listEffects: (scope: { runId?: string; status?: string } = {}) => send("effects.list", scope),
    evidenceContent: (evidenceId: string) => send("evidence.content", { evidenceId }),
    // Knowledge, memory, skills and capability resolution (APP-011): projections and
    // lifecycle controls; every mutation is one §14 command the server validates again.
    listKnowledge: (filter: { status?: string; includeDeleted?: boolean } = {}) => send("knowledge.list", filter),
    deleteKnowledge: (knowledgeId: string) => send("knowledge.delete", { knowledgeId }),
    supersedeKnowledge: (knowledgeId: string, supersededBy: string) => send("knowledge.supersede", { knowledgeId, supersededBy }),
    listMemory: (filter: { scope?: string; subjectRef?: string; includeDeleted?: boolean } = {}) => send("memory.list", filter),
    deleteMemory: (memoryId: string) => send("memory.delete", { memoryId }),
    listSkills: () => send("skills.list", {}),
    installSkill: (params: Record<string, unknown>) => send("skills.install", params),
    promoteSkillVersion: (skillVersionId: string, to: string) => send("skills.promote", { skillVersionId, to }),
    disableSkill: (skillId: string) => send("skills.disable", { skillId }),
    runCapabilities: (runId: string) => send("runs.capabilities", { runId }),
    // The teammate roster (APP-014): the projection and the three definition commands.
    listTeammates: () => send("teammates.list", {}),
    createTeammate: (params: Record<string, unknown>) => send("teammates.create", params),
    updateTeammate: (params: Record<string, unknown>) => send("teammates.update", params),
    archiveTeammate: (teammateId: string) => send("teammates.archive", { teammateId }),
    // The live browser/computer/terminal surface (APP-007): the target list, one session's
    // live view, the two control commands (a refusal is data), and the target-scoped live
    // subscription whose frames arrive on `live.frame`.
    listTargets: () => send("live.targets", {}),
    liveSession: (sessionId: string) => send("live.session", { sessionId }),
    requestTakeover: (subject: { sessionId: string } | { targetId: string }) => send("live.takeover", subject),
    handback: (subject: { sessionId: string } | { targetId: string }) => send("live.handback", subject),
    attachLive: (targetId: string) => send("live.attach", { targetId }),
    // Native computer permissions (ADP-002): the status the guide polls, and the one click that opens a
    // Privacy pane (a permission name, never a URL).
    computerPermissions: (request: { needs?: readonly string[]; pauseInput?: boolean } = {}) => send("computer.permissions", request),
    openComputerSettings: (permission: string) => send("computer.openSettings", { permission }),
    detachLive: () => send("live.detach", {}),
    // Collaboration (APP-009): the thread's membership and attention, and the commands that
    // change membership, react and hand a thread to another teammate (refusals are data).
    viewThread: (threadId: string) => send("threads.view", { threadId }),
    createThread: (params: Record<string, unknown>) => send("threads.create", params),
    addParticipant: (params: Record<string, unknown>) => send("participants.add", params),
    removeParticipant: (params: Record<string, unknown>) => send("participants.remove", params),
    react: (params: { message_id: string; emoji: string; remove: boolean }) => send("messages.react", params),
    handOffThread: (params: Record<string, unknown>) => send("threads.handoff", params),
    // Routines and notifications (APP-010): the projections, one routine command (create,
    // update, pause, resume, delete, trigger now), and the person's own notifications.
    listRoutines: () => send("routines.list", {}),
    routineCommand: (name: string, params: Record<string, unknown>) => send("routines.command", { name, params }),
    listNotifications: (unreadOnly = false) => send("notifications.list", { unreadOnly }),
    markNotificationRead: (notificationId: string) => send("notifications.read", { notificationId }),
    setNotificationPreference: (params: Record<string, unknown>) => send("notifications.preference", params),
    // Diagnostics (OPS-003): the server's tenant bundle merged with the desktop's own frames,
    // redacted and bounded; the page offers it for saving.
    exportDiagnostics: () => send("diagnostics.export", {}),
    // The person's theme preference (system / light / dark) for the window's native chrome.
    setAppearance: (theme: string) => send("appearance.set", { theme }),
    onStreamFrame: (listener: (payload: unknown) => void) => subscribe("stream.frame", listener),
    onStreamState: (listener: (payload: unknown) => void) => subscribe("stream.state", listener),
    onStreamError: (listener: (payload: unknown) => void) => subscribe("stream.error", listener),
    onSessionState: (listener: (payload: unknown) => void) => subscribe("session.state", listener),
    onDeepLink: (listener: (payload: unknown) => void) => subscribe("deepLink.intent", listener),
    onUpdateStatus: (listener: (payload: unknown) => void) => subscribe("update.status", listener),
    onLiveFrame: (listener: (payload: unknown) => void) => subscribe("live.frame", listener),
  };
}

export type PreloadApi = ReturnType<typeof createPreloadApi>;
