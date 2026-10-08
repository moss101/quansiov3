/**
 * The person's own secrets (ADP-009, DOMAIN.md §13.9 A, B): the password manager and the import from another browser.
 *
 * Passwords are typed into uncontrolled inputs that are cleared the moment the request is sent; no answer carries one back, so the list
 * shows an origin and a user name and nothing else. An import asks its own native question in the main process for each type; this page
 * only chooses what to ask about (a profile the scan listed and, for cookies, domains the scan listed) and shows the counts that came back.
 * The agent never sees this page, and none of its channels is a tool.
 */

import { useCallback, useEffect, useRef, useState, type ReactElement, type SyntheticEvent } from "react";

import { fill, t } from "../i18n/strings.js";
import {
  decodeLogins,
  decodeScan,
  decodeVaultStatus,
  importCounts,
  refusalText,
  stagedSummary,
  type SavedLogin,
  type ScannedProfile,
  type VaultStatus,
} from "../secrets-ui.js";

export function SecretsSettings(): ReactElement {
  const form = useRef<HTMLFormElement | null>(null);
  const [status, setStatus] = useState<VaultStatus | null>(null);
  const [logins, setLogins] = useState<readonly SavedLogin[]>([]);
  const [profiles, setProfiles] = useState<readonly ScannedProfile[] | null>(null);
  const [chosen, setChosen] = useState<Readonly<Record<string, readonly string[]>>>({});
  const [message, setMessage] = useState<string | null>(null);

  const load = useCallback(async () => {
    setStatus(decodeVaultStatus(await window.quansio.passwordStatus()));
    setLogins(decodeLogins(await window.quansio.listPasswords()));
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function save(event: SyntheticEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (form.current === null) {
      return;
    }
    const value = (name: string): string => {
      const input = form.current?.elements.namedItem(name);
      return input instanceof HTMLInputElement ? input.value : "";
    };
    const login = { origin: value("origin"), username: value("username"), password: value("password") };
    // The password leaves the form before the call is awaited.
    form.current.reset();
    const answer = await window.quansio.savePassword(login);
    setMessage(refusalText(answer) ?? t("secretsSaved"));
    await load();
  }

  async function remove(id: string): Promise<void> {
    setMessage(refusalText(await window.quansio.removePassword(id)));
    await load();
  }

  async function scan(): Promise<void> {
    const answer = await window.quansio.scanBrowsers();
    const refused = refusalText(answer);
    setMessage(refused);
    setProfiles(refused === null ? decodeScan(answer) : null);
  }

  async function importLogins(profile: ScannedProfile): Promise<void> {
    const answer = await window.quansio.importLogins({ family: profile.family, profile: profile.profile });
    const counts = importCounts(answer);
    setMessage(refusalText(answer) ?? (counts ? fill("secretsImported", { imported: counts.imported, skipped: counts.skipped }) : null));
    await load();
  }

  async function stage(profile: ScannedProfile): Promise<void> {
    const key = `${profile.family}/${profile.profile}`;
    const answer = await window.quansio.stageCookies({ family: profile.family, profile: profile.profile, domains: chosen[key] ?? [] });
    const staged = stagedSummary(answer);
    setMessage(
      refusalText(answer) ??
        (staged ? fill("secretsStaged", { count: staged.cookieCount, domains: staged.domains.join(", "), expires: staged.expiresAt }) : null),
    );
  }

  function toggle(profile: ScannedProfile, domain: string): void {
    const key = `${profile.family}/${profile.profile}`;
    setChosen((previous) => {
      const current = previous[key] ?? [];
      return { ...previous, [key]: current.includes(domain) ? current.filter((entry) => entry !== domain) : [...current, domain] };
    });
  }

  return (
    <section className="secrets-settings" aria-label={t("secretsTitle")}>
      <h3>{t("secretsTitle")}</h3>
      <p className="stage">{t("secretsLead")}</p>
      {status !== null && !status.available ? <p className="notice">{t("secretsUnavailable")}</p> : null}
      {status?.locked === true ? <p className="notice">{t("secretsLocked")}</p> : null}
      <h4>{t("secretsSavedLogins")}</h4>
      {logins.length === 0 ? (
        <p className="stage">{t("secretsNone")}</p>
      ) : (
        <ul>
          {logins.map((login) => (
            <li key={login.id}>
              <span className="live-untrusted">{login.username}</span> · <span className="live-untrusted">{login.origin}</span>{" "}
              <button type="button" onClick={() => { void remove(login.id); }}>{t("secretsRemove")}</button>
            </li>
          ))}
        </ul>
      )}
      <form ref={form} autoComplete="off" onSubmit={(event) => { void save(event); }}>
        <label>{t("secretsOrigin")}<input name="origin" type="text" placeholder="https://example.com" autoComplete="off" spellCheck={false} /></label>
        <label>{t("secretsUsername")}<input name="username" type="text" autoComplete="off" spellCheck={false} /></label>
        <label>{t("secretsPassword")}<input name="password" type="password" autoComplete="new-password" /></label>
        <button type="submit">{t("secretsSave")}</button>
      </form>
      <h4>{t("secretsImportTitle")}</h4>
      <p className="stage">{t("secretsImportLead")}</p>
      <button type="button" onClick={() => { void scan(); }}>{t("secretsScan")}</button>
      {profiles !== null && profiles.length === 0 ? <p className="stage">{t("secretsNoBrowsers")}</p> : null}
      {(profiles ?? []).map((profile) => {
        const key = `${profile.family}/${profile.profile}`;
        return (
          <div key={key} className="secrets-profile" data-profile={key}>
            <strong>{profile.familyLabel}</strong> · {profile.displayName}
            {profile.logins !== null && profile.logins > 0 ? (
              <p>
                {fill("secretsLoginsFound", { count: profile.logins })}{" "}
                <button type="button" onClick={() => { void importLogins(profile); }}>{t("secretsImportLogins")}</button>
              </p>
            ) : null}
            {profile.cookies !== null && profile.cookies.total > 0 ? (
              <fieldset>
                <legend>{fill("secretsCookiesFound", { count: profile.cookies.total })}</legend>
                {profile.cookies.domains.map((entry) => (
                  <label key={entry.domain}>
                    <input type="checkbox" checked={(chosen[key] ?? []).includes(entry.domain)} onChange={() => { toggle(profile, entry.domain); }} />
                    <span className="live-untrusted">{entry.domain}</span> ({entry.count})
                  </label>
                ))}
                <button type="button" disabled={(chosen[key] ?? []).length === 0} onClick={() => { void stage(profile); }}>{t("secretsStageCookies")}</button>
              </fieldset>
            ) : null}
          </div>
        );
      })}
      {message ? <p className="notice">{message}</p> : null}
    </section>
  );
}
