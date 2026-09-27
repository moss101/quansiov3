/**
 * Waiting for a run's turn in E2E tests: the served server answers a command once it is accepted
 * and runs the turn after it, so a test that needs the turn's results waits for it here.
 */

type Handlers = Record<string, (payload: unknown) => Promise<unknown>>;

interface TimelineView {
  readonly run?: { readonly status?: string };
  readonly turns?: readonly { readonly status?: string; readonly input?: { readonly kind?: string; readonly ref?: string } }[];
}

/** Poll an asynchronous read until it answers. */
export async function eventually<T>(read: () => Promise<T | undefined>, what: string, timeoutMs = 20_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await read();
    if (value !== undefined) {
      return value;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

/** Wait until the run's latest turn has ended or parked (on a question or an approval). */
export function settled(handlers: Handlers, runId: string, timeoutMs = 20_000): Promise<TimelineView> {
  const timeline = handlers["runs.timeline"];
  if (!timeline) {
    throw new Error("the bridge has no runs.timeline handler");
  }
  return eventually(async () => {
    const view = (await timeline({ runId })) as TimelineView;
    const turns = view.turns ?? [];
    const status = view.run?.status ?? "";
    const last = turns[turns.length - 1];
    return last && (last.status !== "active" || status.startsWith("WAITING")) ? view : undefined;
  }, `the turn of ${runId}`, timeoutMs);
}

/**
 * Wait until the run has answered one message: a conversation's run takes its messages as turns
 * one after another, so "the latest turn ended" can hold between two of them while a later
 * message still waits its turn.
 */
export function answered(handlers: Handlers, runId: string, messageId: string, timeoutMs = 20_000): Promise<TimelineView> {
  const timeline = handlers["runs.timeline"];
  if (!timeline) {
    throw new Error("the bridge has no runs.timeline handler");
  }
  return eventually(async () => {
    const view = (await timeline({ runId })) as TimelineView;
    const turn = (view.turns ?? []).find((candidate) => candidate.input?.ref === messageId);
    return turn && turn.status !== "active" ? view : undefined;
  }, `the answer to ${messageId}`, timeoutMs);
}
