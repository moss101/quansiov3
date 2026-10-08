/**
 * Saved logins for the page the person is looking at (ADP-009, DOMAIN.md §13.9 A).
 *
 * The list is the password manager's suggestions for the page's own origin: a user name and nothing else. A fill is a click the person
 * makes; main reads the page again, asks a question when the login belongs to another site, and puts the text in the page. This page
 * never holds a password, and the answer to a fill says only whether it happened.
 */

import { useEffect, useState, type ReactElement } from "react";

import { t } from "../i18n/strings.js";
import { decodeLogins, refusalText } from "../secrets-ui.js";
import { shellResult } from "../shell-ui.js";

interface LoginSuggestionsProps {
  readonly sessionId: string;
  /** The URL of the page the session is on. */
  readonly pageUrl: string | null;
}

export function LoginSuggestions({ sessionId, pageUrl }: LoginSuggestionsProps): ReactElement | null {
  const [logins, setLogins] = useState<ReturnType<typeof decodeLogins>>([]);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let current = true;
    if (pageUrl === null) {
      setLogins([]);
      return undefined;
    }
    void window.quansio.suggestPasswords(pageUrl).then((answer) => {
      if (current) {
        setLogins(decodeLogins(answer));
      }
    });
    return () => {
      current = false;
    };
  }, [pageUrl]);

  if (logins.length === 0 && notice === null) {
    return null;
  }

  async function fillIn(id: string): Promise<void> {
    const answer = await window.quansio.fillPassword({ id, sessionId });
    const filled = shellResult(answer)?.["filled"] === true || (answer as { filled?: unknown } | null)?.["filled"] === true;
    setNotice(refusalText(answer) ?? (filled ? t("loginFilled") : null));
  }

  return (
    <section className="live-logins" aria-label={t("loginSuggestions")}>
      <h3>{t("loginSuggestions")}</h3>
      <ul>
        {logins.map((login) => (
          <li key={login.id}>
            <button type="button" onClick={() => { void fillIn(login.id); }}>
              {t("loginFill")} <span className="live-untrusted">{login.username}</span>
            </button>
          </li>
        ))}
      </ul>
      {notice ? <p className="notice">{notice}</p> : null}
    </section>
  );
}
