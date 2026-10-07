// The only surface a composition script sees (ADP-009, DOMAIN.md 7.5 "Composition sandbox").
//
// It is evaluated once per cell in a fresh interpreter, with three native functions handed in by the worker and nothing else:
// `hostCall`, `hostLog` and `hostImage`. It builds the script's API (`call`, `log`, `image`, `state`, `HostError`), removes
// the ways a script could build more code (`eval`, the `Function` constructors) and returns the two functions the worker
// uses to load and read back the persistent `state`. A script has no `fetch`, no files, no process, no timers, no module
// loader: the interpreter does not have them, and nothing here adds one.
(function (native) {
  "use strict";
  const hostCall = native.hostCall;
  const hostLog = native.hostLog;
  const hostImage = native.hostImage;
  const root = globalThis;

  // Every value a host call hands back is frozen all the way down. An element handle is a snapshot's reference: the script
  // can pass it back, and it can not edit it into another element, so it is never silently re-resolved.
  function deepFreeze(value) {
    if (value !== null && typeof value === "object" && !Object.isFrozen(value)) {
      for (const key of Object.keys(value)) {
        deepFreeze(value[key]);
      }
      Object.freeze(value);
    }
    return value;
  }

  // What a host call that failed with a known outcome throws. The script may catch it. A terminal condition is not this: the
  // worker ends the cell and the script's own result is discarded, so catching changes nothing.
  class HostError extends Error {
    constructor(info) {
      super(String(info.message));
      this.name = "HostError";
      this.code = String(info.code);
      this.retryable = info.retryable === true;
      if (info.details !== undefined && info.details !== null) {
        this.details = deepFreeze(info.details);
      }
      Object.freeze(this);
    }
  }

  function call(tool, args) {
    if (typeof tool !== "string") {
      throw new TypeError("call: the tool name must be a string");
    }
    let text;
    try {
      text = JSON.stringify(args === undefined ? {} : args);
    } catch (error) {
      throw new TypeError("call: the arguments are not JSON");
    }
    if (text === undefined) {
      text = "{}";
    }
    const answer = JSON.parse(hostCall(tool, text));
    if (answer.ok === true) {
      return deepFreeze(answer.value);
    }
    throw new HostError(answer.error);
  }

  function show(value) {
    if (typeof value === "string") {
      return value;
    }
    try {
      const text = JSON.stringify(value);
      return text === undefined ? String(value) : text;
    } catch (error) {
      return String(value);
    }
  }

  function log(...values) {
    hostLog(values.map(show).join(" "));
  }

  function image(reference) {
    hostImage(String(reference));
  }

  const api = { call, log, image, HostError };
  for (const name of Object.keys(api)) {
    Object.defineProperty(root, name, { value: api[name], writable: false, configurable: false, enumerable: false });
  }
  Object.defineProperty(root, "console", {
    value: Object.freeze({ log, info: log, warn: log, error: log }),
    writable: false,
    configurable: false,
    enumerable: false,
  });

  // No way to make more code: `eval` and every Function constructor (plain, async, generator, async generator) are gone.
  delete root.eval;
  delete root.Function;
  // Nor a way to schedule work: the worker runs a script's promise jobs to completion and nothing else, so there is no later.
  delete root.queueMicrotask;
  for (const sample of [function () {}, async function () {}, function* () {}, async function* () {}]) {
    Object.defineProperty(Object.getPrototypeOf(sample), "constructor", {
      value: undefined,
      writable: false,
      configurable: false,
    });
  }

  let current = {};
  return {
    // Make the session's `state` the global the script sees: an object whose properties the script may change and which it can
    // not replace.
    begin(json) {
      let parsed = {};
      if (typeof json === "string" && json.length > 0) {
        parsed = JSON.parse(json);
      }
      if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
        parsed = {};
      }
      current = parsed;
      Object.defineProperty(root, "state", { value: current, writable: false, configurable: false, enumerable: true });
    },
    // The state as JSON, or undefined when it can not be (a cycle, a BigInt).
    finish() {
      try {
        return JSON.stringify(current);
      } catch (error) {
        return undefined;
      }
    },
  };
});
