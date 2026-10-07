/**
 * The local-file host (ADP-005, DOMAIN.md §13.7): one object that composes grants, granted roots and entry ids, media
 * URLs, safe edits and the shared watcher, and answers their IPC channels.
 *
 * It owns no authority and imports no Electron. `app.ts` supplies the native dialogs and the media protocol; the
 * window-type and own-thread rules were enforced before a handler here runs (`ipc-trust.ts`), and every payload is
 * validated against a schema first (`schema.ts`), so a handler may assume its caller is a window allowed the channel and
 * that its payload has exactly the fields named and nothing else. No handler takes a path: a payload carries an opaque id
 * main issued, and a call for an id that window was never given answers `FILE_OUTSIDE_ROOTS`, the same as a path that
 * left its root.
 *
 * When a window closes, everything held for it is revoked together: its grants (and the descriptors in them), the
 * files it was handed, its roots and entries, its edit sessions, its media tokens and its watches.
 */

import type { FileChannel } from "../../preload/bridge.js";
import type { WindowHandler } from "../ipc-trust.js";
import * as s from "../schema.js";
import { THREAD_ID, type ManagedWindow } from "../windows.js";
import { type LocalFilesConfig } from "./config.js";
import { inside, realOrNull } from "./fs-ops.js";
import { GrantRegistry, REAL_SCHEDULER, type GrantView, type Scheduler } from "./grants.js";
import { MediaRegistry, type MediaRequest, type MediaResponse } from "./media.js";
import { done, refuse, refusedFile, type Answer, type Refusal } from "./results.js";
import { RootRegistry, isEntryId, isRootId } from "./roots.js";
import { EditSessions, isEditId } from "./safe-edit.js";
import { NODE_WATCH_ADAPTER, WatcherHub, isSubscriptionId, type DirWatchAdapter } from "./watcher.js";

/** The native dialogs (main supplies Electron's `dialog`; a test supplies what a person would pick). */
export interface LocalFileDialogs {
  /** A native open dialog for files: the person's picks, or `null` when they cancel. */
  pickFiles(window: ManagedWindow): Promise<readonly string[] | null>;
  /** A native open dialog for one folder. */
  pickFolder(window: ManagedWindow): Promise<string | null>;
}

/** The slice of the bridge the attach step uses: the existing `CreateArtifact` upload. */
export interface LocalFilesBridge {
  uploadAttachment(raw: unknown): Promise<unknown>;
}

/** The slice of the window registry the host uses. */
export interface LocalFilesWindows {
  byWebContents(id: number): ManagedWindow | null;
  deliver(window: ManagedWindow, channel: string, payload: unknown): void;
}

export interface LocalFilesHostOptions {
  readonly config: LocalFilesConfig;
  readonly windows: LocalFilesWindows;
  readonly dialogs: LocalFileDialogs;
  readonly bridge: LocalFilesBridge;
  /** The largest file the existing upload accepts (`MAX_UPLOAD_BYTES`); a larger granted file is reported, not sent. */
  readonly maxUploadBytes: number;
  readonly log: (line: string) => void;
  readonly now?: () => number;
  readonly scheduler?: Scheduler;
  readonly watchAdapter?: DirWatchAdapter;
  /** The folder the person's home is (for refusing it as a root); tests point it elsewhere. */
  readonly home?: string;
  /** Require the media request to say which window sent it (see `MediaDeps.requireRequester`). */
  readonly requireMediaRequester?: boolean;
  /** Test seam: called after each edit's temporary file is written. */
  readonly afterTemporary?: (target: string) => void;
  /**
   * Folders the person's own picks may not reach: the shell's own data directory holds the session and its keys, so a file
   * inside it is never granted and a folder that is, contains or sits inside one is never a root (`root_not_allowed`).
   */
  readonly protectedPaths?: readonly string[];
}

