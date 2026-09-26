/**
 * Conversation surface (APP-004): threads, messages, the streaming reply, tool and
 * progress cards, agent questions answered in the thread, the composer with attachments,
 * cancel, and the per-turn inspector. State is `conversationReducer`; every read and
 * command goes through `window.quansio`, and the page holds no scope, cursor or bytes it
 * could replay on its own.
 */

import { useCallback, useEffect, useLayoutEffect, useReducer, useRef, useState, type ReactElement } from "react";

import { attachmentFromUpload, postedIdentities } from "../conversation/composer.js";
import { decodeTimeline, type RunInspection } from "../conversation/inspector.js";
import {
  cancellableRun,
  conversationReducer,
  decodeMessages,
  decodeQuestions,
  initialConversation,
  openQuestions,
  type ConversationQuestion,
} from "../conversation/state.js";
import { t } from "../i18n/strings.js";
import { ArrowUpIcon, CloseIcon, PlusIcon, StopIcon } from "./icons.js";

/** A conversation as the sidebar lists it. */
export interface ThreadRow {
  readonly id: string;
  readonly title: string | null;
  readonly kind: string;
  readonly messageCount: number;
}

interface ConversationProps {
  readonly workspaceId: string;
  /** The open thread (chosen in the sidebar); `null` starts a new one. */
  readonly threadId: string | null;
  /** Posting into a new thread creates it; the shell learns which one is now open. */
  readonly onThreadChange: (threadId: string | null) => void;
  /** Message counts and titles changed; the sidebar re-reads its list. */
  readonly onThreadsChanged: () => Promise<void>;
  readonly inspectorOpen: boolean;
  readonly onInspectorOpen: (open: boolean) => void;
}

export function decodeThreads(payload: unknown): ThreadRow[] {
  const record = payload && typeof payload === "object" ? (payload as Record<string, unknown>) : {};
  const rows = Array.isArray(record["threads"]) ? (record["threads"] as unknown[]) : [];
  const threads: ThreadRow[] = [];
  for (const raw of rows) {
    const row = raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
    if (typeof row["id"] !== "string") {
      continue;
    }
    threads.push({
      id: row["id"],
      title: typeof row["title"] === "string" ? row["title"] : null,
      kind: typeof row["kind"] === "string" ? row["kind"] : "direct",
      messageCount: typeof row["message_count"] === "number" ? row["message_count"] : 0,
    });
  }
  return threads;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : t("commandFailed");
}

const COMPOSER_MAX_HEIGHT = 200;

