/**
 * Collaboration roster (APP-009).
 */

export interface Participant {
  readonly id: string;
  readonly status: "active" | "removed" | "suspended";
}

export function receivesProtected(participant: Participant): boolean {
  return participant.status === "active";
}
