/**
 * Where each desktop surface (DOSSIER §15, `../surfaces.ts`) lives in the shell (APP-003): the
 * page it opens, the name a person reads for it and its icon. Several surfaces live inside
 * another page (the agent roster and plan beside the objectives, run status with the chat, the
 * effect timeline under approvals, capability resolution with knowledge, the models and tools of
 * a turn in the chat inspector), so they open that page; connector setup is administered in the
 * web app and has no page here.
 */

import type { ReactElement } from "react";

import type { StringKey } from "../i18n/strings.js";
import type { SurfaceId } from "../surfaces.js";
import {
  BookIcon,
  BranchIcon,
  ChatIcon,
  ChipIcon,
  ClockIcon,
  FileIcon,
  GaugeIcon,
  GlobeIcon,
  KeyIcon,
  ListIcon,
  PeopleIcon,
  PlugIcon,
  ShieldIcon,
  TargetIcon,
  ThreadsIcon,
} from "./icons.js";

export type Page =
  | "conversation"
  | "artifacts"
  | "objectives"
  | "approvals"
  | "knowledge-controls"
  | "teammates"
  | "live-browser-terminal"
  | "collaboration"
  | "automations"
  | "settings";

/** Every surface by a person's name for it (not its internal id). */
export const SURFACE_LABELS: Record<SurfaceId, StringKey> = {
  "conversation": "surfaceConversation",
  "objectives": "surfaceObjectives",
  "teammates": "surfaceTeammates",
  "plan-and-agents": "surfacePlanAndAgents",
  "run-status": "surfaceRunStatus",
  "approvals": "surfaceApprovals",
  "evidence-timeline": "surfaceEvidenceTimeline",
  "live-browser-terminal": "surfaceLiveBrowserTerminal",
  "artifacts": "surfaceArtifacts",
  "knowledge-controls": "surfaceKnowledgeControls",
  "collaboration": "surfaceCollaboration",
  "automations": "surfaceAutomations",
  "connectors": "surfaceConnectors",
  "capability-admin": "surfaceCapabilityAdmin",
  "model-usage": "surfaceModelUsage",
};

/** The page each surface opens; a surface without one is named but not openable here. */
export const OPENS: Partial<Record<SurfaceId, Page>> = {
  "conversation": "conversation",
  "objectives": "objectives",
  "teammates": "teammates",
  "plan-and-agents": "objectives",
  "run-status": "conversation",
  "approvals": "approvals",
  "evidence-timeline": "approvals",
  "live-browser-terminal": "live-browser-terminal",
  "artifacts": "artifacts",
  "knowledge-controls": "knowledge-controls",
  "collaboration": "collaboration",
  "automations": "automations",
  "capability-admin": "knowledge-controls",
  "model-usage": "conversation",
};

/** The navigation entry that is a page's own (the others open it as their host). */
export const PAGE_ENTRY: Partial<Record<Page, SurfaceId>> = {
  "conversation": "conversation",
  "objectives": "objectives",
  "teammates": "teammates",
  "approvals": "approvals",
  "live-browser-terminal": "live-browser-terminal",
  "artifacts": "artifacts",
  "knowledge-controls": "knowledge-controls",
  "collaboration": "collaboration",
  "automations": "automations",
};

export const SURFACE_ICONS: Record<SurfaceId, () => ReactElement> = {
  "conversation": () => <ChatIcon />,
  "objectives": () => <TargetIcon />,
  "teammates": () => <PeopleIcon />,
  "plan-and-agents": () => <BranchIcon />,
  "run-status": () => <GaugeIcon />,
  "approvals": () => <ShieldIcon />,
  "evidence-timeline": () => <ListIcon />,
  "live-browser-terminal": () => <GlobeIcon />,
  "artifacts": () => <FileIcon />,
  "knowledge-controls": () => <BookIcon />,
  "collaboration": () => <ThreadsIcon />,
  "automations": () => <ClockIcon />,
  "connectors": () => <PlugIcon />,
  "capability-admin": () => <KeyIcon />,
  "model-usage": () => <ChipIcon />,
};
