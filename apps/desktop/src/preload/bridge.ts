/**
 * Secure preload bridge (APP-003).
 *
 * The renderer may call only these names. There is no `fs`, `child_process`, `net` or
 * `eval` channel. Main implements the other side against the Rust server (`main/ipc.ts`).
 * `preload/index.ts` is the script Electron loads; it exposes exactly `createPreloadApi`
 * on `window.quansio` through `contextBridge`.
 */

/** Channels `DesktopBridge` implements (server-facing: commands, projections, session). */
export const BRIDGE_CHANNELS = [
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
  "packs.list",
  "packs.rollback",
  "runs.capabilities",
  "teammates.list",
  "teammates.create",
  "teammates.update",
  "teammates.archive",
  "live.targets",
  "live.session",
  "live.takeover",
  "live.handback",
  "live.certificate",
  "live.permission",
  "live.input",
  "live.attach",
  "computer.permissions",
  "computer.openSettings",
  "recording.status",
  "recording.start",
  "recording.finish",
  "recording.cancel",
  "recording.list",
  "recording.compile",
  "recording.submit",
  "live.detach",
  "payment.supply",
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

/**
 * Channels the desktop shell implements (ADP-004): window context, pending events, connectivity, shortcuts,
 * preferences, secondary windows, the activity list and update installation. Which window type may call which
 * is `main/ipc-trust.ts`'s table, and the shell's handlers validate every payload against a schema.
 */
export const SHELL_CHANNELS = [
  "shell.window.context",
  "shell.pending.consume",
  "shell.connectivity.report",
  "shell.shortcuts.get",
  "shell.shortcuts.set",
  "shell.shortcuts.reset",
  "shell.recording.begin",
  "shell.recording.end",
  "shell.preferences.get",
  "shell.preferences.set",
  "shell.popout.open",
  "shell.popout.pin",
  "shell.window.close",
  "shell.quickEntry.submit",
  "shell.activity.list",
  "shell.activity.dismiss",
  "shell.activity.open",
  "shell.remote.answer",
  "shell.indicator.interactive",
  "shell.indicator.moveTo",
  "update.install",
] as const;

/**
 * Channels for the person's local files (ADP-005, DOMAIN.md §13.7). A payload names an opaque id main issued (a grant
 * token, a root, an entry, an edit session, a subscription) and never a path; main validates each against a schema and
 * authorises it per window type (`main/ipc-trust.ts`), and the answers are data: `{ok, result}` or `{ok: false, error}`.
 */
export const FILE_CHANNELS = [
  "files.pick",
  "files.grant.attach",
  "files.grant.discard",
  "files.grant.preview",
  "files.root.pick",
  "files.root.list",
  "files.root.release",
  "files.root.entries",
  "files.media.url",
  "files.edit.open",
  "files.edit.save",
  "files.edit.close",
  "files.watch.subscribe",
  "files.watch.unsubscribe",
] as const;

/**
 * Channels for the person's own secrets (ADP-009, DOMAIN.md §13.9 A, B): the password manager and the import from another browser.
 * Every one is the main window's alone. A payload names an opaque id main issued (a saved login, a profile the scan listed) and
 * never a path; no answer carries a password or a cookie value, and no agent tool corresponds to any of them.
 */
export const SECRET_CHANNELS = [
  "passwords.status",
  "passwords.list",
  "passwords.suggest",
  "passwords.save",
  "passwords.remove",
  "passwords.fill",
  "import.scan",
  "import.logins",
  "import.cookies",
] as const;

/** Channels the renderer may invoke (renderer → main, request/response). */
export const PRELOAD_ALLOWLIST = [...BRIDGE_CHANNELS, ...SHELL_CHANNELS, ...FILE_CHANNELS, ...SECRET_CHANNELS] as const;

export type BridgeChannel = (typeof BRIDGE_CHANNELS)[number];
export type ShellChannel = (typeof SHELL_CHANNELS)[number];
export type FileChannel = (typeof FILE_CHANNELS)[number];
export type SecretChannel = (typeof SECRET_CHANNELS)[number];
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
  // The desktop shell (ADP-004): menu and navigation commands, the activity list, connectivity, shortcuts.
  "shell.command",
  "shell.activity",
  "shell.connectivity",
  "shell.shortcuts",
  "shell.recording.ended",
  // Local files (ADP-005): what changed in a watched folder, and a grant that a deep link asked the shell to open a dialog for.
  "files.changes",
  "files.grant",
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
    // Capability packs (ADP-008): the projection (revisions, install reports) and the one control, a rollback.
    listPacks: () => send("packs.list", {}),
    rollbackPack: (pack: string) => send("packs.rollback", { pack }),
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
    // A person's answer to a certificate error or a camera/microphone request (ADP-001). The server refuses anyone else.
    resolveCertificate: (decision: { sessionId: string; tabId: string; certificateId: string; decision: "allow" | "deny" }) => send("live.certificate", decision),
    decidePermission: (decision: { sessionId: string; tabId: string; origin: string; permission: "camera" | "microphone"; decision: "allow" | "deny" }) => send("live.permission", decision),
    // A person's click or typed text in the browser session they hold (APP-007). The server refuses it unless they hold it.
    sendBrowserInput: (input: { sessionId: string; tabId: string } & ({ kind: "click"; x: number; y: number; button?: "left" | "middle" | "right"; clickCount?: 1 | 2 | 3; modifiers?: readonly string[] } | { kind: "drag"; fromX: number; fromY: number; toX: number; toY: number; modifiers?: readonly string[] } | { kind: "text"; text: string } | { kind: "key"; key: string } | { kind: "scroll"; x: number; y: number; deltaX: number; deltaY: number })) => send("live.input", input),
    attachLive: (targetId: string) => send("live.attach", { targetId }),
    // Native computer permissions (ADP-002): the status the guide polls, and the one click that opens a
    // Privacy pane (a permission name, never a URL).
    computerPermissions: (request: { needs?: readonly string[]; pauseInput?: boolean } = {}) => send("computer.permissions", request),
    openComputerSettings: (permission: string) => send("computer.openSettings", { permission }),
    detachLive: () => send("live.detach", {}),
    // One-time payment values for one checkout (ADP-009, DOMAIN.md 13.9 C): the origin and the fields the person typed. The
    // answer names fields and expiry times and never a value; there is no call that reads one back.
    supplyPayment: (request: { origin: string; fields: Record<string, string> }) => send("payment.supply", request),
    // Skill recording (ADP-003): the recorder's own state, start, the person's finish (a genuine click
    // only; the page passes `gesture`), cancel, the caller's recordings, compile and submit as DRAFT.
    // The renderer never supplies the owner fence: main holds it.
    recordingStatus: () => send("recording.status", {}),
    startRecording: () => send("recording.start", {}),
    finishRecording: (request: { gesture: boolean }) => send("recording.finish", request),
    cancelRecording: () => send("recording.cancel", {}),
    listRecordings: () => send("recording.list", {}),
    compileRecording: (recordingId: string) => send("recording.compile", { recordingId }),
    submitRecordingSkill: (params: { recordingId: string; name: string; semver: string; slots: readonly { slot_id: string; classification: string }[] }) =>
      send("recording.submit", params),
    // Collaboration (APP-009): the thread's membership and attention, and the commands that
    // change membership, react and hand a thread to another teammate (refusals are data).
    viewThread: (threadId: string) => send("threads.view", { threadId }),
    createThread: (params: Record<string, unknown>) => send("threads.create", params),
    addParticipant: (params: Record<string, unknown>) => send("participants.add", params),
    removeParticipant: (params: Record<string, unknown>) => send("participants.remove", params),
    react: (params: { message_id: string; emoji: string; remove: boolean }) => send("messages.react", params),
    handOffThread: (params: Record<string, unknown>) => send("threads.handoff", params),
    // Routines and notifications (APP-010, ADP-007): the projections, one routine command (create,
    // update, pause, resume, delete, trigger now, dry run, rotate an inbound hook's secret), and the
    // person's own notifications.
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
    // The desktop shell (ADP-004). The page learns what kind of window it is from main (never from its URL),
    // pulls what was queued before it subscribed, and reports connectivity; everything else is a request main
    // validates and may refuse for this window type.
    windowContext: () => send("shell.window.context", {}),
    consumePending: () => send("shell.pending.consume", {}),
    reportConnectivity: (online: boolean) => send("shell.connectivity.report", { online }),
    shortcuts: () => send("shell.shortcuts.get", {}),
    setShortcut: (id: string, binding: string | null) => send("shell.shortcuts.set", { id, binding }),
    resetShortcut: (id: string) => send("shell.shortcuts.reset", { id }),
    beginShortcutRecording: () => send("shell.recording.begin", {}),
    endShortcutRecording: () => send("shell.recording.end", {}),
    shellPreferences: () => send("shell.preferences.get", {}),
    setShellPreferences: (changes: { keepAwake?: boolean; installWhenIdle?: boolean }) => send("shell.preferences.set", changes),
    openPopout: (request: { threadId: string; tab?: string; sidebar?: boolean; x?: number; y?: number }) => send("shell.popout.open", request),
    pinPopout: (pinned: boolean) => send("shell.popout.pin", { pinned }),
    closeWindow: () => send("shell.window.close", {}),
    quickEntrySubmit: (request: { content: string; teammateId?: string | null }) => send("shell.quickEntry.submit", request),
    activityItems: () => send("shell.activity.list", {}),
    dismissActivity: (key: string) => send("shell.activity.dismiss", { key }),
    openActivity: (key: string) => send("shell.activity.open", { key }),
    // The computer's owner answers a remote-control request, or stops a standing session (ADP-009, DOMAIN.md 8.6.2). The page names
    // the activity item and the answer; main reads the computer and the viewer from the item, never from the page.
    answerRemote: (key: string, answer: "view" | "control" | "deny" | "stop") => send("shell.remote.answer", { key, answer }),
    setIndicatorInteractive: (active: boolean) => send("shell.indicator.interactive", { active }),
    moveIndicator: (x: number, y: number) => send("shell.indicator.moveTo", { x, y }),
    installUpdate: (mode: "now" | "when_idle" | "cancel_when_idle") => send("update.install", { mode }),
    onShellCommand: (listener: (payload: unknown) => void) => subscribe("shell.command", listener),
    onActivity: (listener: (payload: unknown) => void) => subscribe("shell.activity", listener),
    onConnectivity: (listener: (payload: unknown) => void) => subscribe("shell.connectivity", listener),
    onShortcuts: (listener: (payload: unknown) => void) => subscribe("shell.shortcuts", listener),
    onShortcutRecordingEnded: (listener: (payload: unknown) => void) => subscribe("shell.recording.ended", listener),
    // Local files (ADP-005). The page asks main to open a native dialog and is told what was picked as names, sizes and
    // an opaque token; it never names a path. A grant is claimed once, by the window that holds it.
    pickLocalFiles: (request: { threadId?: string | null } = {}) => send("files.pick", request),
    attachLocalFiles: (request: { token: string; threadId?: string | null }) => send("files.grant.attach", request),
    discardLocalFiles: (token: string) => send("files.grant.discard", { token }),
    previewLocalFile: (token: string, index: number) => send("files.grant.preview", { token, index }),
    pickFolder: () => send("files.root.pick", {}),
    listFolders: () => send("files.root.list", {}),
    releaseFolder: (rootId: string) => send("files.root.release", { rootId }),
    folderEntries: (rootId: string, entryId?: string | null) => send("files.root.entries", { rootId, entryId: entryId ?? null }),
    fileMediaUrl: (entryId: string) => send("files.media.url", { entryId }),
    openFileForEdit: (entryId: string) => send("files.edit.open", { entryId }),
    saveEditedFile: (request: { editId: string; text: string; onConflict?: "fail" | "overwrite" }) => send("files.edit.save", { onConflict: "fail", ...request }),
    closeFileEdit: (editId: string) => send("files.edit.close", { editId }),
    watchFolder: (rootId: string, entryId?: string | null) => send("files.watch.subscribe", { rootId, entryId: entryId ?? null }),
    unwatchFolder: (subscriptionId: string) => send("files.watch.unsubscribe", { subscriptionId }),
    // The person's own secrets (ADP-009): the password manager (metadata only; a password is typed in and never read back) and the
    // import from another browser, one type at a time (each asks its own native question in main).
    passwordStatus: () => send("passwords.status", {}),
    listPasswords: () => send("passwords.list", {}),
    suggestPasswords: (pageUrl: string) => send("passwords.suggest", { pageUrl }),
    savePassword: (login: { origin: string; username: string; password: string }) => send("passwords.save", login),
    removePassword: (id: string) => send("passwords.remove", { id }),
    fillPassword: (request: { id: string; sessionId: string }) => send("passwords.fill", request),
    scanBrowsers: () => send("import.scan", {}),
    importLogins: (request: { family: string; profile: string }) => send("import.logins", request),
    stageCookies: (request: { family: string; profile: string; domains: readonly string[] }) => send("import.cookies", request),
    onFileChanges: (listener: (payload: unknown) => void) => subscribe("files.changes", listener),
    onFileGrant: (listener: (payload: unknown) => void) => subscribe("files.grant", listener),
  };
}

export type PreloadApi = ReturnType<typeof createPreloadApi>;
