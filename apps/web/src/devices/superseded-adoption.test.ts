// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * A device told `key-generation-superseded` with no session in the page
 * (ADR-0074 section 3.3; base scenario 4b): it reads its own DeviceGroup
 * journal once, outside the session, before it decides. A kept device of a
 * rotation finds the `key-rotation` envelope there and adopts the new keys;
 * a device whose account was reset finds none and is signed out.
 */
import { describe, expect, it, vi } from "vitest";

import type { PendingRotation } from "./key-rotation";
import { settleSuperseded, type SupersededPorts, type TryKeyOutcome } from "./superseded-adoption";
import {
  readJournalStandalone,
  type StandaloneJournal,
  type StandaloneJournalRow,
} from "./sync/standalone-journal-read";

const enc = (s: string): Uint8Array => new TextEncoder().encode(s);
const dec = (b: Uint8Array): string => new TextDecoder().decode(b);

/** This device's DeviceGroup as stored: a persisted cursor and a journal. */
function storedDeviceGroup(opts: {
  readonly cursor: number;
  readonly journal: readonly { readonly id: number; readonly body: string }[];
  readonly pageSize?: number;
}): StandaloneJournal & { fetchedFrom: number[]; freed: boolean } {
  const fetchedFrom: number[] = [];
  const self = {
    cursor: opts.cursor,
    fetchedFrom,
    freed: false,
    fetch(after: number): Promise<{ rows: StandaloneJournalRow[]; hasMore: boolean }> {
      fetchedFrom.push(after);
      const rest = opts.journal.filter((r) => r.id > after);
      const page = rest.slice(0, opts.pageSize ?? 100);
      return Promise.resolve({
        rows: page.map((r) => ({ id: r.id, payload: enc(r.body) })),
        hasMore: rest.length > page.length,
      });
    },
    apply(row: StandaloneJournalRow): Promise<Uint8Array | null> {
      const body = dec(row.payload);
      if (body === "own-row") return Promise.reject(new Error("CannotDecryptOwnMessage"));
      // A commit applies and carries nothing; an application row may carry
      // a service envelope.
      return Promise.resolve(body.startsWith("envelope:") ? enc(body.slice("envelope:".length)) : null);
    },
    free(): void {
      self.freed = true;
    },
  };
  return self;
}

function pendingRow(key: string): PendingRotation {
  return { newSignaturePubkeyB64: key, envelopeB64: "", preparedAtMs: 1 };
}

/** The ports of a device with no session in the page, holding key `H`; the
 *  server's account key is `serverKey`. */
function supersededDevice(opts: {
  readonly serverKey: string;
  readonly open: () => Promise<StandaloneJournal | null>;
  readonly classify?: (err: unknown) => "unreadable" | "offline";
}): { ports: SupersededPorts; pending: PendingRotation[]; tried: string[]; intake: ReturnType<typeof vi.fn> } {
  const pending: PendingRotation[] = [];
  const tried: string[] = [];
  // The intake stores a verified `key-rotation` envelope as pending.
  const intake = vi.fn((envelope: Uint8Array) => {
    const body = dec(envelope);
    if (body.startsWith("key-rotation:")) pending.push(pendingRow(body.slice("key-rotation:".length)));
    return Promise.resolve();
  });
  const ports: SupersededPorts = {
    heldKeyB64: "H",
    pending: () => Promise.resolve([...pending]),
    inSession: () => false,
    readJournal: () =>
      readJournalStandalone({
        open: opts.open,
        intake,
        classify: opts.classify ?? (() => "offline"),
      }),
    tryKey: (keyB64): Promise<TryKeyOutcome> => {
      tried.push(keyB64);
      return Promise.resolve(keyB64 === opts.serverKey ? "adopted" : "not-accepted");
    },
  };
  return { ports, pending, tried, intake };
}

