// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  decodeSyncState,
  encodeSyncState,
  isEmptySyncState,
  SYNC_STATE_VERSION,
  type SyncStateSnapshot,
} from "./sync-state-store";

const EMPTY: SyncStateSnapshot = { drafts: {}, flags: {}, readCursors: {}, order: null };

const full: SyncStateSnapshot = {
  drafts: {
    aa: { text: "half a sentence", replyToId: new Uint8Array([1, 2, 3]), updatedAtHlc: 7n },
    bb: { text: "", replyToId: new Uint8Array(0), updatedAtHlc: 1n },
  },
  flags: {
    aa: {
      pinned: true,
      muted: false,
      archived: false,
      unreadOverride: true,
      updatedAtHlc: 9n,
    },
  },
  readCursors: {
    aa: {
      upToMessageId: new Uint8Array([9, 9]),
      readAtMs: 1_700_000_000_000n,
      updatedAtHlc: 11n,
    },
  },
  order: { groupIds: [new Uint8Array([1]), new Uint8Array([2])], updatedAtHlc: 13n },
};

describe("encode / decode", () => {
  it("round-trips everything a reload used to lose", () => {
    const back = decodeSyncState(encodeSyncState(full));
    expect(back).toEqual(full);
  });

  it("keeps the HLC exactly, because it is what decides who wins", () => {
    // The restored snapshot is folded against the journal by HLC. A stamp
    // that came back as a float, or rounded, would let a stale sibling write
    // overwrite a newer local one.
    const big = 2n ** 63n - 1n;
    const back = decodeSyncState(
      encodeSyncState({
        ...EMPTY,
        flags: {
          aa: {
            pinned: true,
            muted: false,
            archived: false,
            unreadOverride: false,
            updatedAtHlc: big,
          },
        },
      }),
    );
    expect(back?.flags.aa?.updatedAtHlc).toBe(big);
  });

  it("round-trips an empty snapshot", () => {
    expect(decodeSyncState(encodeSyncState(EMPTY))).toEqual(EMPTY);
  });

  it("refuses a payload that is not this schema", () => {
    expect(decodeSyncState("not json")).toBeNull();
    expect(decodeSyncState("null")).toBeNull();
    expect(decodeSyncState(JSON.stringify({ v: SYNC_STATE_VERSION + 1 }))).toBeNull();
  });

  it("tolerates a payload missing whole sections", () => {
    // A row written by a build that carried fewer maps reads as empty ones
    // rather than as no row at all.
    expect(decodeSyncState(JSON.stringify({ v: SYNC_STATE_VERSION }))).toEqual(EMPTY);
  });
});

describe("a malformed entry", () => {
  it("is dropped without taking the rest of the snapshot with it", () => {
    // Losing one chat's pin is better than losing every chat's, and the poll
    // can refill a single entry from a sibling's next write.
    const wire = JSON.parse(encodeSyncState(full)) as Record<string, unknown>;
    (wire["f"] as Record<string, unknown>)["broken"] = { p: "yes", h: "1" };
    const back = decodeSyncState(JSON.stringify(wire));
    expect(back?.flags.broken).toBeUndefined();
    expect(back?.flags.aa?.pinned).toBe(true);
    expect(Object.keys(back?.drafts ?? {})).toHaveLength(2);
  });

  it("drops a draft whose HLC is not a number, and only that draft", () => {
    // Asserting the survivor as well as the casualty: a decoder that gave up
    // on the whole snapshot would satisfy the first half alone.
    const wire = JSON.parse(encodeSyncState(full)) as {
      d: Record<string, Record<string, unknown>>;
    };
    const aa = wire.d["aa"];
    if (aa === undefined) throw new Error("fixture lost its draft");
    aa["h"] = "later";
    const back = decodeSyncState(JSON.stringify(wire));
    expect(back?.drafts.aa).toBeUndefined();
    expect(back?.drafts.bb?.text).toBe("");
    expect(back?.flags.aa?.pinned).toBe(true);
  });

  it("drops the WHOLE order when one id is unreadable", () => {
    // The order is one value, not a map: keeping the readable ids would
    // silently reorder the chat list rather than leave it alone.
    const wire = JSON.parse(encodeSyncState(full)) as Record<string, unknown>;
    (wire["o"] as { g: unknown[] }).g[1] = "!!!not base64!!!";
    expect(decodeSyncState(JSON.stringify(wire))?.order).toBeNull();
  });
});

describe("isEmptySyncState", () => {
  it("is true for a snapshot with nothing in it", () => {
    expect(isEmptySyncState(EMPTY)).toBe(true);
  });

  it("is false when any one map holds something", () => {
    expect(isEmptySyncState(full)).toBe(false);
    expect(isEmptySyncState({ ...EMPTY, order: full.order })).toBe(false);
    expect(isEmptySyncState({ ...EMPTY, drafts: full.drafts })).toBe(false);
  });
});