export function Conversation({ workspaceId, threadId, onThreadChange, onThreadsChanged, inspectorOpen, onInspectorOpen }: ConversationProps): ReactElement {
  const [state, dispatch] = useReducer(conversationReducer, initialConversation(workspaceId, threadId));
  const [inspection, setInspection] = useState<RunInspection | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const fileInput = useRef<HTMLInputElement | null>(null);
  const composer = useRef<HTMLTextAreaElement | null>(null);
  const scroller = useRef<HTMLDivElement | null>(null);
  // The thread last agreed with the shell: a change from the sidebar is followed, and a thread
  // created here by posting is reported back, without either undoing the other.
  const agreed = useRef<string | null>(threadId);

  useEffect(() => {
    agreed.current = threadId;
    dispatch({ type: "thread.selected", threadId });
  }, [threadId]);

  useEffect(() => {
    if (state.threadId !== agreed.current) {
      agreed.current = state.threadId;
      onThreadChange(state.threadId);
    }
  }, [state.threadId, onThreadChange]);

  // Frames arrive for the whole workspace; the reducer keeps the ones for this thread.
  useEffect(
    () =>
      window.quansio.onStreamFrame((frame) => {
        if (frame && typeof frame === "object" && "kind" in frame) {
          dispatch({ type: "frame", frame: frame as never });
        }
      }),
    [],
  );

  // Stale projections are re-read; an event says what changed, the projection says how.
  useEffect(() => {
    const current = state.threadId;
    if (!current || !state.stale.messages) {
      return;
    }
    let cancelled = false;
    void window.quansio
      .threadMessages(current)
      .then((payload) => {
        if (!cancelled) {
          dispatch({ type: "messages.loaded", threadId: current, messages: decodeMessages(payload) });
        }
      })
      .then(onThreadsChanged)
      .catch((error: unknown) => {
        setNotice(errorText(error));
      });
    return () => {
      cancelled = true;
    };
  }, [state.threadId, state.stale.messages, onThreadsChanged]);

  useEffect(() => {
    const current = state.threadId;
    if (!current || !state.stale.questions) {
      return;
    }
    let cancelled = false;
    void window.quansio
      .listQuestions({ threadId: current })
      .then((payload) => {
        if (!cancelled) {
          dispatch({ type: "questions.loaded", questions: decodeQuestions(payload) });
        }
      })
      .catch((error: unknown) => {
        setNotice(errorText(error));
      });
    return () => {
      cancelled = true;
    };
  }, [state.threadId, state.stale.questions]);

  // The inspector reads the timeline for the inspected run whenever its state changes.
  const inspectedRunId = state.inspectedRunId;
  const inspectedStatus = inspectedRunId ? state.runs[inspectedRunId]?.status : undefined;
  useEffect(() => {
    if (!inspectedRunId) {
      setInspection(null);
      return;
    }
    let cancelled = false;
    void window.quansio
      .runTimeline(inspectedRunId)
      .then((payload) => {
        if (!cancelled) {
          setInspection(decodeTimeline(payload));
        }
      })
      .catch((error: unknown) => {
        setNotice(errorText(error));
      });
    return () => {
      cancelled = true;
    };
  }, [inspectedRunId, inspectedStatus, state.stale.messages]);

  const send = useCallback(async () => {
    if (!state.composer.text.trim() || state.composer.posting) {
      return;
    }
    dispatch({ type: "composer.posting" });
    try {
      const result = await window.quansio.postMessage({
        threadId: state.threadId,
        content: state.composer.text.trim(),
        attachments: state.composer.attachments.map((attachment) => attachment.artifactId),
      });
      const identities = postedIdentities(result);
      dispatch({ type: "composer.posted", ...identities });
      dispatch({ type: "inspect", runId: identities.runId });
    } catch (error) {
      dispatch({ type: "composer.failed", error: errorText(error) });
    }
  }, [state.composer, state.threadId]);

  const attach = useCallback(async (file: File) => {
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      let binary = "";
      for (const byte of bytes) {
        binary += String.fromCharCode(byte);
      }
      const result = await window.quansio.uploadAttachment({
        name: file.name,
        mediaType: file.type,
        bytesBase64: btoa(binary),
        threadId: state.threadId,
      });
      dispatch({ type: "composer.attach", attachment: attachmentFromUpload(result) });
    } catch (error) {
      setNotice(errorText(error));
    }
  }, [state.threadId]);

  const answer = useCallback(async (question: ConversationQuestion, value: unknown) => {
    try {
      await window.quansio.answerQuestion(question.id, value);
    } catch (error) {
      setNotice(errorText(error));
    }
  }, []);

  const cancel = useCallback(async () => {
    const run = cancellableRun(state);
    if (!run) {
      return;
    }
    try {
      await window.quansio.cancelTurn(run.runId);
    } catch (error) {
      setNotice(errorText(error));
    }
  }, [state]);

  // The composer grows with what is typed, up to a limit, then scrolls.
  const text = state.composer.text;
  useLayoutEffect(() => {
    const field = composer.current;
    if (field) {
      field.style.height = "auto";
      field.style.height = `${String(Math.min(field.scrollHeight, COMPOSER_MAX_HEIGHT))}px`;
    }
  }, [text]);

  const live = cancellableRun(state);
  const questions = openQuestions(state);
  const activeRuns = Object.values(state.runs).filter((run) => run.streaming || run.stage || run.cards.length > 0);
  const empty = state.messages.length === 0 && activeRuns.length === 0 && questions.length === 0;

  // New replies keep the thread scrolled to its end.
  const lastStreaming = activeRuns.map((run) => run.streaming.length).join(",");
  useEffect(() => {
    const element = scroller.current;
    if (element) {
      element.scrollTop = element.scrollHeight;
    }
  }, [state.messages.length, lastStreaming, questions.length]);

  return (
    <section className={inspectorOpen ? "conversation with-inspector" : "conversation"} aria-label={t("conversation")}>
      <div className="thread">
        <div className="thread-scroll" ref={scroller}>
          {empty ? (
            <div className="thread-empty">
              <h3>{t("emptyThreadTitle")}</h3>
              <p>{t("emptyThreadHint")}</p>
            </div>
          ) : null}
          <ol className="messages" aria-live="polite">
            {state.messages.map((message) => (
              <li key={message.id} className={`message message-${message.authorKind}`} data-run={message.runId ?? undefined}>
                <span className="author">{message.authorKind === "agent" ? t("agent") : message.authorKind}</span>
                <div className="bubble">
                  <p>{message.text}</p>
                  {message.attachments.length > 0 ? (
                    <ul className="attachments">
                      {message.attachments.map((artifactId) => (
                        <li key={artifactId}>{artifactId}</li>
                      ))}
                    </ul>
                  ) : null}
                </div>
                {message.runId ? (
                  <button
                    type="button"
                    className="inspect"
                    onClick={() => {
                      dispatch({ type: "inspect", runId: message.runId });
                      onInspectorOpen(true);
                    }}
                  >
                    {t("inspectTurn")}
                  </button>
                ) : null}
              </li>
            ))}
            {activeRuns.map((run) => (
              <li key={run.runId} className="message message-live" data-run={run.runId}>
                <span className="author">{t("agent")}</span>
                <div className="bubble">
                  {run.stage ? <span className="stage">{run.stage}</span> : null}
                  {run.cards.length > 0 ? (
                    <ul className="cards">
                      {run.cards.map((card) => (
                        <li key={card.stepId} className={`card card-${card.kind} card-${card.status}`}>
                          {card.kind}
                          {card.ref ? ` · ${card.ref}` : ""} · {card.status}
                        </li>
                      ))}
                    </ul>
                  ) : null}
                  {run.streaming ? <p className="streaming">{run.streaming}</p> : null}
                </div>
              </li>
            ))}
          </ol>
          {questions.length > 0 ? (
            <div className="questions">
              {questions.map((question) => (
                <QuestionCard key={question.id} question={question} onAnswer={answer} />
              ))}
            </div>
          ) : null}
        </div>
        <div className="composer">
          {notice ? <p className="notice">{notice}</p> : null}
          {state.composer.error ? <p className="notice">{state.composer.error}</p> : null}
          {state.composer.attachments.length > 0 ? (
            <div className="composer-chips">
              {state.composer.attachments.map((attachment) => (
                <span key={attachment.artifactId} className="chip">
                  {attachment.title}
                  <button type="button" aria-label={t("remove")} onClick={() => { dispatch({ type: "composer.detach", artifactId: attachment.artifactId }); }}>
                    ×
                  </button>
                </span>
              ))}
            </div>
          ) : null}
          <div className="composer-box">
            <input
              ref={fileInput}
              type="file"
              hidden
              onChange={(event) => {
                const file = event.target.files?.[0];
                if (file) {
                  void attach(file);
                }
                event.target.value = "";
              }}
            />
            <button type="button" className="icon-button round" aria-label={t("attach")} onClick={() => fileInput.current?.click()}>
              <PlusIcon />
            </button>
            <textarea
              ref={composer}
              rows={1}
              value={state.composer.text}
              placeholder={t("composerPlaceholder")}
              onChange={(event) => { dispatch({ type: "composer.text", text: event.target.value }); }}
              onKeyDown={(event) => {
                // Return sends, Shift+Return breaks the line; an IME composition is never sent.
                if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  void send();
                }
              }}
            />
            {live ? (
              <button type="button" className="icon-button round cancel" aria-label={t("cancelTurn")} onClick={() => void cancel()}>
                <StopIcon />
              </button>
            ) : null}
            <button
              type="button"
              className="icon-button round send"
              aria-label={t("send")}
              onClick={() => void send()}
              disabled={!state.composer.text.trim() || state.composer.posting}
            >
              <ArrowUpIcon />
            </button>
          </div>
        </div>
      </div>
      {inspectorOpen ? (
        <aside className="inspector" aria-label={t("inspector")}>
          <header>
            <h2>{t("inspector")}</h2>
            <button type="button" className="icon-button small" aria-label={t("close")} onClick={() => { onInspectorOpen(false); }}>
              <CloseIcon />
            </button>
          </header>
          {inspection ? <Inspector inspection={inspection} /> : <p>{t("inspectorEmpty")}</p>}
        </aside>
      ) : null}
    </section>
  );
}

