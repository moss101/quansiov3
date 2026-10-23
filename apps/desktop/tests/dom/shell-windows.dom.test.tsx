// @vitest-environment jsdom
/** ADP-004 / ADP-009 on a page: the shell's small windows (indicator, quick entry, pop-out) and the live session's payment and login panels. */

import { act } from "react";
import { afterEach, describe, expect, it } from "vitest";

import { Indicator } from "../../src/renderer/Indicator.js";
import { LoginSuggestions } from "../../src/renderer/LoginSuggestions.js";
import { PaymentInput, originOfPage } from "../../src/renderer/PaymentInput.js";
import { Popout } from "../../src/renderer/Popout.js";
import { QuickEntry } from "../../src/renderer/QuickEntry.js";
import { click, mount, text, type, type Rig } from "./rig.js";

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

function sent(channel: string): { channel: string; payload: unknown }[] {
  return rig.calls.filter((call) => call.channel === channel);
}

const ok = (result: Record<string, unknown> = {}): Record<string, unknown> => ({ ok: true, result });
const refused = (code: string, message: string): Record<string, unknown> => ({ ok: false, error: { code, message, details: {} } });

const ITEMS = {
  items: [
    { key: "a1", kind: "approval", group: "waiting", title: "Send the email?" },
    { key: "r1", kind: "run", group: "running", title: "Weekly report" },
    { key: "e1", kind: "run", group: "error", title: "Broken run" },
    { key: "x1", kind: "remote_ask_control", group: "waiting", title: "Ada wants control" },
    { key: "x2", kind: "remote_watching", group: "running", title: "Ada is watching" },
    { key: "bad", kind: "run", group: "nonsense", title: "dropped" },
  ],
};

function pointer(type: string, init: Record<string, unknown> = {}): Event {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true, ...init });
  Object.assign(event, { pointerId: 1 });
  return event;
}

describe("the Indicator window", () => {
  it("lists activity grouped waiting first, with remote sessions answered rather than dismissed", async () => {
    rig = await mount(<Indicator />, { "shell.activity.list": ok(ITEMS) });
    expect(sent("shell.pending.consume")).toHaveLength(1);
    expect(rig.container.querySelector(".indicator-count")?.textContent).toBe("5");
    expect(rig.container.querySelector(".indicator-anchor")?.className).toContain("waiting");
    expect([...rig.container.querySelectorAll(".indicator-group h2")].map((node) => node.textContent)).toHaveLength(3);
    expect(text(rig)).not.toContain("dropped");
    const remote = [...rig.container.querySelectorAll(".indicator-item")].find((item) => item.textContent.includes("wants control"));
    expect([...(remote?.querySelectorAll("button") ?? [])].map((button) => button.textContent)).toEqual(["Allow view", "Allow control", "Deny"]);
    await click(rig, "Allow control");
    expect(sent("shell.remote.answer")[0]?.payload).toEqual({ key: "x1", answer: "control" });
    await click(rig, "Stop");
    expect(sent("shell.remote.answer")[1]?.payload).toEqual({ key: "x2", answer: "stop" });
  });

  it("opens and dismisses an ordinary row", async () => {
    rig = await mount(<Indicator />, { "shell.activity.list": ok(ITEMS) });
    await click(rig, "Open");
    expect(sent("shell.activity.open")[0]?.payload).toEqual({ key: "a1" });
    await click(rig, "Dismiss");
    expect(sent("shell.activity.dismiss")[0]?.payload).toEqual({ key: "a1" });
  });

  it("says nothing needs you when empty, and follows pushed activity", async () => {
    rig = await mount(<Indicator />, { "shell.activity.list": ok({ items: [] }) });
    expect(text(rig)).toContain("Nothing needs you.");
    expect(rig.container.querySelector(".indicator-anchor")?.className).not.toContain("waiting");
    await rig.push("shell.activity", ITEMS);
    expect(rig.container.querySelector(".indicator-count")?.textContent).toBe("5");
  });

  it("tells main when the pointer is over the window so only then it takes mouse events", async () => {
    rig = await mount(<Indicator />, { "shell.activity.list": ok({ items: [] }) });
    const list = rig.container.querySelector(".indicator-list") as HTMLElement;
    await act(async () => {
      list.dispatchEvent(new MouseEvent("pointerover", { bubbles: true }));
      list.dispatchEvent(new MouseEvent("pointerenter"));
      await Promise.resolve();
    });
    const anchor = rig.container.querySelector(".indicator-anchor") as HTMLElement;
    await act(async () => {
      anchor.dispatchEvent(new MouseEvent("pointerover", { bubbles: true }));
      anchor.dispatchEvent(new MouseEvent("pointerout", { bubbles: true, relatedTarget: document.body }));
      await Promise.resolve();
    });
    await rig.flush();
    const flips = sent("shell.indicator.interactive").map((call) => (call.payload as { active: boolean }).active);
    expect(flips.length).toBeGreaterThan(0);
    expect(flips[0]).toBe(true);
  });

  it("collapses on a click of the anchor and moves the window on a drag", async () => {
    rig = await mount(<Indicator />, { "shell.activity.list": ok(ITEMS) });
    const anchor = rig.container.querySelector(".indicator-anchor") as HTMLButtonElement;
    Object.assign(anchor, { setPointerCapture: () => undefined, releasePointerCapture: () => undefined });
    const fire = async (kind: string, init: Record<string, unknown>): Promise<void> => {
      await act(async () => {
        anchor.dispatchEvent(pointer(kind, init));
        await Promise.resolve();
      });
    };
    await fire("pointerdown", { clientX: 10, clientY: 10 });
    await fire("pointermove", { clientX: 40, clientY: 30, screenX: 540, screenY: 330, buttons: 1 });
    await fire("pointerup", { clientX: 40, clientY: 30 });
    await rig.flush();
    expect(sent("shell.indicator.moveTo")[0]?.payload).toEqual({ x: 530, y: 320 });
    expect(rig.container.querySelector(".indicator-list")).not.toBeNull();
    await fire("pointerdown", { clientX: 10, clientY: 10 });
    await fire("pointerup", { clientX: 11, clientY: 11 });
    await rig.flush();
    expect(rig.container.querySelector(".indicator-list")).toBeNull();
    expect(anchor.getAttribute("aria-expanded")).toBe("false");
  });
});

