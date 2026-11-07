// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0087 section 13a on the web client: the reaction rules (cap, drawn
 * counts, batching), label resolution against the real wasm core, and the ban
 * list fold.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  channelTapVerdict,
  countBatches,
  drawnCounts,
  foldBanList,
  joinAuthorKeys,
  nextBanStamp,
  resolveLabelCounts,
  splitAuthorKeys,
  withBanned,
  withoutBanned,
} from "./feedback-rules";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

describe("own reactions on a post", () => {
  it("takes off what the person holds, even at the cap", () => {
    expect(channelTapVerdict("👍", ["👍", "❤️", "😂"], 3, true)).toBe("remove");
  });
  it("refuses a fourth", () => {
    expect(channelTapVerdict("🙏", ["👍", "❤️", "😂"], 3, true)).toBe("cap-reached");
  });
  it("adds under the cap, and never what the channel does not take", () => {
    expect(channelTapVerdict("🙏", ["👍"], 3, true)).toBe("add");
    expect(channelTapVerdict("🙏", ["👍"], 3, false)).toBe("not-allowed");
  });
  it("matches the core's cap", () => {
    expect(wasm.channelReactionsPerPostMax()).toBe(3);
  });
});

describe("drawn counts", () => {
  it("adds a write the counts do not show yet, and never shows an own chip at zero", () => {
    const resolved = new Map([["👍", 4]]);
    const out = drawnCounts(resolved, ["❤️"], new Map([["❤️", 1], ["👍", 1]]));
    expect(out.get("👍")).toBe(5);
    expect(out.get("❤️")).toBe(1);
  });
  it("takes a chip away when a take-off empties it", () => {
    const out = drawnCounts(new Map([["👍", 1]]), [], new Map([["👍", -1]]));
    expect(out.has("👍")).toBe(false);
  });
});

describe("count batches", () => {
  it("joins repeats and splits at fifty", () => {
    const cursors = Array.from({ length: 120 }, (_, i) => (i % 110) + 1);
    const batches = countBatches(cursors);
    expect(batches.map((b) => b.length)).toEqual([50, 50, 10]);
    expect(new Set(batches.flat()).size).toBe(110);
  });
  it("drops cursors that cannot name a post", () => {
    expect(countBatches([0, -1, 1.5, 7])).toEqual([[7]]);
  });
});

describe("label resolution through the core", () => {
  const seed = new Uint8Array(32).fill(0x42);
  const channelId = new Uint8Array(16).fill(0x11);

  it("resolves the labels it can produce and drops the rest", () => {
    const key = wasm.ChannelReactionKey.derive(seed, channelId, 3);
    const thumbs = hex(key.label(9n, "👍"));
    const heart = hex(key.label(9n, "❤️"));
    key.free();
    const labels = new Map([
      [thumbs, 5],
      [heart, 2],
      ["00".repeat(16), 7],
    ]);
    const out = resolveLabelCounts(wasm, seed, channelId, 3, 9, labels, ["😂", "❤️", "👍"]);
    expect(out.get("👍")).toBe(5);
    expect(out.get("❤️")).toBe(2);
    expect(out.size).toBe(2);
  });

  it("does not resolve a label of another post or another epoch", () => {
    const key = wasm.ChannelReactionKey.derive(seed, channelId, 3);
    const otherPost = hex(key.label(10n, "👍"));
    key.free();
    const other = wasm.ChannelReactionKey.derive(seed, channelId, 4);
    const otherEpoch = hex(other.label(9n, "👍"));
    other.free();
    const out = resolveLabelCounts(
      wasm,
      seed,
      channelId,
      3,
      9,
      new Map([
        [otherPost, 1],
        [otherEpoch, 1],
      ]),
      ["👍"],
    );
    expect(out.size).toBe(0);
  });
});

