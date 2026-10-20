/**
 * The live viewer in a real Electron window, driving a worker's real Chrome (APP-007, EXEC-002).
 *
 * A person asks for a page; the agent's `browser.navigate` runs on the worker's Chrome; the Live surface shows the picture
 * the worker streamed; the person takes the session over and clicks, right-clicks, double-clicks, drags and types on the
 * picture; the page, a loopback site that counts the requests it really receives, sees each of them; and the person hands
 * the session back. Nothing between the keyboard and the page is simulated: the Electron renderer, the main process, the
 * server over HTTP and a WebSocket, PostgreSQL, the worker daemon and Chrome are the real ones.
 *
 * Environment: the QA-008 harness (`QUANSIO_TEST_POSTGRES_URL`, the server, seed and `qworkerd` binaries, the built desktop
 * app) and Google Chrome.
 */

import { createServer, type Server } from "node:http";
import { existsSync } from "node:fs";

import { afterAll, beforeAll, describe, expect, it } from "vitest";

import { chromeBlocked, desktopBlocked, emailOf, launchDesktop, openSurface, OWNER, signInDesktop, Stack, waitFor, type Desktop } from "./harness.js";

const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const blocked = desktopBlocked() ?? (existsSync(CHROME) ? null : chromeBlocked());

/** A page that is one button over the whole viewport and reports what the pointer and keyboard did, by request. */
const PAGE = `<!doctype html><html><body style="margin:0"><button id="b" style="position:fixed;inset:0;width:100%;height:100%;font-size:40px">Press me</button>
<input id="i" style="position:fixed;left:10px;bottom:10px;width:300px" aria-label="type here">
<script>
function ping(p){var x=new XMLHttpRequest();x.open('GET',p,false);x.send();}
var b=document.getElementById('b'),i=document.getElementById('i'),down=null;
b.addEventListener('click',function(e){ping(e.detail===1?'/click':'/click2');});
b.addEventListener('dblclick',function(){ping('/dbl');});
b.addEventListener('contextmenu',function(e){e.preventDefault();ping('/ctx');});
b.addEventListener('mousedown',function(e){down=[e.clientX,e.clientY];});
b.addEventListener('mouseup',function(e){if(down&&Math.abs(e.clientX-down[0])>40)ping('/drag');down=null;});
i.addEventListener('input',function(){ping('/typed-'+encodeURIComponent(i.value));});
</script></body></html>`;

describe.skipIf(blocked !== null)("the live viewer in a real Electron window (real worker, real Chrome)", () => {
  let stack: Stack;
  let desktop: Desktop;
  let site: Server;
  let origin: string;
  const hits = new Map<string, number>();

  beforeAll(async () => {
    site = createServer((request, response) => {
      const path = request.url ?? "/";
      hits.set(path, (hits.get(path) ?? 0) + 1);
      response.writeHead(200, { "content-type": path === "/" ? "text/html" : "text/plain" });
      response.end(path === "/" ? PAGE : "ok");
    });
    await new Promise<void>((resolve) => site.listen(0, "127.0.0.1", resolve));
    const address = site.address();
    origin = `http://127.0.0.1:${typeof address === "object" && address ? String(address.port) : "0"}/`;
    stack = await Stack.start("liveworker", { browse: true });
    desktop = await launchDesktop(stack.origin);
    await signInDesktop(desktop.page, stack, emailOf(OWNER));
  }, 180_000);

  afterAll(async () => {
    await desktop.close();
    await stack.drop();
    site.close();
  }, 60_000);

  const hit = (path: string): number => hits.get(path) ?? 0;

  it("shows what the agent opened, and a person who takes it over can click, right-click, double-click, drag and type on it", async () => {
    const page = desktop.page;
    await openSurface(page, "Chat");
    await page.getByRole("textbox", { name: "Message your teammates…" }).fill(`look at this [[scenario:browse ${origin}]]`);
    await page.getByRole("button", { name: "Send" }).click();
    // An action derived from a page the agent has not proven trustworthy asks first; the person approves it from the preview.
    await openSurface(page, "Approvals");
    const trust = page.getByRole("region", { name: "Trust" });
    await waitFor(async () => (await trust.getByRole("heading", { name: "Approvals (1)" }).count()) > 0, "the request to open the page", 30_000);
    await trust.getByRole("button", { name: /^Approve browser\.navigate/ }).click();
    const dialog = trust.getByRole("dialog");
    await dialog.waitFor();
    await dialog.getByRole("button", { name: "Approve", exact: true }).click();
    // The agent's call ran on the worker's Chrome: the site really received the page request.
    await waitFor(() => hit("/") >= 1, "the agent's browser to request the page", 60_000);

    await openSurface(page, "Live browser and terminal");
    const live = page.getByRole("region", { name: "Live browser and terminal" });
    // The list names a session by the page it shows, or by its id until the control plane has learned the page.
    const entry = live.getByRole("button", { name: /^(bsn_|http:\/\/127\.0\.0\.1)/ });
    await waitFor(async () => (await entry.count()) > 0, "the agent's session in the list", 60_000);
    await entry.first().click();
    const picture = live.getByRole("img", { name: "The live browser session" });
    await picture.waitFor({ state: "visible", timeout: 60_000 });
    const source = (await picture.getAttribute("src")) ?? "";
    expect(source.startsWith("data:image/"), "the picture is a frame the worker streamed").toBe(true);

    // Watching is not driving: a click on the picture before taking over reaches nothing.
    await picture.click();
    await new Promise((resolve) => setTimeout(resolve, 600));
    expect(hit("/click")).toBe(0);

    await live.getByRole("button", { name: "Take over" }).first().click();
    await waitFor(async () => (await live.getByRole("button", { name: /hand back/i }).count()) > 0, "control to pass to the person", 30_000);

    await picture.click();
    await waitFor(() => hit("/click") >= 1, "the page to see the person's click", 30_000);
    await picture.click({ button: "right" });
    await waitFor(() => hit("/ctx") >= 1, "the page to see the right click", 30_000);
    await picture.dblclick();
    await waitFor(() => hit("/dbl") >= 1, "the page to see the double click", 30_000);
    const box = await picture.boundingBox();
    if (!box) {
      throw new Error("the picture has no box");
    }
    await page.mouse.move(box.x + box.width * 0.2, box.y + box.height * 0.3);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.7, box.y + box.height * 0.35, { steps: 6 });
    await page.mouse.up();
    await waitFor(() => hit("/drag") >= 1, "the page to see the drag", 30_000);

    // The page's field is reached by a click at its place on the picture, then typing on the picture's keyboard focus.
    await page.mouse.click(box.x + box.width * 0.1, box.y + box.height * 0.97);
    await picture.focus();
    await page.keyboard.type("hi");
    await waitFor(() => [...hits.keys()].some((path) => path.startsWith("/typed-hi")), "the page to see the typing", 30_000);

    await live.getByRole("button", { name: /hand back/i }).first().click();
    await waitFor(async () => (await live.getByText("Controlled by agent", { exact: false }).count()) > 0, "control to return to the agent", 30_000);
    const clicks = hit("/click");
    await picture.click();
    await new Promise((resolve) => setTimeout(resolve, 600));
    expect(hit("/click"), "after the hand back the picture takes no input").toBe(clicks);
  }, 240_000);
});