describe("the Quick entry window", () => {
  it("sends the text on Return and clears it, never on an empty field or Shift+Return", async () => {
    rig = await mount(<QuickEntry />, { "shell.quickEntry.submit": ok() });
    const field = (): HTMLTextAreaElement => rig.container.querySelector("textarea") as HTMLTextAreaElement;
    const press = async (init: KeyboardEventInit): Promise<void> => {
      await act(async () => {
        field().dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }));
        await Promise.resolve();
      });
      await rig.flush();
    };
    await press({ key: "Enter" });
    expect(sent("shell.quickEntry.submit")).toHaveLength(0);
    await type(rig, "Ask your teammates", "  plan the launch ");
    await press({ key: "Enter", shiftKey: true });
    expect(sent("shell.quickEntry.submit")).toHaveLength(0);
    await press({ key: "Enter" });
    expect(sent("shell.quickEntry.submit")[0]?.payload).toEqual({ content: "plan the launch" });
    expect(field().value).toBe("");
  });

  it("keeps the text and shows the refusal when main refuses or throws", async () => {
    rig = await mount(<QuickEntry />, { "shell.quickEntry.submit": refused("PERMISSION_DENIED", "signed out") });
    await type(rig, "Ask your teammates", "hello");
    await click(rig, "Send");
    expect(rig.container.querySelector("[role=alert]")?.textContent).toBe("signed out");
    expect((rig.container.querySelector("textarea") as HTMLTextAreaElement).value).toBe("hello");
    rig.answer("shell.quickEntry.submit", () => Promise.reject(new Error("offline")));
    await click(rig, "Send");
    expect(rig.container.querySelector("[role=alert]")?.textContent).toBe("offline");
  });

  it("hides the window on Escape", async () => {
    rig = await mount(<QuickEntry />);
    await act(async () => {
      rig.container.querySelector("textarea")?.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
      await Promise.resolve();
    });
    expect(sent("shell.window.close")).toHaveLength(1);
  });
});

describe("the pop-out window", () => {
  it("says the thread cannot be opened when its context cannot be read", async () => {
    rig = await mount(<Popout threadId="thr_1" inspectorOpen={false} />, { "session.state": () => Promise.reject(new Error("gone")) });
    expect(text(rig)).toContain("This thread could not be opened.");
  });

  it("waits for a workspace, then keeps the window on top or closes it on request", async () => {
    rig = await mount(<Popout threadId="thr_1" inspectorOpen={false} />, { "session.state": {}, "shell.popout.pin": ok() });
    expect(text(rig)).toContain("Connecting to the runtime");
    await click(rig, "Keep on top");
    expect(sent("shell.popout.pin")[0]?.payload).toEqual({ pinned: true });
    expect(rig.container.querySelector("[aria-pressed]")?.getAttribute("aria-pressed")).toBe("true");
    await click(rig, "Close");
    expect(sent("shell.window.close")).toHaveLength(1);
  });

  it("does not show a pin that main refused", async () => {
    rig = await mount(<Popout threadId="thr_1" inspectorOpen={false} />, { "session.state": {}, "shell.popout.pin": refused("INTERNAL", "x") });
    await click(rig, "Keep on top");
    expect(rig.container.querySelector("[aria-pressed]")?.getAttribute("aria-pressed")).toBe("false");
  });

  it("binds the conversation surface to the thread once the workspace is known", async () => {
    rig = await mount(<Popout threadId="thr_1" inspectorOpen={false} />, {
      "session.state": { workspaceId: "ws_1" }, "session.me": { user: { id: "usr_1" } },
      "teammates.list": { teammates: [{ id: "agt_1", name: "Atlas", status: "active", thread: { id: "ath_1" } }] },
    });
    expect(text(rig)).not.toContain("Connecting to the runtime");
    expect(sent("threads.messages").some((call) => JSON.stringify(call.payload).includes("thr_1"))).toBe(true);
  });
});

