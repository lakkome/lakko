// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/calls.md` section 2.1 — the service worker's read of the muted set.
 *
 * The worker cannot import `src/*`, so `mutedFromSyncStateRow` is a hand-
 * rolled reader of a payload the APP writes through `encodeSyncState`. That
 * is a drift hazard of exactly the shape ADR-0044 was written about, and it
 * fails in the dangerous direction: a renamed field does not throw, it
 * silently reports every conversation as unmuted and the rule quietly stops
 * being implemented again.
 *
 * So these tests drive the real writer and read the result back, rather than
 * asserting a wire shape typed out by hand.
 *
 * What they do NOT cover is the storage plumbing around it — opening
 * `lakko-device-sync`, unwrapping under the device key — which lives in
 * `public/sw.js` and needs IndexedDB and WebCrypto that vitest's bare node
 * environment does not have.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { encodeSyncState } from "../devices/sync/sync-state-store";
import type { FlagsState } from "../devices/sync/state";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { conversationHexFromSyncStateRow, mutedFromSyncStateRow } from "../../public/sw-push-decrypt.js";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

const GROUP_HEX = "ab".repeat(32);

function hexBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i += 1) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}
const OTHER_HEX = "cd".repeat(32);

function flags(over: Partial<FlagsState> = {}): FlagsState {
  return {
    pinned: false,
    muted: false,
    archived: false,
    unreadOverride: false,
    updatedAtHlc: 7n,
    ...over,
  };
}

/** What the app actually stores, read back the way the worker reads it. */
function readBack(
  state: Record<string, FlagsState>,
  groupHex = GROUP_HEX,
): boolean {
  // The app folds its own writes into the Rust fold and stores its bytes.
  const s = new wasm.SyncedState();
  for (const [hex, f] of Object.entries(state)) {
    s.applyFlags(f.updatedAtHlc, hexBytes(hex), f.pinned, f.muted, f.archived, f.unreadOverride);
  }
  const json = encodeSyncState({ synced: s.toBytes(), pending: [] });
  return mutedFromSyncStateRow(JSON.parse(json), groupHex, wasm) as boolean;
}

describe("the worker's read of the app's muted set", () => {
  it("reads a mute the app wrote", () => {
    expect(readBack({ [GROUP_HEX]: flags({ muted: true }) })).toBe(true);
  });

  it("reads an unmuted conversation as unmuted", () => {
    expect(readBack({ [GROUP_HEX]: flags({ muted: false }) })).toBe(false);
  });

  it("does not confuse mute with the three flags stored beside it", () => {
    // They share one row and one last-write-wins struct, and `p` / `a` / `u`
    // sit next to `m` on the wire.
    expect(
      readBack({
        [GROUP_HEX]: flags({ pinned: true, archived: true, unreadOverride: true }),
      }),
    ).toBe(false);
  });

  it("answers for the conversation asked about, not a neighbour", () => {
    expect(
      readBack({ [OTHER_HEX]: flags({ muted: true }), [GROUP_HEX]: flags() }),
    ).toBe(false);
    expect(
      readBack({ [OTHER_HEX]: flags(), [GROUP_HEX]: flags({ muted: true }) }),
    ).toBe(true);
  });

  it("is not muted when the conversation has no flags row at all", () => {
    // The ordinary case for a chat nobody has ever pinned or muted.
    expect(readBack({})).toBe(false);
  });
});

describe("what the worker refuses to read as muted", () => {
  it("a payload from a schema this build does not know", () => {
    // Guessing at an unknown schema could silence calls on the strength of a
    // field that means something else now.
    // The pre-ADR-0058 row, whose flags were readable JSON, among them.
    expect(mutedFromSyncStateRow({ v: 1, f: { [GROUP_HEX]: { m: true } } }, GROUP_HEX, wasm)).toBe(
      false,
    );
    expect(mutedFromSyncStateRow({ v: 3, s: "AA==" }, GROUP_HEX, wasm)).toBe(false);
  });

  it("anything that is not a payload", () => {
    for (const bad of [null, undefined, "", 0, [], { v: 2 }, { v: 2, s: null }, { v: 2, s: "" }]) {
      expect(mutedFromSyncStateRow(bad, GROUP_HEX, wasm)).toBe(false);
    }
  });

  it("bytes the fold refuses", () => {
    expect(mutedFromSyncStateRow({ v: 2, s: btoa("not a fold") }, GROUP_HEX, wasm)).toBe(false);
  });

  it("a group id that is not hex, or no wasm at all", () => {
    const json = JSON.parse(
      encodeSyncState({ synced: new wasm.SyncedState().toBytes(), pending: [] }),
    );
    expect(mutedFromSyncStateRow(json, "zz", wasm)).toBe(false);
    expect(mutedFromSyncStateRow(json, GROUP_HEX, null)).toBe(false);
  });
});

describe("a successor group is its conversation (ADR-0065 R.5)", () => {
  const SUCCESSOR_HEX = "ef".repeat(32);

  /** The app's row: the conversation `GROUP_HEX` muted (or not), and
   *  `SUCCESSOR_HEX` registered as continuing it. */
  function row(muted: boolean): unknown {
    const s = new wasm.SyncedState();
    s.applyFlags(7n, hexBytes(GROUP_HEX), false, muted, false, false);
    expect(s.setLineage(9n, hexBytes(SUCCESSOR_HEX), hexBytes(GROUP_HEX))).toBe(true);
    return JSON.parse(encodeSyncState({ synced: s.toBytes(), pending: [] }));
  }

  it("a wake for the successor reads the mute its conversation carries", () => {
    expect(mutedFromSyncStateRow(row(true), SUCCESSOR_HEX, wasm)).toBe(true);
    expect(mutedFromSyncStateRow(row(false), SUCCESSOR_HEX, wasm)).toBe(false);
    // The first group still answers for itself.
    expect(mutedFromSyncStateRow(row(true), GROUP_HEX, wasm)).toBe(true);
  });

  it("the successor's notification is grouped under the conversation id", () => {
    expect(conversationHexFromSyncStateRow(row(false), SUCCESSOR_HEX, wasm)).toBe(GROUP_HEX);
    expect(conversationHexFromSyncStateRow(row(false), GROUP_HEX, wasm)).toBe(GROUP_HEX);
    // A group no register names is its own conversation.
    expect(conversationHexFromSyncStateRow(row(false), OTHER_HEX, wasm)).toBe(OTHER_HEX);
  });

  it("an unreadable row names no conversation", () => {
    for (const bad of [null, { v: 1 }, { v: 2, s: "" }, { v: 2, s: btoa("not a fold") }]) {
      expect(conversationHexFromSyncStateRow(bad, SUCCESSOR_HEX, wasm)).toBeNull();
    }
    expect(conversationHexFromSyncStateRow(row(false), "zz", wasm)).toBeNull();
  });
});
