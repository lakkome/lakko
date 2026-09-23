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
import { describe, expect, it } from "vitest";

import { encodeSyncState } from "../devices/sync/sync-state-store";
import type { FlagsState } from "../devices/sync/state";
// @ts-expect-error — plain JS ESM in public/, no .d.ts; typed via JSDoc.
import { mutedFromSyncStateRow } from "../../public/sw-push-decrypt.js";

const GROUP_HEX = "ab".repeat(32);
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
  const json = encodeSyncState({ drafts: {}, flags: state, readCursors: {}, order: null });
  return mutedFromSyncStateRow(JSON.parse(json), groupHex) as boolean;
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
    expect(mutedFromSyncStateRow({ v: 2, f: { [GROUP_HEX]: { m: true } } }, GROUP_HEX)).toBe(
      false,
    );
  });

  it("anything that is not a payload", () => {
    for (const bad of [null, undefined, "", 0, [], { v: 1 }, { v: 1, f: null }]) {
      expect(mutedFromSyncStateRow(bad, GROUP_HEX)).toBe(false);
    }
  });

  it("a flag that is not a boolean", () => {
    // Never truthiness: a string or a number in that slot means the row is
    // not what this reader thinks it is.
    for (const m of ["true", 1, {}, null]) {
      expect(mutedFromSyncStateRow({ v: 1, f: { [GROUP_HEX]: { m } } }, GROUP_HEX)).toBe(false);
    }
  });
});
