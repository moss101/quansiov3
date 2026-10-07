/**
 * The tray and what closing the main window does (ADP-004, DOMAIN.md §13.6).
 *
 * Windows and Linux get a tray with two entries, Open and Quit; macOS has the Dock and the menu bar instead and no
 * tray. Closing the main window hides it to the tray when there is one. Where there is no tray (a Linux desktop
 * without a status area, or macOS), a window hidden from every surface could not be brought back, so closing
 * *minimizes* it on Windows and Linux; on macOS closing hides it as macOS apps do and the Dock icon brings it back.
 * While the app is quitting a close is a close.
 */

import { deflateSync } from "node:zlib";

import { t } from "../i18n/strings.js";

/** Whether the platform gets a tray at all. */
export function wantsTray(platform: string): boolean {
  return platform === "win32" || platform === "linux";
}

export type CloseBehaviour = "close" | "hide" | "minimize";

export function closeBehaviour(input: { readonly platform: string; readonly trayAvailable: boolean; readonly quitting: boolean }): CloseBehaviour {
  if (input.quitting) {
    return "close";
  }
  if (input.platform === "darwin") {
    return "hide";
  }
  return input.trayAvailable ? "hide" : "minimize";
}

export type TrayAction = "open" | "quit";

export interface TrayEntry {
  readonly label: string;
  readonly action: TrayAction;
}

/** The tray's menu: Open, Quit. */
export function trayMenu(): readonly TrayEntry[] {
  return [
    { label: t("trayOpen"), action: "open" },
    { label: t("trayQuit"), action: "quit" },
  ];
}

// ---- the tray icon, drawn here so no image asset has to ship ----

const CRC_TABLE = (() => {
  const table: number[] = [];
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table.push(c >>> 0);
  }
  return table;
})();

function crc32(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc = (CRC_TABLE[(crc ^ byte) & 0xff] ?? 0) ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type: string, data: Uint8Array): Buffer {
  const body = Buffer.concat([Buffer.from(type, "ascii"), Buffer.from(data)]);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

/**
 * A `size`×`size` PNG of a filled disc with a lighter ring, in the product's accent colour: the tray icon. Built
 * from first principles (a PNG is a signature, a header, deflated scanlines and an end marker), so the shell
 * carries no binary asset and copies no one's artwork.
 */
export function trayIconPng(size = 32): Buffer {
  const rows: Buffer[] = [];
  const centre = (size - 1) / 2;
  const radius = size / 2 - 1;
  for (let y = 0; y < size; y += 1) {
    const row = Buffer.alloc(1 + size * 4);
    for (let x = 0; x < size; x += 1) {
      const distance = Math.hypot(x - centre, y - centre);
      const offset = 1 + x * 4;
      if (distance <= radius) {
        const ring = distance > radius - 2.5;
        row[offset] = ring ? 0xc7 : 0x4f;
        row[offset + 1] = ring ? 0xd2 : 0x46;
        row[offset + 2] = ring ? 0xfe : 0xe5;
        row[offset + 3] = 0xff;
      }
    }
    rows.push(row);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(size, 0);
  header.writeUInt32BE(size, 4);
  header[8] = 8; // bit depth
  header[9] = 6; // RGBA
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.concat(rows))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
