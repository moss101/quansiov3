/**
 * Identity marks (APP-003): a teammate is a letter on its own tone, a conversation with several
 * teammates is up to four of those in one frame, and the person is their initials. Decorative:
 * the name beside a face is what a screen reader reads.
 */

import type { ReactElement } from "react";

import type { Face as FaceData } from "../conversation/presentation.js";

export type FaceSize = "xs" | "sm" | "md" | "lg";

export function Face({ face, size = "lg" }: { readonly face: FaceData; readonly size?: FaceSize }): ReactElement {
  return (
    <span className={`face face-${size} tone-${String(face.tone)}`} aria-hidden="true">
      {face.letter}
    </span>
  );
}

/**
 * A conversation's faces in one square frame: one fills it, two sit on a diagonal, three and
 * four share it, and teammates beyond the third become "+N". No faces is a blank frame.
 */
export function Faces({ faces, more, size = "lg" }: { readonly faces: readonly FaceData[]; readonly more: number; readonly size?: FaceSize }): ReactElement {
  const shown = faces.slice(0, more > 0 ? 3 : 4);
  const count = shown.length + (more > 0 ? 1 : 0);
  if (count <= 1) {
    return shown[0] ? <Face face={shown[0]} size={size} /> : <span className={`face face-${size} face-blank`} aria-hidden="true" />;
  }
  return (
    <span className={`faces faces-${size} faces-${String(count)}`} aria-hidden="true">
      {shown.map((face) => (
        <span key={face.key} className={`face face-in tone-${String(face.tone)}`}>
          {face.letter}
        </span>
      ))}
      {more > 0 ? <span className="face face-in face-more">+{String(more)}</span> : null}
    </span>
  );
}

/** The signed-in person: their initials on a neutral disc. */
export function Initials({ text, size = "lg" }: { readonly text: string; readonly size?: FaceSize }): ReactElement {
  return (
    <span className={`face face-${size} face-person`} aria-hidden="true">
      {text}
    </span>
  );
}
