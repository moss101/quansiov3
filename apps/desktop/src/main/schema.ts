/**
 * A small schema validator for IPC payloads (ADP-004).
 *
 * Every payload a window sends is untrusted data. The shell's own channels describe the exact shape they
 * accept with these combinators instead of ad-hoc `typeof` checks: a schema names every field, bounds every
 * string and number, and refuses a field it does not know (the §15 `VALIDATION_UNKNOWN_FIELD` stance), so a
 * renderer cannot smuggle an extra key through a handler that spreads its payload. It is deliberately tiny and
 * dependency-free (the desktop adds no runtime dependency); it is not a general validation library.
 */

/** A validation failure: where in the payload, and what was wrong. */
export class SchemaError extends Error {
  readonly path: string;

  constructor(path: string, message: string) {
    super(`${path}: ${message}`);
    this.name = "SchemaError";
    this.path = path;
  }
}

/** A parser from untrusted data to `T`; it throws {@link SchemaError} rather than returning a partial value. */
export interface Schema<T> {
  readonly parse: (value: unknown, path: string) => T;
}

/** The result of {@link safeParse}. */
export type Parsed<T> = { readonly ok: true; readonly value: T } | { readonly ok: false; readonly error: SchemaError };

/** Parse without throwing. */
export function safeParse<T>(schema: Schema<T>, value: unknown, path = "payload"): Parsed<T> {
  try {
    return { ok: true, value: schema.parse(value, path) };
  } catch (error) {
    if (error instanceof SchemaError) {
      return { ok: false, error };
    }
    throw error;
  }
}

/** Parse, throwing {@link SchemaError}. */
export function parse<T>(schema: Schema<T>, value: unknown, path = "payload"): T {
  return schema.parse(value, path);
}

export interface StringRules {
  readonly min?: number;
  readonly max: number;
  readonly pattern?: RegExp;
}

/** A string within its bounds (and pattern, when one is given). */
export function string(rules: StringRules): Schema<string> {
  return {
    parse(value, path) {
      if (typeof value !== "string") {
        throw new SchemaError(path, "must be a string");
      }
      if (value.length > rules.max) {
        throw new SchemaError(path, `must be at most ${String(rules.max)} characters`);
      }
      if (value.length < (rules.min ?? 0)) {
        throw new SchemaError(path, `must be at least ${String(rules.min ?? 0)} characters`);
      }
      if (rules.pattern && !rules.pattern.test(value)) {
        throw new SchemaError(path, "has the wrong shape");
      }
      return value;
    },
  };
}

export interface IntegerRules {
  readonly min: number;
  readonly max: number;
}

/** A finite integer within its bounds. */
export function integer(rules: IntegerRules): Schema<number> {
  return {
    parse(value, path) {
      if (typeof value !== "number" || !Number.isInteger(value)) {
        throw new SchemaError(path, "must be an integer");
      }
      if (value < rules.min || value > rules.max) {
        throw new SchemaError(path, `must be between ${String(rules.min)} and ${String(rules.max)}`);
      }
      return value;
    },
  };
}

/** A boolean. */
export function boolean(): Schema<boolean> {
  return {
    parse(value, path) {
      if (typeof value !== "boolean") {
        throw new SchemaError(path, "must be true or false");
      }
      return value;
    },
  };
}

/** Exactly one value. */
export function literal<const T extends string | number | boolean>(expected: T): Schema<T> {
  return {
    parse(value, path) {
      if (value !== expected) {
        throw new SchemaError(path, `must be ${String(expected)}`);
      }
      return expected;
    },
  };
}

/** One of a closed set of strings. */
export function oneOf<const T extends readonly string[]>(values: T): Schema<T[number]> {
  return {
    parse(value, path) {
      if (typeof value !== "string" || !values.includes(value)) {
        throw new SchemaError(path, `must be one of ${values.join(", ")}`);
      }
      return value;
    },
  };
}

/** A value that may be absent (`undefined`). */
export function optional<T>(inner: Schema<T>): Schema<T | undefined> {
  return {
    parse(value, path) {
      return value === undefined ? undefined : inner.parse(value, path);
    },
  };
}

/** A value that may be `null`. */
export function nullable<T>(inner: Schema<T>): Schema<T | null> {
  return {
    parse(value, path) {
      return value === null ? null : inner.parse(value, path);
    },
  };
}

/** A list of at most `max` items. */
export function array<T>(item: Schema<T>, max: number): Schema<readonly T[]> {
  return {
    parse(value, path) {
      if (!Array.isArray(value)) {
        throw new SchemaError(path, "must be a list");
      }
      if (value.length > max) {
        throw new SchemaError(path, `must have at most ${String(max)} items`);
      }
      return (value as unknown[]).map((entry, index) => item.parse(entry, `${path}[${String(index)}]`));
    },
  };
}

/**
 * An object with exactly the named fields. A field whose schema accepts `undefined` may be absent; any
 * other field is required; a key the shape does not name is refused. The result carries only named keys, and
 * leaves an absent optional key absent (not `undefined`) so it satisfies `exactOptionalPropertyTypes`.
 */
export function object<T extends object>(shape: { readonly [K in keyof T]-?: Schema<T[K]> }): Schema<T> {
  return {
    parse(value, path) {
      if (value === null || typeof value !== "object" || Array.isArray(value)) {
        throw new SchemaError(path, "must be an object");
      }
      const record = value as Record<string, unknown>;
      const known = new Set(Object.keys(shape));
      for (const key of Object.keys(record)) {
        if (!known.has(key)) {
          throw new SchemaError(`${path}.${key}`, "is not a known field");
        }
      }
      const out: Record<string, unknown> = {};
      for (const [key, field] of Object.entries<Schema<unknown>>(shape)) {
        const parsed = field.parse(record[key], `${path}.${key}`);
        if (parsed !== undefined) {
          out[key] = parsed;
        }
      }
      return out as T;
    },
  };
}

/** A map from keys matching `keyPattern` to values, with at most `max` entries. */
export function record<T>(value: Schema<T>, keyPattern: RegExp, max: number): Schema<Readonly<Record<string, T>>> {
  return {
    parse(input, path) {
      if (input === null || typeof input !== "object" || Array.isArray(input)) {
        throw new SchemaError(path, "must be an object");
      }
      const keys = Object.keys(input);
      if (keys.length > max) {
        throw new SchemaError(path, `must have at most ${String(max)} entries`);
      }
      const out: Record<string, T> = {};
      for (const key of keys) {
        if (!keyPattern.test(key)) {
          throw new SchemaError(`${path}.${key}`, "is not an accepted key");
        }
        out[key] = value.parse((input as Record<string, unknown>)[key], `${path}.${key}`);
      }
      return out;
    },
  };
}

/** The empty payload: `undefined`, `null` or `{}`; anything else is refused. */
export function empty(): Schema<Record<string, never>> {
  return {
    parse(value, path) {
      if (value === undefined || value === null) {
        return {};
      }
      if (typeof value === "object" && !Array.isArray(value) && Object.keys(value).length === 0) {
        return {};
      }
      const first = typeof value === "object" && !Array.isArray(value) ? Object.keys(value)[0] : undefined;
      throw first === undefined ? new SchemaError(path, "takes no fields") : new SchemaError(`${path}.${first}`, "is not a known field");
    },
  };
}
