/**
 * People's marks in the web app (APP-012): initials on a tone chosen from the identity, so a
 * person keeps their colour on every page. Decorative: the name beside a face is what a screen
 * reader reads.
 */

import type { ReactElement } from "react";

/** Tones for faces; the stylesheet names each `tone-N`. */
export const FACE_TONES = 8;

/** A stable tone for an identity. */
export function faceTone(id: string): number {
  let hash = 0;
  for (const char of id) {
    hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  }
  return hash % FACE_TONES;
}

function firstLetter(text: string): string {
  const match = /[\p{L}\p{N}]/u.exec(text);
  return match ? match[0].toUpperCase() : "";
}

/** Initials: first and last word of a display name, else the email's first letter. */
export function initials(displayName: string | null | undefined, email: string | null | undefined): string {
  const words = (displayName ?? "").trim().split(/\s+/).filter(Boolean);
  if (words.length > 0) {
    const first = firstLetter(words[0] ?? "");
    const last = words.length > 1 ? firstLetter(words[words.length - 1] ?? "") : "";
    return `${first}${last}` || "?";
  }
  return (email ? firstLetter(email) : "") || "?";
}

export function Face({ id, text, size = "md", neutral = false }: { readonly id: string; readonly text: string; readonly size?: "sm" | "md" | "lg"; readonly neutral?: boolean }): ReactElement {
  return (
    <span className={`face face-${size} ${neutral ? "face-person" : `tone-${String(faceTone(id))}`}`} aria-hidden="true">
      {text}
    </span>
  );
}
