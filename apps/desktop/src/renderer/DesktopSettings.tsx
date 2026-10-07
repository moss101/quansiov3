/**
 * Desktop settings (ADP-004): keep-awake, updates and keyboard shortcuts. These are this computer's own choices and
 * are kept in the app's own preferences file; none is a workspace setting and none reaches the server.
 *
 * Recording a shortcut pauses every global shortcut and menu accelerator in main until the new keys are in or the
 * recording ends (Escape, clicking away, leaving the page, or a crash of the page). A change main cannot apply (a
 * conflict, an invalid binding, a shortcut the system would not register) keeps the previous binding and says why.
 */

import { useCallback, useEffect, useId, useRef, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { bindingFromKeyEvent } from "../shortcut-grammar.js";
import {
  decodeShortcuts,
  shellRefusal,
  shellResult,
  shortcutFailureKey,
  SHORTCUT_LABELS,
  type ShortcutRow,
  type ShortcutsView,
} from "../shell-ui.js";

interface Preferences {
  readonly keepAwake: boolean;
  readonly installWhenIdle: boolean;
  readonly waitingForIdle: boolean;
  readonly updateState: string;
  readonly platform: string;
}

function decodePreferences(answer: unknown): Preferences | null {
  const result = shellResult(answer);
  if (result === null) {
    return null;
  }
  const status = result["updateStatus"] !== null && typeof result["updateStatus"] === "object" ? (result["updateStatus"] as Record<string, unknown>) : {};
  return {
    keepAwake: result["keepAwake"] === true,
    installWhenIdle: result["installWhenIdle"] === true,
    waitingForIdle: result["waitingForIdle"] === true,
    updateState: typeof status["state"] === "string" ? status["state"] : "unavailable",
    platform: typeof result["platform"] === "string" ? result["platform"] : "",
  };
}

const UPDATE_TEXT: Readonly<Record<string, "updateUnavailable" | "updateIdle" | "updateChecking" | "updateDownloaded" | "updateError">> = {
  unavailable: "updateUnavailable",
  idle: "updateIdle",
  checking: "updateChecking",
  downloaded: "updateDownloaded",
  error: "updateError",
};

export function DesktopSettings(): ReactElement {
  const ids = useId();
  const [prefs, setPrefs] = useState<Preferences | null>(null);
  const [shortcuts, setShortcuts] = useState<ShortcutsView | null>(null);
  const [recording, setRecording] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const recordingRef = useRef<string | null>(null);
  recordingRef.current = recording;

  const load = useCallback(async () => {
    setPrefs(decodePreferences(await window.quansio.shellPreferences()));
    setShortcuts(decodeShortcuts(shellResult(await window.quansio.shortcuts())));
  }, []);

  useEffect(() => {
    void load();
    const offShortcuts = window.quansio.onShortcuts((payload) => {
      setShortcuts((previous) => decodeShortcuts(payload, previous?.platform ?? ""));
    });
    const offEnded = window.quansio.onShortcutRecordingEnded(() => {
      setRecording(null);
    });
    const offStatus = window.quansio.onUpdateStatus(() => {
      void load();
    });
    return () => {
      offShortcuts();
      offEnded();
      offStatus();
      // Leaving the page ends a recording that is still open.
      if (recordingRef.current !== null) {
        void window.quansio.endShortcutRecording();
      }
    };
  }, [load]);

  const platform = shortcuts?.platform || prefs?.platform || "";

  // While recording, every key is the person's answer: Escape cancels, anything else is tried as the binding.
  useEffect(() => {
    if (recording === null) {
      return;
    }
    const onKey = (event: KeyboardEvent): void => {
      event.preventDefault();
      event.stopPropagation();
      if (event.key === "Escape") {
        void window.quansio.endShortcutRecording().then(() => {
          setRecording(null);
        });
        return;
      }
      const binding = bindingFromKeyEvent(event, platform, 64);
      if (binding === null) {
        return; // only modifiers so far, or a key the grammar does not have
      }
      void window.quansio.setShortcut(recording, binding).then((answer) => {
        const refusal = shellRefusal(answer);
        if (refusal) {
          const text = t(shortcutFailureKey(refusal.code));
          const taken = typeof refusal.details["with"] === "string" ? refusal.details["with"].replace(/_/g, " ") : "";
          setMessage(text.replace("{with}", taken));
        } else {
          setMessage(null);
        }
        setRecording(null);
        void window.quansio.endShortcutRecording();
      });
    };
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
    };
  }, [recording, platform]);

  const beginRecording = useCallback((id: string) => {
    setMessage(null);
    void window.quansio.beginShortcutRecording().then((answer) => {
      if (shellRefusal(answer) === null) {
        setRecording(id);
      }
    });
  }, []);

  const change = useCallback(
    (id: string, binding: string | null) => {
      void window.quansio.setShortcut(id, binding).then((answer) => {
        const refusal = shellRefusal(answer);
        setMessage(refusal ? t(shortcutFailureKey(refusal.code)) : null);
        void load();
      });
    },
    [load],
  );

  const reset = useCallback(
    (id: string) => {
      void window.quansio.resetShortcut(id).then(() => load());
    },
    [load],
  );

  const setPreference = useCallback(
    (changes: { keepAwake?: boolean; installWhenIdle?: boolean }) => {
      void window.quansio.setShellPreferences(changes).then((answer) => {
        setPrefs(decodePreferences(answer));
      });
    },
    [],
  );

  const row = (entry: ShortcutRow): ReactElement => {
    const label = SHORTCUT_LABELS[entry.id];
    return (
      <li key={entry.id} className="shortcut-row">
        <span className="shortcut-name">{label ? t(label) : entry.id}</span>
        <kbd className="shortcut-binding">{recording === entry.id ? t("shortcutRecording") : (entry.display ?? t("shortcutNone"))}</kbd>
        {entry.scope === "global" && entry.binding && !entry.registered ? <span className="shortcut-note">{t("shortcutNotRegistered")}</span> : null}
        <button type="button" disabled={recording !== null && recording !== entry.id} onClick={() => { beginRecording(entry.id); }}>
          {t("shortcutRecord")}
        </button>
        <button type="button" disabled={entry.binding === null} onClick={() => { change(entry.id, null); }}>
          {t("shortcutClear")}
        </button>
        <button type="button" onClick={() => { reset(entry.id); }}>
          {t("shortcutReset")}
        </button>
      </li>
    );
  };

  return (
    <section className="desktop-settings" aria-label={t("settingsDesktop")}>
      <h3>{t("settingsDesktop")}</h3>
      <div>
        <input
          id={`${ids}-awake`}
          type="checkbox"
          checked={prefs?.keepAwake === true}
          disabled={prefs === null}
          onChange={(event) => { setPreference({ keepAwake: event.target.checked }); }}
        />
        <label htmlFor={`${ids}-awake`}>{t("settingsKeepAwake")}</label>
      </div>
      <h4>{t("updateSection")}</h4>
      <p role="status">{prefs?.waitingForIdle ? t("updateWaitingForIdle") : t(UPDATE_TEXT[prefs?.updateState ?? "unavailable"] ?? "updateUnavailable")}</p>
      <div>
        <input
          id={`${ids}-idle`}
          type="checkbox"
          checked={prefs?.installWhenIdle === true}
          disabled={prefs === null}
          onChange={(event) => { setPreference({ installWhenIdle: event.target.checked }); }}
        />
        <label htmlFor={`${ids}-idle`}>{t("updateInstallWhenIdleOption")}</label>
      </div>
      <button
        type="button"
        disabled={prefs?.updateState !== "downloaded"}
        onClick={() => {
          void window.quansio.installUpdate("now").then((answer) => {
            const result = shellResult(answer);
            if (result?.["kind"] === "not_downloaded") {
              setMessage(t("updateNothingDownloaded"));
            }
            void load();
          });
        }}
      >
        {t("updateRestart")}
      </button>
      <h4>{t("shortcutsTitle")}</h4>
      <p>{t("shortcutsGlobalNote")}</p>
      <p role="status">{message ?? ""}</p>
      <ul className="shortcut-list">{(shortcuts?.shortcuts ?? []).map(row)}</ul>
    </section>
  );
}
