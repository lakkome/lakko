// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 9 — the rules of the end of a membership and the durable
 * record they run from.
 */
import { describe, expect, it } from "vitest";

import {
  decodeMembershipEndRow,
  encodeMembershipEndRow,
  endKey,
  endTriggered,
  EMPTY_MEMBERSHIP_END_ROW,
  heardRegisterVerdict,
  heldWelcomeStep,
  MAX_HELD_WELCOMES,
  MEMBERSHIP_END_ROW_VERSION,
  MembershipEndStore,
  stillActiveGroupIdOf,
  type EndRecord,
  type MembershipEndBackend,
  type MembershipEndRow,
} from "./membership-end";

const OWN = "own-key";
const OTHER = "other-key";

function trigger(over: Partial<Parameters<typeof endTriggered>[0]> = {}): boolean {
  return endTriggered({
    kind: "group",
    isActive: false,
    rosterPersonKeysB64: [OTHER],
    ownPersonKeysB64: [OWN],
    ...over,
  });
}

describe("the trigger", () => {
  it("fires when the group is not active and no leaf of this account is left", () => {
    expect(trigger()).toBe(true);
  });

  it("does not fire for a device whose own leaf was retired while the account stays", () => {
    // ADR-0050: `isActive` is false on this device, and another device of the
    // account is still in the frozen roster. It keeps its history.
    expect(trigger({ rosterPersonKeysB64: [OTHER, OWN] })).toBe(false);
  });

  it("recognises the account under any key its leaves can be bound to", () => {
    expect(
      trigger({ rosterPersonKeysB64: [OTHER, "predecessor"], ownPersonKeysB64: [OWN, "predecessor"] }),
    ).toBe(false);
  });

  it("does not fire while the group is active", () => {
    expect(trigger({ isActive: true })).toBe(false);
  });

  it("treats every unknown as no: a torn read is not evidence", () => {
    expect(trigger({ isActive: undefined })).toBe(false);
    expect(trigger({ rosterPersonKeysB64: null })).toBe(false);
    expect(trigger({ rosterPersonKeysB64: [] })).toBe(false);
    expect(trigger({ ownPersonKeysB64: [] })).toBe(false);
  });

  it("never ends a one-to-one: its membership is fixed (ADR-0062)", () => {
    expect(trigger({ kind: "chat" })).toBe(false);
  });
});

describe("a heard register", () => {
  const verdict = (over: Partial<Parameters<typeof heardRegisterVerdict>[0]>) =>
    heardRegisterVerdict({ held: true, isActive: true, epoch: 5n, registerEpoch: 7n, ...over });

  it("ends a group this device has not caught up with", () => {
    expect(verdict({})).toBe("end");
    // At the same epoch this is the group that ended.
    expect(verdict({ epoch: 7n })).toBe("end");
  });

  it("ends a group already inactive, at any epoch", () => {
    expect(verdict({ isActive: false, epoch: 99n })).toBe("end");
  });

  it("comes back: active above the register's epoch is a later truth", () => {
    expect(verdict({ epoch: 8n })).toBe("came-back");
  });

  it("does nothing for a group not held, or a handle that cannot be read", () => {
    expect(verdict({ held: false })).toBe("nothing");
    expect(verdict({ isActive: undefined })).toBe("nothing");
    expect(verdict({ epoch: undefined })).toBe("nothing");
  });
});

describe("a refused Welcome", () => {
  const id = new Uint8Array([1, 2, 3]);
  const refusal = Object.assign(new Error("group still active"), {
    refusalReason: "groupStillActive",
    refusalGroupId: id,
  });

  it("names its group, through a wrapper", () => {
    expect(stillActiveGroupIdOf(refusal)).toBe(id);
    expect(stillActiveGroupIdOf({ variant: { cause: refusal } })).toBe(id);
    expect(stillActiveGroupIdOf({ cause: refusal })).toBe(id);
  });

  it("is told from every other failure", () => {
    expect(stillActiveGroupIdOf(new Error("group still active"))).toBeNull();
    expect(stillActiveGroupIdOf({ refusalReason: "noRoleTable", refusalGroupId: id })).toBeNull();
    expect(stillActiveGroupIdOf({ refusalReason: "groupStillActive" })).toBeNull();
    expect(stillActiveGroupIdOf(null)).toBeNull();
  });

  it("goes only once its journal was read to the head", () => {
    expect(heldWelcomeStep(true)).toBe("drop");
    expect(heldWelcomeStep(false)).toBe("keep");
  });
});

