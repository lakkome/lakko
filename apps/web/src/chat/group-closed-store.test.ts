// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0065 R.6 - the claim state's codec and store (`group-closed-store.ts`). */
import { describe, expect, it } from "vitest";

import {
  decodeGroupClosedRow,
  encodeGroupClosedRow,
  GroupClosedStore,
  MAX_PENDING_CLAIMS,
  NO_CLAIMS,
  withAcceptedClaim,
  withPendingClaim,
  type GroupClosedBackend,
  type StoredGroupClosed,
} from "./group-closed-store";
import type { SuccessorClaim } from "./successor-claim";

function claim(messageId: number, successor = "S"): SuccessorClaim {
  return {
    messageId,
    successorGroupIdB64: successor,
    senderKeyB64: "sender",
    senderWasAdmin: true,
    adminKeysAtReceiptB64: ["sender", "other"],
  };
}

describe("the claim state", () => {
  it("keeps pending claims in journal order, each once, bounded", () => {
    let state = NO_CLAIMS;
    state = withPendingClaim(state, claim(9));
    state = withPendingClaim(state, claim(3));
    state = withPendingClaim(state, claim(9));
    expect(state.pending.map((c) => c.messageId)).toEqual([3, 9]);
    for (let i = 100; i < 100 + MAX_PENDING_CLAIMS + 5; i += 1) state = withPendingClaim(state, claim(i));
    expect(state.pending).toHaveLength(MAX_PENDING_CLAIMS);
    expect(state.pending[0]?.messageId).toBe(3);
  });
  it("an accepted claim spends the pending list and keeps the own-claim record", () => {
    const state: StoredGroupClosed = {
      accepted: null,
      pending: [claim(3)],
      own: { successorGroupIdB64: "mine", messageId: 12 },
    };
    expect(withAcceptedClaim(state, claim(3))).toEqual({
      accepted: claim(3),
      pending: [],
      own: { successorGroupIdB64: "mine", messageId: 12 },
    });
  });
});

describe("the row codec", () => {
  it("round-trips", () => {
    const rows = new Map<string, StoredGroupClosed>([
      ["G1", { accepted: claim(5, "S5"), pending: [], own: null }],
      ["G2", { accepted: null, pending: [claim(2), claim(8)], own: { successorGroupIdB64: "mine", messageId: null } }],
      [
        "G3",
        {
          accepted: claim(4, "mine"),
          pending: [],
          own: null,
          prepared: { successorGroupIdB64: "mine", welcomeB64: "d2VsY29tZQ==", targetKeysB64: ["a", "b"], delivered: false },
        },
      ],
      [
        "G4",
        {
          accepted: null,
          pending: [],
          own: null,
          prepared: { successorGroupIdB64: "mine", welcomeB64: null, targetKeysB64: [], delivered: true },
        },
      ],
    ]);
    expect(decodeGroupClosedRow(encodeGroupClosedRow(rows))).toEqual(rows);
  });
  it("does not read a row of the removed visible flow, or junk", () => {
    expect(decodeGroupClosedRow(JSON.stringify({ v: 2, g: { G: { t: "1", s: null, b: null } } }))).toBeNull();
    expect(decodeGroupClosedRow("not json")).toBeNull();
    expect(decodeGroupClosedRow("null")).toBeNull();
  });
  it("drops a malformed entry rather than trusting it", () => {
    const json = JSON.stringify({
      v: 3,
      g: {
        ok: { a: null, p: [], o: null },
        bad: { a: { m: "x", s: "S", k: "k", a: true, t: [] }, p: [], o: null },
        junkPending: { a: null, p: [{ m: -1 }, { m: 4, s: "S", k: "k", a: false, t: ["z"] }], o: { s: 5, m: 1 } },
      },
    });
    const rows = decodeGroupClosedRow(json);
    expect(rows?.has("bad")).toBe(false);
    expect(rows?.get("ok")).toEqual(NO_CLAIMS);
    expect(rows?.get("junkPending")?.pending.map((c) => c.messageId)).toEqual([4]);
    expect(rows?.get("junkPending")?.own).toBeNull();
  });
});

describe("GroupClosedStore", () => {
  it("writes through and reloads per account", async () => {
    const disk = new Map<string, ReadonlyMap<string, StoredGroupClosed>>();
    const backend: GroupClosedBackend = {
      load: async (account) => disk.get(account) ?? null,
      save: async (account, rows) => {
        disk.set(account, new Map(rows));
      },
    };
    const store = new GroupClosedStore(backend);
    await store.open("acct");
    await store.set("acct", "G", withAcceptedClaim(NO_CLAIMS, claim(7)));
    const again = new GroupClosedStore(backend);
    await again.open("acct");
    expect(again.get("G")?.accepted?.messageId).toBe(7);
    await again.remove("acct", "G");
    expect(disk.get("acct")?.has("G")).toBe(false);
  });
  it("reports an unreadable backend instead of reading it as empty", async () => {
    const backend: GroupClosedBackend = {
      load: async () => {
        throw new Error("idb");
      },
      save: async () => undefined,
    };
    expect(await new GroupClosedStore(backend).open("acct")).toBe(false);
  });
});
