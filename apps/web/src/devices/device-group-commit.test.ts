// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0081 decision 2 - the DeviceGroup commit outbox, driven over its
 * ports. Each failure path of the ADR's Context has a case:
 *
 *   1. a crash between the DS registration and the persist (before ADR-0081
 *      the DS held N+1 and every device N): the entry written before the
 *      send carries the commit, and the restart finishes it;
 *   2. a failed publish after the persist (nothing retried it): the entry
 *      stays and is retried;
 *   3. a swallowed persist failure (the commit published while the device
 *      stayed at N): making N+1 current fails closed and the entry stays.
 *
 * The model is a tiny account: a durable epoch, a DS that registers an epoch
 * only together with its row, and an outbox slot.
 */
import { describe, expect, it } from "vitest";

import {
  decodeDeviceGroupCommitEntry,
  encodeDeviceGroupCommitEntry,
  resendOwedDeviceGroupCommit,
  runDeviceGroupCommit,
  withDeviceGroupCommitLock,
  type DeviceGroupCommitEntry,
  type DeviceGroupCommitPorts,
  type DurableProof,
} from "./device-group-commit";

class Conflict extends Error {
  readonly status = 409;
}

interface Model {
  ports: DeviceGroupCommitPorts;
  /** The device's durable DeviceGroup epoch. */
  durable(): bigint;
  /** Rows on the DS journal, `[epoch, row]`. */
  journal: Array<[bigint, string]>;
  entry(): DeviceGroupCommitEntry | null;
  log: string[];
  /** Fail the next N calls of a step. */
  failNext: Record<string, number>;
  /** The DS registers this epoch for another commit first. */
  takeEpochElsewhere(epoch: bigint): void;
}

function proofAt(epoch: bigint): DurableProof {
  return { epoch, sign: () => `proof@${epoch}`, free: () => undefined };
}

function model(startEpoch = 5n): Model {
  let durable = startEpoch;
  let slot: DeviceGroupCommitEntry | null = null;
  const journal: Array<[bigint, string]> = [];
  const registered = new Map<bigint, string>(); // epoch -> row
  const log: string[] = [];
  const failNext: Record<string, number> = {};
  const step = (name: string): void => {
    log.push(name);
    const n = failNext[name] ?? 0;
    if (n > 0) {
      failNext[name] = n - 1;
      throw new Error(`${name} failed`);
    }
  };
  const ports: DeviceGroupCommitPorts = {
    async saveEntry(e) {
      step("save");
      slot = e;
    },
    async loadEntry() {
      return slot;
    },
    async deleteEntry() {
      step("delete");
      slot = null;
    },
    async durableProof() {
      return proofAt(durable);
    },
    async commit(e, proof) {
      step("commit");
      const epoch = BigInt(e.epoch);
      const held = registered.get(epoch);
      if (held !== undefined) {
        if (held === e.rowB64) return { id: journal.findIndex(([ep]) => ep === epoch) + 1 };
        throw new Conflict("409");
      }
      if (proof.epoch + 1n !== epoch) throw new Error("bad proof epoch");
      registered.set(epoch, e.rowB64);
      journal.push([epoch, e.rowB64]);
      return { id: journal.length };
    },
    async makeCurrent(e) {
      step("make-current");
      durable = BigInt(e.epoch);
    },
    announce() {
      log.push("announce");
    },
    async deliverWelcome() {
      step("welcome");
    },
    isConflict: (err) => err instanceof Conflict,
  };
  return {
    ports,
    durable: () => durable,
    journal,
    entry: () => slot,
    log,
    failNext,
    takeEpochElsewhere(epoch) {
      registered.set(epoch, "elsewhere");
      journal.push([epoch, "elsewhere"]);
    },
  };
}

function entryFor(epoch: bigint, welcome = true): DeviceGroupCommitEntry {
  return {
    v: 1,
    kind: welcome ? "add" : "remove",
    journalIdB64: "am91cm5hbA==",
    epoch: epoch.toString(),
    epochKeyB64: "a2V5",
    rowB64: `row-${epoch}`,
    nextSnapshotB64: "c25hcA==",
    createdAtMs: 1,
    ...(welcome ? { welcomeB64: "d2VsY29tZQ==", welcomeRendezvousB64: "cmVuZGV6dm91cw==" } : {}),
  };
}

describe("ADR-0081 decision 2 - the order", () => {
  it("entry beside N, the commit, N+1 current, the Welcome, the entry deleted", async () => {
    const m = model();
    const out = await runDeviceGroupCommit(entryFor(6n), proofAt(5n), m.ports);
    expect(out).toEqual({ kind: "landed", messageId: 1 });
    expect(m.log).toEqual(["save", "commit", "make-current", "announce", "welcome", "delete"]);
    expect(m.durable()).toBe(6n);
    expect(m.entry()).toBeNull();
    expect(m.journal).toEqual([[6n, "row-6"]]);
  });

  it("a failed entry write aborts with nothing sent", async () => {
    const m = model();
    m.failNext["save"] = 1;
    await expect(runDeviceGroupCommit(entryFor(6n), proofAt(5n), m.ports)).rejects.toThrow(/save/);
    expect(m.journal).toEqual([]);
    expect(m.durable()).toBe(5n);
  });

  it("a proof of another epoch is refused before anything is written", async () => {
    const m = model();
    await expect(runDeviceGroupCommit(entryFor(6n), proofAt(4n), m.ports)).rejects.toThrow(/proof/);
    expect(m.log).toEqual([]);
  });
});