interface QuestionCardProps {
  readonly question: ConversationQuestion;
  readonly onAnswer: (question: ConversationQuestion, value: unknown) => Promise<void>;
}

function QuestionCard({ question, onAnswer }: QuestionCardProps): ReactElement {
  const [text, setText] = useState("");
  const [chosen, setChosen] = useState<readonly string[]>([]);
  const choice = question.kind === "single_choice" || question.kind === "multi_choice";
  return (
    <div className="question" data-question={question.id}>
      <header>
        <span className="author">{t("agent")}</span>
        <span className="pill">{t("actionNeeded")}</span>
      </header>
      <p>{question.prompt}</p>
      {question.kind === "confirm" ? (
        <div>
          <button type="button" className="primary" onClick={() => void onAnswer(question, true)}>{t("yes")}</button>
          <button type="button" onClick={() => void onAnswer(question, false)}>{t("no")}</button>
        </div>
      ) : choice ? (
        <div>
          {question.options.map((option) => (
            <label key={option}>
              <input
                type={question.kind === "multi_choice" ? "checkbox" : "radio"}
                name={question.id}
                value={option}
                checked={chosen.includes(option)}
                onChange={() => {
                  setChosen(question.kind === "multi_choice" ? (chosen.includes(option) ? chosen.filter((item) => item !== option) : [...chosen, option]) : [option]);
                }}
              />
              {option}
            </label>
          ))}
          <button
            type="button"
            className="primary"
            disabled={chosen.length === 0}
            onClick={() => void onAnswer(question, question.kind === "multi_choice" ? chosen : chosen[0])}
          >
            {t("answer")}
          </button>
        </div>
      ) : (
        <div>
          <input type="text" aria-label={question.prompt} value={text} onChange={(event) => { setText(event.target.value); }} />
          <button type="button" className="primary" disabled={!text.trim()} onClick={() => void onAnswer(question, text.trim())}>
            {t("answer")}
          </button>
        </div>
      )}
    </div>
  );
}

