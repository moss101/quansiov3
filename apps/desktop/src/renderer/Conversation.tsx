/**
 * Conversation surface (APP-004): the transcript (grouped bubbles, time dividers, the streaming
 * reply and what the teammate is doing), agent questions answered as cards in the thread, the
 * composer with attachments and cancel, and the per-turn details pane. State is
 * `conversationReducer`; every read and command goes through `window.quansio`, and the page
 * holds no scope, cursor or bytes it could replay on its own.
 */

import { useCallback, useEffect, useLayoutEffect, useMemo, useReducer, useRef, useState, type ReactElement } from "react";

import { postedIdentities, postedRunStatus } from "../conversation/composer.js";
import { attachFromDisk, attachPushedGrant, type AttachOutcome } from "../conversation/local-attach.js";
import { decodeTimeline, type RunInspection } from "../conversation/inspector.js";
import { byAgentThread, dividerDay, faceLetter, faceTone, transcript, turnInFlight, type Face as FaceData, type RosterEntry } from "../conversation/presentation.js";
import {
  cancellableRun,
  conversationReducer,
  decodeMessages,
  decodeQuestions,
  initialConversation,
  openQuestions,
  runIsLive,
  type ConversationQuestion,
  type RunActivity,
} from "../conversation/state.js";
import { t } from "../i18n/strings.js";
import { Face } from "./Face.js";
import { ArrowDownIcon, ArrowUpIcon, CloseIcon, InfoIcon, PlusIcon, SpinnerIcon, StopIcon } from "./icons.js";
import { Prose } from "./Prose.js";

interface ConversationProps {
  readonly workspaceId: string;
  /** The open thread (chosen in the sidebar); `null` starts a new one. */
  readonly threadId: string | null;
  /** The teammate this thread's replies come from, when there is one. */
  readonly teammateId: string | null;
  readonly teammateName: string | null;
  /** The conversation's own face, for replies from a teammate the roster does not name. */
  readonly threadFace: FaceData | null;
  /** The workspace's teammates: who replied, and whom a new conversation can be addressed to. */
  readonly roster: readonly RosterEntry[];
  /** The signed-in person, so their own messages are never labelled with a name. */
  readonly personId: string | null;
  /** Posting into a new thread creates it; the shell learns which one is now open. */
  readonly onThreadChange: (threadId: string | null) => void;
  /** Message counts and titles changed; the sidebar re-reads its list. */
  readonly onThreadsChanged: () => Promise<void>;
  readonly inspectorOpen: boolean;
  readonly onInspectorOpen: (open: boolean) => void;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : t("commandFailed");
}

/** The composer grows to six lines, then scrolls. */
const COMPOSER_MAX_HEIGHT = 120;
/** Closer than this to the end counts as reading the latest messages. */
const PINNED_SLACK_PX = 80;

const CARD_DONE: ReadonlySet<string> = new Set(["completed", "succeeded", "failed", "cancelled", "denied"]);

/** What a live run is doing, as the working row says it. */
function activityLabel(run: RunActivity): string {
  if (run.status === "WAITING_QUESTION") {
    return t("waitingOnAnswer");
  }
  const step = [...run.cards].reverse().find((card) => !CARD_DONE.has(card.status.toLowerCase()));
  if (step?.ref && step.kind === "tool_call") {
    return `${t("usingTool")} ${step.ref}…`;
  }
  return t("working");
}

/** A divider's words: "Today 5:39 PM", "Yesterday 5:39 PM", "Mon 5:39 PM" or "Sep 12, 5:39 PM". */
function dividerLabel(at: Date, now: Date): string {
  const time = at.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  switch (dividerDay(at, now)) {
    case "today":
      return `${t("today")} ${time}`;
    case "yesterday":
      return `${t("yesterday")} ${time}`;
    case "week":
      return `${at.toLocaleDateString(undefined, { weekday: "short" })} ${time}`;
    default:
      return `${at.toLocaleDateString(undefined, { month: "short", day: "numeric", ...(at.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }) })}, ${time}`;
  }
}

