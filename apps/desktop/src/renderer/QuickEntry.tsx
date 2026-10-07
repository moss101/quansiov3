/**
 * Quick entry (ADP-004): a small window a global shortcut summons to start a new thread.
 *
 * It has one field. Return sends the text through main's `shell.quickEntry.submit`, which posts it through the same
 * `PostMessage` command the main window uses (a new thread, no attachments, no authority of its own); Escape, or
 * losing focus, hides the window. A refusal is shown in place and the text stays, so nothing typed is lost.
 */

import { useCallback, useEffect, useRef, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { shellRefusal } from "../shell-ui.js";

export function QuickEntry(): ReactElement {
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const field = useRef<HTMLTextAreaElement | null>(null);

  // The window is summoned again and again: it takes focus each time, and nothing queued is waiting for it.
  useEffect(() => {
    void window.quansio.consumePending();
    const focus = (): void => {
      field.current?.focus();
    };
    window.addEventListener("focus", focus);
    focus();
    return () => {
      window.removeEventListener("focus", focus);
    };
  }, []);

  const close = useCallback(() => {
    void window.quansio.closeWindow();
  }, []);

  const send = useCallback(async () => {
    const content = text.trim();
    if (!content || busy) {
      return;
    }
    setBusy(true);
    setProblem(null);
    try {
      const answer = await window.quansio.quickEntrySubmit({ content });
      const refusal = shellRefusal(answer);
      if (refusal) {
        setProblem(refusal.message || t("commandFailed"));
      } else {
        setText("");
      }
    } catch (error) {
      setProblem(error instanceof Error ? error.message : t("commandFailed"));
    } finally {
      setBusy(false);
    }
  }, [text, busy]);

  return (
    <main className="quick-entry">
      <textarea
        ref={field}
        aria-label={t("quickEntryPlaceholder")}
        placeholder={t("quickEntryPlaceholder")}
        value={text}
        rows={3}
        onChange={(event) => {
          setText(event.target.value);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            close();
          } else if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) {
            event.preventDefault();
            void send();
          }
        }}
      />
      <div className="quick-entry-foot">
        {problem ? <span className="quick-entry-problem" role="alert">{problem}</span> : <span />}
        <button type="button" className="primary" disabled={busy || !text.trim()} onClick={() => { void send(); }}>
          {t("quickEntrySend")}
        </button>
      </div>
    </main>
  );
}
