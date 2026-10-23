// @vitest-environment jsdom
/** EXEC-002 on a page: the WorkerPanel over the real preload API. */

import { afterEach, describe, expect, it, vi } from "vitest";

import { t } from "../../src/i18n/strings.js";
import { WorkerPanel } from "../../src/renderer/WorkerPanel.js";
import { click, mount, text, type Rig } from "./rig.js";

function status(extra: Record<string, unknown> = {}): Record<string, unknown> {
  return { available: true, reason: null, state: "stopped", root: null, attached: false, exitCode: null, lastError: null, ...extra };
}

let rig: Rig;
afterEach(async () => {
  await rig.unmount();
});

describe("the worker panel", () => {
  it("says why a worker cannot be started here, and offers no control", async () => {
    rig = await mount(<WorkerPanel />, { "worker.status": status({ available: false, reason: "no worker binary" }) });
    expect(rig.container.querySelector(".worker-unavailable")?.textContent).toBe("no worker binary");
    expect(rig.container.querySelector("button")).toBeNull();
    await rig.unmount();
    rig = await mount(<WorkerPanel />, { "worker.status": status({ available: false }) });
    expect(text(rig)).toContain(t("workerUnavailable"));
  });

  it("starts a stopped worker, refreshes, and tells the shell", async () => {
    const changed = vi.fn();
    rig = await mount(<WorkerPanel onChanged={changed} />, { "worker.status": status(), "worker.start": { ok: true } });
    expect(rig.container.querySelector(".worker-state-stopped")?.textContent).toBe(t("workerStopped"));
    rig.answer("worker.status", status({ state: "running", attached: true, root: "/work/folder" }));
    await click(rig, t("workerStart"));
    expect(rig.calls.some((call) => call.channel === "worker.start")).toBe(true);
    expect(changed).toHaveBeenCalledTimes(1);
    expect(rig.container.querySelector(".worker-state-running")?.textContent).toBe(t("workerRunning"));
    expect(rig.container.querySelector(".worker-root")?.textContent).toBe("/work/folder");
    expect(rig.container.querySelector(".worker-start")).toBeNull();
  });

  it("stops a running worker, and says when it is running but not attached", async () => {
    rig = await mount(<WorkerPanel />, { "worker.status": status({ state: "running", attached: false }), "worker.stop": { ok: true } });
    expect(rig.container.querySelector(".worker-state")?.textContent).toBe(t("workerNotAttached"));
    rig.answer("worker.status", status());
    await click(rig, t("workerStop"));
    expect(rig.calls.some((call) => call.channel === "worker.stop")).toBe(true);
    expect(rig.container.querySelector(".worker-state-stopped")).not.toBeNull();
  });

  it("shows a refusal as a notice and an exited worker's last error", async () => {
    rig = await mount(<WorkerPanel />, { "worker.status": status({ state: "exited", exitCode: 1, lastError: "boom: no display" }), "worker.start": { ok: false, error: { code: "WORKER_UNAVAILABLE", message: "pick a folder first" } } });
    expect(rig.container.querySelector(".worker-state")?.textContent).toBe(t("workerExited"));
    expect(rig.container.querySelector(".worker-error")?.textContent).toBe("boom: no display");
    await click(rig, t("workerStart"));
    expect(rig.container.querySelector(".notice")?.textContent).toBe(`${t("refused")} WORKER_UNAVAILABLE: pick a folder first`);
  });

  it("shows a starting worker with a stop control", async () => {
    rig = await mount(<WorkerPanel />, { "worker.status": status({ state: "starting" }) });
    expect(rig.container.querySelector(".worker-state")?.textContent).toBe(t("workerStarting"));
    expect(rig.container.querySelector(".worker-stop")).not.toBeNull();
  });
});
