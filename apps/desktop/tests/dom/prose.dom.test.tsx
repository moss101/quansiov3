// @vitest-environment jsdom
/** APP-004 on a page: message text becomes elements, never markup, and only http(s) links become anchors. */

import { afterEach, describe, expect, it } from "vitest";

import { Prose } from "../../src/renderer/Prose.js";
import { mount, type Rig } from "./rig.js";

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

describe("Prose", () => {
  it("renders headings, quotes, fenced code, ordered and bullet lists and inline spans", async () => {
    const source = ["# Title", "## Sub", "> quoted", "> more", "", "```", "const a = 1;", "```", "", "1. first", "2) second", "", "* bullet", "", "see **bold**, `code` and [docs](https://example.com/d) or https://example.com/x."].join("\n");
    rig = await mount(<Prose text={source} />);
    const root = rig.container;
    expect(root.querySelector("h4")?.textContent).toBe("Title");
    expect(root.querySelector("h5")?.textContent).toBe("Sub");
    expect(root.querySelector("blockquote")?.textContent).toBe("quoted\nmore");
    expect(root.querySelector("pre code")?.textContent).toBe("const a = 1;");
    expect([...root.querySelectorAll("ol li")].map((li) => li.textContent)).toEqual(["first", "second"]);
    expect([...root.querySelectorAll("ul li")].map((li) => li.textContent)).toEqual(["bullet"]);
    expect(root.querySelector("strong")?.textContent).toBe("bold");
    const links = [...root.querySelectorAll("a")];
    expect(links.map((a) => a.getAttribute("href"))).toEqual(["https://example.com/d", "https://example.com/x"]);
    expect(links[0]?.getAttribute("rel")).toBe("noreferrer noopener");
    // Trailing punctuation stays outside a bare link.
    expect(root.querySelector("p:last-child")?.textContent.endsWith(".")).toBe(true);
  });

  it("keeps markup and non-http links as inert text", async () => {
    rig = await mount(<Prose text={'<img src=x onerror=alert(1)> [x](javascript:alert(1)) <script>boom()</script>'} />);
    expect(rig.container.querySelector("img, script")).toBeNull();
    expect(rig.container.querySelector("a")).toBeNull();
    expect(rig.container.textContent).toContain("<script>boom()</script>");
  });
});
