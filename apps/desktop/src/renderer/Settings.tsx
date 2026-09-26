/**
 * Settings (APP-002): the workspace's and the person's own layer of the layered settings, the
 * effective result after the merge, and what a change sets in motion (a restart or a reindex).
 *
 * The server owns the merge (security controls only ever get stricter) and says what a change
 * restarts or reindexes; this page shows the effective values it returns rather than what was
 * asked for, so a change a stricter layer overrides is visible as such.
 */

import { useCallback, useEffect, useId, useState, type ReactElement } from "react";

import { applyTheme, themeFromMe } from "../appearance.js";

import { t, type StringKey } from "../i18n/strings.js";

interface WorkspaceView {
  readonly id: string;
  readonly name: string;
  readonly role: string;
  readonly effective_settings: Record<string, unknown>;
}

interface SettingsResult {
  readonly effective_settings?: Record<string, unknown>;
  readonly effects?: readonly { readonly kind: string; readonly reason: string }[];
}

const TARGETS: readonly [string, StringKey][] = [
  ["local_capsule", "settingsTargetLocal"],
  ["cloud", "settingsTargetCloud"],
  ["private_worker", "settingsTargetPrivate"],
];
const THEMES: readonly [string, StringKey][] = [
  ["system", "settingsThemeSystem"],
  ["light", "settingsThemeLight"],
  ["dark", "settingsThemeDark"],
];
const SECURITY: readonly [string, StringKey][] = [
  ["telemetry", "settingsTelemetry"],
  ["share_analytics", "settingsShareAnalytics"],
  ["clipboard_to_model", "settingsClipboard"],
  ["unsigned_updates", "settingsUnsignedUpdates"],
];

function refusal(error: unknown): string {
  return error instanceof Error ? error.message : t("commandFailed");
}

export function Settings({ workspaceId }: { readonly workspaceId: string }): ReactElement {
  const ids = useId();
  const [workspace, setWorkspace] = useState<WorkspaceView | null>(null);
  const [name, setName] = useState("");
  const [target, setTarget] = useState("cloud");
  const [security, setSecurity] = useState<Record<string, boolean>>({});
  const [retention, setRetention] = useState("365");
  const [theme, setTheme] = useState("system");
  const [status, setStatus] = useState<string | null>(null);

  const load = useCallback(async () => {
    const payload = await window.quansio.me();
    const me = payload as { readonly workspaces: readonly WorkspaceView[] };
    const current = me.workspaces.find((candidate) => candidate.id === workspaceId) ?? null;
    // A saved theme takes effect at once, as the effective settings now say it.
    const effectiveTheme = themeFromMe(payload, workspaceId);
    applyTheme(effectiveTheme, document.documentElement);
    void window.quansio.setAppearance(effectiveTheme);
    setWorkspace(current);
    if (current) {
      const effective = current.effective_settings;
      setName(current.name);
      setTarget(typeof effective["execution_target"] === "string" ? effective["execution_target"] : "cloud");
      setSecurity(Object.fromEntries(SECURITY.map(([key]) => [key, effective[key] === true])));
      setRetention(String(typeof effective["retention_days"] === "number" ? effective["retention_days"] : 365));
      setTheme(typeof effective["theme"] === "string" ? effective["theme"] : "system");
    }
  }, [workspaceId]);

  useEffect(() => {
    void load();
  }, [load]);

  const report = useCallback(
    async (result: unknown) => {
      const effects = (result as SettingsResult).effects ?? [];
      setStatus([t("settingsSaved"), ...effects.map((effect) => effect.reason)].join(" "));
      await load();
    },
    [load],
  );

  const saveWorkspace = useCallback(() => {
    const days = Number.parseInt(retention, 10);
    const settings: Record<string, unknown> = { execution_target: target, ...security };
    if (Number.isFinite(days) && days > 0) {
      settings["retention_days"] = days;
    }
    window.quansio.updateWorkspace(workspaceId, { name: name.trim(), settings }).then(report, (error: unknown) => {
      setStatus(refusal(error));
    });
  }, [workspaceId, name, target, security, retention, report]);

  const saveMine = useCallback(() => {
    window.quansio.updateUserSettings(workspaceId, { theme }).then(report, (error: unknown) => {
      setStatus(refusal(error));
    });
  }, [workspaceId, theme, report]);

  const admin = workspace?.role === "admin";
  const effective = workspace?.effective_settings ?? {};

  return (
    <section className="settings" aria-label={t("settings")}>
      <h2>{t("settings")}</h2>
      <p role="status">{status ?? ""}</p>
      <form
        aria-label={t("settingsWorkspace")}
        onSubmit={(event) => {
          event.preventDefault();
          saveWorkspace();
        }}
      >
        <h3>{t("settingsWorkspace")}</h3>
        {admin ? null : <p>{t("settingsAdminOnly")}</p>}
        <fieldset disabled={!admin}>
          <label htmlFor={`${ids}-name`}>{t("settingsWorkspaceName")}</label>
          <input id={`${ids}-name`} type="text" value={name} onChange={(event) => { setName(event.target.value); }} />
          <label htmlFor={`${ids}-target`}>{t("settingsExecutionTarget")}</label>
          <select id={`${ids}-target`} value={target} onChange={(event) => { setTarget(event.target.value); }}>
            {TARGETS.map(([value, label]) => (
              <option key={value} value={value}>{t(label)}</option>
            ))}
          </select>
          <h4>{t("settingsPrivacy")}</h4>
          <p>{t("settingsPrivacyNote")}</p>
          {SECURITY.map(([key, label]) => (
            <div key={key}>
              <input
                id={`${ids}-${key}`}
                type="checkbox"
                checked={security[key] === true}
                onChange={(event) => { setSecurity((current) => ({ ...current, [key]: event.target.checked })); }}
              />
              <label htmlFor={`${ids}-${key}`}>{t(label)}</label>
            </div>
          ))}
          <label htmlFor={`${ids}-retention`}>{t("settingsRetention")}</label>
          <input id={`${ids}-retention`} type="number" min={1} value={retention} onChange={(event) => { setRetention(event.target.value); }} />
          <button type="submit">{t("settingsSaveWorkspace")}</button>
        </fieldset>
      </form>
      <form
        aria-label={t("settingsYou")}
        onSubmit={(event) => {
          event.preventDefault();
          saveMine();
        }}
      >
        <h3>{t("settingsYou")}</h3>
        <label htmlFor={`${ids}-theme`}>{t("settingsTheme")}</label>
        <select id={`${ids}-theme`} value={theme} onChange={(event) => { setTheme(event.target.value); }}>
          {THEMES.map(([value, label]) => (
            <option key={value} value={value}>{t(label)}</option>
          ))}
        </select>
        <button type="submit">{t("settingsSaveMine")}</button>
      </form>
      <h3>{t("settingsEffective")}</h3>
      <dl>
        {Object.entries(effective)
          .sort(([left], [right]) => left.localeCompare(right))
          .map(([key, value]) => (
            <div key={key}>
              <dt>{key}</dt>
              <dd>{typeof value === "string" ? value : JSON.stringify(value)}</dd>
            </div>
          ))}
      </dl>
    </section>
  );
}
