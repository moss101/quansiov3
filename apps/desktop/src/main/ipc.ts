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
import { ServerClient, type CommandResponse, type Credential, type FetchLike, type OAuthProvider, type SessionScope } from "../client/server.js";
import { StreamClient, type EventFrame, type LiveFrame, type SocketFactory, type StreamErrorFrame, type StreamState } from "../client/stream.js";
import { PRELOAD_ALLOWLIST, PRELOAD_PUSH_CHANNELS, type BridgeCommand, type PreloadChannel, type PushChannel } from "../preload/bridge.js";
import { newCommandId } from "../client/ids.js";
import { ANSWER_COMMAND, CANCEL_COMMAND, MAX_UPLOAD_BYTES, POST_COMMAND, UPLOAD_COMMAND, answerParams, cancelParams } from "../conversation/composer.js";
import { parseDeepLink, type DeepLinkIntent } from "./deeplinks.js";
import { SessionFile, type ActiveRunSession, type DesktopSession } from "./session-file.js";
import { SessionManager, type SessionState } from "./session-manager.js";
import type { TokenStore } from "./token-store.js";
import type { UpdateHooks, UpdateStatus } from "./updates.js";

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
  /** Opens an OAuth authorization URL in the system browser (main supplies `shell.openExternal`). */
  readonly openExternal?: (url: string) => Promise<void>;
  /** Injectable for tests; defaults to the global `fetch`. */
  readonly fetch?: FetchLike;
}

export class DesktopBridge {
  readonly server: ServerClient;
  readonly localRuntime: LocalRuntimeClient;
  /** Present in session mode; the trusted-header harness has none. */
  readonly sessions: SessionManager | null;
  readonly #options: DesktopBridgeOptions;
  #stream: StreamClient | null = null;
  #streamState: StreamState = "closed";

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
    if (state.kind === "signed-in" && state.workspaceId) {
      this.#ensureSessionFile(state.workspaceId);
    } else if (state.kind === "signed-out") {
      this.#stream?.close();
      this.#stream = null;
    }
    this.#options.push("session.state", state);
  }

  /** The handlers, keyed by allowlisted channel; `registerBridgeHandlers` installs them. */
  handlers(): Record<PreloadChannel, (payload: unknown) => Promise<unknown>> {
    return {
      // Every handler is async so a refused payload is a rejection the renderer sees as an
      // error result, never an exception escaping into the IPC layer.
      "commands.invoke": (payload) => Promise.resolve(payload).then((raw) => this.invoke(asBridgeCommand(raw))),
      "stream.subscribe": () => Promise.resolve().then(() => this.subscribe()),
      "stream.reconnect": () => Promise.resolve().then(() => this.reconnect()),
      "session.activeRun": () => Promise.resolve().then(() => this.activeRun()),
      "session.state": () => Promise.resolve().then(() => this.sessionState()),
      "session.me": () => this.#requireSessions().me(),
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
    };
  }

  // ---- the conversation surface (APP-004) ----

  /** `PostMessage` with optional thread and attachments; the run it starts becomes the active run. */
  async postMessage(raw: unknown): Promise<unknown> {
    const { threadId, content, attachments } = asPost(raw);
    const params: Record<string, unknown> = { workspace_id: this.#requireWorkspace(), content };
    if (threadId) {
      params["thread_id"] = threadId;
    }
    if (attachments.length > 0) {
      params["attachments"] = attachments;
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
  }

  #onLive(frame: LiveFrame): void {
    // Live frames (model deltas, terminal bytes, browser frames, progress) reach the renderer
    // only here; they are never persisted and never move the cursor.
    this.#options.push("stream.frame", frame);
  }
}

/** Install one handler per allowlisted channel and nothing else. */
export function registerBridgeHandlers(ipc: IpcMainLike, bridge: DesktopBridge): readonly string[] {
  const handlers = bridge.handlers();
  const registered: string[] = [];
  for (const channel of PRELOAD_ALLOWLIST) {
    ipc.handle(channel, (_event, payload) => handlers[channel](payload));
    registered.push(channel);
  }
  return registered;
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

function asPost(payload: unknown): { threadId: string | null; content: string; attachments: string[] } {
  const record = asRecord(payload, "messages.post");
  const content = record["content"];
  if (typeof content !== "string" || !content.trim()) {
    throw new Error("messages.post needs content");
  }
  const threadId = record["threadId"];
  if (threadId !== undefined && threadId !== null && (typeof threadId !== "string" || !threadId.startsWith("thr_"))) {
    throw new Error("messages.post threadId must be a thr_ id");
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
  return { threadId: typeof threadId === "string" ? threadId : null, content, attachments };
}

function asUrl(payload: unknown): string {
  if (!payload || typeof payload !== "object" || typeof (payload as { url?: unknown }).url !== "string") {
    throw new Error("deepLink.open needs {url}");
  }
  return (payload as { url: string }).url;
}
