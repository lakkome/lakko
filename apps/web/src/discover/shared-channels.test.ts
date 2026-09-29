// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as realWasm from "../../wasm/lakko_ffi_wasm";
import {
  aggregateShared,
  applyIncomingList,
  channelsToShare,
  decodeSharedChannelsRow,
  emptySharedChannelsRow,
  encodeSharedChannelsRow,
  freshListStamp,
  sameChannels,
  sniffSharedChannels,
  type SharedList,
} from "./shared-channels";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const ID_A = "aa".repeat(16);
const ID_B = "bb".repeat(16);
const isNewer = (a: bigint, b: bigint): boolean => realWasm.sharedChannelsIsNewer(a, b);
const list = (stamp: bigint, ...ids: string[]): SharedList => ({
  updatedAtMs: stamp,
  channels: ids.map((id) => ({ channelIdHex: id, handle: `h_${id.slice(0, 4)}`, name: `N ${id.slice(0, 2)}` })),
});

describe("the sniff, against the real core codec", () => {
  it("recognises a composed list and keeps its entries in order", () => {
    const ids = new Uint8Array(32);
    ids.fill(0xaa, 0, 16);
    ids.fill(0xbb, 16, 32);
    const bytes = realWasm.composeSharedChannelsEnvelope(7n, ids, ["alpha", "beta"], ["Alpha", "Beta"]);
    const s = sniffSharedChannels(realWasm, bytes);
    expect(s.kind).toBe("list");
    if (s.kind !== "list") return;
    expect(s.value.updatedAtMs).toBe(7n);
    expect(s.value.channels.map((c) => c.channelIdHex)).toEqual([ID_A, ID_B]);
    expect(s.value.channels[1]?.handle).toBe("beta");
  });

  it("drops a damaged list and ignores what is not one", () => {
    const good = realWasm.composeSharedChannelsEnvelope(7n, new Uint8Array(0), [], []);
    const bad = new Uint8Array(good.length + 1);
    bad.set(good, 0);
    expect(sniffSharedChannels(realWasm, bad).kind).toBe("malformed");
    expect(sniffSharedChannels(realWasm, new TextEncoder().encode("lunch at six?")).kind).toBe("none");
    expect(sniffSharedChannels(realWasm, new TextEncoder().encode('{"kind":"chat"}')).kind).toBe("none");
  });
});

describe("the store's ordering", () => {
  it("files per member, strictly newer wins, equal is not newer", () => {
    let row = emptySharedChannelsRow();
    let r = applyIncomingList(row, "g", "alice", list(10n, ID_A), isNewer);
    expect(r.verdict).toBe("accept");
    row = r.row;
    r = applyIncomingList(row, "g", "alice", list(10n, ID_B), isNewer);
    expect(r.verdict).toBe("reject-stale");
    r = applyIncomingList(row, "g", "alice", list(9n, ID_B), isNewer);
    expect(r.verdict).toBe("reject-stale");
    r = applyIncomingList(row, "g", "alice", list(11n), isNewer);
    expect(r.verdict).toBe("accept");
    expect(r.row.received.get("g")?.get("alice")?.channels).toEqual([]);
    // Another member's list is its own, whatever its stamp.
    r = applyIncomingList(r.row, "g", "bob", list(1n, ID_A), isNewer);
    expect(r.verdict).toBe("accept");
  });

  it("refuses a list it cannot attribute", () => {
    expect(applyIncomingList(emptySharedChannelsRow(), "g", undefined, list(1n, ID_A), isNewer).verdict).toBe(
      "reject-unattributed",
    );
  });

  it("stamps a sent list after the previous one even when the clock went back", () => {
    expect(freshListStamp(100, undefined)).toBe(100n);
    expect(freshListStamp(100, list(500n))).toBe(501n);
    expect(freshListStamp(900, list(500n))).toBe(900n);
  });

  it("round-trips through the sealed row codec", () => {
    let row = applyIncomingList(emptySharedChannelsRow(), "g", "alice", list(10n, ID_A, ID_B), isNewer).row;
    row = { ...row, sent: new Map([["g", list(3n, ID_A)]]), shareOn: new Set(["g"]) };
    const back = decodeSharedChannelsRow(encodeSharedChannelsRow(row));
    expect(back.received.get("g")?.get("alice")).toEqual(list(10n, ID_A, ID_B));
    expect(back.sent.get("g")).toEqual(list(3n, ID_A));
    expect([...back.shareOn]).toEqual(["g"]);
    expect(decodeSharedChannelsRow("not json").shareOn.size).toBe(0);
  });
});

describe("what is shared", () => {
  it("only subscribed, listed channels with a handle", () => {
    const out = channelsToShare([
      { channelIdHex: ID_B, subscribed: true, listed: true, handle: "beta", name: " Beta " },
      { channelIdHex: ID_A, subscribed: true, listed: true, handle: "alpha", name: "Alpha" },
      { channelIdHex: "cc".repeat(16), subscribed: true, listed: false, handle: "private", name: "P" },
      { channelIdHex: "dd".repeat(16), subscribed: false, listed: true, handle: "mine", name: "M" },
      { channelIdHex: "ee".repeat(16), subscribed: true, listed: true, handle: undefined, name: "E" },
    ]);
    expect(out.map((c) => c.channelIdHex)).toEqual([ID_A, ID_B]);
    expect(out[1]?.name).toBe("Beta");
    expect(sameChannels(out, [...out])).toBe(true);
    expect(sameChannels(out, out.slice(1))).toBe(false);
  });

  it("aggregates across groups, skipping this person and groups no longer held", () => {
    let row = emptySharedChannelsRow();
    row = applyIncomingList(row, "g1", "alice", list(1n, ID_A), isNewer).row;
    row = applyIncomingList(row, "g1", "bob", list(1n, ID_A, ID_B), isNewer).row;
    row = applyIncomingList(row, "g1", "me", list(1n, ID_B), isNewer).row;
    row = applyIncomingList(row, "g2", "carol", list(1n, ID_B), isNewer).row;
    const out = aggregateShared(row, "me", new Set(["g1"]));
    expect(out.map((c) => c.channelIdHex)).toEqual([ID_A, ID_B]);
    expect(out[0]?.byGroup.get("g1")).toEqual(["alice", "bob"]);
    expect(out[1]?.byGroup.get("g1")).toEqual(["bob"]);
    expect(out[1]?.byGroup.has("g2")).toBe(false);
  });
});
