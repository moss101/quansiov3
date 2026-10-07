/**
 * Commands the shell sends the main window (ADP-004): the menu, the tray, a notification click, the activity
 * indicator and a deep link all say "go here" or "do this" the same way. Shared by main (which builds them) and the
 * renderer (which validates what arrives, because a push is data like any other).
 */

export type ShellCommand =
  | { readonly kind: "new-thread" }
  | { readonly kind: "find" }
  | { readonly kind: "open-settings" }
  | { readonly kind: "popout-current-thread" }
  | { readonly kind: "open-thread"; readonly threadId: string }
  | { readonly kind: "open-run"; readonly runId: string }
  | { readonly kind: "open-approval"; readonly approvalId: string };

const ID = /^[a-z]{1,4}_[0-9A-Za-z]{1,64}$/;

/** A command from a push payload, or `null` when it is not one. */
export function parseShellCommand(raw: unknown): ShellCommand | null {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    return null;
  }
  const record = raw as Record<string, unknown>;
  const id = (key: string, prefix: string): string | null => {
    const value = record[key];
    return typeof value === "string" && ID.test(value) && value.startsWith(prefix) ? value : null;
  };
  switch (record["kind"]) {
    case "new-thread":
    case "find":
    case "open-settings":
    case "popout-current-thread":
      return { kind: record["kind"] };
    case "open-thread": {
      const threadId = id("threadId", "thr_");
      return threadId ? { kind: "open-thread", threadId } : null;
    }
    case "open-run": {
      const runId = id("runId", "run_");
      return runId ? { kind: "open-run", runId } : null;
    }
    case "open-approval": {
      const approvalId = id("approvalId", "apr_");
      return approvalId ? { kind: "open-approval", approvalId } : null;
    }
    default:
      return null;
  }
}
