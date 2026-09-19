/**
 * Chat / objective composer (APP-004): what the page sends, as §14 commands.
 *
 * Every send is a public command with a client-minted `cmd_` id, so a retry replays rather
 * than repeats. An attachment is an Artifact first (`CreateArtifact`, CORE-007) and a
 * message attachment second (`PostMessage.attachments`); the composer never carries bytes
 * into a message. An objective is a WorkGraph/Run identity the server assigns, never a
 * UI-only workflow: the run a message starts is the objective's run, and the inspector
 * (`inspector.ts`) shows the work node it belongs to.
 */

import type { ComposerAttachment, ConversationState } from "./state.js";

/** Largest upload the composer sends (the server's `CreateArtifact` bound). */
export const MAX_UPLOAD_BYTES = 8 * 1024 * 1024;

export const POST_COMMAND = "PostMessage";
export const ANSWER_COMMAND = "AnswerQuestion";
export const CANCEL_COMMAND = "CancelTurn";
export const UPLOAD_COMMAND = "CreateArtifact";

/** A file the renderer picked, already read into memory. */
export interface UploadInput {
  readonly name: string;
  readonly mediaType: string;
  readonly bytes: Uint8Array;
}

/** `CreateArtifact` params for an upload; refuses an empty or oversized file up front. */
export function uploadParams(workspaceId: string, file: UploadInput, threadId: string | null = null): Record<string, unknown> {
  if (file.bytes.byteLength === 0 || file.bytes.byteLength > MAX_UPLOAD_BYTES) {
    throw new Error(`upload must be 1..=${String(MAX_UPLOAD_BYTES)} bytes`);
  }
  const title = file.name.trim().slice(0, 200) || "attachment";
  const params: Record<string, unknown> = {
    workspace_id: workspaceId,
    title,
    media_type: file.mediaType || "application/octet-stream",
    bytes_base64: toBase64(file.bytes),
  };
  if (threadId) {
    params["thread_id"] = threadId;
  }
  return params;
}

/** The attachment a `CreateArtifact` result becomes in the composer. */
export function attachmentFromUpload(result: unknown): ComposerAttachment {
  const record = result && typeof result === "object" ? (result as Record<string, unknown>) : {};
  const artifactId = record["artifact_id"];
  if (typeof artifactId !== "string" || !artifactId.startsWith("art_")) {
    throw new Error("CreateArtifact did not return an artifact id");
  }
  return {
    artifactId,
    title: typeof record["title"] === "string" ? record["title"] : artifactId,
    sizeBytes: typeof record["size_bytes"] === "number" ? record["size_bytes"] : 0,
  };
}

/** `PostMessage` params from the composer; the thread is the selected one or a new direct thread. */
export function postParams(state: ConversationState): Record<string, unknown> {
  const content = state.composer.text.trim();
  if (!content) {
    throw new Error("a message needs text");
  }
  const params: Record<string, unknown> = { workspace_id: state.workspaceId, content };
  if (state.threadId) {
    params["thread_id"] = state.threadId;
  }
  if (state.composer.attachments.length > 0) {
    params["attachments"] = state.composer.attachments.map((attachment) => attachment.artifactId);
  }
  return params;
}

/** `AnswerQuestion` params; the value's shape follows the question kind. */
export function answerParams(questionId: string, value: unknown): Record<string, unknown> {
  if (!questionId.startsWith("q_")) {
    throw new Error("questions are q_ identities");
  }
  return { question_id: questionId, value };
}

/** `CancelTurn` params. */
export function cancelParams(runId: string, generation = 1): Record<string, unknown> {
  if (!runId.startsWith("run_")) {
    throw new Error("runs are run_ identities");
  }
  return { run_id: runId, generation };
}

/** The `PostMessage` result the composer keeps: the thread, the message and the run. */
export function postedIdentities(result: unknown): { threadId: string; runId: string; messageId: string } {
  const record = result && typeof result === "object" ? (result as Record<string, unknown>) : {};
  const threadId = record["thread_id"];
  const runId = record["run_id"];
  const messageId = record["message_id"];
  if (typeof threadId !== "string" || typeof runId !== "string" || typeof messageId !== "string") {
    throw new Error("PostMessage did not return thread, run and message identities");
  }
  return { threadId, runId, messageId };
}

function toBase64(bytes: Uint8Array): string {
  if (typeof Buffer !== "undefined") {
    return Buffer.from(bytes).toString("base64");
  }
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}
