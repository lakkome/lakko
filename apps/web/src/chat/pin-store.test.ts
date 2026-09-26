// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0052 decision 3 / `docs/ux/message-actions.md` section 4.1 — the
 * receive-side verdict matrix for a shared pin, the local stamp, the row
 * codec, and the store's write-through.
 *
 * The order (`isNewer`) is the wasm core's; here it is the same strict rule
 * spelled out inline so the matrix can be pinned without loading the module.
 */
import { describe, expect, it } from "vitest";

import {
  PinStore,
  decodePinRow,
  encodePinRow,
  freshPinStamp,
  pinVerdict,
  pinVerdictAccepts,
  type PinBackend,
  type PinRows,
} from "./pin-store";

const OWNER = new Uint8Array(32).fill(0x01);
const ADMIN = new Uint8Array(32).fill(0x02);
const MEMBER = new Uint8Array(32).fill(0x03);

const isNewer = (a: bigint, b: bigint): boolean => a > b;

function verdict(over: Partial<Parameters<typeof pinVerdict>[0]>): ReturnType<typeof pinVerdict> {
  return pinVerdict({
    oneToOne: false,
    ownerKey: OWNER,
    adminKeys: [ADMIN],
    senderKey: OWNER,
    stored: { pinned: false, updatedAtMs: 100n },
    storedUnreadable: false,
    incoming: { pinned: true, updatedAtMs: 200n },
    verifyFailed: false,
    isNewer,
    ...over,
  });
}

describe("pinVerdict — the section 4.1 receiver matrix", () => {
  it("an identical action and stamp is accepted without authority or order", () => {
    const v = verdict({
      senderKey: MEMBER,
      incoming: { pinned: false, updatedAtMs: 100n },
      verifyFailed: true,
    });
    expect(v).toBe("accept-identical");
  });

  it("the owner may pin when newer", () => {
    expect(verdict({})).toBe("accept-owner");
  });

  it("an admin may pin when newer", () => {
    expect(verdict({ senderKey: ADMIN })).toBe("accept-admin");
  });

  it("an admin may clear a pin the owner set (the room's administrators take it down)", () => {
    expect(
      verdict({
        senderKey: ADMIN,
        stored: { pinned: true, updatedAtMs: 100n },
        incoming: { pinned: false, updatedAtMs: 150n },
      }),
    ).toBe("accept-admin");
  });

  it("a plain member is refused, whatever the stamp", () => {
    expect(verdict({ senderKey: MEMBER })).toBe("reject-not-authorized");
    expect(verdict({ senderKey: MEMBER, stored: null })).toBe("reject-not-authorized");
  });

  it("either participant of a one-to-one may pin, whatever the role table says", () => {
    expect(verdict({ oneToOne: true, senderKey: MEMBER })).toBe("accept-participant");
    expect(verdict({ oneToOne: true, senderKey: undefined })).toBe("accept-participant");
  });

  it("a table-less group is open to any member", () => {
    expect(verdict({ ownerKey: undefined, senderKey: MEMBER })).toBe("accept-no-owner");
  });

  it("an unresolvable sender in a group with a table is unverifiable", () => {
    expect(verdict({ senderKey: undefined })).toBe("reject-unverifiable");
  });

  it("a torn role-table read is unverifiable for a changed action", () => {
    expect(verdict({ verifyFailed: true })).toBe("reject-unverifiable");
  });

  it("an unreadable local row is unverifiable for a changed action", () => {
    expect(verdict({ storedUnreadable: true })).toBe("reject-unverifiable");
  });

  it("nothing stored for that message: authority alone decides", () => {
    expect(verdict({ stored: null })).toBe("accept-owner");
    expect(verdict({ stored: null, senderKey: MEMBER })).toBe("reject-not-authorized");
  });

  it("an EQUAL stamp with a different action is stale (a re-send is not an update)", () => {
    expect(
      verdict({
        stored: { pinned: false, updatedAtMs: 200n },
        incoming: { pinned: true, updatedAtMs: 200n },
      }),
    ).toBe("reject-stale");
  });

  it("a late pin never undoes a later unpin", () => {
    // The room pinned at 100, unpinned at 300; a pin stamped 200 arrives last.
    expect(
      verdict({
        stored: { pinned: false, updatedAtMs: 300n },
        incoming: { pinned: true, updatedAtMs: 200n },
      }),
    ).toBe("reject-stale");
  });

  it("the order is per message: authority is checked before the stamp", () => {
    // Authority first, so a stale envelope from a plain member reports the
    // refusal that matters.
    expect(
      verdict({
        senderKey: MEMBER,
        stored: { pinned: true, updatedAtMs: 300n },
        incoming: { pinned: false, updatedAtMs: 200n },
      }),
    ).toBe("reject-not-authorized");
  });

  it("pinVerdictAccepts is true for exactly the accept verdicts", () => {
    for (const v of [
      "accept-identical",
      "accept-owner",
      "accept-admin",
      "accept-participant",
      "accept-no-owner",
    ] as const) {
      expect(pinVerdictAccepts(v)).toBe(true);
    }
    for (const v of ["reject-not-authorized", "reject-unverifiable", "reject-stale"] as const) {
      expect(pinVerdictAccepts(v)).toBe(false);
    }
  });
});

