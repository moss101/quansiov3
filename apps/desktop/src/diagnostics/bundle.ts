/**
 * Desktop diagnostic bundle (OPS-003).
 *
 * Export is bounded and redacted. Secret canaries never leave the renderer.
 */

export const DEFAULT_SECRET_CANARY = "qncy_test_canary_not_for_prod";
export const CANARY_PLACEHOLDER = "[REDACTED:canary]";
export const DEFAULT_BUNDLE_LIMIT = 64 * 1024;

const TOKEN_PATTERNS: readonly [RegExp, string][] = [
  [/Bearer\s+\S+/g, "[REDACTED:bearer]"],
  [/\bsk-[A-Za-z0-9_-]+/g, "[REDACTED:secret]"],
  [/\bghp_[A-Za-z0-9]+/g, "[REDACTED:secret]"],
  [/\bAKIA[0-9A-Z]{16}/g, "[REDACTED:secret]"],
  [/\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b/g, "[REDACTED:email]"],
];

export function redactText(input: string, canary = DEFAULT_SECRET_CANARY): string {
  let out = input.split(canary).join(CANARY_PLACEHOLDER);
  for (const [pattern, placeholder] of TOKEN_PATTERNS) {
    out = out.replace(pattern, placeholder);
  }
  return out;
}

export interface DiagnosticFrame {
  readonly correlationId: string;
  readonly service: "runtime" | "intelligence" | "desktop";
  readonly msg: string;
}

export interface DiagnosticBundle {
  readonly correlationId: string;
  readonly frames: readonly DiagnosticFrame[];
  readonly truncated: boolean;
}

export function exportBundle(
  correlationId: string,
  frames: readonly DiagnosticFrame[],
  maxBytes = DEFAULT_BUNDLE_LIMIT,
): DiagnosticBundle {
  const redacted = frames.map((frame) => ({
    ...frame,
    msg: redactText(frame.msg),
  }));
  let kept = redacted;
  let truncated = false;
  const encode = (items: readonly DiagnosticFrame[]) =>
    JSON.stringify({ correlationId, frames: items, truncated });
  while (encode(kept).length > maxBytes && kept.length > 0) {
    truncated = true;
    kept = kept.slice(0, -1);
  }
  return { correlationId, frames: kept, truncated };
}
