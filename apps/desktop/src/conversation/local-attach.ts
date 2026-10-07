/**
 * Attaching files from the person's disk (ADP-005, DOMAIN.md 13.7 A): the composer's side of a file grant.
 *
 * The page asks main for a native dialog and is told what was picked as names and sizes plus an opaque token. It never
 * sees a path or a file's bytes: it asks main to claim the token for a thread, and main uploads each file through the
 * ordinary `CreateArtifact` path and answers with the artifacts, which become composer attachments exactly like any other.
 * The token lives in a variable for as long as this call takes and is never stored.
 */

import { t } from "../i18n/strings.js";
import { attachmentFromUpload } from "./composer.js";
import type { ComposerAttachment } from "./state.js";

/** The calls the composer needs (a slice of `window.quansio`). */
export interface LocalAttachApi {
  readonly pickLocalFiles: (request: { threadId?: string | null }) => Promise<unknown>;
  readonly attachLocalFiles: (request: { token: string; threadId?: string | null }) => Promise<unknown>;
  readonly discardLocalFiles: (token: string) => Promise<unknown>;
}

/** A grant main issued: what the page may show of it. */
export interface GrantSummary {
  readonly token: string;
  readonly files: readonly { readonly index: number; readonly name: string; readonly sizeBytes: number }[];
}

export type AttachOutcome =
  | { readonly kind: "attached"; readonly attachments: readonly ComposerAttachment[]; readonly notices: readonly string[] }
  | { readonly kind: "cancelled" }
  | { readonly kind: "failed"; readonly message: string };

interface Refused {
  readonly ok: false;
  readonly error: { readonly code: string; readonly message: string; readonly details?: { readonly reason?: string } | null };
}

function refusalOf(answer: unknown): Refused | null {
  if (answer !== null && typeof answer === "object" && (answer as { ok?: unknown }).ok === false) {
    return answer as Refused;
  }
  return null;
}

function messageOf(refused: Refused): string {
  if (refused.error.code === "FILE_GRANT_INVALID" && refused.error.details?.reason === "expired") {
    return t("attachGrantExpired");
  }
  return refused.error.message || t("attachRefused");
}

function grantOf(answer: unknown): GrantSummary | null {
  const result = (answer as { result?: { grant?: unknown } } | null)?.result;
  const grant = result?.grant;
  if (grant === null || typeof grant !== "object") {
    return null;
  }
  const record = grant as { token?: unknown; files?: unknown };
  if (typeof record.token !== "string" || !Array.isArray(record.files)) {
    return null;
  }
  return { token: record.token, files: record.files as GrantSummary["files"] };
}

/** Claim a grant for a thread and turn what was uploaded into attachments. */
export async function attachGrant(api: LocalAttachApi, grant: GrantSummary, threadId: string | null): Promise<AttachOutcome> {
  const answer = await api.attachLocalFiles({ token: grant.token, threadId });
  const refused = refusalOf(answer);
  if (refused) {
    return { kind: "failed", message: messageOf(refused) };
  }
  const result = (answer as { result?: { attached?: unknown; skipped?: unknown } } | null)?.result;
  const attached = Array.isArray(result?.attached) ? (result.attached as unknown[]) : [];
  const skipped = Array.isArray(result?.skipped) ? (result.skipped as { name?: unknown; message?: unknown }[]) : [];
  const notices = skipped.map((file) => `${typeof file.name === "string" ? file.name : "?"}: ${typeof file.message === "string" ? file.message : t("attachRefused")}`);
  return { kind: "attached", attachments: attached.map((upload) => attachmentFromUpload(upload)), notices };
}

/** The attach control: a native dialog, then the claim. A cancelled dialog is not an error. */
export async function attachFromDisk(api: LocalAttachApi, threadId: string | null): Promise<AttachOutcome> {
  const picked = await api.pickLocalFiles({ threadId });
  const refused = refusalOf(picked);
  if (refused) {
    return { kind: "failed", message: messageOf(refused) };
  }
  const grant = grantOf(picked);
  return grant === null ? { kind: "cancelled" } : attachGrant(api, grant, threadId);
}

/**
 * A grant a deep link caused main to make (`files.grant` push): attached to the thread it names, or to the open one when
 * it names none. A grant for another thread is discarded at once rather than left to expire.
 */
export async function attachPushedGrant(api: LocalAttachApi, push: unknown, openThreadId: string | null): Promise<AttachOutcome> {
  const record = push !== null && typeof push === "object" ? (push as { grant?: unknown; error?: unknown; threadId?: unknown }) : {};
  const refused = refusalOf({ ok: false, error: record.error });
  if (record.error !== undefined && refused) {
    return { kind: "failed", message: messageOf(refused) };
  }
  const grant = grantOf({ result: { grant: record.grant } });
  if (grant === null) {
    return { kind: "cancelled" };
  }
  const target = typeof record.threadId === "string" ? record.threadId : null;
  if (target !== null && target !== openThreadId) {
    await api.discardLocalFiles(grant.token);
    return { kind: "cancelled" };
  }
  return attachGrant(api, grant, openThreadId);
}
