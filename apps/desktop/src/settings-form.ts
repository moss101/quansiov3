/**
 * The Settings form's fields and how a read of the effective settings meets what the person is
 * typing (APP-002/APP-003). The read is asynchronous — when the page opens and after every save —
 * so a field the person changed since it was last saved keeps their value; every other field
 * takes the value the server says is in effect.
 */

export interface SettingsFields {
  readonly name: string;
  readonly target: string;
  readonly retention: string;
  readonly theme: string;
  /** The autonomy mode in force (`standard`, `ask_for_consequences`, `ask_every_action`, `read_only`). */
  readonly autonomy: string;
  /** Security controls by key (`telemetry`, …). */
  readonly security: Readonly<Record<string, boolean>>;
}

/** A field's key: `name`, `target`, `retention`, `theme`, or `security.<control>`. */
export type FieldKey = string;

/** The fields as the server has them for a workspace view from `/v1/me`. */
export function fieldsFromWorkspace(workspace: { readonly name: string; readonly effective_settings: Readonly<Record<string, unknown>> }, securityKeys: readonly string[]): SettingsFields {
  const effective = workspace.effective_settings;
  return {
    name: workspace.name,
    target: typeof effective["execution_target"] === "string" ? effective["execution_target"] : "cloud",
    retention: String(typeof effective["retention_days"] === "number" ? effective["retention_days"] : 365),
    theme: typeof effective["theme"] === "string" ? effective["theme"] : "system",
    autonomy: typeof effective["autonomy_mode"] === "string" ? effective["autonomy_mode"] : "standard",
    security: Object.fromEntries(securityKeys.map((key) => [key, effective[key] === true])),
  };
}

/** The server's fields, except where the person has an unsaved edit. */
export function mergeLoaded(current: SettingsFields, loaded: SettingsFields, edited: ReadonlySet<FieldKey>): SettingsFields {
  const security: Record<string, boolean> = { ...loaded.security };
  for (const key of Object.keys(current.security)) {
    if (edited.has(`security.${key}`)) {
      security[key] = current.security[key] === true;
    }
  }
  return {
    name: edited.has("name") ? current.name : loaded.name,
    target: edited.has("target") ? current.target : loaded.target,
    retention: edited.has("retention") ? current.retention : loaded.retention,
    theme: edited.has("theme") ? current.theme : loaded.theme,
    autonomy: edited.has("autonomy") || edited.has("myAutonomy") ? current.autonomy : loaded.autonomy,
    security,
  };
}

function valueOf(fields: SettingsFields, key: FieldKey): unknown {
  return key.startsWith("security.") ? fields.security[key.slice("security.".length)] === true : fields[key as Exclude<keyof SettingsFields, "security">];
}

/**
 * The edits still unsaved after a save of `keys` with the values in `sent`: a field saved with
 * the value it still has is no longer an edit; one changed again while the save was in flight is.
 */
export function afterSave(edited: ReadonlySet<FieldKey>, keys: readonly FieldKey[], sent: SettingsFields, now: SettingsFields): Set<FieldKey> {
  const next = new Set(edited);
  for (const key of keys) {
    if (valueOf(sent, key) === valueOf(now, key)) {
      next.delete(key);
    }
  }
  return next;
}
