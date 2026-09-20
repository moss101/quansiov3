/**
 * Knowledge and memory (APP-011, INT-006, INT-007): what Quansio knows and remembers, with
 * where it came from, and the two controls a person has over it — forget and supersede.
 *
 * The page decodes `GET /v1/knowledge` and `GET /v1/memory` into views that carry
 * provenance, scope, confidence and lifecycle state. Retrievability is the server's word
 * (`retrievable`): only ACTIVE knowledge answers retrieval, and only ACTIVE, unexpired
 * memory does; a deleted or superseded entry is a tombstone shown as such, never as
 * knowledge. A delete or supersede is sent as `DeleteKnowledge` / `SupersedeKnowledge` /
 * `DeleteMemory`; the page refuses an edge the §11.4 ladder does not have before any
 * command is sent, and the server refuses it again (`CONFLICT_STATE`).
 *
 * Memory is not recovery (DOSSIER.md D-007): a memory row that carries a recovery field
 * is refused by the decoder and reported, so checkpoint, cursor or protocol state can
 * never be rendered as a memory.
 */

export type KnowledgeStatus = "candidate" | "verified" | "active" | "superseded" | "quarantined" | "deleted";
export type MemoryStatus = "candidate" | "active" | "deleted";

export const KNOWLEDGE_STATUSES: readonly KnowledgeStatus[] = ["candidate", "verified", "active", "superseded", "quarantined", "deleted"];
export const MEMORY_STATUSES: readonly MemoryStatus[] = ["candidate", "active", "deleted"];

/** Knowledge states from which `deleted` is a legal edge (DOMAIN.md §11.4). */
export const KNOWLEDGE_DELETE_FROM: readonly KnowledgeStatus[] = ["candidate", "verified", "active", "superseded", "quarantined"];
/** Knowledge states from which `superseded` is a legal edge (DOMAIN.md §11.4). */
export const KNOWLEDGE_SUPERSEDE_FROM: readonly KnowledgeStatus[] = ["verified", "active"];
/** Memory states from which `deleted` is a legal edge (DOMAIN.md §11.4). */
export const MEMORY_DELETE_FROM: readonly MemoryStatus[] = ["candidate", "active"];

export const DELETE_KNOWLEDGE_COMMAND = "DeleteKnowledge";
export const SUPERSEDE_KNOWLEDGE_COMMAND = "SupersedeKnowledge";
export const DELETE_MEMORY_COMMAND = "DeleteMemory";

export interface Provenance {
  readonly sourceKind: string;
  readonly ref: string;
  readonly digest: string | null;
  readonly retrievedAt: string | null;
}

export interface KnowledgeView {
  readonly id: string;
  readonly workspaceId: string | null;
  readonly scope: string;
  readonly kind: string;
  readonly contentRef: string | null;
  readonly content: unknown;
  readonly provenance: readonly Provenance[];
  readonly confidence: number;
  readonly version: number;
  readonly status: KnowledgeStatus;
  /** The server's word: only ACTIVE knowledge answers retrieval. */
  readonly retrievable: boolean;
  readonly supersededBy: string | null;
  readonly createdAt: string;
  readonly updatedAt: string;
}

export interface MemoryView {
  readonly id: string;
  readonly workspaceId: string | null;
  readonly scope: string;
  readonly subjectRef: string;
  readonly content: string;
  readonly provenance: { readonly kind: string; readonly ref: string | null };
  readonly confidence: number;
  readonly status: MemoryStatus;
  /** The server's word: ACTIVE and not past its expiry. */
  readonly retrievable: boolean;
  readonly lastUsedAt: string | null;
  readonly expiresAt: string | null;
  readonly createdAt: string;
  readonly updatedAt: string;
}

/** The only fields a memory row may carry; anything else is not memory. */
export const MEMORY_FIELDS: readonly string[] = [
  "id",
  "workspace_id",
  "scope",
  "subject_ref",
  "content",
  "provenance",
  "confidence",
  "status",
  "retrievable",
  "last_used_at",
  "expires_at",
  "created_at",
  "updated_at",
];

/** Names that belong to recovery, never to memory (INT-007's structural rule). */
const RECOVERY_FIELD = /checkpoint|cursor|position|effect_id|generation|lease|protocol_state|resume_token|attempt|backoff/;

/** True when a field name belongs to the recovery layers. */
export function isRecoveryField(name: string): boolean {
  return RECOVERY_FIELD.test(name);
}

