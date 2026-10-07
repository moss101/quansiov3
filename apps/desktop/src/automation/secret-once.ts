/**
 * The inbound hook's signing secret, shown once (ADP-007, DOMAIN.md §13.8 A).
 *
 * `CreateRoutine` and `RotateRoutineHook` return the endpoint path and a `whsec_…` signing secret
 * in their result; the server never returns the secret again (the projection says `secret: "set"`).
 * The page therefore holds the secret in exactly one place — this state — from the command's result
 * until the person dismisses it. Nothing else keeps it: not the routine list, not a notice, not a
 * settled payload that is stored. After dismissal the state has no secret to render.
 */

export type RevealKind = "created" | "rotated";

export interface SecretReveal {
  readonly kind: RevealKind;
  readonly routineId: string;
  /** `/v1/hooks/hk_…`; the endpoint is the address, the signature is the capability. */
  readonly path: string;
  /** The signing secret, or `null` when this result carried none (a replayed create). */
  readonly secret: string | null;
  readonly replayed: boolean;
}

/** `null` is nothing shown. */
export type SecretState = SecretReveal | null;

export const NOTHING_SHOWN: SecretState = null;

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

/**
 * The reveal a command result makes, or `null` when it names no hook (a routine with another
 * trigger kind has nothing to show). A result with a hook and no `secret` is a replayed create:
 * the path is shown and the page says the secret was shown the first time.
 */
export function revealFrom(kind: RevealKind, result: unknown): SecretReveal | null {
  const source = record(result);
  const hook = record(source["hook"]);
  const path = hook["path"];
  const id = source["id"];
  if (typeof path !== "string" || path === "" || typeof id !== "string") {
    return null;
  }
  const secret = source["secret"];
  return {
    kind,
    routineId: id,
    path,
    secret: typeof secret === "string" && secret !== "" ? secret : null,
    replayed: source["replayed"] === true,
  };
}

/** Dismiss: the state forgets the secret. (A new reveal replaces the state outright, so an earlier secret is gone with it.) */
export function dismiss(): SecretState {
  return NOTHING_SHOWN;
}

/**
 * A command result without its one-time field, for anything that keeps a result around (a log, a
 * notice, a stored settled payload). The secret is never part of what is kept.
 */
export function withoutSecret(result: unknown): unknown {
  const source = record(result);
  if (!("secret" in source)) {
    return result;
  }
  const { secret, ...rest } = source;
  return { ...rest, secret: typeof secret === "string" ? "shown once" : null };
}

/** What a copy button copies. */
export type CopyTarget = "path" | "secret";

/** The text a copy button puts on the clipboard, or `null` when there is nothing to copy. */
export function copyText(state: SecretState, target: CopyTarget): string | null {
  if (!state) {
    return null;
  }
  return target === "path" ? state.path : state.secret;
}
