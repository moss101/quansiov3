/**
 * Knowledge, memory and skill controls (APP-011).
 *
 * Semantic memory is not recovery. Deleted entries disappear from retrieval.
 * Unapproved skill versions cannot be activated from the UI.
 */

export type SkillStatus =
  | "DRAFT"
  | "CANDIDATE"
  | "EVALUATING"
  | "APPROVED"
  | "ACTIVE"
  | "REJECTED";

export interface KnowledgeEntry {
  readonly id: string;
  readonly status: "active" | "deleted" | "superseded" | "quarantined";
  readonly kind: "knowledge" | "memory";
}

export interface SkillVersion {
  readonly id: string;
  readonly status: SkillStatus;
}

export function retrievalIndex(entries: readonly KnowledgeEntry[]): readonly string[] {
  return entries.filter((entry) => entry.status === "active").map((entry) => entry.id);
}

export function deleteEntry(
  entries: readonly KnowledgeEntry[],
  id: string,
): readonly KnowledgeEntry[] {
  return entries.map((entry) => (entry.id === id ? { ...entry, status: "deleted" } : entry));
}

export function canActivate(version: SkillVersion): boolean {
  return version.status === "APPROVED" || version.status === "ACTIVE";
}

export function activate(version: SkillVersion): SkillVersion {
  if (!canActivate(version)) {
    throw new Error("unapproved skill cannot be activated through the UI");
  }
  return { ...version, status: "ACTIVE" };
}

/** Checkpoint/recovery fields must never appear on a memory entry in this UI. */
export function isRecoveryField(name: string): boolean {
  return /checkpoint|cursor|generation|lease|protocol_state|resume_token/.test(name);
}