describe("freshPinStamp — the local action's stamp", () => {
  it("is the clock when nothing is stored or the clock is ahead", () => {
    expect(freshPinStamp(1_000, undefined)).toBe(1_000n);
    expect(freshPinStamp(1_000, { pinned: true, updatedAtMs: 999n })).toBe(1_000n);
  });

  it("moves strictly past a stored stamp the clock lags, so the unpin is not dropped", () => {
    expect(freshPinStamp(1_000, { pinned: true, updatedAtMs: 1_000n })).toBe(1_001n);
    expect(freshPinStamp(1_000, { pinned: true, updatedAtMs: 5_000n })).toBe(5_001n);
  });

  it("the fresh stamp always passes the strict order against what was stored", () => {
    const stored = { pinned: true, updatedAtMs: 2_000n };
    expect(isNewer(freshPinStamp(1_000, stored), stored.updatedAtMs)).toBe(true);
  });
});

describe("the row codec", () => {
  const rows: PinRows = {
    shared: new Map([
      [
        "g1",
        new Map([
          [7, { pinned: true, updatedAtMs: 1_700n }],
          [9, { pinned: false, updatedAtMs: 1_800n }],
        ]),
      ],
    ]),
    personal: new Map([["g1", new Map([[4, 500n]])], ["g2", new Map([[1, 600n]])]]),
  };

  it("round-trips shared pins, stored unpins and personal pins", () => {
    const back = decodePinRow(encodePinRow(rows));
    expect(back).not.toBeNull();
    expect(back?.shared.get("g1")?.get(7)).toEqual({ pinned: true, updatedAtMs: 1_700n });
    expect(back?.shared.get("g1")?.get(9)).toEqual({ pinned: false, updatedAtMs: 1_800n });
    expect(back?.personal.get("g1")?.get(4)).toBe(500n);
    expect(back?.personal.get("g2")?.get(1)).toBe(600n);
  });

  it("keeps a u64 stamp exact through JSON", () => {
    const big = 18_446_744_073_709_551_615n;
    const back = decodePinRow(
      encodePinRow({
        shared: new Map([["g", new Map([[1, { pinned: true, updatedAtMs: big }]])]]),
        personal: new Map(),
      }),
    );
    expect(back?.shared.get("g")?.get(1)?.updatedAtMs).toBe(big);
  });

  it("drops a malformed entry rather than trusting it", () => {
    const json = JSON.stringify({
      v: 1,
      s: {
        g: {
          "1": { p: 1, t: "10" },
          "2": { p: 2, t: "10" },
          "3": { p: 1, t: "ten" },
          x: { p: 1, t: "10" },
          "99999999999999999999": { p: 1, t: "10" },
        },
      },
      m: { g: { "5": "7", "6": 7, y: "1" } },
    });
    const back = decodePinRow(json);
    expect([...(back?.shared.get("g")?.keys() ?? [])]).toEqual([1]);
    expect([...(back?.personal.get("g")?.keys() ?? [])]).toEqual([5]);
  });

  it("refuses a payload of another schema", () => {
    expect(decodePinRow("not json")).toBeNull();
    expect(decodePinRow(JSON.stringify({ v: 2, s: {}, m: {} }))).toBeNull();
    expect(decodePinRow("null")).toBeNull();
  });
});

