/**
 * Main-process side of the preload bridge (APP-003).
 *
 * Exactly the channels in `PRELOAD_ALLOWLIST` get a handler. Each handler validates the
 * renderer's payload as untrusted data, performs the operation through the typed clients
 * and the durable session, and returns a projection that carries no scope headers, tokens
 * or cursors the renderer could replay on its own. Frames from the stream are pushed to the
 * renderer over `PRELOAD_PUSH_CHANNELS`; main is the only process that opens a socket.
 */

import { LocalRuntimeClient } from "../client/local-runtime.js";
import { ApiError, ServerClient, type CommandResponse, type Credential, type FetchLike, type OAuthProvider, type SessionScope } from "../client/server.js";
import { StreamClient, type EventFrame, type LiveFrame, type SocketFactory, type StreamErrorFrame, type StreamState } from "../client/stream.js";
import { PRELOAD_PUSH_CHANNELS, type BridgeChannel, type BridgeCommand, type PushChannel } from "../preload/bridge.js";
import { newCommandId } from "../client/ids.js";
import { ANSWER_COMMAND, CANCEL_COMMAND, MAX_UPLOAD_BYTES, POST_COMMAND, UPLOAD_COMMAND, answerParams, cancelParams } from "../conversation/composer.js";
import { MAX_IMAGE_PREVIEW_BYTES, MAX_TEXT_PREVIEW_BYTES, isActiveContent, isExecutable, mediaKind, openAllowed, previewPlan } from "../artifacts/preview.js";
import { searchQueryString, searchRequest } from "../artifacts/search.js";
import { OBJECTIVE_COMMAND, PROPOSE_COMMAND } from "../work/monitor.js";
import { WorkerLaunchError, type WorkerLauncher } from "./worker-launcher.js";
import { browserInput as inputCommand, type UserButton, type UserModifier, computerCommand, decidePermission as permissionCommand, handback as handbackCommand, requestTakeover as takeoverCommand, resolveCertificate as certificateCommand } from "../live/session.js";
import { desktopNotice, type DesktopNotice } from "../automation/notifications.js";
import { parseDeepLink, type DeepLinkIntent } from "./deeplinks.js";
import { parseOpenSettings, parsePermissionRequest, readSnapshot, settingsUrl, notApplicable, type PermissionSnapshot, type PermissionSource } from "./permissions.js";
import { RECORDING_COMMANDS } from "../recording/state.js";
import { RecordingHost } from "./recorder.js";
import type { LocalSecretsConfig } from "./secrets/config.js";
import { PaymentSupplier, PaymentSupplyError } from "./secrets/payments.js";
import { SessionFile, type ActiveRunSession, type DesktopSession } from "./session-file.js";
import { SessionManager, type SessionState } from "./session-manager.js";
import type { TokenStore } from "./token-store.js";
import type { UpdateHooks, UpdateStatus } from "./updates.js";
import { decodeMaintenance, type MaintenanceStatus } from "./work-status.js";

/** The slice of `ipcMain` the bridge registers on. */
export interface IpcMainLike {
  handle(channel: string, listener: (event: unknown, payload: unknown) => unknown): void;
}

/** How main pushes to the renderer (`webContents.send`). */
export type PushSender = (channel: PushChannel, payload: unknown) => void;

/** What the renderer may learn about the session: identities and progress, never scope. */
export interface ActiveRunProjection {
  readonly connected: boolean;
  readonly workspaceId: string | null;
  readonly activeRun: ActiveRunSession | null;
  readonly streamState: StreamState;
}

/**
 * How the desktop authenticates: a real session (sign-in through the bridge, refresh token in
 * the keychain) or, for development harnesses only, trusted scope headers with a fixed
 * workspace (`QUANSIO_TRUST_SCOPE_HEADERS=1` on the server).
 */
export type DesktopAuth =
  | { readonly kind: "session"; readonly tokens: TokenStore }
  | { readonly kind: "trusted-headers"; readonly scope: SessionScope; readonly workspaceId: string };

export interface DesktopBridgeOptions {
  readonly serverOrigin: string;
  readonly auth: DesktopAuth;
  readonly session: SessionFile;
  readonly openSocket: SocketFactory;
  readonly updates: UpdateHooks;
  readonly consumer: string;
  readonly push: PushSender;
  /**
   * Sees every durable event frame after the cursor has advanced (ADP-004: the shell derives its activity list
   * and keep-awake from the stream main already holds). It cannot change the frame.
   */
  readonly observe?: (frame: EventFrame) => void;
  /** Opens an OAuth authorization URL in the system browser (main supplies `shell.openExternal`). */
  readonly openExternal?: (url: string) => Promise<void>;
  /**
   * Shows a native desktop notification (APP-010; main supplies Electron's `Notification`).
   * Called for a `notification.created` event that asks for the desktop channel.
   */
  readonly notifyNative?: (notice: DesktopNotice) => void;
  /**
   * Applies the person's theme preference to the window's native chrome (main supplies
   * Electron's `nativeTheme.themeSource`); the renderer applies it to the page itself.
   */
  readonly setAppearance?: (theme: Appearance) => void;
  /** Injectable for tests; defaults to the global `fetch`. */
  readonly fetch?: FetchLike;
  /**
   * File operations main performs for the artifact workspace (APP-008). `saveAs` asks the
   * user where to put a file (Electron's save dialog) and writes it there; `openWithOs`
   * writes a file under the app's own directory and hands it to the OS (`shell.openPath`).
   * Absent both, save and open are refused; the preview never needs them.
   */
  readonly files?: DesktopFiles;
  /** Starts and stops the worker on this computer (EXEC-002); absent, the app offers none. */
  readonly worker?: WorkerLauncher;
  /**
   * What the system says about the OS permissions native computer use needs (ADP-002; main supplies
   * Electron's `systemPreferences` on macOS). Absent, the guide reports that it does not apply.
   */
  readonly computerPermissions?: PermissionSource;
  /**
   * Opens one Privacy pane in System Settings (main supplies `shell.openExternal`). It is reached only
   * through `computer.openSettings`, which takes a permission name and builds the URL itself, and only
   * a person's click in the page calls that; no agent output reaches it.
   */
  readonly openSystemSettings?: (url: string) => Promise<void>;
  /**
   * The `quansio-recorder` executable (ADP-003), when this build ships one. Absent, `recording.start` answers
   * `RECORDER_UNAVAILABLE`; nothing else about the desktop changes.
   */
  readonly recorderBinary?: string | null;
  /**
   * The local-secrets limits (ADP-009, `config/local-secrets.yaml`). Absent, `payment.supply` answers `POLICY_DENIED`
   * and the payment form is the only thing that does not work.
   */
  readonly localSecrets?: LocalSecretsConfig;
}

/** The slice of the host's file/shell surface the bridge uses; main supplies the real one. */
export interface DesktopFiles {
  readonly saveAs: (suggestedName: string, bytes: Uint8Array) => Promise<string | null>;
  readonly openWithOs: (suggestedName: string, bytes: Uint8Array, mediaType: string) => Promise<string>;
}

/** Largest artifact main fetches for the renderer's preview. */
export const MAX_PREVIEW_FETCH_BYTES = Math.max(MAX_TEXT_PREVIEW_BYTES, MAX_IMAGE_PREVIEW_BYTES);
/** Largest artifact main saves or opens on the user's behalf. */
export const MAX_FILE_BYTES = 256 * 1024 * 1024;

export class DesktopBridge {
  readonly server: ServerClient;
  readonly localRuntime: LocalRuntimeClient;
  /** Present in session mode; the trusted-header harness has none. */
  readonly sessions: SessionManager | null;
  /** The person's recorder (ADP-003): started, finished and cancelled only through the recording channels. */
  readonly recording: RecordingHost;
  /** The person's one-time payment values (ADP-009): supplied only through `payment.supply`, never the generic command path. */
  readonly payments: PaymentSupplier;
  readonly #options: DesktopBridgeOptions;
  #stream: StreamClient | null = null;
  #accountKey: string | null = null;
  #streamState: StreamState = "closed";
  /** The target-scoped live subscription (APP-007): cursor-less, frames only, one at a time. */
  #live: { readonly targetId: string; readonly client: StreamClient } | null = null;

  constructor(options: DesktopBridgeOptions) {
    this.#options = options;
    const client = (credential: Credential): ServerClient =>
      new ServerClient(
        options.fetch
          ? { origin: options.serverOrigin, credential, fetch: options.fetch }
          : { origin: options.serverOrigin, credential },
      );
    if (options.auth.kind === "session") {
      this.sessions = new SessionManager({
        serverOrigin: options.serverOrigin,
        tokens: options.auth.tokens,
        client,
        onState: (state) => {
          this.#onSessionState(state);
        },
      });
      this.server = this.sessions.client;
    } else {
      this.sessions = null;
      this.server = client({ kind: "trusted-headers", scope: options.auth.scope });
      this.#ensureSessionFile(options.auth.workspaceId);
    }
    this.localRuntime = new LocalRuntimeClient(this.server);
    this.recording = new RecordingHost({
      server: this.server,
      binary: options.recorderBinary ?? null,
      identity: () => this.#identity(),
    });
    this.payments = new PaymentSupplier({
      config: options.localSecrets ?? null,
      server: this.server,
      workspaceId: () => this.#requireWorkspace(),
      newCommandId,
    });
  }

