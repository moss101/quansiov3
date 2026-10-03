/**
 * macOS permission onboarding for native computer use (ADP-002, DOMAIN.md §8.6.1 E).
 *
 * A run that needs an OS permission it does not have is parked by the runtime; this module is the
 * desktop's half. It reads what the system says about the four permissions, tells the page which of
 * them are in the way, and — only when the person clicks — opens the matching Privacy pane. It never
 * grants, requests or changes a permission, and nothing here is reachable from the model: the agent
 * does not operate System Settings, so there is no IPC path from agent output to `openSettings`, and
 * the one place a Settings URL exists is this file (`tests/permissions.test.ts` scans for others).
 *
 * The page polls this through `computer.permissions` (see `live/permissions.ts`), and the run resumes
 * only through the person's own `ResumeRun`, which the page enables when every permission the parked
 * run needs reads `granted`.
 */

export const PERMISSION_NAMES = ["accessibility", "screen_recording", "input_monitoring", "microphone"] as const;
export type PermissionName = (typeof PERMISSION_NAMES)[number];

export const PERMISSION_STATES = ["granted", "denied", "not_determined", "restricted"] as const;
export type PermissionState = (typeof PERMISSION_STATES)[number];

/** The status the bridge reports: exactly four permissions, each in one of four states. */
export type PermissionStatus = Readonly<Record<PermissionName, PermissionState>>;

/**
 * The Privacy pane anchor each permission's guide opens. These are the values of
 * `config/computer-use.yaml: permission_guide.panes`; a test holds the two equal.
 */
export const SETTINGS_PANES: Readonly<Record<PermissionName, string>> = {
  accessibility: "Privacy_Accessibility",
  screen_recording: "Privacy_ScreenCapture",
  input_monitoring: "Privacy_ListenEvent",
  microphone: "Privacy_Microphone",
};

/** How often the status is read while the guide is open. Equals `permission_guide.poll_interval_ms`. */
export const PERMISSION_POLL_MS = 2000;

/**
 * How long the person must have been away from the keyboard and pointer before a lapsed
 * foreground-control grant counts as given back. It must be at least the bridge's own pause window
 * (1.5 s), so the page never enables Continue before the helper would report the grant held.
 */
export const PAUSE_IDLE_SECONDS = 2;

/** The Settings URL for a permission's pane. */
export function settingsUrl(permission: PermissionName): string {
  return `x-apple.systempreferences:com.apple.preference.security?${SETTINGS_PANES[permission]}`;
}

export function isPermissionName(value: unknown): value is PermissionName {
  return typeof value === "string" && (PERMISSION_NAMES as readonly string[]).includes(value);
}

/**
 * Parse a status strictly: all four permissions, no others, each a known state. A status that is
 * only partly understood is refused, never completed with a guess that something is granted.
 */
export function parsePermissionStatus(raw: unknown): PermissionStatus {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("a permission status must be an object");
  }
  const record = raw as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (!isPermissionName(key)) {
      throw new Error(`unknown permission ${JSON.stringify(key)}`);
    }
  }
  const status: Partial<Record<PermissionName, PermissionState>> = {};
  for (const name of PERMISSION_NAMES) {
    const state = record[name];
    if (typeof state !== "string" || !(PERMISSION_STATES as readonly string[]).includes(state)) {
      throw new Error(`${name} is missing or not a known state`);
    }
    status[name] = state as PermissionState;
  }
  return status as PermissionStatus;
}

/** What the person can do about one permission. */
export type GuideAction = "none" | "open_settings" | "contact_administrator";

export interface PermissionRow {
  readonly permission: PermissionName;
  readonly state: PermissionState;
  /** The parked run needs it. */
  readonly needed: boolean;
  /** False when this process has no probe for it and `state` is a placeholder, not a reading. */
  readonly probed: boolean;
  readonly action: GuideAction;
}

/** What the page shows and decides on. */
export interface PermissionSnapshot {
  /** False on a host with no such permissions (not macOS). */
  readonly applicable: boolean;
  readonly rows: readonly PermissionRow[];
  /** Whether the person has been away from the keyboard and pointer long enough. Null when unknown. */
  readonly idleSeconds: number | null;
  /** The person was asked to pause input (a lapsed foreground-control grant). */
  readonly pauseInput: boolean;
  /** Every needed permission is granted and, when asked to pause, input has been paused. */
  readonly ready: boolean;
  readonly pollIntervalMs: number;
}

export interface SnapshotInput {
  readonly status: PermissionStatus;
  /** Permissions with no probe in this process; their state is reported but not relied on. */
  readonly unprobed?: readonly PermissionName[];
  readonly needs: readonly PermissionName[];
  readonly pauseInput: boolean;
  readonly idleSeconds: number | null;
}