describe("a superseded device with no session reads its own DeviceGroup journal first", () => {
  it("a kept device of a rotation finds the envelope and adopts: no wipe", async () => {
    const dg = storedDeviceGroup({
      cursor: 10,
      journal: [
        { id: 4, body: "envelope:key-rotation:OLD" }, // behind the cursor: not read again
        { id: 11, body: "commit" },
        { id: 12, body: "own-row" },
        { id: 13, body: "envelope:device-card" },
        { id: 14, body: "envelope:key-rotation:NEW" },
      ],
      pageSize: 2,
    });
    const device = supersededDevice({ serverKey: "NEW", open: () => Promise.resolve(dg) });
    await expect(settleSuperseded(device.ports)).resolves.toBe("adopted");
    expect(dg.fetchedFrom).toEqual([10, 12]);
    expect(device.pending.map((p) => p.newSignaturePubkeyB64)).toEqual(["NEW"]);
    expect(device.tried).toEqual(["H", "NEW"]);
    expect(dg.freed).toBe(true);
  });

  it("the same device with no envelope in the journal is a reset", async () => {
    const dg = storedDeviceGroup({ cursor: 10, journal: [{ id: 11, body: "commit" }, { id: 12, body: "envelope:device-card" }] });
    // Auth's key is a reset's new key, which this device never learns.
    const device = supersededDevice({ serverKey: "RESET-KEY", open: () => Promise.resolve(dg) });
    await expect(settleSuperseded(device.ports)).resolves.toBe("reset");
    expect(device.tried).toEqual(["H"]);
    expect(dg.fetchedFrom).toEqual([10]);
    expect(dg.freed).toBe(true);
  });

  it("a device with no DeviceGroup state at all is a reset", async () => {
    const device = supersededDevice({ serverKey: "RESET-KEY", open: () => Promise.resolve(null) });
    await expect(settleSuperseded(device.ports)).resolves.toBe("reset");
  });

  it("a journal that refuses this device is a reset", async () => {
    const dg = storedDeviceGroup({ cursor: 0, journal: [] });
    dg.fetch = () => Promise.reject(new Error("401"));
    const device = supersededDevice({
      serverKey: "RESET-KEY",
      open: () => Promise.resolve(dg),
      classify: () => "unreadable",
    });
    await expect(settleSuperseded(device.ports)).resolves.toBe("reset");
  });

  it("no answer from the DS is 'later', never a wipe", async () => {
    const dg = storedDeviceGroup({ cursor: 0, journal: [] });
    dg.fetch = () => Promise.reject(new Error("network"));
    const device = supersededDevice({ serverKey: "RESET-KEY", open: () => Promise.resolve(dg) });
    await expect(settleSuperseded(device.ports)).resolves.toBe("later");
    expect(dg.freed).toBe(true);
  });

  it("a stored envelope is tried before any journal read", async () => {
    const open = vi.fn(() => Promise.resolve(null));
    const device = supersededDevice({ serverKey: "NEW", open });
    device.pending.push(pendingRow("NEW"));
    await expect(settleSuperseded(device.ports)).resolves.toBe("adopted");
    expect(open).not.toHaveBeenCalled();
  });
});

describe("readJournalStandalone", () => {
  it("gives up at its deadline as 'offline'", async () => {
    let clock = 0;
    const dg = storedDeviceGroup({ cursor: 0, journal: [] });
    dg.fetch = (after) => {
      clock += 15_000;
      return Promise.resolve({ rows: [{ id: after + 1, payload: enc("commit") }], hasMore: true });
    };
    const read = await readJournalStandalone({
      open: () => Promise.resolve(dg),
      intake: () => Promise.resolve(),
      classify: () => "offline",
      deadlineMs: 40_000,
      now: () => clock,
    });
    expect(read).toBe("offline");
    expect(dg.freed).toBe(true);
  });

  it("an open that throws is 'offline'", async () => {
    const read = await readJournalStandalone({
      open: () => Promise.reject(new Error("idb")),
      intake: () => Promise.resolve(),
      classify: () => "offline",
    });
    expect(read).toBe("offline");
  });
});

describe("settleSuperseded inside a session", () => {
  it("a handle still loading is 'later'", async () => {
    const ports: SupersededPorts = {
      heldKeyB64: "H",
      pending: () => Promise.resolve([]),
      inSession: () => true,
      readJournal: () => Promise.resolve("not-loaded"),
      tryKey: () => Promise.resolve("not-accepted"),
    };
    await expect(settleSuperseded(ports)).resolves.toBe("later");
  });

  it("device-revoked from the upgrade ends it at once", async () => {
    const readJournal = vi.fn(() => Promise.resolve("read" as const));
    const ports: SupersededPorts = {
      heldKeyB64: "H",
      pending: () => Promise.resolve([]),
      inSession: () => true,
      readJournal,
      tryKey: () => Promise.resolve("device-revoked"),
    };
    await expect(settleSuperseded(ports)).resolves.toBe("device-revoked");
    expect(readJournal).not.toHaveBeenCalled();
  });
});