  /** Who is signed in, for the recorder's owner fence. */
  #identity(): { readonly tenantId: string; readonly userId: string } | null {
    if (this.#options.auth.kind === "trusted-headers") {
      return { tenantId: this.#options.auth.scope.tenantId, userId: this.#options.auth.scope.userId };
    }
    const state = this.sessions?.state();
    return state?.kind === "signed-in" ? { tenantId: state.tenantId, userId: state.userId } : null;
  }

  /** The signed-in person, once known (the activity list shows a remote session to the computer's owner). */
  userId(): string | null {
    return this.#identity()?.userId ?? null;
  }

  /**
   * The computer's owner answers a remote-control request or stops a session (ADP-009, DOMAIN.md 8.6.2). Only these three commands
   * pass; the server decides who may.
   */
  async remoteControl(name: "GrantRemoteControl" | "DenyRemoteControl" | "StopRemoteControl", params: Record<string, unknown>): Promise<unknown> {
    return (await this.server.invoke(name, newCommandId(), params)).result;
  }

  /** Resume a stored session from the keychain (session mode); false when there is none. */
  async resume(): Promise<boolean> {
    if (!this.sessions) {
      return true;
    }
    return this.sessions.resume();
  }

  /** The workspace the desktop acts in, once known. */
  workspaceId(): string | null {
    if (this.#options.auth.kind === "trusted-headers") {
      return this.#options.auth.workspaceId;
    }
    const state = this.sessions?.state();
    return state?.kind === "signed-in" ? state.workspaceId : null;
  }