function memoryBackend(initial: PinRows | null = null, throwOnLoad = false): {
  backend: PinBackend;
  saved: PinRows[];
} {
  const saved: PinRows[] = [];
  return {
    saved,
    backend: {
      load: async () => {
        if (throwOnLoad) throw new Error("boom");
        return initial;
      },
      save: async (_account, rows) => {
        saved.push(rows);
      },
    },
  };
}

describe("PinStore — write-through", () => {
  it("opens the account's row and reports it readable", async () => {
    const { backend } = memoryBackend({
      shared: new Map([["g", new Map([[1, { pinned: true, updatedAtMs: 5n }]])]]),
      personal: new Map([["g", new Map([[2, 9n]])]]),
    });
    const store = new PinStore(backend);
    expect(await store.open("acct")).toBe(true);
    expect(store.getShared("g", 1)).toEqual({ pinned: true, updatedAtMs: 5n });
    expect(store.personal("g").get(2)).toBe(9n);
  });

  it("reports an unreadable row as such and starts empty", async () => {
    const errors: unknown[] = [];
    const store = new PinStore(memoryBackend(null, true).backend, (e) => errors.push(e));
    expect(await store.open("acct")).toBe(false);
    expect(store.shared("g").size).toBe(0);
    expect(errors).toHaveLength(1);
  });

  it("persists a shared action, a personal pin and their removal", async () => {
    const { backend, saved } = memoryBackend();
    const store = new PinStore(backend);
    await store.open("acct");
    await store.setShared("acct", "g", 1, { pinned: true, updatedAtMs: 5n });
    await store.setPersonal("acct", "g", 1, 10n);
    await store.setPersonal("acct", "g", 2, 11n);
    expect(saved.at(-1)?.shared.get("g")?.get(1)?.pinned).toBe(true);
    expect([...(saved.at(-1)?.personal.get("g")?.keys() ?? [])]).toEqual([1, 2]);
    await store.setPersonal("acct", "g", 2, null);
    expect(store.personal("g").has(2)).toBe(false);
    await store.dropMessage("acct", "g", 1);
    expect(store.getShared("g", 1)).toBeUndefined();
    expect(store.personal("g").size).toBe(0);
    expect(saved.at(-1)?.shared.has("g")).toBe(false);
  });

  it("an unpin is stored, not deleted", async () => {
    const store = new PinStore(memoryBackend().backend);
    await store.open("acct");
    await store.setShared("acct", "g", 1, { pinned: true, updatedAtMs: 5n });
    await store.setShared("acct", "g", 1, { pinned: false, updatedAtMs: 6n });
    expect(store.getShared("g", 1)).toEqual({ pinned: false, updatedAtMs: 6n });
  });

  it("forgets a conversation whole", async () => {
    const { backend, saved } = memoryBackend();
    const store = new PinStore(backend);
    await store.open("acct");
    await store.setShared("acct", "g", 1, { pinned: true, updatedAtMs: 5n });
    await store.setPersonal("acct", "h", 3, 1n);
    await store.forgetGroup("acct", "g");
    expect(store.shared("g").size).toBe(0);
    expect(store.personal("h").get(3)).toBe(1n);
    expect(saved.at(-1)?.shared.has("g")).toBe(false);
  });

  it("a write that lands before the load settles wins over the stored row", async () => {
    let release: (() => void) | undefined;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    const backend: PinBackend = {
      load: async () => {
        await gate;
        return {
          shared: new Map([["g", new Map([[1, { pinned: true, updatedAtMs: 5n }]])]]),
          personal: new Map(),
        };
      },
      save: async () => undefined,
    };
    const store = new PinStore(backend);
    const opening = store.open("acct");
    await store.setShared("acct", "g", 1, { pinned: false, updatedAtMs: 9n });
    release?.();
    await opening;
    expect(store.getShared("g", 1)).toEqual({ pinned: false, updatedAtMs: 9n });
  });

  it("notifies subscribers on every change and close clears memory", async () => {
    const store = new PinStore(memoryBackend().backend);
    let ticks = 0;
    const off = store.subscribe(() => {
      ticks += 1;
    });
    await store.open("acct");
    await store.setPersonal("acct", "g", 1, 1n);
    store.close();
    expect(ticks).toBe(3);
    expect(store.personal("g").size).toBe(0);
    off();
  });
});
