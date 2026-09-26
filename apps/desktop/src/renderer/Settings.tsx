/**
 * Settings (APP-002): the workspace's and the person's own layer of the layered settings, the
 * effective result after the merge, and what a change sets in motion (a restart or a reindex).
 *
 * The server owns the merge (security controls only ever get stricter) and says what a change
 * restarts or reindexes; this page shows the effective values it returns rather than what was
 * asked for, so a change a stricter layer overrides is visible as such.
 */

import { useCallback, useEffect, useId, useRef, useState, type ReactElement } from "react";

import { applyTheme, themeFromMe } from "../appearance.js";
import { t, type StringKey } from "../i18n/strings.js";
import { afterSave, fieldsFromWorkspace, mergeLoaded, type FieldKey, type SettingsFields } from "../settings-form.js";

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

const EMPTY: SettingsFields = { name: "", target: "cloud", retention: "365", theme: "system", security: {} };
const WORKSPACE_FIELDS: readonly FieldKey[] = ["name", "target", "retention", ...SECURITY.map(([key]) => `security.${key}`)];

export function Settings({ workspaceId }: { readonly workspaceId: string }): ReactElement {
  const ids = useId();
  const [workspace, setWorkspace] = useState<WorkspaceView | null>(null);
  const [fields, setFields] = useState<SettingsFields>(EMPTY);
  const [loaded, setLoaded] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  // Fields the person changed since they were last saved: a read of the effective settings never
  // overwrites them (the read is asynchronous, on opening and after every save).
  const edited = useRef<Set<FieldKey>>(new Set());
  const current = useRef<SettingsFields>(EMPTY);
  current.current = fields;

  const edit = useCallback((key: FieldKey, change: (previous: SettingsFields) => SettingsFields) => {
    edited.current.add(key);
    setFields(change);
  }, []);

  const load = useCallback(async () => {
    const payload = await window.quansio.me();
    const me = payload as { readonly workspaces: readonly WorkspaceView[] };
    const view = me.workspaces.find((candidate) => candidate.id === workspaceId) ?? null;
    // A saved theme takes effect at once, as the effective settings now say it.
    const effectiveTheme = themeFromMe(payload, workspaceId);
    applyTheme(effectiveTheme, document.documentElement);
    void window.quansio.setAppearance(effectiveTheme);
    setWorkspace(view);
    if (view) {
      const fromServer = fieldsFromWorkspace(view, SECURITY.map(([key]) => key));
      setFields((previous) => mergeLoaded(previous, fromServer, edited.current));
    }
    setLoaded(true);
  }, [workspaceId]);

  useEffect(() => {
    void load();
  }, [load]);

  const saved = useCallback(
    async (result: unknown, keys: readonly FieldKey[], sent: SettingsFields) => {
      edited.current = afterSave(edited.current, keys, sent, current.current);
      const effects = (result as SettingsResult).effects ?? [];
      setStatus([t("settingsSaved"), ...effects.map((effect) => effect.reason)].join(" "));
      await load();
    },
    [load],
  );

  const saveWorkspace = useCallback(() => {
    const sent = current.current;
    const days = Number.parseInt(sent.retention, 10);
    const settings: Record<string, unknown> = { execution_target: sent.target, ...sent.security };
    if (Number.isFinite(days) && days > 0) {
      settings["retention_days"] = days;
    }
    window.quansio.updateWorkspace(workspaceId, { name: sent.name.trim(), settings }).then((result) => saved(result, WORKSPACE_FIELDS, sent), (error: unknown) => {
      setStatus(refusal(error));
    });
  }, [workspaceId, saved]);

  const saveMine = useCallback(() => {
    const sent = current.current;
    window.quansio.updateUserSettings(workspaceId, { theme: sent.theme }).then((result) => saved(result, ["theme"], sent), (error: unknown) => {
      setStatus(refusal(error));
    });
  }, [workspaceId, saved]);

  const { name, target, retention, theme, security } = fields;
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
        {/* Said once the role is known, not while the page is still reading it. */}
        {loaded && !admin ? <p>{t("settingsAdminOnly")}</p> : null}
        <fieldset disabled={!admin}>
          <label htmlFor={`${ids}-name`}>{t("settingsWorkspaceName")}</label>
          <input id={`${ids}-name`} type="text" value={name} onChange={(event) => { const value = event.target.value; edit("name", (previous) => ({ ...previous, name: value })); }} />
          <label htmlFor={`${ids}-target`}>{t("settingsExecutionTarget")}</label>
          <select id={`${ids}-target`} value={target} onChange={(event) => { const value = event.target.value; edit("target", (previous) => ({ ...previous, target: value })); }}>
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
                onChange={(event) => { const checked = event.target.checked; edit(`security.${key}`, (previous) => ({ ...previous, security: { ...previous.security, [key]: checked } })); }}
              />
              <label htmlFor={`${ids}-${key}`}>{t(label)}</label>
            </div>
          ))}
          <label htmlFor={`${ids}-retention`}>{t("settingsRetention")}</label>
          <input id={`${ids}-retention`} type="number" min={1} value={retention} onChange={(event) => { const value = event.target.value; edit("retention", (previous) => ({ ...previous, retention: value })); }} />
          <button type="submit" disabled={!loaded}>{t("settingsSaveWorkspace")}</button>
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
        <select id={`${ids}-theme`} value={theme} onChange={(event) => { const value = event.target.value; edit("theme", (previous) => ({ ...previous, theme: value })); }}>
          {THEMES.map(([value, label]) => (
            <option key={value} value={value}>{t(label)}</option>
          ))}
        </select>
        <button type="submit" disabled={!loaded}>{t("settingsSaveMine")}</button>
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
