// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The page's own DeviceGroup handle as a superseded device reads it inside a
 * session (ADR-0074 section 3.3), and what each answer means. Outside a
 * session the device opens its own persisted DeviceGroup instead
 * (`standalone-journal-read.ts`, tested in `superseded-adoption.test.ts`).
 */
import { describe, expect, it, vi } from "vitest";
import { journalReadVerdict } from "../key-rotation";
import { landingVerdict } from "../../auth/landing-rule";
import { readJournalOnce, type JournalCatchUp, type KeyRotationPort } from "./key-rotation-port";

function port(loaded: () => boolean, answer: JournalCatchUp = "read"): KeyRotationPort & {
  catchUp: ReturnType<typeof vi.fn>;
} {
  return {
    epoch: () => null,
    post: () => Promise.reject(new Error("not used")),
    catchUp: vi.fn(() => Promise.resolve(answer)),
    loaded,
  };
}

describe("readJournalOnce outside a session", () => {
  it("does not wait for or read the page's handle", async () => {
    const p = port(() => false, "offline");
    const sleep = vi.fn(() => Promise.resolve());
    expect(await readJournalOnce({ port: p, inSession: false, waitMs: 20_000, sleep })).toBe("not-loaded");
    expect(p.catchUp).not.toHaveBeenCalled();
    expect(sleep).not.toHaveBeenCalled();
    expect(await readJournalOnce({ port: null, inSession: true, waitMs: 20_000 })).toBe("not-loaded");
  });
});

describe("base scenario 4b: what the landing could not see", () => {
  it("the directory still publishes this device's key at its own generation", () => {
    // Own key at own generation 3; auth is at 4 but discovery never heard.
    expect(landingVerdict({ keyMatches: true, keyGeneration: 3n }, 3n)).toBe("matches");
  });
});

describe("readJournalOnce inside a session (a kept device after a rotation)", () => {
  it("reads the journal when the handle is loaded", async () => {
    const p = port(() => true, "read");
    const read = await readJournalOnce({ port: p, inSession: true, waitMs: 20_000 });
    expect(read).toBe("read");
    expect(p.catchUp).toHaveBeenCalledTimes(1);
    expect(journalReadVerdict(read, true)).toEqual({ kind: "journal", journal: "read" });
  });

  it("waits for a handle that is still loading, then reads", async () => {
    let clock = 0;
    let loaded = false;
    const p = port(() => loaded, "read");
    const sleep = vi.fn((ms: number) => {
      clock += ms;
      if (clock >= 1_000) loaded = true;
      return Promise.resolve();
    });
    const read = await readJournalOnce({ port: p, inSession: true, waitMs: 20_000, sleep, now: () => clock });
    expect(read).toBe("read");
    expect(p.catchUp).toHaveBeenCalledTimes(1);
  });

  it("a handle that never loads in a session is 'later', never a wipe", async () => {
    let clock = 0;
    const p = port(() => false);
    const read = await readJournalOnce({
      port: p,
      inSession: true,
      waitMs: 20_000,
      sleep: (ms) => {
        clock += ms;
        return Promise.resolve();
      },
      now: () => clock,
    });
    expect(read).toBe("not-loaded");
    expect(p.catchUp).not.toHaveBeenCalled();
    expect(journalReadVerdict(read, true)).toEqual({ kind: "later" });
  });
});

describe("journalReadVerdict", () => {
  it("offline is never a wipe", () => {
    expect(journalReadVerdict("offline", true)).toEqual({ kind: "later" });
    expect(journalReadVerdict("offline", false)).toEqual({ kind: "later" });
  });
  it("not-loaded: later in a session, reset outside one (no DeviceGroup state)", () => {
    expect(journalReadVerdict("not-loaded", true)).toEqual({ kind: "later" });
    expect(journalReadVerdict("not-loaded", false)).toEqual({ kind: "reset" });
  });
  it("an unreadable journal goes on to the reset decision", () => {
    expect(journalReadVerdict("unreadable", false)).toEqual({ kind: "journal", journal: "unreadable" });
  });
});