const TOKEN = s.string({ min: 64, max: 64, pattern: /^[0-9a-f]{64}$/ });
const ROOT_ID = s.string({ min: 38, max: 38, pattern: /^lroot_[0-9a-f]{32}$/ });
const ENTRY_ID = s.string({ min: 36, max: 36, pattern: /^ent_[0-9a-f]{32}$/ });
const EDIT_ID = s.string({ min: 37, max: 37, pattern: /^edit_[0-9a-f]{32}$/ });
const SUBSCRIPTION_ID = s.string({ min: 20, max: 20, pattern: /^sub_[0-9a-f]{16}$/ });
const THREAD = s.string({ min: 30, max: 30, pattern: THREAD_ID });

const NOTHING = s.empty();
const PICK = s.object<{ threadId?: string | null }>({ threadId: s.optional(s.nullable(THREAD)) });
const ATTACH = s.object<{ token: string; threadId?: string | null }>({ token: TOKEN, threadId: s.optional(s.nullable(THREAD)) });
const ONLY_TOKEN = s.object<{ token: string }>({ token: TOKEN });
const PREVIEW = s.object<{ token: string; index: number }>({ token: TOKEN, index: s.integer({ min: 0, max: 99 }) });
const ROOT_ONLY = s.object<{ rootId: string }>({ rootId: ROOT_ID });
const ENTRIES = s.object<{ rootId: string; entryId?: string | null }>({ rootId: ROOT_ID, entryId: s.optional(s.nullable(ENTRY_ID)) });
const ENTRY_ONLY = s.object<{ entryId: string }>({ entryId: ENTRY_ID });
const EDIT_ONLY = s.object<{ editId: string }>({ editId: EDIT_ID });
const UNSUBSCRIBE = s.object<{ subscriptionId: string }>({ subscriptionId: SUBSCRIPTION_ID });

type Handlers = Record<FileChannel, WindowHandler>;

/** What a window is told about a skipped file when it is attached. */
export interface AttachSkip {
  readonly name: string;
  readonly reason: "too_large_for_upload" | "empty" | "upload_failed";
  readonly message: string;
}

export class LocalFilesHost {
  readonly #o: LocalFilesHostOptions;
  readonly grants: GrantRegistry;
  readonly roots: RootRegistry;
  readonly media: MediaRegistry;
  readonly edits: EditSessions;
  readonly watcher: WatcherHub;
  /** Windows with a native dialog open: a second request while one is open is dropped. */
  readonly #dialogs = new Set<number>();

  constructor(options: LocalFilesHostOptions) {
    this.#o = options;
    const config = options.config;
    const now = options.now ?? Date.now;
    const scheduler = options.scheduler ?? REAL_SCHEDULER;
    let media: MediaRegistry | null = null;
    let edits: EditSessions | null = null;
    let watcher: WatcherHub | null = null;
    this.grants = new GrantRegistry({
      limits: { ttlMs: config.grantTtlMs, maxLive: config.grantMaxLive, maxFiles: config.grantMaxFiles, maxFileBytes: config.grantMaxFileBytes, maxTotalBytes: config.grantMaxTotalBytes },
      now,
      scheduler,
      onRevoked: (key) => {
        media?.revokeGrant(key);
      },
    });
    this.roots = new RootRegistry(
      { rootsMax: config.rootsMax, entriesMax: config.entriesMax, listMax: config.watchMaxEntries },
      (_windowId, rootId) => {
        media?.revokeRoot(rootId);
        edits?.revokeRoot(rootId);
        watcher?.revokeRoot(rootId);
      },
      options.home,
    );
    this.media = media = new MediaRegistry({
      maxTokens: config.mediaMaxTokens,
      grants: this.grants,
      roots: this.roots,
      ...(options.requireMediaRequester === undefined ? {} : { requireRequester: options.requireMediaRequester }),
    });
    this.watcher = watcher = new WatcherHub({
      limits: { debounceMs: config.watchDebounceMs, maxDirs: config.watchMaxDirs, maxSubscribers: config.watchMaxSubscribers, queueMax: config.watchQueueMax, maxEntries: config.watchMaxEntries, selfWriteTtlMs: config.selfWriteTtlMs },
      adapter: options.watchAdapter ?? NODE_WATCH_ADAPTER,
      scheduler,
      now,
      deliver: (windowId, delivery) => {
        const window = options.windows.byWebContents(windowId);
        if (window !== null) {
          options.windows.deliver(window, "files.changes", delivery);
        }
      },
      mint: (subscription, name) => this.roots.mint(subscription.windowId, subscription.rootId, subscription.relative === "" ? name : `${subscription.relative}/${name}`),
    });
    this.edits = edits = new EditSessions({
      maxBytes: config.editMaxBytes,
      maxSessions: config.editMaxSessions,
      resolve: (windowId, entryId) => this.roots.resolve(windowId, entryId),
      onWritten: (absolute, baseline) => {
        this.watcher.noteSelfWrite(absolute, baseline);
      },
      ...(options.afterTemporary === undefined ? {} : { hooks: { afterTemporary: options.afterTemporary } }),
    });
  }