function text(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function number(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function list(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

/** Decode `GET /v1/knowledge`; a row without an id or a lifecycle state is dropped. */
export function decodeKnowledge(payload: unknown): KnowledgeView[] {
  const views: KnowledgeView[] = [];
  for (const raw of list(record(payload)["knowledge"])) {
    const row = record(raw);
    const id = text(row["id"]);
    const status = text(row["status"]);
    if (!id || !id.startsWith("kn_") || !status || !(KNOWLEDGE_STATUSES as readonly string[]).includes(status)) {
      continue;
    }
    views.push({
      id,
      workspaceId: text(row["workspace_id"]),
      scope: text(row["scope"]) ?? "",
      kind: text(row["kind"]) ?? "",
      contentRef: text(row["content_ref"]),
      content: row["content"] ?? null,
      provenance: list(row["provenance"]).map(record).flatMap((item) => {
        const sourceKind = text(item["source_kind"]);
        const ref = text(item["ref"]);
        return sourceKind && ref ? [{ sourceKind, ref, digest: text(item["digest"]), retrievedAt: text(item["retrieved_at"]) }] : [];
      }),
      confidence: number(row["confidence"]) ?? 0,
      version: number(row["version"]) ?? 1,
      status: status as KnowledgeStatus,
      retrievable: row["retrievable"] === true,
      supersededBy: text(row["superseded_by"]),
      createdAt: text(row["created_at"]) ?? "",
      updatedAt: text(row["updated_at"]) ?? "",
    });
  }
  return views;
}

export interface DecodedMemory {
  readonly entries: readonly MemoryView[];
  /** Rows the page refused to show as memory, with the fields that disqualified them. */
  readonly refused: readonly { readonly id: string | null; readonly fields: readonly string[] }[];
}

/**
 * Decode `GET /v1/memory`. A row is memory only when every field it carries is one of
 * {@link MEMORY_FIELDS}; a row with a recovery field, or any field outside that set, is
 * refused rather than rendered with the extra ignored.
 */
export function decodeMemory(payload: unknown): DecodedMemory {
  const entries: MemoryView[] = [];
  const refused: { id: string | null; fields: string[] }[] = [];
  for (const raw of list(record(payload)["memory"])) {
    const row = record(raw);
    const id = text(row["id"]);
    const foreign = Object.keys(row).filter((key) => !MEMORY_FIELDS.includes(key) || isRecoveryField(key));
    if (foreign.length > 0) {
      refused.push({ id, fields: foreign.sort() });
      continue;
    }
    const status = text(row["status"]);
    if (!id || !id.startsWith("mem_") || !status || !(MEMORY_STATUSES as readonly string[]).includes(status)) {
      continue;
    }
    const provenance = record(row["provenance"]);
    entries.push({
      id,
      workspaceId: text(row["workspace_id"]),
      scope: text(row["scope"]) ?? "",
      subjectRef: text(row["subject_ref"]) ?? "",
      content: text(row["content"]) ?? "",
      provenance: { kind: text(provenance["kind"]) ?? "", ref: text(provenance["ref"]) },
      confidence: number(row["confidence"]) ?? 0,
      status: status as MemoryStatus,
      retrievable: row["retrievable"] === true,
      lastUsedAt: text(row["last_used_at"]),
      expiresAt: text(row["expires_at"]),
      createdAt: text(row["created_at"]) ?? "",
      updatedAt: text(row["updated_at"]) ?? "",
    });
  }
  return { entries, refused };
}

/** What future retrieval may answer with, as the server says it — never inferred from status alone. */
export function retrievable<T extends { readonly retrievable: boolean }>(entries: readonly T[]): readonly T[] {
  return entries.filter((entry) => entry.retrievable);
}

/** Entries that are tombstones: still shown, with their provenance, never as knowledge. */
export function tombstones(entries: readonly KnowledgeView[]): readonly KnowledgeView[] {
  return entries.filter((entry) => entry.status === "deleted" || entry.status === "superseded");
}

export type Control =
  | { readonly ok: true; readonly command: string; readonly params: Record<string, string> }
  | { readonly ok: false; readonly code: "CONFLICT_STATE" | "VALIDATION_SCHEMA"; readonly reason: string };

/** `DeleteKnowledge`, or the refusal the ladder gives before the command is sent. */
export function deleteKnowledge(entry: KnowledgeView): Control {
  if (!KNOWLEDGE_DELETE_FROM.includes(entry.status)) {
    return { ok: false, code: "CONFLICT_STATE", reason: `${entry.status} knowledge cannot be deleted; deletion is terminal` };
  }
  return { ok: true, command: DELETE_KNOWLEDGE_COMMAND, params: { knowledge_id: entry.id } };
}

/** `SupersedeKnowledge` with an existing successor, or the refusal. */
export function supersedeKnowledge(entry: KnowledgeView, successor: KnowledgeView): Control {
  if (!KNOWLEDGE_SUPERSEDE_FROM.includes(entry.status)) {
    return { ok: false, code: "CONFLICT_STATE", reason: `only verified or active knowledge can be superseded, not ${entry.status}` };
  }
  if (successor.id === entry.id) {
    return { ok: false, code: "VALIDATION_SCHEMA", reason: "an entry cannot supersede itself" };
  }
  if (successor.status === "deleted") {
    return { ok: false, code: "CONFLICT_STATE", reason: "a deleted entry cannot be a successor" };
  }
  return { ok: true, command: SUPERSEDE_KNOWLEDGE_COMMAND, params: { knowledge_id: entry.id, superseded_by: successor.id } };
}

/** `DeleteMemory`, or the refusal. */
export function deleteMemory(entry: MemoryView): Control {
  if (!MEMORY_DELETE_FROM.includes(entry.status)) {
    return { ok: false, code: "CONFLICT_STATE", reason: `${entry.status} memory cannot be deleted; deletion is terminal` };
  }
  return { ok: true, command: DELETE_MEMORY_COMMAND, params: { memory_id: entry.id } };
}

/** Entries that may stand in for `entry`: in force or on the way, never itself, never a tombstone. */
export function successorsFor(entry: KnowledgeView, all: readonly KnowledgeView[]): readonly KnowledgeView[] {
  return all.filter((candidate) => candidate.id !== entry.id && candidate.status !== "deleted" && candidate.status !== "superseded");
}

/** One line of provenance for a card. */
export function describeProvenance(entry: KnowledgeView): string {
  return entry.provenance.map((item) => `${item.sourceKind}:${item.ref}`).join(", ") || "no provenance";
}