function record(over: Partial<EndRecord> = {}): EndRecord {
  return {
    key: endKey(2, "aa"),
    refKind: 2,
    idHex: "aa",
    segmentsB64: ["qg=="],
    routeIdsB64: ["qg=="],
    title: "Book club",
    left: false,
    origin: "applied",
    endedEpoch: "7",
    stage: "recorded",
    syncedCleared: false,
    atMs: 1,
    ...over,
  };
}

describe("the stored row", () => {
  const row: MembershipEndRow = {
    records: [record()],
    notices: [{ key: "2:aa", title: "Book club", routeIdsB64: ["qg=="], draft: "half a sentence", atMs: 1 }],
    welcomes: [{ messageId: 9, groupIdB64: "qg==", welcomeB64: "AQ==", heldAtMs: 2 }],
  };

  it("round-trips", () => {
    expect(decodeMembershipEndRow(encodeMembershipEndRow(row))).toEqual(row);
  });

  it("drops a malformed entry alone", () => {
    const raw = JSON.parse(encodeMembershipEndRow(row)) as { records: unknown[] };
    raw.records.push({ key: "x" });
    expect(decodeMembershipEndRow(JSON.stringify(raw))?.records).toEqual(row.records);
  });

  it("refuses another version and junk", () => {
    const other = JSON.stringify({ ...row, v: MEMBERSHIP_END_ROW_VERSION + 1 });
    expect(decodeMembershipEndRow(other)).toBeNull();
    expect(decodeMembershipEndRow("not json")).toBeNull();
  });
});

function backend(): MembershipEndBackend & { saved: MembershipEndRow | null; fail: boolean } {
  return {
    saved: null,
    fail: false,
    async load() {
      return this.saved;
    },
    async save(_account, row) {
      if (this.fail) throw new Error("disk full");
      this.saved = row;
    },
  };
}

describe("the store", () => {
  it("writes the record and its notice in one row write", async () => {
    const b = backend();
    const store = new MembershipEndStore(b);
    await store.open("acct");
    await store.begin(record(), { key: "2:aa", title: "t", routeIdsB64: [], draft: null, atMs: 1 });
    expect(b.saved?.records).toHaveLength(1);
    expect(b.saved?.notices).toHaveLength(1);
  });

  it("rejects when the row could not be written, so nothing goes on", async () => {
    const b = backend();
    const store = new MembershipEndStore(b);
    await store.open("acct");
    b.fail = true;
    await expect(store.begin(record(), null)).rejects.toThrow("disk full");
  });

  it("survives a reload: what was on disk is what is owed", async () => {
    const b = backend();
    const first = new MembershipEndStore(b);
    await first.open("acct");
    await first.begin(record(), null);
    await first.update(record({ stage: "published" }));
    const second = new MembershipEndStore(b);
    await second.open("acct");
    expect(second.records().map((r) => r.stage)).toEqual(["published"]);
  });

  it("keeps the notice when the record finishes, until it is dismissed", async () => {
    const b = backend();
    const store = new MembershipEndStore(b);
    await store.open("acct");
    await store.begin(record(), { key: "2:aa", title: "t", routeIdsB64: [], draft: null, atMs: 1 });
    await store.finish("2:aa");
    expect(store.records()).toEqual([]);
    expect(store.notices()).toHaveLength(1);
    await store.dismiss("2:aa");
    expect(b.saved?.notices).toEqual([]);
  });

  it("holds a Welcome once, and past the bound drops the oldest", async () => {
    const b = backend();
    const store = new MembershipEndStore(b);
    await store.open("acct");
    const welcome = (messageId: number) => ({ messageId, groupIdB64: "g", welcomeB64: "w", heldAtMs: 1 });
    await store.holdWelcome(welcome(1));
    await store.holdWelcome(welcome(1));
    expect(store.welcomes()).toHaveLength(1);
    for (let i = 2; i <= MAX_HELD_WELCOMES + 1; i += 1) await store.holdWelcome(welcome(i));
    expect(store.welcomes()).toHaveLength(MAX_HELD_WELCOMES);
    expect(store.welcomes().some((w) => w.messageId === 1)).toBe(false);
  });

  it("refuses a change with no account bound", async () => {
    const store = new MembershipEndStore(backend());
    await expect(store.begin(record(), null)).rejects.toThrow("not bound");
  });

  it("starts empty when the row is unreadable", async () => {
    const b = backend();
    b.load = async () => {
      throw new Error("no");
    };
    const store = new MembershipEndStore(b);
    await store.open("acct");
    expect(store.records()).toEqual(EMPTY_MEMBERSHIP_END_ROW.records);
  });
});
