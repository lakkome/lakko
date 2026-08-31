// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, it, expect } from "vitest";
import {
  applyOp,
  editDeleteAuthorized,
  OWN_REACTION_SENTINEL,
  type MessageOpState,
} from "./message-ops";

const EMPTY = (): Map<number, MessageOpState> => new Map();

// Stand-in signature keys. Real ones are base64 32-byte Ed25519 keys;
// what matters to these rules is only that distinct members carry
// distinct strings, and that no member's string is a leaf index.
const ALICE = "alice-key";
const BOB = "bob-key";
const CAROL = "carol-key";

describe("applyOp — edit", () => {
  it("sets editedText on the target", () => {
    const next = applyOp(EMPTY(), { op: "edit", target: 7, text: "new" });
    expect(next.get(7)?.editedText).toBe("new");
  });

  it("overwrites a prior edit (LWW on the single journal)", () => {
    const a = applyOp(EMPTY(), { op: "edit", target: 7, text: "v1" });
    const b = applyOp(a, { op: "edit", target: 7, text: "v2" });
    expect(b.get(7)?.editedText).toBe("v2");
  });

  it("does not mutate the previous map (replace-to-rerender)", () => {
    const prev = EMPTY();
    const next = applyOp(prev, { op: "edit", target: 1, text: "x" });
    expect(prev.has(1)).toBe(false);
    expect(next).not.toBe(prev);
  });
});

describe("applyOp — delete", () => {
  it("tombstones the target", () => {
    const next = applyOp(EMPTY(), { op: "delete", target: 3 });
    expect(next.get(3)?.deleted).toBe(true);
  });

  it("preserves an existing edit + reactions when deleting", () => {
    let m = applyOp(EMPTY(), { op: "edit", target: 3, text: "hi" });
    m = applyOp(m, {
      op: "reaction",
      target: 3,
      emoji: "👍",
      reactor: BOB,
    });
    m = applyOp(m, { op: "delete", target: 3 });
    expect(m.get(3)?.deleted).toBe(true);
    expect(m.get(3)?.editedText).toBe("hi");
    expect(m.get(3)?.reactions.get("👍")?.size).toBe(1);
  });
});

describe("applyOp — reaction toggle", () => {
  it("adds a reaction from a member", () => {
    const m = applyOp(EMPTY(), {
      op: "reaction",
      target: 5,
      emoji: "❤️",
      reactor: ALICE,
    });
    expect(m.get(5)?.reactions.get("❤️")).toEqual(new Set([ALICE]));
  });

  it("toggles the same (member, emoji) off, pruning the empty set", () => {
    const a = applyOp(EMPTY(), {
      op: "reaction",
      target: 5,
      emoji: "❤️",
      reactor: ALICE,
    });
    const b = applyOp(a, {
      op: "reaction",
      target: 5,
      emoji: "❤️",
      reactor: ALICE,
    });
    // The emoji entry is gone once its last reactor toggles off.
    expect(b.get(5)?.reactions.has("❤️")).toBe(false);
  });

  it("aggregates distinct members on one emoji (count)", () => {
    let m = applyOp(EMPTY(), {
      op: "reaction",
      target: 9,
      emoji: "😂",
      reactor: ALICE,
    });
    m = applyOp(m, { op: "reaction", target: 9, emoji: "😂", reactor: BOB });
    m = applyOp(m, { op: "reaction", target: 9, emoji: "😂", reactor: CAROL });
    expect(m.get(9)?.reactions.get("😂")?.size).toBe(3);
  });

  it("one member toggling off leaves the others intact", () => {
    let m = applyOp(EMPTY(), {
      op: "reaction",
      target: 9,
      emoji: "😂",
      reactor: ALICE,
    });
    m = applyOp(m, { op: "reaction", target: 9, emoji: "😂", reactor: BOB });
    m = applyOp(m, { op: "reaction", target: 9, emoji: "😂", reactor: ALICE });
    expect(m.get(9)?.reactions.get("😂")).toEqual(new Set([BOB]));
  });

  it("keeps distinct emojis separate on the same message", () => {
    let m = applyOp(EMPTY(), {
      op: "reaction",
      target: 4,
      emoji: "👍",
      reactor: ALICE,
    });
    m = applyOp(m, { op: "reaction", target: 4, emoji: "🙏", reactor: ALICE });
    expect(m.get(4)?.reactions.get("👍")?.size).toBe(1);
    expect(m.get(4)?.reactions.get("🙏")?.size).toBe(1);
  });

  it("supports the own-reaction sentinel identity", () => {
    const m = applyOp(EMPTY(), {
      op: "reaction",
      target: 4,
      emoji: "😮",
      reactor: OWN_REACTION_SENTINEL,
    });
    expect(m.get(4)?.reactions.get("😮")?.has(OWN_REACTION_SENTINEL)).toBe(true);
  });

  it("does not mutate a prior map's nested reaction set", () => {
    const a = applyOp(EMPTY(), {
      op: "reaction",
      target: 5,
      emoji: "❤️",
      reactor: ALICE,
    });
    const before = a.get(5)?.reactions.get("❤️")?.size;
    applyOp(a, { op: "reaction", target: 5, emoji: "❤️", reactor: BOB });
    // `a` is unchanged — the second apply cloned the nested set.
    expect(a.get(5)?.reactions.get("❤️")?.size).toBe(before);
    expect(a.get(5)?.reactions.get("❤️")?.has(BOB)).toBe(false);
  });
});

describe("editDeleteAuthorized — author-only gate (invariant #4)", () => {
  it("accepts when the op sender is the message author", () => {
    expect(editDeleteAuthorized(BOB, BOB)).toBe(true);
  });

  it("REFUSES a non-author editing/deleting another's message", () => {
    expect(editDeleteAuthorized(BOB, CAROL)).toBe(false);
  });

  it("fails closed when the author is unknown (pre-v2 stored row)", () => {
    expect(editDeleteAuthorized(undefined, BOB)).toBe(false);
  });

  it("fails closed when the op sender is unknown (control frame)", () => {
    expect(editDeleteAuthorized(BOB, undefined)).toBe(false);
  });

  it("fails closed when both are unknown", () => {
    expect(editDeleteAuthorized(undefined, undefined)).toBe(false);
  });

  it("does not falsy-coerce an empty identity into a match", () => {
    // Not reachable from a real roster (a key is 44 base64 chars),
    // but the old numeric gate had a leaf-0 hazard and the
    // replacement must not have its string analogue.
    expect(editDeleteAuthorized("", undefined)).toBe(false);
    expect(editDeleteAuthorized(undefined, "")).toBe(false);
  });
});