export function Conversation(props: ConversationProps): ReactElement {
  const { workspaceId, threadId, teammateId, teammateName, threadFace, roster, personId, onThreadChange, onThreadsChanged, inspectorOpen, onInspectorOpen } = props;
  const [state, dispatch] = useReducer(conversationReducer, initialConversation(workspaceId, threadId));
  const [inspection, setInspection] = useState<RunInspection | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  // Whom a new conversation is addressed to; null lets the workspace route it.
  const [addressee, setAddressee] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [pinned, setPinned] = useState(true);
  // A native file dialog or an attach in flight: the control waits for it instead of opening a second one.
  const [attaching, setAttaching] = useState(false);
  const composer = useRef<HTMLTextAreaElement | null>(null);
  const scroller = useRef<HTMLDivElement | null>(null);
  // The thread last agreed with the shell: a change from the sidebar is followed, and a thread
  // created here by posting is reported back, without either undoing the other.
  const agreed = useRef<string | null>(threadId);

  useEffect(() => {
    agreed.current = threadId;
    dispatch({ type: "thread.selected", threadId });
    setAddressee(null);
    setPinned(true);
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

  // The details pane reads the timeline for the inspected run whenever its state changes.
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
          const timeline = decodeTimeline(payload);
          setInspection(timeline);
          // The timeline is also the run's current state: it settles a run whose terminal
          // event arrived before this thread knew the run.
          if (timeline.runId === inspectedRunId && timeline.status !== "?") {
            dispatch({ type: "run.loaded", runId: inspectedRunId, status: timeline.status });
          }
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
        // A new conversation goes to the teammate chosen for it; a thread keeps its own routing.
        ...(state.threadId === null && addressee ? { teammateId: addressee } : {}),
      });
      const identities = postedIdentities(result);
      dispatch({ type: "composer.posted", ...identities });
      const status = postedRunStatus(result);
      if (status) {
        dispatch({ type: "run.loaded", runId: identities.runId, status });
      }
      dispatch({ type: "inspect", runId: identities.runId });
      setPinned(true);
    } catch (error) {
      dispatch({ type: "composer.failed", error: errorText(error) });
    }
  }, [state.composer, state.threadId, addressee]);

  // What an attach produced: its files become composer attachments like any other, and what could not be attached is said.
  const attached = useCallback((outcome: AttachOutcome) => {
    if (outcome.kind === "failed") {
      setNotice(outcome.message);
      return;
    }
    if (outcome.kind === "attached") {
      for (const attachment of outcome.attachments) {
        dispatch({ type: "composer.attach", attachment });
      }
      if (outcome.notices.length > 0) {
        setNotice(outcome.notices.join(" "));
      }
    }
  }, []);

  // The attach control (ADP-005): a native dialog in main, never a file input; the page learns names and an opaque token
  // and asks main to attach them, and no path or bytes cross to the page.
  const attach = useCallback(async () => {
    setAttaching(true);
    try {
      attached(await attachFromDisk(window.quansio, state.threadId));
    } catch (error) {
      setNotice(errorText(error));
    } finally {
      setAttaching(false);
    }
  }, [state.threadId, attached]);

  // An `attach` deep link made main open the dialog; the grant it produced arrives here.
  useEffect(
    () =>
      window.quansio.onFileGrant((push) => {
        void attachPushedGrant(window.quansio, push, state.threadId).then(attached, (error: unknown) => {
          setNotice(errorText(error));
        });
      }),
    [state.threadId, attached],
  );

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

  // The composer grows with what is typed; past one line it becomes a box with its controls
  // along the bottom.
  const text = state.composer.text;
  useLayoutEffect(() => {
    const field = composer.current;
    if (!field) {
      return;
    }
    // An empty field is one line whatever its placeholder would wrap to.
    if (!text) {
      field.style.height = "";
      setExpanded(false);
      return;
    }
    field.style.height = "auto";
    const height = Math.min(field.scrollHeight, COMPOSER_MAX_HEIGHT);
    field.style.height = `${String(height)}px`;
    setExpanded(text.includes("\n") || field.scrollHeight > 28);
  }, [text]);

  const rosterIndex = useMemo(() => byAgentThread(roster), [roster]);
  const questions = openQuestions(state);
  // Work is a turn in flight, not an open run: a conversation's run stays open between turns.
  const liveRuns = Object.values(state.runs).filter((run) => runIsLive(run) && turnInFlight(run, state.messages));
  const cancellable = cancellableRun(state);
  const live = cancellable && liveRuns.some((run) => run.runId === cancellable.runId) ? cancellable : null;
  const items = useMemo(() => transcript(state.messages), [state.messages]);
  const empty = state.threadId === null && state.messages.length === 0 && liveRuns.length === 0;
  // Names are shown only when more than one teammate or person speaks besides the reader.
  const voices = new Set(state.messages.filter((message) => message.authorKind !== "system" && message.authorId !== personId && message.authorKind !== "user").map((message) => message.authorId));
  const showNames = voices.size > 1;
  const replyFace = (authorId: string): ReactElement => {
    const teammate = rosterIndex.get(authorId);
    return <Face face={{ key: authorId, letter: faceLetter(teammate?.name ?? t("agent")), tone: faceTone(teammate?.id ?? authorId) }} size="md" />;
  };
  const liveFaceId = teammateId ? roster.find((entry) => entry.id === teammateId)?.agentThreadId ?? teammateId : null;
  const now = new Date();

  // Keep the latest messages in view while the reader is at the end; leave them where they
  // are when they have scrolled back.
  const lastStreaming = liveRuns.map((run) => run.streaming.length).join(",");
  useEffect(() => {
    const element = scroller.current;
    if (element && pinned) {
      element.scrollTop = element.scrollHeight;
    }
  }, [state.messages.length, lastStreaming, questions.length, liveRuns.length, pinned]);

  const addresseeName = addressee ? (roster.find((entry) => entry.id === addressee)?.name ?? null) : null;
  const placeholderName = state.threadId === null ? addresseeName : teammateName;
  const placeholder = placeholderName ? `${t("messageTo")} ${placeholderName}` : t("composerPlaceholder");
  const canSend = Boolean(state.composer.text.trim()) && !state.composer.posting;

  return (
    <section className={inspectorOpen ? "conversation with-details" : "conversation"} aria-label={t("conversation")}>
      <div className="thread">
        <div
          className="thread-scroll"
          ref={scroller}
          onScroll={(event) => {
            const element = event.currentTarget;
            setPinned(element.scrollHeight - element.scrollTop - element.clientHeight < PINNED_SLACK_PX);
          }}
        >
          <div className="transcript">
            {empty ? (
              <div className="thread-empty">
                <h3>{t("emptyThreadTitle")}</h3>
                <p>{t("emptyThreadHint")}</p>
                {roster.length > 0 ? (
                  <div className="addressees" role="group" aria-label={t("sendTo")}>
                    <button type="button" className="addressee" aria-pressed={addressee === null} onClick={() => { setAddressee(null); }}>
                      {t("anyTeammate")}
                    </button>
                    {roster.map((entry) => (
                      <button key={entry.id} type="button" className="addressee" aria-pressed={addressee === entry.id} onClick={() => { setAddressee(entry.id); }}>
                        <Face face={{ key: entry.id, letter: faceLetter(entry.name), tone: faceTone(entry.id) }} size="xs" />
                        {entry.name}
                      </button>
                    ))}
                  </div>
                ) : null}
              </div>
            ) : null}
            <ol className="messages" aria-live="polite">
              {items.map((item, index) => {
                if (item.kind === "divider") {
                  return (
                    <li key={item.key} className="meta meta-time">
                      <time dateTime={item.at.toISOString()}>{dividerLabel(item.at, now)}</time>
                    </li>
                  );
                }
                const { message, continues } = item;
                if (message.authorKind === "system") {
                  return (
                    <li key={item.key} className="meta meta-event" data-run={message.runId ?? undefined}>
                      {message.text}
                    </li>
                  );
                }
                const next = items[index + 1];
                const joinsNext = next?.kind === "message" && next.continues;
                const mine = message.authorKind === "user";
                const name = message.authorKind === "agent" ? (rosterIndex.get(message.authorId)?.name ?? t("agent")) : message.authorKind;
                return (
                  <li
                    key={item.key}
                    className={`row row-${mine ? "user" : "agent"}${continues ? "" : " starts"}`}
                    data-run={message.runId ?? undefined}
                  >
                    <span className={showNames && !continues && !mine ? "sender" : "sr-only"}>{name}</span>
                    <div className="row-body">
                      <div className={`bubble${continues ? " joins-prev" : ""}${joinsNext ? " joins-next" : ""}`}>
                        <Prose text={message.text} />
                        {message.attachments.length > 0 ? (
                          <ul className="attachments">
                            {message.attachments.map((artifactId) => (
                              <li key={artifactId}>{artifactId}</li>
                            ))}
                          </ul>
                        ) : null}
                      </div>
                      {message.runId ? (
                        <div className="row-tools">
                          <button
                            type="button"
                            className="icon-button ghost tiny inspect"
                            aria-label={t("inspectTurn")}
                            data-tip={t("inspectTurn")}
                            onClick={() => {
                              dispatch({ type: "inspect", runId: message.runId });
                              onInspectorOpen(true);
                            }}
                          >
                            <InfoIcon size={14} />
                          </button>
                        </div>
                      ) : null}
                    </div>
                  </li>
                );
              })}
              {liveRuns.map((run) =>
                run.streaming ? (
                  <li key={run.runId} className="row row-agent starts message-live" data-run={run.runId}>
                    <span className="sr-only">{t("agent")}</span>
                    <div className="row-body">
                      <div className="bubble streaming">
                        <Prose text={run.streaming} />
                      </div>
                    </div>
                  </li>
                ) : (
                  <li key={run.runId} className="working message-live" data-run={run.runId} role="status">
                    {liveFaceId ? replyFace(liveFaceId) : threadFace ? <Face face={threadFace} size="md" /> : <span className="face face-md face-blank" aria-hidden="true" />}
                    <span className="shimmer">{activityLabel(run)}</span>
                  </li>
                ),
              )}
              {questions.map((question) => (
                <li key={question.id} className="row row-agent starts">
                  <QuestionCard question={question} onAnswer={answer} />
                </li>
              ))}
            </ol>
          </div>
        </div>
        {!pinned ? (
          <button
            type="button"
            className="jump"
            aria-label={t("jumpToLatest")}
            onClick={() => {
              setPinned(true);
            }}
          >
            <ArrowDownIcon />
          </button>
        ) : null}
        <div className="composer">
          {notice ? <p className="tray" role="alert">{notice}<button type="button" className="icon-button ghost tiny" aria-label={t("close")} onClick={() => { setNotice(null); }}><CloseIcon size={12} /></button></p> : null}
          {state.composer.error ? <p className="tray" role="alert">{state.composer.error}</p> : null}
          <div className={expanded ? "composer-pill expanded" : "composer-pill"} onMouseDown={(event) => {
            if (event.target === event.currentTarget) {
              event.preventDefault();
              composer.current?.focus();
            }
          }}>
            {state.composer.attachments.length > 0 ? (
              <div className="composer-chips">
                {state.composer.attachments.map((attachment) => (
                  <span key={attachment.artifactId} className="chip">
                    {attachment.title}
                    <button type="button" aria-label={t("remove")} onClick={() => { dispatch({ type: "composer.detach", artifactId: attachment.artifactId }); }}>
                      <CloseIcon size={10} />
                    </button>
                  </span>
                ))}
              </div>
            ) : null}
            <textarea
              ref={composer}
              rows={1}
              value={state.composer.text}
              placeholder={placeholder}
              // One name for the field whoever it is addressed to; the placeholder says whom.
              aria-label={t("composerPlaceholder")}
              onChange={(event) => { dispatch({ type: "composer.text", text: event.target.value }); }}
              onKeyDown={(event) => {
                // Return sends, Shift+Return breaks the line; an IME composition is never sent.
                if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
                  event.preventDefault();
                  void send();
                }
              }}
            />
            <button type="button" className="circle lead" aria-label={t("attachFromDisk")} data-tip={t("attachFromDisk")} disabled={attaching} onClick={() => void attach()}>
              {attaching ? <SpinnerIcon size={14} /> : <PlusIcon size={14} />}
            </button>
            <div className="composer-trail">
              {live ? (
                <button type="button" className="circle" aria-label={t("cancelTurn")} data-tip={t("cancelTurn")} onClick={() => void cancel()}>
                  <StopIcon size={14} />
                </button>
              ) : null}
              <button type="button" className="circle send" aria-label={t("send")} onClick={() => void send()} disabled={!canSend}>
                <ArrowUpIcon size={16} />
              </button>
            </div>
          </div>
        </div>
      </div>
      {inspectorOpen ? (
        <aside className="details" id="details" aria-label={t("inspector")}>
          <header>
            <h2>{t("inspector")}</h2>
            <button type="button" className="icon-button ghost small" aria-label={t("close")} onClick={() => { onInspectorOpen(false); }}>
              <CloseIcon size={14} />
            </button>
          </header>
          {inspection ? <Inspector inspection={inspection} /> : <p className="muted">{t("inspectorEmpty")}</p>}
        </aside>
      ) : null}
    </section>
  );
}