describe("the ban list", () => {
  const a = "aa".repeat(32);
  const b = "bb".repeat(32);
  const c = "cc".repeat(32);

  it("keeps the strictly newer list", () => {
    const older = { updatedAtMs: 10n, authors: [a] };
    const newer = { updatedAtMs: 11n, authors: [b] };
    const isNewer = (x: bigint, y: bigint): boolean => wasm.banListIsNewer(x, y);
    expect(foldBanList(older, newer, isNewer)).toBe(newer);
    expect(foldBanList(newer, older, isNewer)).toBe(newer);
    // A re-issue with the same stamp changes nothing.
    const same = { updatedAtMs: 11n, authors: [c] };
    expect(foldBanList(newer, same, isNewer)).toBe(newer);
  });

  it("appends a ban once and drops the oldest past the cap", () => {
    expect(withBanned([a], a.toUpperCase(), 10)).toEqual([a]);
    expect(withBanned([a, b], c, 2)).toEqual([b, c]);
    expect(withoutBanned([a, b], a)).toEqual([b]);
  });

  it("stamps strictly after the stored list", () => {
    expect(nextBanStamp(undefined, 5n)).toBe(5n);
    expect(nextBanStamp(9n, 5n)).toBe(10n);
    expect(nextBanStamp(3n, 5n)).toBe(5n);
  });

  it("round-trips through the core's ban list post", () => {
    const list = new wasm.BanList(77n, joinAuthorKeys([a, b]));
    const plaintext = wasm.composeBanList(list);
    list.free();
    const c2 = wasm.classifyChannelPostPlaintext(plaintext);
    expect(c2.kind).toBe("ban-list");
    const decoded = c2.banList;
    expect(decoded).toBeDefined();
    expect(decoded?.updatedAtMs).toBe(77n);
    expect(splitAuthorKeys(decoded?.authors ?? new Uint8Array())).toEqual([a, b]);
    decoded?.free();
    c2.free();
  });
});

describe("what is kept on disk", () => {
  const channel = "0f".repeat(16);
  const a = "aa".repeat(32);
  const b = "bb".repeat(32);

  it("round-trips the ban lists and keeps names only for banned authors", async () => {
    const { decodePersistedBanLists, encodePersistedBanLists } = await import("./feedback-rules");
    const encoded = encodePersistedBanLists(
      new Map([[channel, { updatedAtMs: 12345678901234567n, authors: [a] }]]),
      new Map([
        [a, "Anna"],
        [b, "Bo"],
      ]),
    );
    const json = JSON.parse(JSON.stringify(encoded)) as unknown;
    const back = decodePersistedBanLists(json);
    expect(back.lists.get(channel)).toEqual({ updatedAtMs: 12345678901234567n, authors: [a] });
    expect(back.names.get(a)).toBe("Anna");
    expect(back.names.has(b)).toBe(false);
  });

  it("leaves out what is malformed instead of throwing", async () => {
    const { decodePersistedBanLists } = await import("./feedback-rules");
    expect(decodePersistedBanLists(null).lists.size).toBe(0);
    const back = decodePersistedBanLists({
      lists: [
        { channel: "nothex", updatedAtMs: "1", authors: [a] },
        { channel, updatedAtMs: "x", authors: [a] },
        { channel, updatedAtMs: "5", authors: [a, "short", 7] },
      ],
      names: { [a]: 3, [b]: "Bo" },
    });
    expect(back.lists.get(channel)).toEqual({ updatedAtMs: 5n, authors: [a] });
    expect(back.names.get(b)).toBe("Bo");
    expect(back.names.has(a)).toBe(false);
  });

  it("reads back waiting reactions, once each", async () => {
    const { decodePersistedWaiting } = await import("./feedback-rules");
    const ok = { idHex: channel, cursor: 9, epoch: 2, emoji: "👍" };
    expect(
      decodePersistedWaiting([
        ok,
        ok,
        { ...ok, cursor: 0 },
        { ...ok, epoch: -1 },
        { ...ok, emoji: "" },
        { ...ok, idHex: "XY" },
        "junk",
      ]),
    ).toEqual([ok]);
    expect(decodePersistedWaiting({})).toEqual([]);
  });
});
