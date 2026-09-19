/**
 * Typed local-runtime client (APP-003, DOSSIER.md §4.1, §11).
 *
 * The desktop hosts the local capsule substrate and the native broker, and they register
 * with the server as execution targets exactly like cloud targets: there is no desktop-only
 * runtime and no privileged local API. This client is therefore the same public API v1,
 * narrowed to the execution commands of DOMAIN.md §14 (`ProvisionTarget`, `StopTarget`,
 * `ResetTarget`, `CreateCheckpoint`, `RestoreCheckpoint`) and the `/v1/targets` projection,
 * with the substrates the desktop can offer on this host.
 */

import type { CommandResponse, ServerClient } from "./server.js";

/** Substrates a desktop host may offer (DOMAIN.md §8.1). */
export type LocalSubstrate = "macos_local_capsule" | "windows_local_capsule" | "windows_native";

export type TargetClass = "persistent_workspace_computer" | "isolated_task_runtime";

export interface ProvisionTargetParams {
  readonly workspace_id: string;
  readonly target_class: TargetClass;
  readonly substrate: LocalSubstrate;
  /** Digest-verified guest image the desktop update channel delivered (EXEC-003). */
  readonly guest_image_digest?: string;
}

export interface TargetProjection {
  readonly id: string;
  readonly workspace_id: string;
  readonly substrate: string;
  readonly status: string;
  readonly generation: number;
  readonly [key: string]: unknown;
}

/** Execution commands the client may issue, exactly as the §14 catalog names them. */
export const EXECUTION_COMMANDS = [
  "ProvisionTarget",
  "StopTarget",
  "ResetTarget",
  "CreateCheckpoint",
  "RestoreCheckpoint",
] as const;

export type ExecutionCommand = (typeof EXECUTION_COMMANDS)[number];

/** The substrates this host can offer, decided from the platform alone (no probing here). */
export function offeredSubstrates(platform: string, arch: string): readonly LocalSubstrate[] {
  if (platform === "darwin") {
    // Apple Silicon is the primary capsule host; Intel is best-effort (DOSSIER.md §21.1).
    return arch === "arm64" || arch === "x64" ? ["macos_local_capsule"] : [];
  }
  if (platform === "win32") {
    return ["windows_local_capsule", "windows_native"];
  }
  return [];
}

export class LocalRuntimeClient {
  readonly #server: ServerClient;
  readonly #platform: string;
  readonly #arch: string;

  constructor(server: ServerClient, platform: string = process.platform, arch: string = process.arch) {
    this.#server = server;
    this.#platform = platform;
    this.#arch = arch;
  }

  /** Substrates the desktop will register for this host. */
  substrates(): readonly LocalSubstrate[] {
    return offeredSubstrates(this.#platform, this.#arch);
  }

  /** Register a local execution target through the public command, like any other target. */
  provision(commandId: string, params: ProvisionTargetParams): Promise<CommandResponse<TargetProjection>> {
    if (!this.substrates().includes(params.substrate)) {
      return Promise.reject(new Error(`substrate ${params.substrate} is not offered on ${this.#platform}/${this.#arch}`));
    }
    return this.#server.invoke<TargetProjection>("ProvisionTarget", commandId, { ...params });
  }

  stop(commandId: string, targetId: string): Promise<CommandResponse<TargetProjection>> {
    return this.#server.invoke<TargetProjection>("StopTarget", commandId, { target_id: targetId });
  }

  reset(commandId: string, targetId: string): Promise<CommandResponse<TargetProjection>> {
    return this.#server.invoke<TargetProjection>("ResetTarget", commandId, { target_id: targetId });
  }

  checkpoint(commandId: string, targetId: string, kind: "workspace_files" | "browser_session" | "terminal" | "full") {
    return this.#server.invoke("CreateCheckpoint", commandId, { target_id: targetId, kind });
  }

  restore(commandId: string, targetId: string, checkpointId: string) {
    return this.#server.invoke("RestoreCheckpoint", commandId, { target_id: targetId, checkpoint_id: checkpointId });
  }
}