interface QuestionCardProps {
  readonly question: ConversationQuestion;
  readonly onAnswer: (question: ConversationQuestion, value: unknown) => Promise<void>;
}

/** A question the teammate is waiting on, as an action card in the thread. */
function QuestionCard({ question, onAnswer }: QuestionCardProps): ReactElement {
  const [text, setText] = useState("");
  const [chosen, setChosen] = useState<readonly string[]>([]);
  const choice = question.kind === "single_choice" || question.kind === "multi_choice";
  return (
    <div className="card-action question" data-question={question.id}>
      <header>
        <span className="card-title">{t("question")}</span>
        <span className="badge badge-waiting">
          <span className="spin" aria-hidden="true"><SpinnerIcon size={12} /></span>
          {t("actionNeeded")}
        </span>
      </header>
      <p>{question.prompt}</p>
      {question.kind === "confirm" ? (
        <div className="card-buttons">
          <button type="button" className="card-button primary" onClick={() => void onAnswer(question, true)}>{t("yes")}</button>
          <button type="button" className="card-button outline" onClick={() => void onAnswer(question, false)}>{t("no")}</button>
        </div>
      ) : choice ? (
        <>
          <div className="options" role={question.kind === "multi_choice" ? "group" : "radiogroup"} aria-label={question.prompt}>
            {question.options.map((option) => (
              <label key={option} className="option">
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
          </div>
          <div className="card-buttons">
            <button
              type="button"
              className="card-button primary"
              disabled={chosen.length === 0}
              onClick={() => void onAnswer(question, question.kind === "multi_choice" ? chosen : chosen[0])}
            >
              {t("answer")}
            </button>
          </div>
        </>
      ) : (
        <div className="card-buttons">
          <input type="text" className="card-input" aria-label={question.prompt} value={text} onChange={(event) => { setText(event.target.value); }} />
          <button type="button" className="card-button primary" disabled={!text.trim()} onClick={() => void onAnswer(question, text.trim())}>
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