  /** A window closed: everything held for it goes with it. */
  windowClosed(window: ManagedWindow): void {
    const id = window.handle.webContentsId;
    this.grants.revokeWindow(id);
    this.roots.revokeWindow(id);
    this.edits.revokeWindow(id);
    this.media.revokeWindow(id);
    this.watcher.revokeWindow(id);
    this.#dialogs.delete(id);
  }

  /** The application is quitting. */
  dispose(): void {
    this.grants.revokeAll();
    this.watcher.dispose();
  }

  /** The media protocol's handler (`app.ts` registers it for the scheme). */
  mediaResponse(request: MediaRequest): MediaResponse {
    return this.#o.config.enabled ? this.media.handle(request) : { status: 404, headers: { "Cache-Control": "no-store" }, body: null };
  }

  /**
   * Open a native dialog for `window` and hold what the person picks as a grant. A request while this window's dialog
   * is already open is dropped. The one entry point for a button (`files.pick`) and for the `attach` deep link.
   */
  async pick(window: ManagedWindow): Promise<Answer<{ readonly grant: GrantView | null }>> {
    const id = window.handle.webContentsId;
    if (!this.#o.config.enabled) {
      return refuse("POLICY_DENIED", "local files are switched off");
    }
    if (this.#dialogs.has(id)) {
      this.#o.log("quansio desktop: a file dialog is already open in that window; the request was dropped");
      return refuse("RATE_LIMITED", "a file dialog is already open");
    }
    this.#dialogs.add(id);
    let picked: readonly string[] | null;
    try {
      picked = await this.#o.dialogs.pickFiles(window);
    } finally {
      this.#dialogs.delete(id);
    }
    // The window may have closed while the dialog was open: nothing is held for a window that is gone.
    if (window.closed || picked === null || picked.length === 0) {
      return done({ grant: null });
    }
    const forbidden = this.#protectedFile(picked);
    if (forbidden !== null) {
      return refusedFile("root_not_allowed", "that file belongs to Quansio itself and cannot be attached", forbidden);
    }
    const created = this.grants.create(id, picked);
    if (!created.ok) {
      return refusedFile(created.failure.reason, grantRefusalText(created.failure.reason), created.failure.name === "" ? undefined : created.failure.name);
    }
    return done({ grant: created.grant });
  }

  /** The `attach` deep link: show the dialog and hand the page the grant it produced. */
  async requestAttach(window: ManagedWindow, threadId: string | null): Promise<void> {
    const answer = await this.pick(window);
    if (answer.ok && answer.result.grant !== null) {
      this.#o.windows.deliver(window, "files.grant", { grant: answer.result.grant, threadId });
    } else if (!answer.ok) {
      this.#o.windows.deliver(window, "files.grant", { error: answer.error, threadId });
    }
  }

