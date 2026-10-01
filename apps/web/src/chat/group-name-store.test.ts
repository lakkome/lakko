// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0051 §2 — the receive-side verdict matrix for a shared group name, the
 * row codec, and the store's write-through.
 *
 * The order (`isNewer`) is the wasm core's; here it is the same strict rule
 * spelled out inline so the matrix can be pinned without loading the module.
 */
import { describe, expect, it } from "vitest";

import { sniffGroupName } from "./group-name-envelope";
import {
  GroupNameStore,
  decodeGroupNameRow,
  encodeGroupNameRow,
  groupNameErrorText,
  groupNameVerdict,
  groupNameVerdictAccepts,
  type GroupNameBackend,
  type StoredGroupName,
} from "./group-name-store";

const OWNER = new Uint8Array(32).fill(0x01);
const ADMIN = new Uint8Array(32).fill(0x02);
const MEMBER = new Uint8Array(32).fill(0x03);

const isNewer = (a: bigint, b: bigint): boolean => a > b;

function verdict(over: Partial<Parameters<typeof groupNameVerdict>[0]>): ReturnType<typeof groupNameVerdict> {
  return groupNameVerdict({
    kind: "group",
    ownerKey: OWNER,
    adminKeys: [ADMIN],
    senderKey: OWNER,
    stored: { name: "Trio", updatedAtMs: 100n },
    storedUnreadable: false,
    incoming: { name: "Quartet", updatedAtMs: 200n },
    verifyFailed: false,
    isNewer,
    ...over,
  });
}

describe("groupNameVerdict — the ADR-0051 §2 matrix", () => {
  it("a one-to-one drops every name, even the owner's and an identical one (ADR-0062 §7)", () => {
    expect(verdict({ kind: "chat" })).toBe("reject-one-to-one");
    expect(verdict({ kind: "chat", incoming: { name: "Trio", updatedAtMs: 100n } })).toBe(
      "reject-one-to-one",
    );
    expect(verdict({ kind: "chat", ownerKey: undefined })).toBe("reject-one-to-one");
    expect(groupNameVerdictAccepts("reject-one-to-one")).toBe(false);
  });

  it("an identical name and stamp is accepted without authority or order", () => {
    const v = verdict({
      senderKey: MEMBER,
      incoming: { name: "Trio", updatedAtMs: 100n },
      verifyFailed: true,
    });
    expect(v).toBe("accept-identical");
  });

  it("the owner may rename when newer", () => {
    expect(verdict({})).toBe("accept-owner");
  });

  it("an admin may rename when newer", () => {
    expect(verdict({ senderKey: ADMIN })).toBe("accept-admin");
  });

  it("a plain member is refused, whatever the stamp", () => {
    expect(verdict({ senderKey: MEMBER })).toBe("reject-not-authorized");
    expect(verdict({ senderKey: MEMBER, stored: null })).toBe("reject-not-authorized");
  });

  it("no role table: any member may rename when newer", () => {
    expect(verdict({ ownerKey: undefined, adminKeys: [], senderKey: MEMBER })).toBe(
      "accept-no-owner",
    );
  });

  it("an unresolvable sender is refused when the group has a role table", () => {
    expect(verdict({ senderKey: undefined })).toBe("reject-unverifiable");
  });

  it("an unreadable role table is refused even for the owner", () => {
    expect(verdict({ verifyFailed: true })).toBe("reject-unverifiable");
  });

  it("an unreadable local row is refused: nothing to order against", () => {
    expect(verdict({ stored: null, storedUnreadable: true })).toBe("reject-unverifiable");
  });

  it("authority runs before the order: a member's newer name is not 'stale'", () => {
    expect(verdict({ senderKey: MEMBER, incoming: { name: "X", updatedAtMs: 50n } })).toBe(
      "reject-not-authorized",
    );
  });

  it("a growth re-send of an older name never clobbers a later rename", () => {
    expect(verdict({ incoming: { name: "Old", updatedAtMs: 50n } })).toBe("reject-stale");
  });

  it("equal stamps keep what is stored (equal is not newer)", () => {
    expect(verdict({ incoming: { name: "Other", updatedAtMs: 100n } })).toBe("reject-stale");
  });

  it("nothing stored: the first authorised name is accepted at any stamp", () => {
    expect(verdict({ stored: null, incoming: { name: "First", updatedAtMs: 1n } })).toBe(
      "accept-owner",
    );
  });

  it("an empty name (a clear) is an ordinary newer write", () => {
    expect(verdict({ incoming: { name: "", updatedAtMs: 300n } })).toBe("accept-owner");
  });

  it("verdictAccepts is exactly the accept-* set", () => {
    expect(groupNameVerdictAccepts("accept-identical")).toBe(true);
    expect(groupNameVerdictAccepts("accept-owner")).toBe(true);
    expect(groupNameVerdictAccepts("accept-admin")).toBe(true);
    expect(groupNameVerdictAccepts("accept-no-owner")).toBe(true);
    expect(groupNameVerdictAccepts("reject-not-authorized")).toBe(false);
    expect(groupNameVerdictAccepts("reject-unverifiable")).toBe(false);
    expect(groupNameVerdictAccepts("reject-stale")).toBe(false);
  });
});