function Inspector({ inspection }: { readonly inspection: RunInspection }): ReactElement {
  return (
    <div className="inspection" data-run={inspection.runId}>
      <dl>
        <dt>{t("run")}</dt>
        <dd>{inspection.runId} · {inspection.status}</dd>
        <dt>{t("objective")}</dt>
        <dd>{inspection.workNodeId}</dd>
        {inspection.attachments.length > 0 ? (
          <>
            <dt>{t("attachments")}</dt>
            <dd>{inspection.attachments.join(", ")}</dd>
          </>
        ) : null}
      </dl>
      <ol className="turns">
        {inspection.turns.map((turn) => (
          <li key={turn.turnId} className="turn" data-turn={turn.turnId}>
            <h3>
              {t("turn")} {String(turn.seq)} · {turn.inputKind} · {turn.status}
            </h3>
            <dl>
              <dt>{t("model")}</dt>
              <dd>{turn.modelRoute ?? t("modelNotRecorded")}{turn.chosenBy ? ` (${turn.chosenBy})` : ""}</dd>
              <dt>{t("contextSources")}</dt>
              <dd>
                {turn.sources.length === 0 ? t("none") : (
                  <ul>
                    {turn.sources.map((source, index) => (
                      <li key={`${source.kind}-${String(index)}`}>
                        {source.kind} {source.ref} · {t("trust")} {source.trustLevel}
                      </li>
                    ))}
                  </ul>
                )}
              </dd>
              <dt>{t("toolsUsed")}</dt>
              <dd>
                {turn.tools.length === 0 ? t("none") : (
                  <ul>
                    {turn.tools.map((tool, index) => (
                      <li key={`${tool.toolName}-${String(index)}`}>
                        {tool.toolName} · {tool.status}
                        {tool.effectClass ? ` · ${tool.effectClass}` : ""}
                        {tool.effectStatus ? ` (${tool.effectStatus})` : ""}
                      </li>
                    ))}
                  </ul>
                )}
              </dd>
              {turn.question ? (
                <>
                  <dt>{t("question")}</dt>
                  <dd>
                    {turn.question.prompt} · {turn.question.status}
                    {turn.question.answer !== null ? ` · ${JSON.stringify(turn.question.answer)}` : ""}
                  </dd>
                </>
              ) : null}
            </dl>
          </li>
        ))}
      </ol>
    </div>
  );
}
