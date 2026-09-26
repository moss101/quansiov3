/**
 * Onboarding screen (APP-002): sign in, choose where work runs, confirm the security
 * defaults. Every step is a bridge call; no configuration file is ever read or written.
 */

import { useCallback, useEffect, useReducer, useState, type ReactElement } from "react";

import {
  EXECUTION_TARGETS,
  INITIAL_ONBOARDING,
  SECURITY_DEFAULTS,
  executionTargetSettings,
  reduceOnboarding,
  securityDefaultSettings,
  type ExecutionTarget,
  type SessionProjection,
} from "../onboarding/flow.js";
import { t } from "../i18n/strings.js";

/** How each sign-in provider is written for a person. */
const PROVIDER_NAMES = { google: "Google", microsoft: "Microsoft", github: "GitHub" } as const;

export function Onboarding({ onReady }: { readonly onReady: () => void }): ReactElement {
  const [state, dispatch] = useReducer(reduceOnboarding, INITIAL_ONBOARDING);
  const [email, setEmail] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [token, setToken] = useState("");
  const [oauth, setOauth] = useState<readonly string[]>([]);

  useEffect(() => {
    void window.quansio.sessionState().then((session) => {
      dispatch({ kind: "session", state: session as SessionProjection });
    });
    return window.quansio.onSessionState((session) => {
      dispatch({ kind: "session", state: session as SessionProjection });
    });
  }, []);

  useEffect(() => {
    if (state.step === "ready") {
      onReady();
    }
    if (state.step === "choose-execution-target" || state.step === "confirm-defaults") {
      void window.quansio.me().then((me) => {
        setOauth((me as { auth_methods_offered?: { oauth?: readonly string[] } }).auth_methods_offered?.oauth ?? []);
      });
    }
  }, [state.step, onReady]);

  const fail = useCallback((error: unknown) => {
    dispatch({ kind: "failed", message: error instanceof Error ? error.message : t("commandFailed") });
  }, []);

  const sendLink = useCallback(() => {
    window.quansio.startMagicLink(email.trim(), displayName.trim()).then(() => {
      dispatch({ kind: "link-sent", email: email.trim() });
    }, fail);
  }, [email, displayName, fail]);

  const completeLink = useCallback(() => {
    window.quansio.completeMagicLink(token.trim()).then(undefined, fail);
  }, [token, fail]);

  const chooseTarget = useCallback(
    (target: ExecutionTarget) => {
      if (!state.workspaceId) {
        return;
      }
      window.quansio.updateWorkspace(state.workspaceId, { settings: executionTargetSettings(target) }).then(() => {
        dispatch({ kind: "target-chosen", target });
      }, fail);
    },
    [state.workspaceId, fail],
  );

  const confirmDefaults = useCallback(() => {
    if (!state.workspaceId) {
      return;
    }
    window.quansio.updateWorkspace(state.workspaceId, { settings: securityDefaultSettings() }).then(() => {
      dispatch({ kind: "defaults-confirmed" });
    }, fail);
  }, [state.workspaceId, fail]);

  return (
    <section className="onboarding">
      <h1>{t("appName")}</h1>
      {state.error ? <p className="notice">{state.error}</p> : null}
      {state.step === "sign-in" ? (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            sendLink();
          }}
        >
          <h2>{t("signIn")}</h2>
          <input type="email" required placeholder={t("emailPlaceholder")} value={email} onChange={(event) => { setEmail(event.target.value); }} />
          <input type="text" placeholder={t("displayNamePlaceholder")} value={displayName} onChange={(event) => { setDisplayName(event.target.value); }} />
          <button type="submit" disabled={!email.includes("@")}>{t("sendMagicLink")}</button>
          <div className="oauth">
            {(["google", "microsoft", "github"] as const).map((provider) => (
              <button key={provider} type="button" disabled={oauth.length > 0 && !oauth.includes(provider)} onClick={() => { window.quansio.startOAuth(provider).then(undefined, fail); }}>
                {t("continueWith")} {PROVIDER_NAMES[provider]}
              </button>
            ))}
          </div>
        </form>
      ) : null}
      {state.step === "check-inbox" ? (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            completeLink();
          }}
        >
          <h2>{t("checkInbox")}</h2>
          <p>{state.email}</p>
          <input type="text" placeholder={t("pasteToken")} value={token} onChange={(event) => { setToken(event.target.value); }} />
          <button type="submit" disabled={!token.startsWith("qml_")}>{t("continue")}</button>
        </form>
      ) : null}
      {state.step === "choose-execution-target" ? (
        <div>
          <h2>{t("chooseExecutionTarget")}</h2>
          {EXECUTION_TARGETS.map((target) => (
            <button key={target} type="button" onClick={() => { chooseTarget(target); }}>
              {target}
            </button>
          ))}
        </div>
      ) : null}
      {state.step === "confirm-defaults" ? (
        <div>
          <h2>{t("securityDefaults")}</h2>
          <ul>
            {Object.entries(SECURITY_DEFAULTS).map(([key, value]) => (
              <li key={key}>
                {key}: {String(value)}
              </li>
            ))}
          </ul>
          <button type="button" onClick={confirmDefaults}>{t("confirm")}</button>
        </div>
      ) : null}
    </section>
  );
}