describe("group-name row codec", () => {
  it("round-trips, keeping the u64 stamp exact", () => {
    const rows = new Map<string, StoredGroupName>([
      ["g1", { name: "Trio", updatedAtMs: 1_700_000_000_123n }],
      ["g2", { name: "", updatedAtMs: 18_446_744_073_709_551_615n }],
    ]);
    const decoded = decodeGroupNameRow(encodeGroupNameRow(rows));
    expect(decoded).not.toBeNull();
    expect([...(decoded ?? [])]).toEqual([...rows]);
  });

  it("drops a malformed entry alone and refuses a foreign schema", () => {
    const decoded = decodeGroupNameRow(
      JSON.stringify({ v: 1, g: { ok: { n: "A", t: "5" }, bad: { n: 3, t: "5" }, worse: { n: "B", t: "-1" } } }),
    );
    expect([...(decoded ?? [])]).toEqual([["ok", { name: "A", updatedAtMs: 5n }]]);
    expect(decodeGroupNameRow(JSON.stringify({ v: 2, g: {} }))).toBeNull();
    expect(decodeGroupNameRow("not json")).toBeNull();
  });
});

describe("GroupNameStore", () => {
  function backend(): GroupNameBackend & { saved: ReadonlyMap<string, StoredGroupName>[] } {
    const saved: ReadonlyMap<string, StoredGroupName>[] = [];
    return {
      saved,
      load: async () => new Map([["g1", { name: "Stored", updatedAtMs: 10n }]]),
      save: async (_account, rows) => {
        saved.push(rows);
      },
    };
  }

  it("opens the account's row, reads a cleared name as none, and writes through", async () => {
    const b = backend();
    const store = new GroupNameStore(b);
    await store.open("acct");
    expect(store.nameOf("g1")).toBe("Stored");
    await store.set("acct", "g1", { name: "", updatedAtMs: 20n });
    expect(store.nameOf("g1")).toBeNull();
    expect(store.get("g1")).toEqual({ name: "", updatedAtMs: 20n });
    expect(b.saved.at(-1)?.get("g1")).toEqual({ name: "", updatedAtMs: 20n });
  });

  it("a write for another account is kept in memory but not persisted under the open one", async () => {
    const b = backend();
    const store = new GroupNameStore(b);
    await store.open("acct");
    await store.set("other", "g9", { name: "X", updatedAtMs: 1n });
    expect(b.saved).toHaveLength(0);
  });

  it("notifies subscribers and empties on close", async () => {
    const store = new GroupNameStore(backend());
    let ticks = 0;
    store.subscribe(() => {
      ticks += 1;
    });
    await store.open("acct");
    store.close();
    expect(ticks).toBe(2);
    expect(store.nameOf("g1")).toBeNull();
  });
});

describe("sniffGroupName", () => {
  const decoder = (behaviour: "ok" | "wrongDomain" | "tooLong" | "plain") => ({
    decodeGroupNameEnvelope: (): { name: string; updatedAtMs: bigint } => {
      if (behaviour === "ok") return { name: "Trio", updatedAtMs: 7n };
      if (behaviour === "plain") throw new Error("no code");
      throw Object.assign(new Error(behaviour), { groupNameError: behaviour });
    },
  });
  const bytes = new Uint8Array([0x6c, 0x61]);

  it("skips anything that does not start like a service envelope", () => {
    expect(sniffGroupName(decoder("ok"), new Uint8Array([0x7b]))).toEqual({ kind: "none" });
  });
  it("classifies a decodable envelope", () => {
    expect(sniffGroupName(decoder("ok"), bytes)).toEqual({
      kind: "name",
      value: { name: "Trio", updatedAtMs: 7n },
    });
  });
  it("wrongDomain and a code-less throw are 'none'; any other code is 'malformed'", () => {
    expect(sniffGroupName(decoder("wrongDomain"), bytes)).toEqual({ kind: "none" });
    expect(sniffGroupName(decoder("plain"), bytes)).toEqual({ kind: "none" });
    expect(sniffGroupName(decoder("tooLong"), bytes)).toEqual({ kind: "malformed" });
  });
});

describe("groupNameErrorText", () => {
  it("has plain copy for every composer code and a fallback", () => {
    expect(groupNameErrorText("name.tooLong")).toBe("The name is too long.");
    expect(groupNameErrorText("name.lineBreak")).toBe("The name must be one line.");
    expect(groupNameErrorText("name.controlOrFormatCharacter")).toContain("cannot be shown");
    expect(groupNameErrorText(undefined)).toBe("That name cannot be used.");
  });
});
