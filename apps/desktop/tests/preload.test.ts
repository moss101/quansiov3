/**
 * Permission boundary test (APP-003): what the page can reach through `window.quansio` and
 * nothing more. The real preload script is loaded with Electron's `contextBridge` and
 * `ipcRenderer` replaced by recording fakes, so the exposed object is the shipped one.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

import { FORBIDDEN_RENDERER_APIS, PRELOAD_ALLOWLIST, PRELOAD_PUSH_CHANNELS, createPreloadApi, isForbiddenRendererApi, preloadAllows, pushAllows } from "../src/index.js";

const exposed: Record<string, unknown> = {};
const invoked: { channel: string; payload: unknown }[] = [];
const listeners = new Map<string, Set<(event: unknown, payload: unknown) => void>>();

vi.mock("electron", () => ({
  contextBridge: {
    exposeInMainWorld: (name: string, api: unknown) => {
      exposed[name] = api;
    },
  },
  ipcRenderer: {
    invoke: (channel: string, payload: unknown) => {
      invoked.push({ channel, payload });
      return Promise.resolve({ ok: true, channel });
    },
    on: (channel: string, listener: (event: unknown, payload: unknown) => void) => {
      if (!listeners.has(channel)) {
        listeners.set(channel, new Set());
      }
      listeners.get(channel)?.add(listener);
    },
    removeListener: (channel: string, listener: (event: unknown, payload: unknown) => void) => {
      listeners.get(channel)?.delete(listener);
    },
  },
}));

describe("preload bridge", () => {
  beforeEach(() => {
    invoked.length = 0;
    listeners.clear();
  });

  it("exposes exactly the allowlisted invoke and push surface on window.quansio", async () => {
    await import("../src/preload/index.js");
    const api = exposed["quansio"] as Record<string, (...args: unknown[]) => unknown>;
    expect(api).toBeDefined();
    expect(Object.keys(api).sort()).toEqual(
      [
        "activeRun",
        "sessionState",
        "me",
        "startMagicLink",
        "completeMagicLink",
        "startOAuth",
        "signOut",
        "updateWorkspace",
        "updateUserSettings",
        "invokeCommand",
        "openDeepLink",
        "reconnectStream",
        "subscribeStream",
        "updateStatus",
        "listThreads",
        "threadMessages",
        "listQuestions",
        "runTimeline",
        "postMessage",
        "answerQuestion",
        "cancelTurn",
        "uploadAttachment",
        "listArtifacts",
        "artifactVersions",
        "artifactContent",
        "saveArtifact",
        "setAppearance",
        "openArtifact",
        "getEvidence",
        "search",
        "workGraph",
        "proposeGraphEdit",
        "createObjective",
        "listApprovals",
        "approveEffect",
        "denyEffect",
        "listEffects",
        "evidenceContent",
        "listKnowledge",
        "deleteKnowledge",
        "supersedeKnowledge",
        "listMemory",
        "deleteMemory",
        "listSkills",
        "installSkill",
        "promoteSkillVersion",
        "disableSkill",
        "runCapabilities",
        "listTeammates",
        "createTeammate",
        "updateTeammate",
        "archiveTeammate",
        "listTargets",
        "liveSession",
        "requestTakeover",
        "handback",
        "attachLive",
        "computerPermissions",
        "openComputerSettings",
        "recordingStatus",
        "startRecording",
        "finishRecording",
        "cancelRecording",
        "listRecordings",
        "compileRecording",
        "submitRecordingSkill",
        "detachLive",
        "viewThread",
        "createThread",
        "addParticipant",
        "removeParticipant",
        "react",
        "handOffThread",
        "listRoutines",
        "routineCommand",
        "listNotifications",
        "markNotificationRead",
        "setNotificationPreference",
        "exportDiagnostics",
        "onStreamFrame",
        "onStreamState",
        "onStreamError",
        "onSessionState",
        "onDeepLink",
        "onUpdateStatus",
        "onLiveFrame",
        // The desktop shell (ADP-004)
        "windowContext",
        "consumePending",
        "reportConnectivity",
        "shortcuts",
        "setShortcut",
        "resetShortcut",
        "beginShortcutRecording",
        "endShortcutRecording",
        "shellPreferences",
        "setShellPreferences",
        "openPopout",
        "pinPopout",
        "closeWindow",
        "quickEntrySubmit",
        "activityItems",
        "dismissActivity",
        "openActivity",
        "setIndicatorInteractive",
        "moveIndicator",
        "installUpdate",
        "onShellCommand",
        "onActivity",
        "onConnectivity",
        "onShortcuts",
        "onShortcutRecordingEnded",
        // Local files (ADP-005)
        "pickLocalFiles",
        "attachLocalFiles",
        "discardLocalFiles",
        "previewLocalFile",
        "pickFolder",
        "listFolders",
        "releaseFolder",
        "folderEntries",
        "fileMediaUrl",
        "openFileForEdit",
        "saveEditedFile",
        "closeFileEdit",
        "watchFolder",
        "unwatchFolder",
        "onFileChanges",
        "onFileGrant",
      ].sort(),
    );
    for (const key of Object.keys(api)) {
      expect(typeof api[key]).toBe("function");
    }
    // No raw ipcRenderer, no require, no Node globals are exposed.
    expect("ipcRenderer" in api).toBe(false);
    expect("require" in api).toBe(false);
    for (const forbidden of FORBIDDEN_RENDERER_APIS) {
      expect(forbidden in api).toBe(false);
      expect(isForbiddenRendererApi(forbidden)).toBe(true);
    }
  });

  it("routes every invoke to an allowlisted channel with the caller's payload", async () => {
    await import("../src/preload/index.js");
    const api = exposed["quansio"] as ReturnType<typeof createPreloadApi>;
    const command = { name: "PostMessage", commandId: "cmd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", params: { content: "hi" } };
    await api.invokeCommand(command);
    await api.subscribeStream();
    await api.reconnectStream("cursor-1");
    await api.activeRun();
    await api.sessionState();
    await api.me();
    await api.startMagicLink("ada@example.com", "Ada");
    await api.completeMagicLink("qml_token");
    await api.startOAuth("github");
    await api.signOut();
    await api.updateWorkspace("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", { settings: { execution_target: "cloud" } });
    await api.updateUserSettings("ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", { theme: "dark" });
    await api.openDeepLink("quansio://thread/thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.updateStatus();
    await api.listThreads();
    await api.threadMessages("thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.listQuestions({ threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.runTimeline("run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.postMessage({ threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", content: "hi", attachments: ["art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA"] });
    await api.answerQuestion("q_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "alpha");
    await api.cancelTurn("run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.uploadAttachment({ name: "a.bin", mediaType: "application/octet-stream", bytesBase64: "AAEC" });
    await api.listArtifacts({ kind: "document" });
    await api.artifactVersions("art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.artifactContent("art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.saveArtifact("art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.openArtifact("art_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "artv_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.getEvidence("evd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.search({ q: "quokka", channels: ["artifacts"] });
    await api.workGraph();
    await api.proposeGraphEdit({ base_revision: 1, nodes_add: [] });
    await api.createObjective({ title: "ship" });
    await api.listApprovals({ runId: "run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.approveEffect("apr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "a".repeat(64));
    await api.denyEffect("apr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.listEffects({});
    await api.evidenceContent("evd_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.listKnowledge({ includeDeleted: true });
    await api.deleteKnowledge("kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.supersedeKnowledge("kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "kn_01J8Z3K6F1N8VQ2X5W9Y0AAAAB");
    await api.listMemory({ scope: "user" });
    await api.deleteMemory("mem_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.listSkills();
    await api.installSkill({ scope: "workspace", name: "notes", version: { semver: "1.0.0", manifest: {} } });
    await api.promoteSkillVersion("sklv_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", "candidate");
    await api.disableSkill("skl_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.runCapabilities("run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.listTeammates();
    await api.createTeammate({ workspace_id: "ws_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", template_id: "engineer" });
    await api.updateTeammate({ teammate_id: "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", name: "Atlas" });
    await api.archiveTeammate("agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.listTargets();
    await api.liveSession("bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.requestTakeover({ sessionId: "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.handback({ sessionId: "bsn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.attachLive("tgt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.computerPermissions({ needs: ["accessibility"] });
    await api.openComputerSettings("accessibility");
    await api.recordingStatus();
    await api.startRecording();
    await api.finishRecording({ gesture: true });
    await api.cancelRecording();
    await api.listRecordings();
    await api.compileRecording("rec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.submitRecordingSkill({ recordingId: "rec_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", name: "make-a-note", semver: "1.0.0", slots: [] });
    await api.detachLive();
    await api.viewThread("thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.createThread({ kind: "group", title: "launch", participants: [] });
    await api.addParticipant({ thread_id: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", kind: "user", id: "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.removeParticipant({ thread_id: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", kind: "user", id: "usr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.react({ message_id: "msg_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", emoji: "👍", remove: false });
    await api.handOffThread({ thread_id: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA", to_teammate_id: "agt_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.listRoutines();
    await api.routineCommand("PauseRoutine", { routine_id: "rtn_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.listNotifications(true);
    await api.markNotificationRead("ntf_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.setNotificationPreference({ kind: "mention", channels: ["in_app"] });
    await api.exportDiagnostics();
    await api.setAppearance("dark");
    await api.windowContext();
    await api.consumePending();
    await api.reportConnectivity(true);
    await api.shortcuts();
    await api.setShortcut("find", "mod+k");
    await api.resetShortcut("find");
    await api.beginShortcutRecording();
    await api.endShortcutRecording();
    await api.shellPreferences();
    await api.setShellPreferences({ keepAwake: true });
    await api.openPopout({ threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.pinPopout(true);
    await api.closeWindow();
    await api.quickEntrySubmit({ content: "hello" });
    await api.activityItems();
    await api.dismissActivity("run:run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.openActivity("run:run_01J8Z3K6F1N8VQ2X5W9Y0AAAAA");
    await api.setIndicatorInteractive(true);
    await api.moveIndicator(10, 20);
    await api.installUpdate("now");
    // Local files (ADP-005): every call carries an opaque id or nothing, never a path.
    const token = "0".repeat(64);
    await api.pickLocalFiles({ threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.attachLocalFiles({ token, threadId: "thr_01J8Z3K6F1N8VQ2X5W9Y0AAAAA" });
    await api.discardLocalFiles(token);
    await api.previewLocalFile(token, 0);
    await api.pickFolder();
    await api.listFolders();
    await api.releaseFolder("lroot_00000000000000000000000000000000");
    await api.folderEntries("lroot_00000000000000000000000000000000", null);
    await api.fileMediaUrl("ent_00000000000000000000000000000000");
    await api.openFileForEdit("ent_00000000000000000000000000000000");
    await api.saveEditedFile({ editId: "edit_00000000000000000000000000000000", text: "x" });
    await api.closeFileEdit("edit_00000000000000000000000000000000");
    await api.watchFolder("lroot_00000000000000000000000000000000");
    await api.unwatchFolder("sub_0000000000000000");
    expect(invoked.map((call) => call.channel)).toEqual([...PRELOAD_ALLOWLIST]);
    expect(invoked[0]?.payload).toEqual(command);
    for (const call of invoked) {
      expect(preloadAllows(call.channel)).toBe(true);
    }
  });

  it("subscribes only on push channels and unsubscribes cleanly", async () => {
    await import("../src/preload/index.js");
    const api = exposed["quansio"] as ReturnType<typeof createPreloadApi>;
    const frames: unknown[] = [];
    const off = api.onStreamFrame((frame) => frames.push(frame));
    expect([...listeners.keys()]).toEqual(["stream.frame"]);
    for (const listener of listeners.get("stream.frame") ?? []) {
      listener({}, { kind: "live", channel: "run:run_x", frame: { kind: "model.delta", text: "…" } });
    }
    expect(frames).toHaveLength(1);
    off();
    expect(listeners.get("stream.frame")?.size ?? 0).toBe(0);
    for (const channel of PRELOAD_PUSH_CHANNELS) {
      expect(pushAllows(channel)).toBe(true);
    }
    expect(pushAllows("fs.read")).toBe(false);
  });

  it("refuses a channel outside the allowlist even if a caller reaches the send wrapper", () => {
    const calls: string[] = [];
    const api = createPreloadApi(
      (channel, payload) => {
        if (!preloadAllows(channel)) {
          throw new Error("never");
        }
        calls.push(channel);
        return Promise.resolve(payload);
      },
      () => () => undefined,
    );
    void api.activeRun();
    expect(calls).toEqual(["session.activeRun"]);
    expect(preloadAllows("fs.read")).toBe(false);
    expect(preloadAllows("child_process.exec")).toBe(false);
    expect(preloadAllows("commands.invoke")).toBe(true);
  });
});
