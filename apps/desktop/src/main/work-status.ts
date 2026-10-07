/**
 * What the shell knows about running work (ADP-004, DOMAIN.md §13.6 A, E, G).
 *
 * Two sources are combined: the server's maintenance status (the authority on what is executing) and what this
 * desktop has seen on its own stream (fresher by a few milliseconds, and the only source when the server cannot
 * be reached). The result decides whether quitting and installing an update may proceed silently. It is never
 * read as "idle" on a failure: a status that cannot be read is `unknown`, and the callers ask the person.
 */

import { t } from "../i18n/strings.js";

/** The server's answer, decoded. */
export interface MaintenanceStatus {
  readonly idle: boolean;
  readonly activeTurns: number;
  readonly inflightActions: number;
  readonly openTerminals: number;
  readonly asOf: string;
}

/** Decode `GET /v1/maintenance`; `null` for anything that is not that projection. */
export function decodeMaintenance(raw: unknown): MaintenanceStatus | null {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    return null;
  }
  const record = raw as Record<string, unknown>;
  const count = (key: string): number | null => {
    const value = record[key];
    return typeof value === "number" && Number.isInteger(value) && value >= 0 ? value : null;
  };
  const activeTurns = count("active_turns");
  const inflightActions = count("inflight_actions");
  const openTerminals = count("open_terminals");
  const asOf = record["as_of"];
  if (activeTurns === null || inflightActions === null || openTerminals === null || typeof record["idle"] !== "boolean" || typeof asOf !== "string") {
    return null;
  }
  // The flag is derived from the counts; a response that disagrees with itself is not trusted as idle.
  if (record["idle"] !== (activeTurns === 0 && inflightActions === 0)) {
    return null;
  }
  return { idle: record["idle"], activeTurns, inflightActions, openTerminals, asOf };
}

export type WorkStatus =
  | { readonly kind: "idle" }
  | { readonly kind: "busy"; readonly activeTurns: number; readonly inflightActions: number }
  | { readonly kind: "unknown" };

/**
 * Combine the server's status (`null` when it could not be read) with the Turns this desktop has seen start and
 * not yet finish. Busy if either says busy; idle only when the server says idle and nothing local is running;
 * unknown when the server could not be read and nothing local is running.
 */
export function workStatus(server: MaintenanceStatus | null, localRunning: number): WorkStatus {
  const activeTurns = Math.max(server?.activeTurns ?? 0, localRunning);
  const inflightActions = server?.inflightActions ?? 0;
  if (activeTurns > 0 || inflightActions > 0) {
    return { kind: "busy", activeTurns, inflightActions };
  }
  return server === null ? { kind: "unknown" } : { kind: "idle" };
}

/** A probe the gates call: the combined status now. */
export type WorkProbe = () => Promise<WorkStatus>;

/** What a dialog says about work that is not known to be idle. */
export function describeWork(status: Exclude<WorkStatus, { kind: "idle" }>): string {
  if (status.kind === "unknown") {
    return t("workUnknown");
  }
  const parts: string[] = [];
  if (status.activeTurns > 0) {
    parts.push(status.activeTurns === 1 ? t("workTurnsOne") : t("workTurnsMany").replace("{n}", String(status.activeTurns)));
  }
  if (status.inflightActions > 0) {
    parts.push(status.inflightActions === 1 ? t("workActionsOne") : t("workActionsMany").replace("{n}", String(status.inflightActions)));
  }
  return `${parts.join("; ")}.`;
}