  /** Claim a grant and send each file through the existing upload; a file over its bound is reported, not sent. */
  async attach(window: ManagedWindow, token: string, threadId: string | null): Promise<Answer<{ readonly attached: readonly unknown[]; readonly skipped: readonly AttachSkip[] }>> {
    const claim = this.grants.claim(window.handle.webContentsId, token);
    if (!claim.ok) {
      return invalidGrant(claim.reason);
    }
    const attached: unknown[] = [];
    const skipped: AttachSkip[] = [];
    try {
      for (const file of claim.files) {
        if (file.sizeBytes === 0) {
          skipped.push({ name: file.name, reason: "empty", message: "an empty file cannot be attached" });
        } else if (file.sizeBytes > this.#o.maxUploadBytes) {
          skipped.push({ name: file.name, reason: "too_large_for_upload", message: `attachments are limited to ${String(this.#o.maxUploadBytes)} bytes` });
        } else {
          try {
            const bytes = file.read();
            attached.push(
              await this.#o.bridge.uploadAttachment({ name: file.name, mediaType: file.mediaType, bytesBase64: bytes.toString("base64"), ...(threadId === null ? {} : { threadId }) }),
            );
          } catch (error) {
            skipped.push({ name: file.name, reason: "upload_failed", message: error instanceof Error ? error.message : String(error) });
          }
        }
      }
    } finally {
      for (const file of claim.files) {
        file.close();
      }
    }
    return done({ attached, skipped });
  }

