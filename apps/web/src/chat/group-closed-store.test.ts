// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0065 phase 2 (UX 16.5 / 16.8) — the receive-side verdict matrix for a
 * group-closed notice (the group-name gate's shape, exactly), the row codec,
 * the store, and the sniff over the REAL core codec.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
import { base64Encode } from "../auth/account-id";
import { sniffGroupClosed } from "./group-closed-envelope";
import {
  GroupClosedStore,
  decodeGroupClosedRow,
  encodeGroupClosedRow,
  groupClosedVerdict,
  groupClosedVerdictAccepts,
  type GroupClosedBackend,
  type StoredGroupClosed,
} from "./group-closed-store";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const OWNER = new Uint8Array(32).fill(0x01);
const ADMIN = new Uint8Array(32).fill(0x02);
const MEMBER = new Uint8Array(32).fill(0x03);
const SUCCESSOR = new Uint8Array(32).fill(0x77);
const SUCCESSOR_B64 = base64Encode(SUCCESSOR);

const stored: StoredGroupClosed = { closedAtMs: 100n, successorGroupIdB64: "first" };
const incoming: StoredGroupClosed = { closedAtMs: 200n, successorGroupIdB64: SUCCESSOR_B64 };

function verdict(over: Partial<Parameters<typeof groupClosedVerdict>[0]>): ReturnType<typeof groupClosedVerdict> {
  return groupClosedVerdict({
    kind: "group",
    ownerKey: OWNER,
    adminKeys: [ADMIN],
    senderKey: ADMIN,
    stored,
    storedUnreadable: false,
    incoming,
    verifyFailed: false,
    isNewer: (a, b) => realWasm.groupClosedIsNewer(a, b),
    ...over,
  });
}

describe("groupClosedVerdict — the group-name matrix, verbatim", () => {
  it("a one-to-one cannot be closed by anyone's notice", () => {
    expect(verdict({ kind: "chat" })).toBe("reject-one-to-one");
    expect(verdict({ kind: "chat", senderKey: OWNER, stored: null })).toBe("reject-one-to-one");
  });

  it("an identical notice is accepted without authority or order", () => {
    expect(verdict({ senderKey: MEMBER, incoming: stored, verifyFailed: true })).toBe("accept-identical");
  });

  it("the owner and an admin may close, when newer; a plain member never", () => {
    expect(verdict({ senderKey: OWNER })).toBe("accept-owner");
    expect(verdict({})).toBe("accept-admin");
    expect(verdict({ senderKey: MEMBER })).toBe("reject-not-authorized");
    expect(verdict({ senderKey: MEMBER, stored: null })).toBe("reject-not-authorized");
  });

  it("no role table: any member, when newer", () => {
    expect(verdict({ ownerKey: undefined, adminKeys: [], senderKey: MEMBER })).toBe("accept-no-owner");
  });

  it("an unreadable table, an unreadable row or an unresolvable sender is refused (fail-closed)", () => {
    expect(verdict({ verifyFailed: true })).toBe("reject-unverifiable");
    expect(verdict({ storedUnreadable: true })).toBe("reject-unverifiable");
    expect(verdict({ senderKey: undefined })).toBe("reject-unverifiable");
  });

  it("the second notice wins the successor only when strictly newer (the core's order)", () => {
    expect(verdict({ incoming: { ...incoming, closedAtMs: 100n } })).toBe("reject-stale");
    expect(verdict({ incoming: { ...incoming, closedAtMs: 50n } })).toBe("reject-stale");
    expect(verdict({ incoming: { ...incoming, closedAtMs: 101n } })).toBe("accept-admin");
  });

  it("nothing stored: an authorised notice is accepted whatever its stamp", () => {
    expect(verdict({ stored: null, incoming: { ...incoming, closedAtMs: 1n } })).toBe("accept-admin");
  });

  it("verdictAccepts is exactly the accept-* set", () => {
    for (const v of ["accept-identical", "accept-owner", "accept-admin", "accept-no-owner"] as const) {
      expect(groupClosedVerdictAccepts(v)).toBe(true);
    }
    for (const v of ["reject-not-authorized", "reject-unverifiable", "reject-stale", "reject-one-to-one"] as const) {
      expect(groupClosedVerdictAccepts(v)).toBe(false);
    }
  });
});

describe("sniffGroupClosed over the real codec", () => {
  it("recognises a notice with and without a successor", () => {
    const withSuccessor = sniffGroupClosed(realWasm, realWasm.composeGroupClosedEnvelope(5n, SUCCESSOR));
    expect(withSuccessor).toEqual({ kind: "closed", value: { closedAtMs: 5n, successorGroupIdB64: SUCCESSOR_B64 } });
    const without = sniffGroupClosed(realWasm, realWasm.composeGroupClosedEnvelope(6n, undefined));
    expect(without).toEqual({ kind: "closed", value: { closedAtMs: 6n, successorGroupIdB64: null } });
  });

  it("a chat envelope, a group name and a damaged notice are none / none / malformed", () => {
    expect(sniffGroupClosed(realWasm, new TextEncoder().encode('{"kind":"chat"}'))).toEqual({ kind: "none" });
    expect(sniffGroupClosed(realWasm, realWasm.composeGroupNameEnvelope("Trio", 1n))).toEqual({ kind: "none" });
    const good = realWasm.composeGroupClosedEnvelope(5n, SUCCESSOR);
    const bad = new Uint8Array(good.length + 1);
    bad.set(good, 0);
    expect(sniffGroupClosed(realWasm, bad)).toEqual({ kind: "malformed" });
  });
});

describe("group-closed row codec", () => {
  it("round-trips, keeping the u64 stamp exact and a null successor", () => {
    const rows = new Map<string, StoredGroupClosed>([
      ["g1", { closedAtMs: 1_700_000_000_123n, successorGroupIdB64: SUCCESSOR_B64 }],
      ["g2", { closedAtMs: 18_446_744_073_709_551_615n, successorGroupIdB64: null }],
    ]);
    const decoded = decodeGroupClosedRow(encodeGroupClosedRow(rows));
    expect([...(decoded ?? [])]).toEqual([...rows]);
  });

  it("drops a malformed entry alone and refuses a foreign schema", () => {
    const decoded = decodeGroupClosedRow(
      JSON.stringify({ v: 1, g: { ok: { t: "5", s: null }, bad: { t: "x", s: null }, worse: { t: "5", s: 3 } } }),
    );
    expect([...(decoded ?? [])]).toEqual([["ok", { closedAtMs: 5n, successorGroupIdB64: null }]]);
    expect(decodeGroupClosedRow(JSON.stringify({ v: 2, g: {} }))).toBeNull();
    expect(decodeGroupClosedRow("not json")).toBeNull();
  });
});

describe("GroupClosedStore", () => {
  function backend(): GroupClosedBackend & { saved: ReadonlyMap<string, StoredGroupClosed>[] } {
    const saved: ReadonlyMap<string, StoredGroupClosed>[] = [];
    return {
      saved,
      load: async () => new Map([["g1", stored]]),
      save: async (_account, rows) => {
        saved.push(rows);
      },
    };
  }

  it("opens the account's row, writes through, and forgets on request", async () => {
    const b = backend();
    const store = new GroupClosedStore(b);
    await store.open("acct");
    expect(store.get("g1")).toEqual(stored);
    await store.set("acct", "g1", incoming);
    expect(b.saved.at(-1)?.get("g1")).toEqual(incoming);
    await store.remove("acct", "g1");
    expect(store.get("g1")).toBeUndefined();
    expect(b.saved.at(-1)?.has("g1")).toBe(false);
  });

  it("a write for another account is kept in memory but not persisted under the open one", async () => {
    const b = backend();
    const store = new GroupClosedStore(b);
    await store.open("acct");
    await store.set("other", "g9", incoming);
    expect(b.saved).toHaveLength(0);
  });
});
