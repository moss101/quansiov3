/**
 * The preload script Electron loads (APP-003).
 *
 * Runs in the isolated preload world with `contextIsolation: true` and `sandbox: true`.
 * It exposes exactly `createPreloadApi` on `window.quansio`: each invoke goes through
 * `ipcRenderer.invoke` on an allowlisted channel, each subscription through
 * `ipcRenderer.on` on a push channel, and nothing else — no `require`, no Node globals,
 * no raw `ipcRenderer` — reaches the page.
 */

import { contextBridge, ipcRenderer } from "electron";

import { createPreloadApi, preloadAllows, pushAllows, type BridgeSend, type BridgeSubscribe } from "./bridge.js";

const send: BridgeSend = (channel, payload) => {
  if (!preloadAllows(channel)) {
    return Promise.reject(new Error(`channel not allowed: ${channel}`));
  }
  return ipcRenderer.invoke(channel, payload);
};

const subscribe: BridgeSubscribe = (channel, listener) => {
  if (!pushAllows(channel)) {
    throw new Error(`push channel not allowed: ${channel}`);
  }
  const wrapped = (_event: unknown, payload: unknown) => {
    listener(payload);
  };
  ipcRenderer.on(channel, wrapped);
  return () => {
    ipcRenderer.removeListener(channel, wrapped);
  };
};

contextBridge.exposeInMainWorld("quansio", createPreloadApi(send, subscribe));