  /** The name of the first picked file that is inside a protected folder, or `null`. A path that cannot be resolved is not protected here (opening it fails). */
  #protectedFile(picked: readonly string[]): string | null {
    for (const path of picked) {
      const real = realOrNull(path);
      if (real !== null && this.#protectedReal().some((guarded) => inside(real, guarded))) {
        return path.split(/[\\/]/).pop() ?? "";
      }
    }
    return null;
  }

  /** Whether a picked folder is, contains or sits inside a protected folder. */
  #protectedFolder(folder: string): boolean {
    const real = realOrNull(folder);
    return real !== null && this.#protectedReal().some((guarded) => inside(real, guarded) || inside(guarded, real));
  }

  #protectedReal(): string[] {
    return (this.#o.protectedPaths ?? []).map((path) => realOrNull(path) ?? path);
  }

  // ---- the channels ----

  handlers(): Handlers {
    const o = this.#o;
    const guarded =
      <T>(schema: s.Schema<T>, run: (window: ManagedWindow, input: T) => unknown): WindowHandler =>
      (window, payload) => {
        if (!o.config.enabled) {
          return Promise.resolve(refuse("POLICY_DENIED", "local files are switched off"));
        }
        const parsed = s.safeParse(schema, payload);
        if (!parsed.ok) {
          return Promise.resolve(refuse("VALIDATION_SCHEMA", parsed.error.message));
        }
        return Promise.resolve(run(window, parsed.value));
      };
    const windowId = (window: ManagedWindow): number => window.handle.webContentsId;

    return {
      "files.pick": guarded(PICK, (window) => this.pick(window)),
      "files.grant.attach": guarded(ATTACH, (window, input) => this.attach(window, input.token, input.threadId ?? null)),
      "files.grant.discard": guarded(ONLY_TOKEN, (window, input) => done({ discarded: this.grants.discard(windowId(window), input.token) })),
      "files.grant.preview": guarded(PREVIEW, (window, input): Answer<{ url: string }> => {
        const held = this.grants.lookup(windowId(window), input.token);
        const file = held?.files[input.index];
        if (held === null || file === undefined) {
          return invalidGrant("invalid");
        }
        return done({ url: this.media.mint(windowId(window), { kind: "grant", key: held.key, index: input.index }, file.name) });
      }),
      "files.root.pick": guarded(NOTHING, async (window) => {
        const id = windowId(window);
        if (this.#dialogs.has(id)) {
          return refuse("RATE_LIMITED", "a file dialog is already open");
        }
        this.#dialogs.add(id);
        let folder: string | null;
        try {
          folder = await o.dialogs.pickFolder(window);
        } finally {
          this.#dialogs.delete(id);
        }
        if (window.closed || folder === null) {
          return done({ root: null });
        }
        if (this.#protectedFolder(folder)) {
          return refusedFile("root_not_allowed", "that folder belongs to Quansio itself and cannot be opened");
        }
        const granted = this.roots.grant(id, folder);
        return granted.ok ? done({ root: granted.result }) : granted;
      }),
      "files.root.list": guarded(NOTHING, (window) => done({ roots: this.roots.rootsOf(windowId(window)) })),
      "files.root.release": guarded(ROOT_ONLY, (window, input) => done({ released: this.roots.release(windowId(window), input.rootId) })),
      "files.root.entries": guarded(ENTRIES, (window, input) => this.roots.list(windowId(window), input.rootId, input.entryId ?? null)),
      "files.media.url": guarded(ENTRY_ONLY, (window, input): Answer<{ url: string }> => {
        const resolved = this.roots.resolve(windowId(window), input.entryId);
        if (!resolved.ok) {
          return resolved;
        }
        if (resolved.result.kind !== "file") {
          return refusedFile("not_regular", "only a file can be previewed");
        }
        return done({ url: this.media.mint(windowId(window), { kind: "entry", entryId: input.entryId, rootId: resolved.result.rootId }, resolved.result.name) });
      }),
      "files.edit.open": guarded(ENTRY_ONLY, (window, input) => this.edits.open(windowId(window), input.entryId)),
      "files.edit.save": guarded(
        s.object<{ editId: string; text: string; onConflict: "fail" | "overwrite" }>({
          editId: EDIT_ID,
          text: s.string({ max: o.config.editMaxBytes }),
          onConflict: s.oneOf(["fail", "overwrite"] as const),
        }),
        (window, input) => this.edits.save(windowId(window), input.editId, input.text, input.onConflict),
      ),
      "files.edit.close": guarded(EDIT_ONLY, (window, input) => done({ closed: this.edits.close(windowId(window), input.editId) })),
      "files.watch.subscribe": guarded(ENTRIES, (window, input): Answer<{ subscriptionId: string }> => {
        const id = windowId(window);
        const directory = input.entryId == null ? this.roots.resolveRoot(id, input.rootId) : this.roots.resolve(id, input.entryId);
        if (!directory.ok) {
          return directory;
        }
        if (directory.result.rootId !== input.rootId || directory.result.kind !== "directory") {
          return refuse("FILE_OUTSIDE_ROOTS", "that is not a folder of that root");
        }
        return this.watcher.subscribe(id, input.rootId, directory.result.relative, directory.result.absolute);
      }),
      "files.watch.unsubscribe": guarded(UNSUBSCRIBE, (window, input) => done({ unsubscribed: this.watcher.unsubscribe(windowId(window), input.subscriptionId) })),
    };
  }
}

function invalidGrant(reason: "invalid" | "expired"): Refusal {
  return refuse("FILE_GRANT_INVALID", reason === "expired" ? "that file grant expired; pick the files again" : "that is not a file grant this window holds", { reason });
}

function grantRefusalText(reason: string): string {
  switch (reason) {
    case "symlink":
      return "a link to a file cannot be attached; pick the file itself";
    case "not_regular":
      return "only regular files can be attached";
    case "too_large":
      return "that file is larger than a grant allows";
    case "too_many":
      return "too many files were picked at once";
    case "total_too_large":
      return "the files together are larger than a grant allows";
    case "limit":
      return "too many file grants are open; try again in a moment";
    default:
      return "that file could not be read";
  }
}

export { isEditId, isEntryId, isRootId, isSubscriptionId };
