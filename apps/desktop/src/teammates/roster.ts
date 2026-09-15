/**
 * Teammate roster UI model (APP-014).
 */

export interface RosterEntry {
  readonly id: string;
  readonly name: string;
  readonly status: "active" | "archived";
  readonly activeRunId: string | null;
}

export function visibleRoster(entries: readonly RosterEntry[]): readonly RosterEntry[] {
  return entries;
}

export function archiveInRoster(entries: readonly RosterEntry[], id: string): readonly RosterEntry[] {
  return entries.map((entry) => (entry.id === id ? { ...entry, status: "archived", activeRunId: null } : entry));
}
