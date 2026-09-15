/**
 * Chat / objective composer (APP-004).
 *
 * Every objective is a WorkGraph/Run identity, never a UI-only workflow. Reload
 * restores threadId, runId, attachments and answered questions from the session.
 */

export interface ComposerAttachment {
  readonly artifactId: string;
  readonly title: string;
}

export interface AnsweredQuestion {
  readonly questionId: string;
  readonly answer: string;
}

export interface TurnInspector {
  readonly modelRoute?: string;
  readonly contextSources: readonly string[];
  readonly trustLabels: readonly string[];
  readonly toolsUsed: readonly string[];
}

export interface ConversationSession {
  readonly threadId: string;
  readonly runId: string | null;
  readonly objectiveWorkNodeId: string | null;
  readonly messages: readonly string[];
  readonly attachments: readonly ComposerAttachment[];
  readonly answeredQuestions: readonly AnsweredQuestion[];
  readonly inspector: TurnInspector;
  readonly cancelled: boolean;
}

export function newChatSession(threadId: string): ConversationSession {
  return {
    threadId,
    runId: null,
    objectiveWorkNodeId: null,
    messages: [],
    attachments: [],
    answeredQuestions: [],
    inspector: { contextSources: [], trustLabels: [], toolsUsed: [] },
    cancelled: false,
  };
}

/** Bind an objective to a work node so it cannot exist only in the UI. */
export function bindObjective(
  session: ConversationSession,
  workNodeId: string,
  runId: string,
): ConversationSession {
  if (!workNodeId.startsWith("wn_") || !runId.startsWith("run_")) {
    throw new Error("objective must map to WorkGraph/Run identities");
  }
  return { ...session, objectiveWorkNodeId: workNodeId, runId };
}

export function postMessage(session: ConversationSession, text: string): ConversationSession {
  return { ...session, messages: [...session.messages, text] };
}

export function attachArtifact(
  session: ConversationSession,
  attachment: ComposerAttachment,
): ConversationSession {
  if (!attachment.artifactId.startsWith("art_")) {
    throw new Error("attachments must be Artifact identities");
  }
  return { ...session, attachments: [...session.attachments, attachment] };
}

export function answerQuestion(
  session: ConversationSession,
  question: AnsweredQuestion,
): ConversationSession {
  if (!question.questionId.startsWith("q_")) {
    throw new Error("questions must be canonical q_ identities");
  }
  return { ...session, answeredQuestions: [...session.answeredQuestions, question] };
}

export function inspectTurn(
  session: ConversationSession,
  inspector: TurnInspector,
): ConversationSession {
  return { ...session, inspector };
}

export function cancelRun(session: ConversationSession): ConversationSession {
  return { ...session, cancelled: true };
}

/** Reload: the same identities and attachments come back. */
export function reload(session: ConversationSession): ConversationSession {
  return { ...session };
}

export const CANCEL_COMMAND = "CancelRun";
export const ANSWER_COMMAND = "AnswerQuestion";
export const POST_COMMAND = "PostMessage";