describe("ADR-0081 Context, path 1 - a crash after the DS took the commit, before the persist", () => {
  it("the restart re-sends the same row (idempotent) and makes N+1 current", async () => {
    const m = model();
    // The run is killed right after the DS answered: make-current never ran.
    m.failNext["make-current"] = 1;
    await expect(runDeviceGroupCommit(entryFor(6n), proofAt(5n), m.ports)).rejects.toThrow();
    expect(m.durable()).toBe(5n);
    expect(m.journal).toHaveLength(1);
    expect(m.entry()).not.toBeNull();

    const out = await resendOwedDeviceGroupCommit(m.ports);
    expect(out).toEqual({ kind: "landed", messageId: 1 });
    expect(m.durable()).toBe(6n);
    // Nothing appended twice: the re-send answered the original row.
    expect(m.journal).toEqual([[6n, "row-6"]]);
    expect(m.entry()).toBeNull();
  });
});

describe("ADR-0081 Context, path 2 - the publish fails", () => {
  it("the entry stays, the device stays at N, the retry lands it", async () => {
    const m = model();
    m.failNext["commit"] = 1;
    await expect(runDeviceGroupCommit(entryFor(6n), proofAt(5n), m.ports)).rejects.toThrow(/commit/);
    expect(m.durable()).toBe(5n);
    expect(m.entry()?.epoch).toBe("6");
    expect(m.journal).toEqual([]);

    const out = await resendOwedDeviceGroupCommit(m.ports);
    expect(out?.kind).toBe("landed");
    expect(m.durable()).toBe(6n);
    expect(m.journal).toEqual([[6n, "row-6"]]);
  });
});

describe("ADR-0081 Context, path 3 - the persist fails", () => {
  it("making N+1 current fails closed: the Welcome waits, the entry stays", async () => {
    const m = model();
    m.failNext["make-current"] = 1;
    await expect(runDeviceGroupCommit(entryFor(6n), proofAt(5n), m.ports)).rejects.toThrow(/make-current/);
    expect(m.log).not.toContain("welcome");
    expect(m.entry()).not.toBeNull();
    await resendOwedDeviceGroupCommit(m.ports);
    expect(m.durable()).toBe(6n);
    expect(m.log.filter((l) => l === "welcome")).toHaveLength(1);
  });

  it("a Welcome that fails after N+1 is current is delivered by the retry, without writing N+1 again", async () => {
    const m = model();
    m.failNext["welcome"] = 1;
    await expect(runDeviceGroupCommit(entryFor(6n), proofAt(5n), m.ports)).rejects.toThrow(/welcome/);
    expect(m.durable()).toBe(6n);
    m.log.length = 0;
    await resendOwedDeviceGroupCommit(m.ports);
    // The durable state is already at the entry's epoch: no second
    // make-current (that would put an older N+1 back over a newer one).
    expect(m.log).toEqual(["commit", "announce", "welcome", "delete"]);
  });
});

describe("ADR-0081 decision 2, step 5 - a lost epoch", () => {
  it("409 deletes the entry; the device stays at N", async () => {
    const m = model();
    m.takeEpochElsewhere(6n);
    const out = await runDeviceGroupCommit(entryFor(6n), proofAt(5n), m.ports);
    expect(out).toEqual({ kind: "lost" });
    expect(m.entry()).toBeNull();
    expect(m.durable()).toBe(5n);
    expect(m.log).not.toContain("make-current");
    expect(m.log).not.toContain("welcome");
  });

  it("a retry that meets 409 deletes the entry too", async () => {
    const m = model();
    m.failNext["commit"] = 1;
    await expect(runDeviceGroupCommit(entryFor(6n), proofAt(5n), m.ports)).rejects.toThrow();
    m.takeEpochElsewhere(6n);
    expect(await resendOwedDeviceGroupCommit(m.ports)).toEqual({ kind: "lost" });
    expect(m.entry()).toBeNull();
    expect(m.durable()).toBe(5n);
  });

  it("nothing owed: the re-send is a no-op", async () => {
    const m = model();
    expect(await resendOwedDeviceGroupCommit(m.ports)).toBeNull();
    expect(m.log).toEqual([]);
  });
});

describe("serialisation and the codec", () => {
  it("two commits of one account never run at once", async () => {
    const order: string[] = [];
    let release: () => void = () => undefined;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const a = withDeviceGroupCommitLock("acc", async () => {
      order.push("a-start");
      await gate;
      order.push("a-end");
    });
    const b = withDeviceGroupCommitLock("acc", async () => {
      order.push("b");
    });
    await new Promise((r) => setTimeout(r, 0));
    expect(order).toEqual(["a-start"]);
    release();
    await Promise.all([a, b]);
    expect(order).toEqual(["a-start", "a-end", "b"]);
  });

  it("an entry round-trips; a malformed one decodes to null", () => {
    const e = { ...entryFor(9n), priorLeaves: [0, 2], ownLeaf: 0 };
    expect(decodeDeviceGroupCommitEntry(encodeDeviceGroupCommitEntry(e))).toEqual(e);
    expect(decodeDeviceGroupCommitEntry(new TextEncoder().encode("{}"))).toBeNull();
    expect(decodeDeviceGroupCommitEntry(new TextEncoder().encode("not json"))).toBeNull();
    const half = { ...entryFor(9n, false), welcomeB64: "eA==" };
    expect(decodeDeviceGroupCommitEntry(encodeDeviceGroupCommitEntry(half))).toBeNull();
  });
});
