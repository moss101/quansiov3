/**
 * One-time file grants (ADP-005, DOMAIN.md 13.7 A): claim, expiry and replay, over real files.
 *
 * What is under test is the shipped `GrantRegistry`: real temporary files, real descriptors (the tests count the open
 * ones), real symbolic links, a real FIFO. The only thing moved by hand is the clock the time to live reads.
 */

import { execFileSync } from "node:child_process";
import { renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { GrantRegistry, grantKey, type GrantLimits } from "../src/main/local-files/grants.js";

import { CONFIG, link, put, schedulerOf, scratch } from "./local-files-fixtures.js";
import { Clock } from "./shell-fakes.js";

const LIMITS: GrantLimits = {
  ttlMs: CONFIG.grantTtlMs,
  maxLive: CONFIG.grantMaxLive,
  maxFiles: CONFIG.grantMaxFiles,
  maxFileBytes: CONFIG.grantMaxFileBytes,
  maxTotalBytes: CONFIG.grantMaxTotalBytes,
};

const A = 11;
const B = 12;

function rig(limits: Partial<GrantLimits> = {}) {
  const clock = new Clock();
  const revoked: string[] = [];
  const registry = new GrantRegistry({
    limits: { ...LIMITS, ...limits },
    now: () => clock.now,
    scheduler: schedulerOf(clock),
    onRevoked: (key) => revoked.push(key),
  });
  return { clock, registry, revoked, dir: scratch() };
}

function created(result: ReturnType<GrantRegistry["create"]>) {
  if (!result.ok) {
    throw new Error(`refused: ${JSON.stringify(result.failure)}`);
  }
  return result.grant;
}

describe("claim", () => {
  it("gives the picked files to the window that holds the grant, once, and hands the page no path", () => {
    const { registry, dir } = rig();
    const a = put(dir, "notes/a.txt", "alpha");
    const b = put(dir, "b.png", Buffer.from([1, 2, 3, 4]));
    const grant = created(registry.create(A, [a, b]));

    // The page is told a token, names, sizes and types, and nothing that locates a file.
    expect(grant.token).toMatch(/^[0-9a-f]{64}$/);
    expect(grant.files).toEqual([
      { index: 0, name: "a.txt", sizeBytes: 5, mediaType: "text/plain" },
      { index: 1, name: "b.png", sizeBytes: 4, mediaType: "image/png" },
    ]);
    expect(grant.totalBytes).toBe(9);
    expect(JSON.stringify(grant)).not.toContain(dir);
    expect(JSON.stringify(grant)).not.toContain("notes");

    const claim = registry.claim(A, grant.token);
    expect(claim.ok).toBe(true);
    if (!claim.ok) {
      return;
    }
    expect(claim.files.map((file) => file.name)).toEqual(["a.txt", "b.png"]);
    expect(claim.files[0]?.read().toString()).toBe("alpha");
    expect([...(claim.files[1]?.read() ?? [])]).toEqual([1, 2, 3, 4]);
    for (const file of claim.files) {
      file.close();
    }
    expect(registry.openDescriptors()).toBe(0);
  });

  it("cannot be claimed twice: the first claim spends the grant, whatever the claimant does next", () => {
    const { registry, dir, revoked } = rig();
    const grant = created(registry.create(A, [put(dir, "a.txt", "one")]));
    const first = registry.claim(A, grant.token);
    expect(first.ok).toBe(true);
    // The claimant never closes or reads its files: the grant is spent all the same.
    expect(registry.claim(A, grant.token)).toEqual({ ok: false, reason: "invalid" });
    expect(registry.claim(A, grant.token)).toEqual({ ok: false, reason: "invalid" });
    expect(registry.liveCount()).toBe(0);
    expect(revoked).toEqual([grantKey(grant.token)]);
    if (first.ok) {
      first.files.forEach((file) => { file.close(); });
    }
  });

  it("cannot be claimed by another window, and a failed attempt does not spend it", () => {
    const { registry, dir } = rig();
    const grant = created(registry.create(A, [put(dir, "a.txt", "one")]));
    expect(registry.claim(B, grant.token)).toEqual({ ok: false, reason: "invalid" });
    expect(registry.discard(B, grant.token)).toBe(false);
    expect(registry.lookup(B, grant.token)).toBeNull();
    // Still the owner's, still whole.
    expect(registry.liveCount()).toBe(1);
    const mine = registry.claim(A, grant.token);
    expect(mine.ok).toBe(true);
    if (mine.ok) {
      mine.files.forEach((file) => { file.close(); });
    }
  });

  it("answers an unknown, malformed or guessed token as it answers another window's, so tokens cannot be probed", () => {
    const { registry, dir } = rig();
    const grant = created(registry.create(A, [put(dir, "a.txt", "one")]));
    for (const token of ["", "short", "g".repeat(64), grant.token.toUpperCase(), `${grant.token}0`, "0".repeat(64), "../../etc/passwd", "x".repeat(10_000)]) {
      expect(registry.claim(A, token), token.slice(0, 20)).toEqual({ ok: false, reason: "invalid" });
      expect(registry.claim(B, token), token.slice(0, 20)).toEqual({ ok: false, reason: "invalid" });
    }
    expect(registry.liveCount()).toBe(1);
    expect(registry.claim(A, 42 as unknown as string)).toEqual({ ok: false, reason: "invalid" });
  });

  it("two grants carry different random tokens, and a grant is known to the rest of the shell by a hash of its token", () => {
    const { registry, dir } = rig();
    const one = created(registry.create(A, [put(dir, "a.txt", "one")]));
    const two = created(registry.create(A, [put(dir, "b.txt", "two")]));
    expect(one.token).not.toBe(two.token);
    expect(grantKey(one.token)).toMatch(/^[0-9a-f]{64}$/);
    expect(grantKey(one.token)).not.toBe(one.token);
  });
});

describe("expiry", () => {
  it("a grant is valid for its time to live, then is expired for its owner and invalid for everyone else", () => {
    const { registry, dir, clock } = rig();
    const grant = created(registry.create(A, [put(dir, "a.txt", "one")]));
    expect(grant.expiresAt).toBe(clock.now + 60_000);
    clock.advance(59_999);
    expect(registry.lookup(A, grant.token)).not.toBeNull();
    clock.advance(1);
    // The timer fired at the deadline and closed the grant's descriptors with nobody asking.
    expect(registry.openDescriptors()).toBe(0);
    expect(registry.liveCount()).toBe(0);
    expect(registry.claim(A, grant.token)).toEqual({ ok: false, reason: "invalid" });
  });

  it("an expired grant is reported as expired to the window that held it when the timer has not run yet", () => {
    // The check at claim time is the clock's own, not the timer's: a late timer cannot extend a grant.
    let now = 1_000;
    const registry = new GrantRegistry({ limits: LIMITS, now: () => now, scheduler: { set: () => 0, clear: () => undefined } });
    const dir = scratch();
    const grant = created(registry.create(A, [put(dir, "a.txt", "one")]));
    now += 60_000;
    expect(registry.claim(B, grant.token)).toEqual({ ok: false, reason: "invalid" });
    expect(registry.claim(A, grant.token)).toEqual({ ok: false, reason: "expired" });
    // After that it is gone like any spent grant.
    expect(registry.claim(A, grant.token)).toEqual({ ok: false, reason: "invalid" });
    expect(registry.openDescriptors()).toBe(0);
  });

  it("a grant claimed in time is not undone by its timer", () => {
    const { registry, dir, clock } = rig();
    const grant = created(registry.create(A, [put(dir, "a.txt", "content")]));
    clock.advance(30_000);
    const claim = registry.claim(A, grant.token);
    expect(claim.ok).toBe(true);
    clock.advance(120_000);
    if (claim.ok) {
      expect(claim.files[0]?.read().toString()).toBe("content");
      claim.files.forEach((file) => { file.close(); });
    }
    expect(registry.openDescriptors()).toBe(0);
  });
});

describe("limits", () => {
  it("refuses a grant over the file count, the per-file size, the total or the number of live grants, and holds nothing open", () => {
    const { registry, dir } = rig({ maxFiles: 3, maxFileBytes: 100, maxTotalBytes: 150, maxLive: 2 });
    const small = (name: string, size = 10) => put(dir, name, "x".repeat(size));
    expect(registry.create(A, [small("1"), small("2"), small("3"), small("4")])).toEqual({ ok: false, failure: { reason: "too_many", name: "" } });
    expect(registry.create(A, [small("big", 101)])).toEqual({ ok: false, failure: { reason: "too_large", name: "big" } });
    // 100 + 100 is within each file's bound and over the total.
    expect(registry.create(A, [small("p", 100), small("q", 100)])).toEqual({ ok: false, failure: { reason: "total_too_large", name: "q" } });
    expect(registry.openDescriptors()).toBe(0);
    // Exactly at the bounds is accepted.
    created(registry.create(A, [small("a", 100), small("b", 50)]));
    created(registry.create(B, [small("c")]));
    // A third live grant is refused, from either window, until one ends.
    expect(registry.create(A, [small("d")])).toEqual({ ok: false, failure: { reason: "limit", name: "" } });
    expect(registry.liveCount()).toBe(2);
    expect(registry.openDescriptors()).toBe(3);
  });

  it("the shipped limits are the ones the contract names", () => {
    expect(LIMITS).toEqual({ ttlMs: 60_000, maxLive: 32, maxFiles: 20, maxFileBytes: 52_428_800, maxTotalBytes: 104_857_600 });
  });

  it("accepts a file at the per-file limit and refuses one a byte over", () => {
    const { registry, dir } = rig({ maxFileBytes: 64 });
    created(registry.create(A, [put(dir, "exact", "y".repeat(64))]));
    expect(registry.create(A, [put(dir, "over", "y".repeat(65))])).toMatchObject({ ok: false, failure: { reason: "too_large" } });
  });
});

describe("what a grant will not hold", () => {
  it("refuses a directory, a symbolic link, a missing file and a FIFO, naming the file and nothing else", () => {
    const { registry, dir } = rig();
    const real = put(dir, "real.txt", "real");
    const folder = join(dir, "folder");
    put(dir, "folder/inside.txt", "inside");
    const linked = link(real, join(dir, "link.txt"));
    const fifo = join(dir, "pipe");
    execFileSync("mkfifo", [fifo]);
    for (const [path, reason, name] of [
      [folder, "not_regular", "folder"],
      [linked, "symlink", "link.txt"],
      [join(dir, "missing.txt"), "unreadable", "missing.txt"],
      [fifo, "not_regular", "pipe"],
    ] as const) {
      expect(registry.create(A, [real, path]), name).toEqual({ ok: false, failure: { reason, name } });
    }
    // A refused grant leaves nothing open and nothing live.
    expect(registry.openDescriptors()).toBe(0);
    expect(registry.liveCount()).toBe(0);
  });

  it("does not follow a link at the end of the picked path, even to a file inside the same folder", () => {
    const { registry, dir } = rig();
    const target = put(dir, "secret.txt", "secret");
    const alias = link(target, join(dir, "alias.txt"));
    expect(registry.create(A, [alias])).toEqual({ ok: false, failure: { reason: "symlink", name: "alias.txt" } });
  });

  it("reads what was opened, even if the path is swapped for another file afterwards", () => {
    const { registry, dir } = rig();
    const path = put(dir, "doc.txt", "what the person picked");
    const grant = created(registry.create(A, [path]));
    // After the pick, the path is replaced: by another file, then by a link to something else.
    const other = put(dir, "other.txt", "something else entirely");
    renameSync(other, path);
    const claim = registry.claim(A, grant.token);
    expect(claim.ok).toBe(true);
    if (claim.ok) {
      expect(claim.files[0]?.read().toString()).toBe("what the person picked");
      claim.files.forEach((file) => { file.close(); });
    }
  });

  it("a file that grew after the pick is read as far as it was when picked, not beyond", () => {
    const { registry, dir } = rig();
    const path = put(dir, "log.txt", "12345");
    const grant = created(registry.create(A, [path]));
    writeFileSync(path, "123456789012345");
    const claim = registry.claim(A, grant.token);
    expect(claim.ok && claim.files[0]?.read().toString()).toBe("12345");
    if (claim.ok) {
      claim.files.forEach((file) => { file.close(); });
    }
  });
});

describe("revocation", () => {
  it("a window that closes loses its grants, and the files it was handed, with their descriptors", () => {
    const { registry, dir, revoked } = rig();
    const live = created(registry.create(A, [put(dir, "a.txt", "a")]));
    const claimed = created(registry.create(A, [put(dir, "b.txt", "b")]));
    const claim = registry.claim(A, claimed.token);
    const others = created(registry.create(B, [put(dir, "c.txt", "c")]));
    expect(registry.openDescriptors()).toBe(3);
    expect(registry.revokeWindow(A)).toBe(1);
    expect(registry.openDescriptors()).toBe(1);
    expect(registry.claim(A, live.token)).toEqual({ ok: false, reason: "invalid" });
    if (claim.ok) {
      expect(() => claim.files[0]?.read(), "a claimed file was closed with its window").toThrow(/closed/);
    }
    // Another window's grant is untouched.
    expect(registry.lookup(B, others.token)).not.toBeNull();
    expect(revoked).toContain(grantKey(live.token));
    expect(revoked).not.toContain(grantKey(others.token));
  });

  it("a discarded grant cannot be claimed, and only by its window", () => {
    const { registry, dir } = rig();
    const grant = created(registry.create(A, [put(dir, "a.txt", "a")]));
    expect(registry.discard(B, grant.token)).toBe(false);
    expect(registry.discard(A, grant.token)).toBe(true);
    expect(registry.claim(A, grant.token)).toEqual({ ok: false, reason: "invalid" });
    expect(registry.openDescriptors()).toBe(0);
  });
});
