/**
 * A browser-like rig for the renderer's views: jsdom, React's `act`, and the real preload API driven by a scripted `send`.
 * The views therefore run their real decoders and commands; only the other side of the bridge (main) is a table of
 * answers per channel. Unit-level only: nothing here is evidence for a real boundary.
 */

import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { vi } from "vitest";

import { createPreloadApi, type BridgeSend, type PreloadChannel, type PushChannel } from "../../src/preload/bridge.js";

type Answer = unknown;

export interface Rig {
  readonly container: HTMLElement;
  readonly calls: { channel: string; payload: unknown }[];
  /** Answer a channel from now on. */
  answer(channel: PreloadChannel, answer: Answer): void;
  /** Push a frame to every listener of a push channel. */
  push(channel: PushChannel, payload: unknown): Promise<void>;
  flush(): Promise<void>;
  unmount(): Promise<void>;
}

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// jsdom has no layout: scrolling an item into view is a no-op there.
Element.prototype.scrollIntoView = () => undefined;

/** Run `work` inside React's `act` so every state change it causes has been applied when this resolves. */
async function inAct(work: () => void): Promise<void> {
  await act(async () => {
    work();
    await Promise.resolve();
  });
}

export async function flush(rounds = 6): Promise<void> {
  for (let i = 0; i < rounds; i += 1) {
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    });
  }
}

/** Render `element` with a bridge that answers each channel from `answers` (missing channels answer `[]`). */
export async function mount(element: ReactElement, answers: Partial<Record<PreloadChannel, Answer>> = {}): Promise<Rig> {
  const table = new Map<string, Answer>(Object.entries(answers));
  const calls: Rig["calls"] = [];
  const listeners = new Map<string, Set<(payload: unknown) => void>>();
  const send: BridgeSend = (channel, payload) => {
    calls.push({ channel, payload });
    const answer = table.has(channel) ? table.get(channel) : [];
    return Promise.resolve(typeof answer === "function" ? (answer as (p: unknown) => unknown)(payload) : answer);
  };
  const subscribe = (channel: PushChannel, listener: (payload: unknown) => void): (() => void) => {
    const set = listeners.get(channel) ?? new Set();
    set.add(listener);
    listeners.set(channel, set);
    return () => set.delete(listener);
  };
  Object.defineProperty(window, "quansio", { value: createPreloadApi(send, subscribe), configurable: true, writable: true });
  const container = document.createElement("div");
  document.body.appendChild(container);
  let root: Root;
  await inAct(() => {
    root = createRoot(container);
    root.render(element);
  });
  await flush();
  return {
    container,
    calls,
    answer: (channel, answer) => void table.set(channel, answer),
    push: async (channel, payload) => {
      await inAct(() => {
        for (const listener of listeners.get(channel) ?? []) {
          listener(payload);
        }
      });
      await flush();
    },
    flush,
    unmount: async () => {
      await inAct(() => {
        root.unmount();
      });
      container.remove();
    },
  };
}

/** Click the first control within `scope` whose text (or aria-label or title) contains `label`. */
export async function clickIn(rig: Rig, scope: HTMLElement, label: string | RegExp): Promise<void> {
  const match = (text: string): boolean => (typeof label === "string" ? text.includes(label) : label.test(text));
  const target = [...scope.querySelectorAll<HTMLElement>("button, a, [role=button], [role=tab], [role=menuitem], summary, label")].find(
    (element) => match(element.textContent) || match(element.getAttribute("aria-label") ?? "") || match(element.getAttribute("title") ?? ""),
  );
  if (!target) {
    throw new Error(`no control named ${String(label)}; the scope has: ${scope.textContent.slice(0, 400)}`);
  }
  await inAct(() => {
    target.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
  await rig.flush();
}

/** Click the first control on the page whose text (or aria-label) contains `label`. */
export async function click(rig: Rig, label: string | RegExp): Promise<void> {
  await clickIn(rig, rig.container, label);
}

/** Choose `value` in a select, given the element or a selector for it. */
export async function choose(rig: Rig, select: string | HTMLSelectElement, value: string): Promise<void> {
  const element = typeof select === "string" ? rig.container.querySelector<HTMLSelectElement>(select) : select;
  if (!element) {
    throw new Error(`no select ${typeof select === "string" ? select : "element"}`);
  }
  await inAct(() => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")?.set?.call(element, value);
    element.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await rig.flush();
}

/** Set the value of the input/textarea/select found by placeholder, aria-label or name. */
export async function type(rig: Rig, field: string, value: string): Promise<void> {
  const element = rig.container.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(
    `[placeholder*="${field}"], [aria-label*="${field}"], [name="${field}"]`,
  );
  if (!element) {
    throw new Error(`no field ${field}`);
  }
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  await inAct(() => {
    Object.getOwnPropertyDescriptor(prototype, "value")?.set?.call(element, value);
    element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
  });
  await rig.flush();
}

export function text(rig: Rig): string {
  return rig.container.textContent;
}

export { vi };