describe("the payment form", () => {
  it("only accepts an https (or loopback) page as the origin", () => {
    expect(originOfPage("https://shop.example/checkout?a=1")).toBe("https://shop.example");
    expect(originOfPage("http://localhost:3000/pay")).toBe("http://localhost:3000");
    expect(originOfPage("http://shop.example/pay")).toBeNull();
    expect(originOfPage("not a url")).toBeNull();
    expect(originOfPage(null)).toBeNull();
  });

  it("offers nothing without a suppliable origin", async () => {
    rig = await mount(<PaymentInput pageUrl="http://shop.example" />);
    expect(text(rig)).toContain("Open an https page in the session first");
    expect(rig.container.querySelector("form")).toBeNull();
  });

  async function fillAndSubmit(values: Record<string, string>): Promise<void> {
    await act(async () => {
      for (const [name, value] of Object.entries(values)) {
        const input = rig.container.querySelector<HTMLInputElement>(`input[name=${name}]`);
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set?.call(input, value);
      }
      (rig.container.querySelector("form") as HTMLFormElement).requestSubmit();
      await Promise.resolve();
    });
    await rig.flush();
  }

  it("supplies only the filled fields for the page's origin, clears the form, and shows expiry only", async () => {
    rig = await mount(<PaymentInput pageUrl="https://shop.example/checkout" />, { "payment.supply": ok({ supplied: [{ field: "card_number", expiresAt: "2026-10-01T00:05:00Z" }] }) });
    await fillAndSubmit({ card_number: "4242424242424242", cvc: "123" });
    expect(sent("payment.supply")[0]?.payload).toEqual({ origin: "https://shop.example", fields: { card_number: "4242424242424242", cvc: "123" } });
    expect(rig.container.querySelector<HTMLInputElement>("input[name=card_number]")?.value).toBe("");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("Ready for one use on https://shop.example · expires 2026-10-01T00:05:00Z");
    expect(text(rig)).not.toContain("4242");
  });

  it("sends nothing from an empty form, and shows a refusal or a failure", async () => {
    rig = await mount(<PaymentInput pageUrl="https://shop.example" />, { "payment.supply": refused("PERMISSION_DENIED", "no session") });
    await fillAndSubmit({});
    expect(sent("payment.supply")).toHaveLength(0);
    expect(rig.container.querySelector(".notice")?.textContent).toBe("Fill at least one field.");
    await fillAndSubmit({ city: "Oslo" });
    expect(rig.container.querySelector(".notice")?.textContent).toBe("Refused — PERMISSION_DENIED: no session");
    rig.answer("payment.supply", () => Promise.reject(new Error("main down")));
    await fillAndSubmit({ city: "Oslo" });
    expect(rig.container.querySelector(".notice")?.textContent).toBe("main down");
  });
});

describe("the login suggestions", () => {
  const SUGGESTIONS = ok({ entries: [{ id: "pw_1", origin: "https://shop.example", username: "ada" }] });

  it("shows nothing without a page or a suggestion", async () => {
    rig = await mount(<LoginSuggestions sessionId="ses_1" pageUrl={null} />);
    expect(rig.container.innerHTML).toBe("");
    await rig.unmount();
    rig = await mount(<LoginSuggestions sessionId="ses_1" pageUrl="https://shop.example" />, { "passwords.suggest": ok({ entries: [] }) });
    expect(rig.container.innerHTML).toBe("");
  });

  it("asks for the page's suggestions by URL and fills one on a click", async () => {
    rig = await mount(<LoginSuggestions sessionId="ses_1" pageUrl="https://shop.example/login" />, { "passwords.suggest": SUGGESTIONS, "passwords.fill": ok({ filled: true }) });
    expect(sent("passwords.suggest")[0]?.payload).toEqual({ pageUrl: "https://shop.example/login" });
    await click(rig, "Fill");
    expect(sent("passwords.fill")[0]?.payload).toEqual({ id: "pw_1", sessionId: "ses_1" });
    expect(rig.container.querySelector(".notice")?.textContent).toBe("Filled.");
  });

  it("shows a refused fill with its code", async () => {
    rig = await mount(<LoginSuggestions sessionId="ses_1" pageUrl="https://shop.example/login" />, { "passwords.suggest": SUGGESTIONS, "passwords.fill": refused("ORIGIN_MISMATCH", "different site") });
    await click(rig, "Fill");
    expect(rig.container.querySelector(".notice")?.textContent).toBe("ORIGIN_MISMATCH: different site");
  });
});