  #ensureSessionFile(workspaceId: string): void {
    const current = this.#options.session.current();
    if (!current || current.serverOrigin !== this.server.origin || current.workspaceId !== workspaceId) {
      this.#options.session.begin(this.server.origin, workspaceId);
    }
  }

  #onSessionState(state: SessionState): void {
    // The account changed (sign-in, sign-out or a switch): advance the recorder's epoch and end a local
    // recording. A token refresh of the same account is not a change.
    const key = state.kind === "signed-in" ? `${state.tenantId}/${state.userId}` : null;
    if (key !== this.#accountKey) {
      this.#accountKey = key;
      this.recording.accountChanged();
    }
    if (state.kind === "signed-in" && state.workspaceId) {
      this.#ensureSessionFile(state.workspaceId);
    } else if (state.kind === "signed-out") {
      this.#stream?.close();
      this.#stream = null;
    }
    this.#options.push("session.state", state);
  }

  /** The handlers, keyed by allowlisted channel; `registerBridgeHandlers` installs them. */
  handlers(): Record<BridgeChannel, (payload: unknown) => Promise<unknown>> {
    return {
      // Every handler is async so a refused payload is a rejection the renderer sees as an
      // error result, never an exception escaping into the IPC layer.
      "commands.invoke": (payload) => Promise.resolve(payload).then((raw) => this.invoke(asBridgeCommand(raw))),
      "stream.subscribe": () => Promise.resolve().then(() => this.subscribe()),
      "stream.reconnect": () => Promise.resolve().then(() => this.reconnect()),
      "session.activeRun": () => Promise.resolve().then(() => this.activeRun()),
      "session.state": () => Promise.resolve().then(() => this.sessionState()),
      // Signed in, the session manager answers; a trusted-header harness asks the server
      // directly — `/v1/me` resolves the same scope either way.
      "session.me": () => (this.sessions ? this.#requireSessions().me() : this.server.me()),
      "auth.magicLink.start": (payload) =>
        Promise.resolve(payload).then((raw) => {
          const { email, displayName } = asMagicLinkStart(raw);
          return this.#requireSessions().startMagicLink(email, displayName);
        }),
      "auth.magicLink.complete": (payload) =>
        Promise.resolve(payload).then((raw) => this.#requireSessions().completeMagicLink(asToken(raw))),
      "auth.oauth.start": (payload) =>
        Promise.resolve(payload).then(async (raw) => {
          const started = await this.#requireSessions().startOAuth(asProvider(raw));
          await this.#options.openExternal?.(started.authorizationUrl);
          // The renderer learns that a browser round trip is under way, not the URL itself.
          return { state: started.state, opened: this.#options.openExternal !== undefined };
        }),
      "auth.signOut": () => this.#requireSessions().signOut().then(() => this.sessionState()),
      "settings.updateWorkspace": (payload) => Promise.resolve(payload).then((raw) => this.updateWorkspace(raw)),
      "settings.updateUser": (payload) => Promise.resolve(payload).then((raw) => this.updateUserSettings(raw)),
      "deepLink.open": (payload) => Promise.resolve(payload).then((raw) => this.openDeepLink(asUrl(raw))),
      "update.status": () => Promise.resolve().then(() => this.updateStatus()),
      "threads.list": () => this.server.threads(this.#requireWorkspace()),
      "threads.messages": (payload) => Promise.resolve(payload).then((raw) => this.server.messages(asThreadId(raw, "threads.messages"))),
      "questions.list": (payload) => Promise.resolve(payload).then((raw) => this.server.questions(asQuestionScope(raw))),
      "runs.timeline": (payload) => Promise.resolve(payload).then((raw) => this.server.timeline(asRunId(raw, "runs.timeline"))),
      "messages.post": (payload) => Promise.resolve(payload).then((raw) => this.postMessage(raw)),
      "questions.answer": (payload) => Promise.resolve(payload).then((raw) => this.answerQuestion(raw)),
      "turns.cancel": (payload) => Promise.resolve(payload).then((raw) => this.cancelTurn(raw)),
      "artifacts.upload": (payload) => Promise.resolve(payload).then((raw) => this.uploadAttachment(raw)),
      "artifacts.list": (payload) => Promise.resolve(payload).then((raw) => this.server.artifacts(this.#requireWorkspace(), asArtifactFilter(raw))),
      "artifacts.versions": (payload) => Promise.resolve(payload).then((raw) => this.server.artifactVersions(asArtifactId(raw, "artifacts.versions"))),
      "artifacts.content": (payload) => Promise.resolve(payload).then((raw) => this.artifactContent(raw)),
      "artifacts.save": (payload) => Promise.resolve(payload).then((raw) => this.saveArtifact(raw)),
      "artifacts.open": (payload) => Promise.resolve(payload).then((raw) => this.openArtifact(raw)),
      "evidence.get": (payload) => Promise.resolve(payload).then((raw) => this.server.evidence(asEvidenceId(raw))),
      "search.query": (payload) => Promise.resolve(payload).then((raw) => this.search(raw)),
      "work.graph": () => this.server.workgraph(this.#requireWorkspace()),
      "work.propose": (payload) => Promise.resolve(payload).then((raw) => this.proposeGraphEdit(raw)),
      "work.objective": (payload) => Promise.resolve(payload).then((raw) => this.createObjective(raw)),
      "approvals.list": (payload) => Promise.resolve(payload).then((raw) => this.server.approvals(this.#trustScope(raw, "approvals.list"))),
      // Trust decisions and evidence reads return their refusal as data: Electron's IPC
      // flattens a thrown error to its message, and the page has to branch on the code
      // (APPROVAL_SUPERSEDED) and the typed details (digest_mismatch).
      "approvals.approve": (payload) => settled(Promise.resolve(payload).then((raw) => this.approveEffect(raw))),
      "approvals.deny": (payload) => settled(Promise.resolve(payload).then((raw) => this.denyEffect(raw))),
      "effects.list": (payload) => Promise.resolve(payload).then((raw) => this.server.effectsIn(this.#trustScope(raw, "effects.list"))),
      "evidence.content": (payload) => settled(Promise.resolve(payload).then((raw) => this.evidenceContent(raw))),
      // Knowledge, memory and skills (APP-011): lifecycle refusals (a terminal edge, an
      // unapproved activation) are data the page branches on, so they are settled.
      "knowledge.list": (payload) => Promise.resolve(payload).then((raw) => this.server.knowledge(this.#requireWorkspace(), asEntryFilter(raw))),
      "knowledge.delete": (payload) => settled(Promise.resolve(payload).then((raw) => this.deleteKnowledge(raw))),
      "knowledge.supersede": (payload) => settled(Promise.resolve(payload).then((raw) => this.supersedeKnowledge(raw))),
      "memory.list": (payload) => Promise.resolve(payload).then((raw) => this.server.memory(this.#requireWorkspace(), asEntryFilter(raw))),
      "memory.delete": (payload) => settled(Promise.resolve(payload).then((raw) => this.deleteMemory(raw))),
      "skills.list": () => this.server.skills(),
      "skills.install": (payload) => settled(Promise.resolve(payload).then((raw) => this.installSkill(raw))),
      "skills.promote": (payload) => settled(Promise.resolve(payload).then((raw) => this.promoteSkillVersion(raw))),
      "skills.disable": (payload) => settled(Promise.resolve(payload).then((raw) => this.disableSkill(raw))),
      "packs.list": () => this.server.packs(),
      "packs.rollback": (payload) => settled(Promise.resolve(payload).then((raw) => this.rollbackPack(raw))),
      "runs.capabilities": (payload) => Promise.resolve(payload).then((raw) => this.server.run(asRunId(raw, "runs.capabilities"))),
      // The teammate roster (APP-014): definition commands return refusals as data (an
      // archived teammate, a need that is not an effect class, a role that may not).
      "teammates.list": () => this.server.teammates(this.#requireWorkspace()),
      "teammates.create": (payload) => settled(Promise.resolve(payload).then((raw) => this.createTeammate(raw))),
      "teammates.update": (payload) => settled(Promise.resolve(payload).then((raw) => this.updateTeammate(raw))),
      "teammates.archive": (payload) => settled(Promise.resolve(payload).then((raw) => this.archiveTeammate(raw))),
      // The live browser/computer/terminal surface (APP-007): a takeover or handback the
      // server refuses (not active, not held, a stale generation) is data the page shows.
      "live.targets": () => this.server.targets(this.#requireWorkspace()),
      "live.session": (payload) => Promise.resolve(payload).then((raw) => this.server.browserSession(asSessionId(raw, "live.session"))),
      "live.takeover": (payload) => settled(Promise.resolve(payload).then((raw) => this.liveControl("RequestTakeover", raw))),
      "live.handback": (payload) => settled(Promise.resolve(payload).then((raw) => this.liveControl("Handback", raw))),
      // A person's answer to a certificate error or a camera/microphone request (ADP-001): user-only on the server.
      "live.certificate": (payload) => settled(Promise.resolve(payload).then((raw) => this.liveCertificate(raw))),
      "live.permission": (payload) => settled(Promise.resolve(payload).then((raw) => this.livePermission(raw))),
      // A person's click or typing in a browser session they hold: refused unless they hold it, and an input that cannot be
      // delivered is an error the page shows rather than a silent drop.
      "live.input": (payload) => settled(Promise.resolve(payload).then((raw) => this.liveInput(raw))),
      "computer.permissions": (payload) => Promise.resolve(payload).then((raw) => this.computerPermissions(raw)),
      "computer.openSettings": (payload) => Promise.resolve(payload).then((raw) => this.openComputerSettings(raw)),
      // Skill recording (ADP-003): refusals are data the page branches on, so the commands are settled.
      "recording.status": () => Promise.resolve().then(() => this.recording.status()),
      "recording.start": () => Promise.resolve().then(() => this.startRecording()),
      "recording.finish": (payload) => Promise.resolve(payload).then((raw) => this.finishRecording(raw)),
      "recording.cancel": () => Promise.resolve().then(() => this.recording.cancel()),
      "recording.list": () => this.server.recordings(),
      "recording.compile": (payload) => settled(Promise.resolve(payload).then((raw) => this.recordingCommand("CompileRecording", raw))),
      "recording.submit": (payload) => settled(Promise.resolve(payload).then((raw) => this.recordingCommand("SubmitRecordingSkill", raw))),
      // One-time payment values (ADP-009, DOMAIN.md 13.9 C): the answer names fields and times, never a value.
      "payment.supply": (payload) => settled(Promise.resolve(payload).then((raw) => this.payments.supply(raw))),
      "worker.status": () => (this.#options.worker ? this.#options.worker.status() : Promise.resolve(NO_WORKER)),
      "worker.start": () => settled(this.#options.worker ? this.#options.worker.start() : Promise.reject(new WorkerLaunchError("TARGET_UNAVAILABLE", NO_WORKER.reason))),
      "worker.stop": () => settled(this.#options.worker ? this.#options.worker.stop() : Promise.resolve(NO_WORKER)),
      "live.attach": (payload) => Promise.resolve(payload).then((raw) => this.attachLive(asTargetId(raw, "live.attach"))),
      "live.detach": () => Promise.resolve().then(() => this.detachLive()),
      // Collaboration (APP-009): a protected thread refuses a non-participant, and the
      // page branches on that refusal, so these are settled.
      "threads.view": (payload) => settled(Promise.resolve(payload).then((raw) => this.server.thread(asThreadId(raw, "threads.view")))),
      "threads.create": (payload) => settled(Promise.resolve(payload).then((raw) => this.collaborationCommand("CreateThread", raw, "threads.create"))),
      "participants.add": (payload) => settled(Promise.resolve(payload).then((raw) => this.collaborationCommand("AddParticipant", raw, "participants.add"))),
      "participants.remove": (payload) => settled(Promise.resolve(payload).then((raw) => this.collaborationCommand("RemoveParticipant", raw, "participants.remove"))),
      "messages.react": (payload) => settled(Promise.resolve(payload).then((raw) => this.collaborationCommand("ReactToMessage", raw, "messages.react"))),
      "threads.handoff": (payload) => settled(Promise.resolve(payload).then((raw) => this.collaborationCommand("HandOffThread", raw, "threads.handoff"))),
      // Routines and notifications (APP-010): refusals (a bad trigger, a paused routine, a
      // channel that is not one) are data the page shows.
      "routines.list": () => this.server.routines(this.#requireWorkspace()),
      "routines.command": (payload) => settled(Promise.resolve(payload).then((raw) => this.routineCommand(raw))),
      "notifications.list": (payload) =>
        Promise.resolve(payload).then((raw) => this.server.notifications(this.#requireWorkspace(), asRecord(raw ?? {}, "notifications.list")["unreadOnly"] === true)),
      "notifications.read": (payload) => settled(Promise.resolve(payload).then((raw) => this.markNotificationRead(raw))),
      "notifications.preference": (payload) => settled(Promise.resolve(payload).then((raw) => this.setNotificationPreference(raw))),
      "diagnostics.export": () => this.server.exportDiagnostics(),
      "appearance.set": (payload) =>
        Promise.resolve(payload).then((raw) => {
          const theme = asAppearance(raw);
          this.#options.setAppearance?.(theme);
          return { theme };
        }),
    };
  }

  // ---- skill recording (ADP-003) ----

  /** Start the person's recorder in the workspace the desktop acts in. */
  startRecording(): Promise<unknown> {
    return this.recording.start(this.#requireWorkspace());
  }

  /** The person's own finish: the page sends `gesture` from a genuine click; anything else is refused. */
  async finishRecording(raw: unknown): Promise<unknown> {
    const gesture = asRecord(raw ?? {}, "recording.finish")["gesture"] === true;
    return this.recording.finish({ gesture });
  }

  /** Compile or submit with the params the page built; main adds the epoch it holds, never the page. */
  async recordingCommand(name: "CompileRecording" | "SubmitRecordingSkill", raw: unknown): Promise<unknown> {
    const record = asRecord(raw, name);
    const recordingId = record["recordingId"];
    if (typeof recordingId !== "string" || !recordingId.startsWith("rec_")) {
      throw new Error(`${name} needs a rec_ recordingId`);
    }
    const params: Record<string, unknown> = { recording_id: recordingId, epoch: this.recording.epoch };
    if (name === "SubmitRecordingSkill") {
      params["name"] = record["name"];
      params["semver"] = record["semver"];
      params["slots"] = record["slots"];
    }
    return (await this.server.invoke(name, newCommandId(), params)).result;
  }

  // ---- routines and notifications (APP-010) ----

  /** One routine command by name, with the params the page built; the workspace is main's. */
  async routineCommand(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "routines.command");
    const name = record["name"];
    const allowed = ["CreateRoutine", "UpdateRoutine", "PauseRoutine", "ResumeRoutine", "DeleteRoutine", "TriggerRoutineNow", "DryRunRoutine", "RotateRoutineHook"];
    if (typeof name !== "string" || !allowed.includes(name)) {
      throw new Error("routines.command names a routine command");
    }
    const params = { ...asRecord(record["params"], "routines.command params") };
    // A dry run of a draft names a definition instead of a routine; it is still the workspace main acts in.
    const draftDryRun = name === "DryRunRoutine" && params["routine_id"] === undefined;
    if (name === "CreateRoutine" || draftDryRun) {
      params["workspace_id"] = this.#requireWorkspace();
    } else if (typeof params["routine_id"] !== "string" || !params["routine_id"].startsWith("rtn_")) {
      throw new Error(`${name} needs an rtn_ routine_id`);
    }
    return (await this.server.invoke(name, newCommandId(), params)).result;
  }

  async markNotificationRead(raw: unknown): Promise<unknown> {
    const id = asRecord(raw, "notifications.read")["notificationId"];
    if (typeof id !== "string" || !id.startsWith("ntf_")) {
      throw new Error("notifications.read needs an ntf_ id");
    }
    return (await this.server.invoke("MarkNotificationRead", newCommandId(), { notification_id: id })).result;
  }

  async setNotificationPreference(raw: unknown): Promise<unknown> {
    const params = { ...asRecord(raw, "notifications.preference") };
    params["workspace_id"] = this.#requireWorkspace();
    if (typeof params["kind"] !== "string" || !Array.isArray(params["channels"])) {
      throw new Error("notifications.preference needs a kind and a list of channels");
    }
    return (await this.server.invoke("UpdateNotificationPreferences", newCommandId(), params)).result;
  }

  // ---- collaboration (APP-009) ----

  /**
   * One collaboration command with the params the page built (`collaboration/thread.ts`);
   * main adds the workspace to `CreateThread` and nothing else — the server validates the
   * rest again and decides who may.
   */
  async collaborationCommand(name: "CreateThread" | "AddParticipant" | "RemoveParticipant" | "ReactToMessage" | "HandOffThread", raw: unknown, what: string): Promise<unknown> {
    const params = { ...asRecord(raw, what) };
    if (name === "CreateThread" && typeof params["workspace_id"] !== "string") {
      params["workspace_id"] = this.#requireWorkspace();
    }
    if (name !== "CreateThread" && name !== "ReactToMessage" && (typeof params["thread_id"] !== "string" || !params["thread_id"].startsWith("thr_"))) {
      throw new Error(`${what} needs a thr_ thread_id`);
    }
    if (name === "ReactToMessage" && (typeof params["message_id"] !== "string" || !params["message_id"].startsWith("msg_"))) {
      throw new Error(`${what} needs a msg_ message_id`);
    }
    return (await this.server.invoke(name, newCommandId(), params)).result;
  }

  // ---- native computer permissions (ADP-002) ----

  /** The permission guide's snapshot: what the system says, which permissions the parked run needs. */
  async computerPermissions(raw: unknown): Promise<PermissionSnapshot> {
    const source = this.#options.computerPermissions;
    if (!source) {
      return notApplicable();
    }
    return readSnapshot(source, parsePermissionRequest(raw));
  }

  /**
   * Open the Privacy pane for one permission. The payload is a permission name and nothing else; the URL
   * is built here from a fixed table, so the page cannot ask for any other destination.
   */
  async openComputerSettings(raw: unknown): Promise<{ readonly opened: boolean }> {
    const permission = parseOpenSettings(raw);
    const open = this.#options.openSystemSettings;
    if (!open) {
      return { opened: false };
    }
    await open(settingsUrl(permission));
    return { opened: true };
  }

  // ---- the live browser/computer/terminal surface (APP-007) ----

  /** `RequestTakeover` / `Handback` for a `bsn_` session or a `tgt_` target; the server decides. */
  async liveControl(kind: "RequestTakeover" | "Handback", raw: unknown): Promise<unknown> {
    const record = asRecord(raw, `live.${kind === "RequestTakeover" ? "takeover" : "handback"}`);
    const sessionId = record["sessionId"];
    const targetId = record["targetId"];
    const command =
      typeof sessionId === "string"
        ? kind === "RequestTakeover"
          ? takeoverCommand(sessionId)
          : handbackCommand(sessionId)
        : typeof targetId === "string"
          ? computerCommand(kind, targetId)
          : null;
    if (!command) {
      throw new Error("a live control command names a bsn_ session or a tgt_ target");
    }
    return (await this.server.invoke(command.name, newCommandId(), command.params)).result;
  }

  /** `ResolveBrowserCertificate`: the person lets a tab continue past a certificate error, or cancels it. */
  async liveCertificate(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "live.certificate");
    const command = certificateCommand(
      asSessionId(record, "live.certificate"),
      asText(record["tabId"], "live.certificate tabId"),
      asText(record["certificateId"], "live.certificate certificateId"),
      asDecision(record["decision"], "live.certificate"),
    );
    return (await this.server.invoke(command.name, newCommandId(), command.params)).result;
  }

  /** `SendBrowserInput`: the person's click or typed text, for the session they hold. */
  async liveInput(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "live.input");
    const sessionId = asSessionId(record, "live.input");
    const tabId = asText(record["tabId"], "live.input tabId");
    const kind = record["kind"];
    const modifiers = (): UserModifier[] => {
      const raw = record["modifiers"];
      return Array.isArray(raw) ? raw.map((item) => asText(item, "live.input modifiers") as UserModifier) : [];
    };
    const command =
      kind === "click"
        ? inputCommand(sessionId, tabId, {
            kind,
            x: Number(record["x"]),
            y: Number(record["y"]),
            ...(record["button"] === undefined ? {} : { button: asText(record["button"], "live.input button") as UserButton }),
            ...(record["clickCount"] === undefined ? {} : { clickCount: Number(record["clickCount"]) as 1 | 2 | 3 }),
            modifiers: modifiers(),
          })
        : kind === "drag"
          ? inputCommand(sessionId, tabId, {
              kind,
              fromX: Number(record["fromX"]),
              fromY: Number(record["fromY"]),
              toX: Number(record["toX"]),
              toY: Number(record["toY"]),
              modifiers: modifiers(),
            })
          : kind === "text"
            ? inputCommand(sessionId, tabId, { kind, text: asText(record["text"], "live.input text") })
            : kind === "key"
              ? inputCommand(sessionId, tabId, { kind, key: asText(record["key"], "live.input key") })
              : kind === "scroll"
                ? inputCommand(sessionId, tabId, {
                    kind,
                    x: Number(record["x"]),
                    y: Number(record["y"]),
                    deltaX: Number(record["deltaX"]),
                    deltaY: Number(record["deltaY"]),
                  })
                : null;
    if (!command) {
      throw new Error("live.input is a click, a drag, typed text, a key or a scroll");
    }
    return (await this.server.invoke(command.name, newCommandId(), command.params)).result;
  }

  /** `DecideBrowserPermission`: the person grants or denies camera or microphone for one origin. */
  async livePermission(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "live.permission");
    const permission = record["permission"];
    if (permission !== "camera" && permission !== "microphone") {
      throw new Error("live.permission grants camera or microphone only");
    }
    const command = permissionCommand(
      asSessionId(record, "live.permission"),
      asText(record["tabId"], "live.permission tabId"),
      asText(record["origin"], "live.permission origin"),
      permission,
      asDecision(record["decision"], "live.permission"),
    );
    return (await this.server.invoke(command.name, newCommandId(), command.params)).result;
  }

  /**
   * Subscribe to `target:<tgt>` for live frames (`browser.frame`, `terminal.bytes`) and the
   * target's durable events. It is a second, cursor-less subscription beside the workspace
   * one: the durable cursor stays bound to the workspace channel set (see `#channels`), and
   * a live view that lags simply misses frames (DOMAIN.md §9.3). Attaching to the same
   * target again keeps the subscription; another target replaces it.
   */
  attachLive(targetId: string): { readonly targetId: string; readonly attached: boolean } {
    if (this.#live?.targetId === targetId) {
      return { targetId, attached: true };
    }
    this.detachLive();
    const headers = (): Record<string, string> => this.#streamHeaders();
    let client: StreamClient | null = null;
    const current = (): boolean => client !== null && client === this.#live?.client;
    client = new StreamClient({
      url: this.server.origin.replace(/^http/, "ws") + "/v1/stream",
      headers,
      channels: [`target:${targetId}`],
      consumer: `${this.#options.consumer}:live`,
      // Pictures of the target's browsers arrive as binary messages, not base64 inside JSON (OD-008).
      binaryFrames: true,
      openSocket: this.#options.openSocket,
      cursor: null,
      onEvent: (frame) => {
        if (current()) {
          this.#options.push("live.frame", frame);
        }
      },
      onLive: (frame) => {
        if (current()) {
          this.#options.push("live.frame", frame);
        }
      },
      onError: (error) => {
        if (current()) {
          this.#options.push("live.frame", { kind: "error", channel: `target:${targetId}`, error });
        }
      },
      onState: (state) => {
        if (current()) {
          this.#options.push("live.frame", { kind: "state", channel: `target:${targetId}`, state });
        }
      },
    });
    this.#live = { targetId, client };
    client.connect();
    return { targetId, attached: true };
  }

  detachLive(): { readonly attached: false } {
    this.#live?.client.close();
    this.#live = null;
    return { attached: false };
  }

  // ---- the teammate roster (APP-014) ----

  /** `CreateTeammate` with the params the page validated (`createTeammate()`); the workspace is main's. */
  async createTeammate(raw: unknown): Promise<unknown> {
    const params = { ...asRecord(raw, "teammates.create") };
    if (typeof params["workspace_id"] !== "string") {
      params["workspace_id"] = this.#requireWorkspace();
    }
    if (typeof params["template_id"] !== "string" && typeof params["name"] !== "string") {
      throw new Error("teammates.create needs a template_id or a name");
    }
    return (await this.server.invoke("CreateTeammate", newCommandId(), params)).result;
  }

  async updateTeammate(raw: unknown): Promise<unknown> {
    const params = asRecord(raw, "teammates.update");
    if (typeof params["teammate_id"] !== "string" || !params["teammate_id"].startsWith("agt_")) {
      throw new Error("teammates.update needs an agt_ id");
    }
    return (await this.server.invoke("UpdateTeammate", newCommandId(), params)).result;
  }

  async archiveTeammate(raw: unknown): Promise<unknown> {
    const teammateId = asRecord(raw, "teammates.archive")["teammateId"];
    if (typeof teammateId !== "string" || !teammateId.startsWith("agt_")) {
      throw new Error("teammates.archive needs an agt_ id");
    }
    return (await this.server.invoke("ArchiveTeammate", newCommandId(), { teammate_id: teammateId })).result;
  }

  // ---- knowledge, memory and skills (APP-011) ----

  async deleteKnowledge(raw: unknown): Promise<unknown> {
    const knowledgeId = asRecord(raw, "knowledge.delete")["knowledgeId"];
    if (typeof knowledgeId !== "string" || !knowledgeId.startsWith("kn_")) {
      throw new Error("knowledge.delete needs a kn_ id");
    }
    return (await this.server.invoke("DeleteKnowledge", newCommandId(), { knowledge_id: knowledgeId })).result;
  }

  async supersedeKnowledge(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "knowledge.supersede");
    const knowledgeId = record["knowledgeId"];
    const supersededBy = record["supersededBy"];
    if (typeof knowledgeId !== "string" || !knowledgeId.startsWith("kn_") || typeof supersededBy !== "string" || !supersededBy.startsWith("kn_")) {
      throw new Error("knowledge.supersede needs two kn_ ids");
    }
    return (await this.server.invoke("SupersedeKnowledge", newCommandId(), { knowledge_id: knowledgeId, superseded_by: supersededBy })).result;
  }

  async deleteMemory(raw: unknown): Promise<unknown> {
    const memoryId = asRecord(raw, "memory.delete")["memoryId"];
    if (typeof memoryId !== "string" || !memoryId.startsWith("mem_")) {
      throw new Error("memory.delete needs a mem_ id");
    }
    return (await this.server.invoke("DeleteMemory", newCommandId(), { memory_id: memoryId })).result;
  }

  /** `InstallSkill` with the params the page validated (`install()`); main adds nothing. */
  async installSkill(raw: unknown): Promise<unknown> {
    const params = asRecord(raw, "skills.install");
    if (typeof params["name"] !== "string" || typeof params["scope"] !== "string" || !params["version"] || typeof params["version"] !== "object") {
      throw new Error("skills.install needs scope, name and a version");
    }
    return (await this.server.invoke("InstallSkill", newCommandId(), params)).result;
  }

  async promoteSkillVersion(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "skills.promote");
    const versionId = record["skillVersionId"];
    const to = record["to"];
    if (typeof versionId !== "string" || !versionId.startsWith("sklv_") || typeof to !== "string") {
      throw new Error("skills.promote needs a sklv_ id and a target state");
    }
    return (await this.server.invoke("PromoteSkillVersion", newCommandId(), { skill_version_id: versionId, to })).result;
  }

  async disableSkill(raw: unknown): Promise<unknown> {
    const skillId = asRecord(raw, "skills.disable")["skillId"];
    if (typeof skillId !== "string" || !skillId.startsWith("skl_")) {
      throw new Error("skills.disable needs a skl_ id");
    }
    return (await this.server.invoke("DisableSkill", newCommandId(), { skill_id: skillId })).result;
  }

  /** `RollbackPack` for a pack by name (ADP-008); the server refuses when there is nothing to roll back to. */
  async rollbackPack(raw: unknown): Promise<unknown> {
    const pack = asRecord(raw, "packs.rollback")["pack"];
    if (typeof pack !== "string" || !/^[a-z][a-z0-9_-]{0,63}$/.test(pack)) {
      throw new Error("packs.rollback needs a pack name");
    }
    return (await this.server.invoke("RollbackPack", newCommandId(), { pack })).result;
  }

  // ---- approvals, effects and evidence (APP-006) ----

  /**
   * `ApproveEffect` bound to the digest the renderer previewed. Main forwards the digest
   * as given and adds nothing: if the request changed, the server's `APPROVAL_SUPERSEDED`
   * reaches the page as an `ApiError`.
   */
  async approveEffect(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "approvals.approve");
    const requestId = record["approvalRequestId"];
    const digest = record["paramsDigest"];
    if (typeof requestId !== "string" || !requestId.startsWith("apr_")) {
      throw new Error("approvals.approve needs an apr_ id");
    }
    if (typeof digest !== "string" || !/^[0-9a-f]{64}$/.test(digest)) {
      throw new Error("approvals.approve needs the previewed sha256 params_digest");
    }
    return (await this.server.invoke("ApproveEffect", newCommandId(), { approval_request_id: requestId, params_digest: digest })).result;
  }

  async denyEffect(raw: unknown): Promise<unknown> {
    const requestId = asRecord(raw, "approvals.deny")["approvalRequestId"];
    if (typeof requestId !== "string" || !requestId.startsWith("apr_")) {
      throw new Error("approvals.deny needs an apr_ id");
    }
    return (await this.server.invoke("DenyEffect", newCommandId(), { approval_request_id: requestId })).result;
  }

  /** Evidence bytes plus the digest the server recorded; the renderer verifies them again. */
  async evidenceContent(raw: unknown): Promise<{ bytesBase64: string; digest: string; sizeBytes: number }> {
    const evidenceId = asRecord(raw, "evidence.content")["evidenceId"];
    if (typeof evidenceId !== "string" || !evidenceId.startsWith("evd_")) {
      throw new Error("evidence.content needs an evd_ id");
    }
    const fetched = await this.server.evidenceContent(evidenceId);
    if (fetched.bytes.byteLength > MAX_PREVIEW_FETCH_BYTES) {
      throw new Error(`evidence exceeds ${String(MAX_PREVIEW_FETCH_BYTES)} bytes`);
    }
    return { bytesBase64: Buffer.from(fetched.bytes).toString("base64"), digest: fetched.digest, sizeBytes: fetched.bytes.byteLength };
  }

  #trustScope(raw: unknown, what: string): { workspaceId: string; runId?: string; status?: string } {
    const record = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
    const scope: { workspaceId: string; runId?: string; status?: string } = { workspaceId: this.#requireWorkspace() };
    if (record["runId"] !== undefined) {
      if (typeof record["runId"] !== "string" || !record["runId"].startsWith("run_")) {
        throw new Error(`${what} runId must be a run_ id`);
      }
      scope.runId = record["runId"];
    }
    if (record["status"] !== undefined) {
      if (typeof record["status"] !== "string" || !/^[A-Za-z_]{1,40}$/.test(record["status"])) {
        throw new Error(`${what} status must be a status name`);
      }
      scope.status = record["status"];
    }
    return scope;
  }

  // ---- the plan / WorkGraph surface (APP-005) ----

  /**
   * `ProposeGraphEdit` against the revision the renderer observed. The server's
   * `CONFLICT_REVISION` reaches the renderer as an `ApiError` with that code; main adds
   * nothing and retries nothing, because rebasing is the user's decision.
   */
  async proposeGraphEdit(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "work.propose");
    const baseRevision = record["base_revision"];
    if (typeof baseRevision !== "number" || !Number.isInteger(baseRevision) || baseRevision < 1) {
      throw new Error("work.propose needs an integer base_revision");
    }
    const params: Record<string, unknown> = { workspace_id: this.#requireWorkspace(), base_revision: baseRevision };
    for (const key of ["nodes_add", "nodes_update", "edges_add", "edges_remove"]) {
      const value = record[key];
      if (value !== undefined) {
        if (!Array.isArray(value)) {
          throw new Error(`work.propose ${key} must be a list`);
        }
        params[key] = value;
      }
    }
    params["rationale"] = typeof record["rationale"] === "string" ? record["rationale"].slice(0, 500) : "user edit";
    params["capability_needs"] = [];
    return (await this.server.invoke(PROPOSE_COMMAND, newCommandId(), params)).result;
  }

  /** `CreateObjective`: a root node and its objective thread. */
  async createObjective(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "work.objective");
    const title = record["title"];
    if (typeof title !== "string" || !title.trim() || title.length > 200) {
      throw new Error("work.objective needs a title of 1..=200 characters");
    }
    const params: Record<string, unknown> = { workspace_id: this.#requireWorkspace(), title: title.trim() };
    if (typeof record["description"] === "string" && record["description"].trim()) {
      params["description"] = record["description"].trim().slice(0, 2000);
    }
    const contract = record["completionContract"];
    if (contract && typeof contract === "object" && !Array.isArray(contract)) {
      params["completion_contract"] = contract;
    }
    return (await this.server.invoke(OBJECTIVE_COMMAND, newCommandId(), params)).result;
  }

  // ---- the artifact workspace (APP-008) ----

  /**
   * Bytes for the renderer's in-process preview, with the preview plan main computed from
   * the media type the server labelled them with. Active content is delivered with a
   * `text` plan and a `text/plain` label; the renderer has no path that renders it.
   */
  async artifactContent(raw: unknown): Promise<{ bytesBase64: string; mediaType: string; sizeBytes: number; plan: ReturnType<typeof previewPlan> }> {
    const { artifactId, versionId } = asVersionRef(raw, "artifacts.content");
    const fetched = await this.server.artifactContent(artifactId, versionId);
    const plan = previewPlan(fetched.mediaType, fetched.bytes.byteLength);
    if (plan.mode === "download-only" || fetched.bytes.byteLength > MAX_PREVIEW_FETCH_BYTES) {
      return { bytesBase64: "", mediaType: fetched.mediaType, sizeBytes: fetched.bytes.byteLength, plan: { mode: "download-only", reason: plan.mode === "download-only" ? plan.reason : "too large to preview" } };
    }
    return { bytesBase64: Buffer.from(fetched.bytes).toString("base64"), mediaType: fetched.mediaType, sizeBytes: fetched.bytes.byteLength, plan };
  }

  /** Save a version where the user chooses; null when they cancelled. */
  async saveArtifact(raw: unknown): Promise<{ path: string | null; sizeBytes: number }> {
    const files = this.#requireFiles();
    const { artifactId, versionId } = asVersionRef(raw, "artifacts.save");
    const { bytes, title } = await this.#fetchForFile(artifactId, versionId);
    const path = await files.saveAs(suggestedFilename(title, versionId), bytes);
    return { path, sizeBytes: bytes.byteLength };
  }

  /**
   * Open a version with the OS handler. Refused for pages, scripts, programs and unlabelled
   * bytes: those are shown as text or saved, never handed to something that would run them.
   */
  async openArtifact(raw: unknown): Promise<{ path: string; mediaType: string }> {
    const files = this.#requireFiles();
    const { artifactId, versionId } = asVersionRef(raw, "artifacts.open");
    const { bytes, title, mediaType } = await this.#fetchForFile(artifactId, versionId);
    if (!openAllowed(mediaType) || isActiveContent(mediaType) || isExecutable(mediaType)) {
      throw new Error(`${mediaKind(mediaType) || "unlabelled content"} is not opened with the OS; preview it as text or save it`);
    }
    const path = await files.openWithOs(suggestedFilename(title, versionId), bytes, mediaType);
    return { path, mediaType };
  }

  /** Global search through the typed endpoint. */
  async search(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "search.query");
    const q = record["q"];
    if (typeof q !== "string") {
      throw new Error("search.query needs q");
    }
    const channels = record["channels"];
    const maxResults = record["maxResults"];
    const request = searchRequest(
      q,
      Array.isArray(channels) ? channels.filter((item): item is string => typeof item === "string") : undefined,
      typeof maxResults === "number" ? maxResults : undefined,
    );
    return this.server.search(searchQueryString(this.#requireWorkspace(), request));
  }

  async #fetchForFile(artifactId: string, versionId: string): Promise<{ bytes: Uint8Array; title: string; mediaType: string }> {
    const history = (await this.server.artifactVersions(artifactId)) as { artifact?: { title?: unknown }; versions?: { id?: unknown; media_type?: unknown; size_bytes?: unknown }[] };
    const version = history.versions?.find((candidate) => candidate.id === versionId);
    if (!version) {
      throw new Error(`${versionId} is not a version of ${artifactId}`);
    }
    if (typeof version.size_bytes === "number" && version.size_bytes > MAX_FILE_BYTES) {
      throw new Error(`the version exceeds ${String(MAX_FILE_BYTES)} bytes`);
    }
    const fetched = await this.server.artifactContent(artifactId, versionId);
    const title = typeof history.artifact?.title === "string" ? history.artifact.title : artifactId;
    // The recorded media type, not the served label: the server relabels active content as
    // text for transport, and the open decision must see what the bytes really are.
    const mediaType = typeof version.media_type === "string" ? version.media_type : fetched.mediaType;
    return { bytes: fetched.bytes, title, mediaType };
  }

  #requireFiles(): DesktopFiles {
    if (!this.#options.files) {
      throw new Error("file operations are not available in this session");
    }
    return this.#options.files;
  }


  // ---- the conversation surface (APP-004) ----

  /** `PostMessage` with optional thread and attachments; the run it starts becomes the active run. */
  async postMessage(raw: unknown): Promise<unknown> {
    const { threadId, content, attachments, teammateId, mentions } = asPost(raw);
    const params: Record<string, unknown> = { workspace_id: this.#requireWorkspace(), content };
    if (threadId) {
      params["thread_id"] = threadId;
    }
    if (teammateId) {
      params["teammate_id"] = teammateId;
    }
    if (attachments.length > 0) {
      params["attachments"] = attachments;
    }
    if (mentions.length > 0) {
      params["mentions"] = mentions;
    }
    return (await this.invoke({ name: POST_COMMAND, commandId: newCommandId(), params })).result;
  }

  /** `AnswerQuestion`: the answer resumes the parked run and the reply follows. */
  async answerQuestion(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "questions.answer");
    const questionId = record["questionId"];
    if (typeof questionId !== "string" || !questionId.startsWith("q_")) {
      throw new Error("questions.answer needs a q_ id");
    }
    if (!("value" in record)) {
      throw new Error("questions.answer needs a value");
    }
    return (await this.server.invoke(ANSWER_COMMAND, newCommandId(), answerParams(questionId, record["value"]))).result;
  }

  /** `CancelTurn`: stop the run the turn belongs to. */
  async cancelTurn(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "turns.cancel");
    const runId = asRunId(record, "turns.cancel");
    const generation = typeof record["generation"] === "number" && record["generation"] >= 1 ? record["generation"] : 1;
    return (await this.server.invoke(CANCEL_COMMAND, newCommandId(), cancelParams(runId, generation))).result;
  }

  /**
   * `CreateArtifact` from bytes the renderer read from a file it was given; main never
   * opens a path on the renderer's behalf, and the size bound is checked before the send.
   */
  async uploadAttachment(raw: unknown): Promise<unknown> {
    const record = asRecord(raw, "artifacts.upload");
    const name = record["name"];
    const mediaType = record["mediaType"];
    const bytesBase64 = record["bytesBase64"];
    if (typeof name !== "string" || !name.trim() || typeof bytesBase64 !== "string" || !bytesBase64) {
      throw new Error("artifacts.upload needs {name, mediaType, bytesBase64}");
    }
    if (!/^[A-Za-z0-9+/]+={0,2}$/.test(bytesBase64)) {
      throw new Error("artifacts.upload bytesBase64 is not base64");
    }
    const decodedLength = Math.floor((bytesBase64.length * 3) / 4) - (bytesBase64.endsWith("==") ? 2 : bytesBase64.endsWith("=") ? 1 : 0);
    if (decodedLength > MAX_UPLOAD_BYTES) {
      throw new Error(`artifacts.upload exceeds ${String(MAX_UPLOAD_BYTES)} bytes`);
    }
    const params: Record<string, unknown> = {
      workspace_id: this.#requireWorkspace(),
      title: name.trim().slice(0, 200),
      media_type: typeof mediaType === "string" && mediaType ? mediaType.slice(0, 120) : "application/octet-stream",
      bytes_base64: bytesBase64,
    };
    const threadId = record["threadId"];
    if (typeof threadId === "string" && threadId.startsWith("thr_")) {
      params["thread_id"] = threadId;
    }
    const artifactId = record["artifactId"];
    if (typeof artifactId === "string" && artifactId.startsWith("art_")) {
      params["artifact_id"] = artifactId;
    }
    return (await this.server.invoke(UPLOAD_COMMAND, newCommandId(), params)).result;
  }

  #requireWorkspace(): string {
    const workspaceId = this.workspaceId();
    if (!workspaceId) {
      throw new Error("no workspace: sign in first");
    }
    return workspaceId;
  }

  async invoke(command: BridgeCommand): Promise<CommandResponse> {
    if (command.name === "SupplyPaymentInput") {
      // A payment value goes through its own channel: schema-checked, bounded, and never echoed.
      throw new Error("SupplyPaymentInput goes through the payment channel");
    }
    if (RECORDING_COMMANDS.includes(command.name)) {
      // A recording command carries the owner fence and (for the finish) a click: the recording channels
      // add them; the generic command path does not.
      throw new Error(`${command.name} goes through the recording channels`);
    }
    const response = await this.server.invoke(command.name, command.commandId, command.params);
    const result = response.result;
    const workspaceId = this.workspaceId();
    if (command.name === "PostMessage" && result && typeof result === "object" && workspaceId) {
      const { run_id: runId, thread_id: threadId } = result as { run_id?: unknown; thread_id?: unknown };
      if (typeof runId === "string" && typeof threadId === "string") {
        const session = this.#options.session.current();
        this.#options.session.attach({
          runId,
          threadId,
          generation: 1,
          streamCursor: session?.streamCursor ?? "",
          workspaceId,
        });
      }
    }
    return response;
  }

  /** The session projection the renderer may see: state and identities, never tokens. */
  sessionState(): SessionState {
    if (this.#options.auth.kind === "trusted-headers") {
      return {
        kind: "signed-in",
        userId: this.#options.auth.scope.userId,
        tenantId: this.#options.auth.scope.tenantId,
        workspaceId: this.#options.auth.workspaceId,
        onboarded: false,
      };
    }
    return this.#requireSessions().state();
  }

  /** Complete an OAuth sign-in from the callback deep link (main only). */
  completeOAuth(code: string, state: string): Promise<SessionState> {
    return this.#requireSessions().completeOAuth(code, state);
  }

  /** `UpdateWorkspace`: name and/or settings (execution target, model, provider handles, security). */
  async updateWorkspace(raw: unknown): Promise<unknown> {
    const { workspaceId, name, settings } = asWorkspaceUpdate(raw);
    const params: Record<string, unknown> = { workspace_id: workspaceId };
    if (name !== undefined) {
      params["name"] = name;
    }
    if (settings !== undefined) {
      params["settings"] = settings;
    }
    return (await this.server.invoke("UpdateWorkspace", newCommandId(), params)).result;
  }

  /** `UpdateUserSettings` for the caller's own layer. */
  async updateUserSettings(raw: unknown): Promise<unknown> {
    const { workspaceId, settings } = asUserSettingsUpdate(raw);
    return (await this.server.invoke("UpdateUserSettings", newCommandId(), { workspace_id: workspaceId, settings })).result;
  }

  #requireSessions(): SessionManager {
    if (!this.sessions) {
      throw new Error("sign-in is not available in the trusted-header harness mode");
    }
    return this.sessions;
  }

  /** Subscribe from the durable cursor; the renderer's own idea of a cursor is never used. */
  subscribe(): ActiveRunProjection {
    if (this.#stream && this.#stream.state !== "closed") {
      return this.activeRun();
    }
    this.#openStream();
    return this.activeRun();
  }

  /** Reconnect: close the transport and resume the subscription from the durable cursor. */
  reconnect(): ActiveRunProjection {
    this.#stream?.close();
    this.#stream = null;
    this.#openStream();
    return this.activeRun();
  }

  activeRun(): ActiveRunProjection {
    const session = this.#options.session.current();
    return {
      connected: this.#streamState === "open",
      workspaceId: session?.workspaceId ?? this.workspaceId(),
      activeRun: session?.activeRun ?? null,
      streamState: this.#streamState,
    };
  }

  /** Whether a person is signed in (the harness mode counts as signed in). */
  signedIn(): boolean {
    return this.sessions === null || this.sessions.state().kind === "signed-in";
  }

  /** Whether the stream is open now. */
  streamOpen(): boolean {
    return this.#streamState === "open";
  }

  /**
   * The connectivity monitor's reconnect (ADP-004): the one existing reconnect, only when a subscription is
   * wanted (the renderer subscribed and the person has not signed out). `force` resubscribes even when the stream
   * claims to be open (it may be half-dead after the network went away); otherwise an open stream is left alone.
   * Returns whether it reconnected.
   */
  resumeStream(force: boolean): boolean {
    if (this.#stream === null || !this.signedIn()) {
      return false;
    }
    if (!force && this.#streamState === "open") {
      return false;
    }
    this.reconnect();
    return true;
  }

  /**
   * What a restart would interrupt, from the server's maintenance status; `null` when it cannot be read (not an
   * error to the caller: the gates treat it as unknown and ask the person).
   */
  async maintenance(): Promise<MaintenanceStatus | null> {
    try {
      const workspaceId = this.workspaceId();
      return decodeMaintenance(await this.server.maintenance(workspaceId ?? undefined));
    } catch {
      return null;
    }
  }

  /** Parse a deep link and, when valid, tell the renderer where to go. */
  openDeepLink(url: string): DeepLinkIntent | null {
    const intent = parseDeepLink(url);
    if (intent) {
      this.#options.push("deepLink.intent", intent);
    }
    return intent;
  }

  updateStatus(): UpdateStatus {
    return this.#options.updates.status();
  }

  /** The durable session, for tests and diagnostics; never sent to the renderer as-is. */
  session(): DesktopSession | null {
    return this.#options.session.current();
  }

  /** The stream cursor main will resume from. */
  cursor(): string | null {
    return this.#stream?.cursor ?? this.#options.session.current()?.streamCursor ?? null;
  }

  close(): void {
    this.#stream?.close();
    this.#stream = null;
    this.detachLive();
  }

  /**
   * The desktop's one durable subscription. A cursor is bound to the channel set it was
   * issued under, so the set must not change across runs: the workspace channel carries
   * every run's durable events, and run-scoped subscriptions are for narrower consumers.
   */
  #channels(): string[] {
    const workspaceId = this.workspaceId();
    if (!workspaceId) {
      throw new Error("no workspace: sign in first");
    }
    return [`workspace:${workspaceId}`];
  }

  /** Upgrade headers for the stream, evaluated at each connect so a rotated token is used. */
  #streamHeaders(): Record<string, string> {
    const headers: Record<string, string> = { "x-quansio-consumer": this.#options.consumer };
    if (this.#options.auth.kind === "trusted-headers") {
      headers["x-quansio-tenant"] = this.#options.auth.scope.tenantId;
      headers["x-quansio-user"] = this.#options.auth.scope.userId;
    } else {
      const token = this.sessions?.accessToken();
      if (token) {
        headers["authorization"] = `Bearer ${token}`;
      }
    }
    return headers;
  }

  #openStream(cursor: string | null = this.#options.session.current()?.streamCursor ?? null): void {
    const headers = (): Record<string, string> => this.#streamHeaders();
    // Callbacks check that they belong to the current client: a superseded client's late
    // close must not report on the subscription that replaced it.
    let client: StreamClient | null = null;
    const current = (): boolean => client !== null && client === this.#stream;
    client = new StreamClient({
      url: this.server.origin.replace(/^http/, "ws") + "/v1/stream",
      headers,
      channels: this.#channels(),
      consumer: this.#options.consumer,
      openSocket: this.#options.openSocket,
      cursor,
      onEvent: (frame) => {
        if (current()) {
          this.#onEvent(frame);
        }
      },
      onLive: (frame) => {
        if (current()) {
          this.#onLive(frame);
        }
      },
      onError: (error) => {
        if (current()) {
          this.#onError(error);
        }
      },
      onState: (state) => {
        if (current()) {
          this.#streamState = state;
          this.#options.push("stream.state", state);
        }
      },
    });
    this.#stream = client;
    client.connect();
  }

  #onError(error: StreamErrorFrame): void {
    this.#options.push("stream.error", error);
    if (error.code.startsWith("AUTH_") && this.sessions) {
      // The server ended the stream because the session no longer admits it (revoked, expired,
      // the person removed). A refresh either renews it — resume from the cursor — or finds the
      // session gone, which signs out and returns the person to sign-in (the stream closes
      // with the session state).
      this.#stream?.close();
      this.#stream = null;
      void this.sessions.refresh().then((token) => {
        if (token && !this.#stream) {
          this.#openStream();
        }
      });
      return;
    }
    if (!error.retryable) {
      // The server refused the subscription itself, so the durable cursor is no longer a
      // position it will honour. Resume from its current position: continuity is lost only
      // for the interval the server itself declined to replay, and that is reported above.
      this.#stream?.close();
      this.#stream = null;
      this.#options.session.resetCursor();
      this.#openStream(null);
    }
  }

  #onEvent(frame: EventFrame): void {
    this.#options.session.advance(frame.cursor);
    this.#options.push("stream.frame", frame);
    this.#options.observe?.(frame);
    // The server delivers a person's notification events to that person only; one that
    // asks for the desktop channel becomes a native notification here (APP-010).
    const notice = desktopNotice(frame.event);
    if (notice) {
      this.#options.notifyNative?.(notice);
    }
  }

  #onLive(frame: LiveFrame): void {
    // Live frames (model deltas, terminal bytes, browser frames, progress) reach the renderer
    // only here; they are never persisted and never move the cursor.
    this.#options.push("stream.frame", frame);
  }
}

/** A refusal as data, so the renderer sees the §15 code and details across the IPC boundary. */
export type Settled<T> =
  | { readonly ok: true; readonly result: T }
  | { readonly ok: false; readonly error: { readonly code: string; readonly message: string; readonly retryable: boolean; readonly details: unknown } };

/** What the worker channels answer when this build has no launcher. */
const NO_WORKER = { available: false, reason: "this app was started without a worker launcher", state: "stopped", pid: null, root: null, targetId: null, attached: false, exitCode: null, lastError: null } as const;

export async function settled<T>(work: Promise<T>): Promise<Settled<T>> {
  try {
    return { ok: true, result: await work };
  } catch (error) {
    if (error instanceof ApiError) {
      return { ok: false, error: { code: error.code, message: error.message, retryable: error.retryable, details: error.details } };
    }
    if (error instanceof WorkerLaunchError) {
      return { ok: false, error: { code: error.code, message: error.message, retryable: false, details: null } };
    }
    if (error instanceof PaymentSupplyError) {
      // A refusal of a payment supply names a rule, never a value (payments.ts).
      return { ok: false, error: { code: error.code, message: error.message, retryable: false, details: null } };
    }
    return { ok: false, error: { code: "INTERNAL", message: error instanceof Error ? error.message : String(error), retryable: false, details: null } };
  }
}

export { PRELOAD_PUSH_CHANNELS };

function asBridgeCommand(payload: unknown): BridgeCommand {
  if (!payload || typeof payload !== "object") {
    throw new Error("commands.invoke needs {name, commandId, params}");
  }
  const record = payload as Record<string, unknown>;
  const name = record["name"];
  const commandId = record["commandId"];
  const params = record["params"];
  if (typeof name !== "string" || typeof commandId !== "string" || !/^cmd_[0-9A-HJKMNP-TV-Z]{26}$/.test(commandId)) {
    throw new Error("commands.invoke needs a command name and a cmd_ ULID");
  }
  if (params !== undefined && (params === null || typeof params !== "object" || Array.isArray(params))) {
    throw new Error("commands.invoke params must be an object");
  }
  return { name, commandId, params: (params as Record<string, unknown> | undefined) ?? {} };
}

function asRecord(payload: unknown, what: string): Record<string, unknown> {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    throw new Error(`${what} needs an object payload`);
  }
  return payload as Record<string, unknown>;
}

function asMagicLinkStart(payload: unknown): { email: string; displayName: string } {
  const record = asRecord(payload, "auth.magicLink.start");
  const email = record["email"];
  if (typeof email !== "string" || !email.includes("@") || email.length > 254) {
    throw new Error("auth.magicLink.start needs an email address");
  }
  const displayName = typeof record["displayName"] === "string" ? record["displayName"].slice(0, 120) : "";
  return { email, displayName };
}

function asToken(payload: unknown): string {
  const token = asRecord(payload, "auth.magicLink.complete")["token"];
  if (typeof token !== "string" || !token.startsWith("qml_") || token.length > 128) {
    throw new Error("auth.magicLink.complete needs the delivered token");
  }
  return token;
}

function asProvider(payload: unknown): OAuthProvider {
  const provider = asRecord(payload, "auth.oauth.start")["provider"];
  if (provider !== "google" && provider !== "microsoft" && provider !== "github") {
    throw new Error("auth.oauth.start needs google, microsoft or github");
  }
  return provider;
}

function asSettings(value: unknown, what: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${what} settings must be an object`);
  }
  return value as Record<string, unknown>;
}

function asWorkspaceUpdate(payload: unknown): { workspaceId: string; name?: string; settings?: Record<string, unknown> } {
  const record = asRecord(payload, "settings.updateWorkspace");
  const workspaceId = record["workspaceId"];
  if (typeof workspaceId !== "string" || !workspaceId.startsWith("ws_")) {
    throw new Error("settings.updateWorkspace needs a ws_ id");
  }
  const result: { workspaceId: string; name?: string; settings?: Record<string, unknown> } = { workspaceId };
  if (record["name"] !== undefined) {
    if (typeof record["name"] !== "string") {
      throw new Error("settings.updateWorkspace name must be a string");
    }
    result.name = record["name"];
  }
  if (record["settings"] !== undefined) {
    result.settings = asSettings(record["settings"], "settings.updateWorkspace");
  }
  return result;
}

/** The themes a person can choose (`UpdateUserSettings {theme}`). */
export const APPEARANCES = ["system", "light", "dark"] as const;
export type Appearance = (typeof APPEARANCES)[number];

function asAppearance(payload: unknown): Appearance {
  const theme = asRecord(payload, "appearance.set")["theme"];
  if (typeof theme !== "string" || !(APPEARANCES as readonly string[]).includes(theme)) {
    throw new Error("appearance.set needs system, light or dark");
  }
  return theme as Appearance;
}

function asUserSettingsUpdate(payload: unknown): { workspaceId: string; settings: Record<string, unknown> } {
  const record = asRecord(payload, "settings.updateUser");
  const workspaceId = record["workspaceId"];
  if (typeof workspaceId !== "string" || !workspaceId.startsWith("ws_")) {
    throw new Error("settings.updateUser needs a ws_ id");
  }
  return { workspaceId, settings: asSettings(record["settings"], "settings.updateUser") };
}

function asThreadId(payload: unknown, what: string): string {
  const threadId = asRecord(payload, what)["threadId"];
  if (typeof threadId !== "string" || !threadId.startsWith("thr_")) {
    throw new Error(`${what} needs a thr_ id`);
  }
  return threadId;
}

function asText(value: unknown, what: string): string {
  if (typeof value !== "string" || value === "") {
    throw new Error(`${what} needs a non-empty string`);
  }
  return value;
}

function asDecision(value: unknown, what: string): "allow" | "deny" {
  if (value !== "allow" && value !== "deny") {
    throw new Error(`${what} decision is allow or deny`);
  }
  return value;
}

function asSessionId(payload: unknown, what: string): string {
  const sessionId = asRecord(payload, what)["sessionId"];
  if (typeof sessionId !== "string" || !sessionId.startsWith("bsn_")) {
    throw new Error(`${what} needs a bsn_ id`);
  }
  return sessionId;
}

function asTargetId(payload: unknown, what: string): string {
  const targetId = asRecord(payload, what)["targetId"];
  if (typeof targetId !== "string" || !targetId.startsWith("tgt_")) {
    throw new Error(`${what} needs a tgt_ id`);
  }
  return targetId;
}

function asRunId(payload: unknown, what: string): string {
  const runId = asRecord(payload, what)["runId"];
  if (typeof runId !== "string" || !runId.startsWith("run_")) {
    throw new Error(`${what} needs a run_ id`);
  }
  return runId;
}

function asQuestionScope(payload: unknown): { readonly threadId: string } | { readonly runId: string } {
  const record = asRecord(payload, "questions.list");
  if (typeof record["threadId"] === "string" && record["threadId"].startsWith("thr_")) {
    return { threadId: record["threadId"] };
  }
  if (typeof record["runId"] === "string" && record["runId"].startsWith("run_")) {
    return { runId: record["runId"] };
  }
  throw new Error("questions.list needs a thr_ or run_ id");
}

function asPost(payload: unknown): {
  threadId: string | null;
  content: string;
  attachments: string[];
  teammateId: string | null;
  mentions: { kind: "user" | "teammate"; id: string }[];
} {
  const record = asRecord(payload, "messages.post");
  const content = record["content"];
  if (typeof content !== "string" || !content.trim()) {
    throw new Error("messages.post needs content");
  }
  const threadId = record["threadId"];
  if (threadId !== undefined && threadId !== null && (typeof threadId !== "string" || !threadId.startsWith("thr_"))) {
    throw new Error("messages.post threadId must be a thr_ id");
  }
  // The teammate the message is for (APP-014); without one the server routes to the
  // workspace's default teammate.
  const teammateId = record["teammateId"];
  if (teammateId !== undefined && teammateId !== null && (typeof teammateId !== "string" || !teammateId.startsWith("agt_"))) {
    throw new Error("messages.post teammateId must be an agt_ id");
  }
  const rawAttachments = record["attachments"];
  const attachments: string[] = [];
  if (rawAttachments !== undefined) {
    if (!Array.isArray(rawAttachments)) {
      throw new Error("messages.post attachments must be a list of art_ ids");
    }
    for (const item of rawAttachments) {
      if (typeof item !== "string" || !item.startsWith("art_")) {
        throw new Error("messages.post attachments must be a list of art_ ids");
      }
      attachments.push(item);
    }
  }
  // Who the message mentions (APP-009): `usr_` people and `agt_` teammates only.
  const rawMentions = record["mentions"];
  const mentions: { kind: "user" | "teammate"; id: string }[] = [];
  if (rawMentions !== undefined && rawMentions !== null) {
    if (!Array.isArray(rawMentions)) {
      throw new Error("messages.post mentions must be a list of {kind, id}");
    }
    for (const item of rawMentions) {
      const mention = item && typeof item === "object" ? (item as Record<string, unknown>) : {};
      const kind = mention["kind"];
      const id = mention["id"];
      const ok = (kind === "user" && typeof id === "string" && id.startsWith("usr_")) || (kind === "teammate" && typeof id === "string" && id.startsWith("agt_"));
      if (!ok) {
        throw new Error("messages.post mentions must be a list of {kind: user|teammate, id}");
      }
      mentions.push({ kind, id });
    }
  }
  return { threadId: typeof threadId === "string" ? threadId : null, content, attachments, teammateId: typeof teammateId === "string" ? teammateId : null, mentions };
}

function asEntryFilter(raw: unknown): { status?: string; scope?: string; subjectRef?: string; includeDeleted?: boolean } {
  const record = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  const filter: { status?: string; scope?: string; subjectRef?: string; includeDeleted?: boolean } = {};
  if (typeof record["status"] === "string") {
    filter.status = record["status"];
  }
  if (typeof record["scope"] === "string") {
    filter.scope = record["scope"];
  }
  if (typeof record["subjectRef"] === "string") {
    filter.subjectRef = record["subjectRef"];
  }
  if (record["includeDeleted"] === true) {
    filter.includeDeleted = true;
  }
  return filter;
}

function asArtifactFilter(payload: unknown): { kind?: string | null; runId?: string | null } {
  const record = payload && typeof payload === "object" ? (payload as Record<string, unknown>) : {};
  const filter: { kind?: string | null; runId?: string | null } = {};
  if (typeof record["kind"] === "string" && /^[a-z]{1,20}$/.test(record["kind"])) {
    filter.kind = record["kind"];
  }
  if (typeof record["runId"] === "string" && record["runId"].startsWith("run_")) {
    filter.runId = record["runId"];
  }
  return filter;
}

function asArtifactId(payload: unknown, what: string): string {
  const artifactId = asRecord(payload, what)["artifactId"];
  if (typeof artifactId !== "string" || !artifactId.startsWith("art_")) {
    throw new Error(`${what} needs an art_ id`);
  }
  return artifactId;
}

function asVersionRef(payload: unknown, what: string): { artifactId: string; versionId: string } {
  const artifactId = asArtifactId(payload, what);
  const versionId = asRecord(payload, what)["versionId"];
  if (typeof versionId !== "string" || !versionId.startsWith("artv_")) {
    throw new Error(`${what} needs an artv_ id`);
  }
  return { artifactId, versionId };
}

function asEvidenceId(payload: unknown): string {
  const evidenceId = asRecord(payload, "evidence.get")["evidenceId"];
  if (typeof evidenceId !== "string" || !evidenceId.startsWith("evd_")) {
    throw new Error("evidence.get needs an evd_ id");
  }
  return evidenceId;
}

/** A filename the OS accepts, from the artifact's title and the version's identity. */
export function suggestedFilename(title: string, versionId: string): string {
  // No separators, no leading dots (hidden files, `..`), a bounded length, and only a
  // short alphanumeric suffix counts as the extension.
  const cleaned = title.replace(/[^A-Za-z0-9._ -]/g, "_").replace(/^[. ]+/, "").trim().slice(0, 120) || "artifact";
  const match = /^(.*?)(\.[A-Za-z0-9]{1,8})?$/.exec(cleaned);
  const stem = (match?.[1] ?? cleaned) || "artifact";
  const extension = match?.[2] ?? "";
  return `${stem}-${versionId.slice(-8)}${extension}`;
}

function asUrl(payload: unknown): string {
  if (!payload || typeof payload !== "object" || typeof (payload as { url?: unknown }).url !== "string") {
    throw new Error("deepLink.open needs {url}");
  }
  return (payload as { url: string }).url;
}