/** The action the guide offers for a permission in a state. */
export function guideAction(state: PermissionState): GuideAction {
  switch (state) {
    case "granted":
      return "none";
    case "restricted":
      return "contact_administrator";
    case "denied":
    case "not_determined":
      return "open_settings";
  }
}

export function buildSnapshot(input: SnapshotInput): PermissionSnapshot {
  const unprobed = new Set(input.unprobed ?? []);
  const needs = new Set(input.needs);
  const rows = PERMISSION_NAMES.map((permission): PermissionRow => {
    const state = input.status[permission];
    const probed = !unprobed.has(permission);
    return {
      permission,
      state,
      needed: needs.has(permission),
      probed,
      // A permission this process cannot read is not something to send the person to Settings for.
      action: probed ? guideAction(state) : "none",
    };
  });
  const permissionsReady = rows.every((row) => !row.needed || row.state === "granted");
  const paused = !input.pauseInput || (input.idleSeconds !== null && input.idleSeconds >= PAUSE_IDLE_SECONDS);
  return {
    applicable: true,
    rows,
    idleSeconds: input.idleSeconds,
    pauseInput: input.pauseInput,
    ready: permissionsReady && paused,
    pollIntervalMs: PERMISSION_POLL_MS,
  };
}

/** A host with no such permissions. */
export function notApplicable(): PermissionSnapshot {
  return { applicable: false, rows: [], idleSeconds: null, pauseInput: false, ready: true, pollIntervalMs: PERMISSION_POLL_MS };
}

/** What main supplies: a reading of the four permissions, the permissions it cannot probe, the idle time. */
export interface PermissionSource {
  /** Null when the host has no such permissions. */
  readonly read: () => Promise<{ readonly status: PermissionStatus; readonly unprobed: readonly PermissionName[] } | null>;
  /** Seconds since the person's last keyboard or pointer input, when the host can say. */
  readonly idleSeconds: () => number | null;
}

/** The payload of `computer.permissions`. */
export interface PermissionRequest {
  readonly needs: readonly PermissionName[];
  readonly pauseInput: boolean;
}

/** Validate the renderer's request as untrusted data. */
export function parsePermissionRequest(raw: unknown): PermissionRequest {
  if (raw === undefined || raw === null) {
    return { needs: ["accessibility"], pauseInput: false };
  }
  if (typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("computer.permissions needs an object payload");
  }
  const record = raw as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (key !== "needs" && key !== "pauseInput") {
      throw new Error(`computer.permissions has an unknown field ${JSON.stringify(key)}`);
    }
  }
  const needs = record["needs"] === undefined ? ["accessibility"] : record["needs"];
  if (!Array.isArray(needs) || needs.length > PERMISSION_NAMES.length || !needs.every(isPermissionName)) {
    throw new Error("computer.permissions needs must list permission names");
  }
  const pauseInput = record["pauseInput"] === undefined ? false : record["pauseInput"];
  if (typeof pauseInput !== "boolean") {
    throw new Error("computer.permissions pauseInput must be a boolean");
  }
  return { needs: [...new Set(needs)], pauseInput };
}

/** Validate `computer.openSettings`: a permission name and nothing else, never a URL. */
export function parseOpenSettings(raw: unknown): PermissionName {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) {
    throw new Error("computer.openSettings needs an object payload");
  }
  const record = raw as Record<string, unknown>;
  if (Object.keys(record).some((key) => key !== "permission")) {
    throw new Error("computer.openSettings takes only a permission name");
  }
  const permission = record["permission"];
  if (!isPermissionName(permission)) {
    throw new Error("computer.openSettings needs a permission name");
  }
  return permission;
}

/** Read the source once and build the snapshot for a request. */
export async function readSnapshot(source: PermissionSource, request: PermissionRequest): Promise<PermissionSnapshot> {
  const reading = await source.read();
  if (reading === null) {
    return notApplicable();
  }
  return buildSnapshot({
    status: reading.status,
    unprobed: reading.unprobed,
    needs: request.needs,
    pauseInput: request.pauseInput,
    idleSeconds: source.idleSeconds(),
  });
}

/**
 * Electron's `systemPreferences.getMediaAccessStatus` answer as a typed state. Anything it does not
 * say plainly is `not_determined`, never `granted`.
 */
export function mapMediaAccess(status: string): PermissionState {
  switch (status) {
    case "granted":
      return "granted";
    case "denied":
      return "denied";
    case "restricted":
      return "restricted";
    default:
      return "not_determined";
  }
}

/**
 * Permissions for which Electron offers no probe. Input Monitoring is one: the helper's own status
 * (the bridge's `permission_status`) is the authority for it, and until the desktop can read the
 * helper's answer the page shows it as not checked instead of guessing. No computer action needs it.
 */
export const UNPROBED_PERMISSIONS: readonly PermissionName[] = ["input_monitoring"];
