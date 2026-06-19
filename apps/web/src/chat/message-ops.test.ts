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
      reactorLeaf: 2,
    });
    m = applyOp(m, { op: "delete", target: 3 });
    expect(m.get(3)?.deleted).toBe(true);
    expect(m.get(3)?.editedText).toBe("hi");
    expect(m.get(3)?.reactions.get("👍")?.size).toBe(1);
  });
});

describe("applyOp — reaction toggle", () => {
  it("adds a reaction from a leaf", () => {
    const m = applyOp(EMPTY(), {
      op: "reaction",
      target: 5,
      emoji: "❤️",
      reactorLeaf: 1,
    });
    expect(m.get(5)?.reactions.get("❤️")).toEqual(new Set([1]));
  });

  it("toggles the same (leaf, emoji) off, pruning the empty set", () => {
    const a = applyOp(EMPTY(), {
      op: "reaction",
      target: 5,
      emoji: "❤️",
      reactorLeaf: 1,
    });
    const b = applyOp(a, {
      op: "reaction",
      target: 5,
      emoji: "❤️",
      reactorLeaf: 1,
    });
    // The emoji entry is gone once its last reactor toggles off.
    expect(b.get(5)?.reactions.has("❤️")).toBe(false);
  });

  it("aggregates distinct leaves on one emoji (count)", () => {
    let m = applyOp(EMPTY(), {
      op: "reaction",
      target: 9,
      emoji: "😂",
      reactorLeaf: 1,
    });
    m = applyOp(m, { op: "reaction", target: 9, emoji: "😂", reactorLeaf: 2 });
    m = applyOp(m, { op: "reaction", target: 9, emoji: "😂", reactorLeaf: 3 });
    expect(m.get(9)?.reactions.get("😂")?.size).toBe(3);
  });

  it("one leaf toggling off leaves the others intact", () => {
    let m = applyOp(EMPTY(), {
      op: "reaction",
      target: 9,
      emoji: "😂",
      reactorLeaf: 1,
    });
    m = applyOp(m, { op: "reaction", target: 9, emoji: "😂", reactorLeaf: 2 });
    m = applyOp(m, { op: "reaction", target: 9, emoji: "😂", reactorLeaf: 1 });
    expect(m.get(9)?.reactions.get("😂")).toEqual(new Set([2]));
  });

  it("keeps distinct emojis separate on the same message", () => {
    let m = applyOp(EMPTY(), {
      op: "reaction",
      target: 4,
      emoji: "👍",
      reactorLeaf: 1,
    });
    m = applyOp(m, { op: "reaction", target: 4, emoji: "🙏", reactorLeaf: 1 });
    expect(m.get(4)?.reactions.get("👍")?.size).toBe(1);
    expect(m.get(4)?.reactions.get("🙏")?.size).toBe(1);
  });

  it("supports the own-reaction sentinel leaf", () => {
    const m = applyOp(EMPTY(), {
      op: "reaction",
      target: 4,
      emoji: "😮",
      reactorLeaf: OWN_REACTION_SENTINEL,
    });
    expect(m.get(4)?.reactions.get("😮")?.has(OWN_REACTION_SENTINEL)).toBe(true);
  });

  it("does not mutate a prior map's nested reaction set", () => {
    const a = applyOp(EMPTY(), {
      op: "reaction",
      target: 5,
      emoji: "❤️",
      reactorLeaf: 1,
    });
    const before = a.get(5)?.reactions.get("❤️")?.size;
    applyOp(a, { op: "reaction", target: 5, emoji: "❤️", reactorLeaf: 2 });
    // `a` is unchanged — the second apply cloned the nested set.
    expect(a.get(5)?.reactions.get("❤️")?.size).toBe(before);
    expect(a.get(5)?.reactions.get("❤️")?.has(2)).toBe(false);
  });
});

describe("editDeleteAuthorized — author-only gate (invariant #4)", () => {
  it("accepts when the op sender is the message author", () => {
    expect(editDeleteAuthorized(2, 2)).toBe(true);
  });

  it("REFUSES a non-author editing/deleting another's message", () => {
    expect(editDeleteAuthorized(2, 3)).toBe(false);
  });

  it("fails closed when the author leaf is unknown (reloaded msg)", () => {
    expect(editDeleteAuthorized(undefined, 2)).toBe(false);
  });

  it("fails closed when the op sender leaf is unknown (control frame)", () => {
    expect(editDeleteAuthorized(2, undefined)).toBe(false);
  });

  it("fails closed when both leaves are unknown", () => {
    expect(editDeleteAuthorized(undefined, undefined)).toBe(false);
  });

  it("treats leaf 0 as a real leaf (not falsy-coerced)", () => {
    expect(editDeleteAuthorized(0, 0)).toBe(true);
    expect(editDeleteAuthorized(0, 1)).toBe(false);
  });
});
