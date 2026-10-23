/**
 * A browser-like rig for the web shell's views: jsdom, React's `act`, and the real `WebSession`/`WebClient` over a scripted
 * `fetch` (a table of answers per `METHOD path`). The views therefore run their real decoders and commands. Unit-level only:
 * nothing here is evidence for a real boundary.
 */

import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";

import { WebSession } from "../../src/session.js";

export interface Answered {
  readonly status?: number;
  readonly body: unknown;
}

export interface WebRig {
  readonly container: HTMLElement;
  readonly session: WebSession;
  readonly requests: { method: string; path: string; body: unknown }[];
  flush(): Promise<void>;
  unmount(): Promise<void>;
}

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

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

const stored = (workspaceId: string | null): string =>
  JSON.stringify({ access_token: "at", refresh_token: "rt", tenant_id: "tn_1", user_id: "usr_1", ...(workspaceId ? { workspace_id: workspaceId } : {}) });

// jsdom has no layout: scrolling a selected item into view is a no-op there.
Element.prototype.scrollIntoView = () => undefined;

/** Answers are keyed `GET /v1/approvals` (query string ignored); an unlisted request answers 404. */
export async function mountWeb(render: (session: WebSession) => ReactElement, answers: Record<string, unknown> = {}, signedIn = true, workspaceId: string | null = "ws_1"): Promise<WebRig> {
  const requests: WebRig["requests"] = [];
  const fetchImpl = (input: string, init: RequestInit): Promise<Response> => {
    const url = new URL(input, "http://localhost");
    const method = init.method ?? "GET";
    const body: unknown = typeof init.body === "string" ? JSON.parse(init.body) : null;
    requests.push({ method, path: url.pathname + url.search, body });
    const key = `${method} ${url.pathname}`;
    const hit = answers[key];
    if (hit === undefined) {
      return Promise.resolve(new Response(JSON.stringify({ code: "NOT_FOUND", message: `no answer for ${key}`, correlation_id: "c", retryable: false }), { status: 404 }));
    }
    const answered = typeof hit === "function" ? (hit as (b: unknown) => unknown)(body) : hit;
    const wrapped = answered && typeof answered === "object" && "body" in answered ? (answered as Answered) : { body: answered };
    return Promise.resolve(new Response(JSON.stringify(wrapped.body), { status: wrapped.status ?? 200 }));
  };
  const storage = new Map<string, string>(signedIn ? [["quansio.web.session", stored(workspaceId)]] : []);
  const session = new WebSession("http://localhost", {
    storage: { getItem: (k) => storage.get(k) ?? null, setItem: (k, v) => void storage.set(k, v), removeItem: (k) => void storage.delete(k) },
    fetchImpl,
  });
  const container = document.createElement("div");
  document.body.appendChild(container);
  let root: Root;
  await inAct(() => {
    root = createRoot(container);
    root.render(render(session));
  });
  await flush();
  return {
    container,
    session,
    requests,
    flush,
    unmount: async () => {
      await inAct(() => {
        root.unmount();
      });
      container.remove();
    },
  };
}

export async function click(rig: WebRig, label: string | RegExp): Promise<void> {
  const match = (value: string | null): boolean => value !== null && (typeof label === "string" ? value.includes(label) : label.test(value));
  const target = [...rig.container.querySelectorAll<HTMLElement>("button, a, [role=button]")].find((el) => match(el.textContent) || match(el.getAttribute("aria-label")));
  if (!target) {
    throw new Error(`no control named ${String(label)}; the page has: ${rig.container.textContent.slice(0, 400)}`);
  }
  await inAct(() => {
    target.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  });
  await rig.flush();
}

export async function fill(rig: WebRig, field: string, value: string): Promise<void> {
  const element = rig.container.querySelector<HTMLInputElement | HTMLSelectElement>(`[placeholder*="${field}"], [aria-label*="${field}"]`);
  if (!element) {
    throw new Error(`no field ${field}`);
  }
  const prototype = element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
  await inAct(() => {
    Object.getOwnPropertyDescriptor(prototype, "value")?.set?.call(element, value);
    element.dispatchEvent(new Event(element instanceof HTMLSelectElement ? "change" : "input", { bubbles: true }));
  });
  await rig.flush();
}

export function text(rig: WebRig): string {
  return rig.container.textContent;
}
