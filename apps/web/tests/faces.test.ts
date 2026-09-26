/**
 * APP-012: people's marks — initials from a display name or email, and a tone that stays with
 * the identity.
 */

import { describe, expect, it } from "vitest";

import { FACE_TONES, faceTone, initials } from "../src/app/faces.js";

describe("faces", () => {
  it("takes the first and last word's initials, else the email's first letter", () => {
    expect(initials("Ada  Lovelace King", "ada@example.com")).toBe("AK");
    expect(initials("ada", null)).toBe("A");
    expect(initials(" ", "zed@example.com")).toBe("Z");
    expect(initials(undefined, undefined)).toBe("?");
    expect(initials("— —", null)).toBe("?");
  });

  it("keeps a tone per identity within the palette", () => {
    expect(faceTone("usr_1")).toBe(faceTone("usr_1"));
    const tones = ["usr_a", "usr_b", "usr_c", "usr_d", "usr_e", "usr_f", "usr_g", "usr_h"].map(faceTone);
    expect(tones.every((tone) => tone >= 0 && tone < FACE_TONES)).toBe(true);
    expect(new Set(tones).size).toBeGreaterThan(2);
  });
});
