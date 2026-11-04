// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, it, expect } from "vitest";

import init from "../../wasm/lakko_ffi_wasm";
import {
  applyOp,
  editDeleteAuthorized,
  type MessageOpState,
} from "./message-ops";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const EMPTY = (): Map<number, MessageOpState> => new Map();

// Stand-in signature keys. Real ones are base64 32-byte Ed25519 keys;
// what matters to these rules is only that distinct members carry
// distinct strings, and that no member's string is a leaf index.
const keyB64 = (n: number): string => btoa(String.fromCharCode(...new Uint8Array(32).fill(n)));
const ALICE = keyB64(1);
const BOB = keyB64(2);
const CAROL = keyB64(3);

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
    m = react(m, 3, "👍", BOB, "add", 10);
    m = applyOp(m, { op: "delete", target: 3 });
    expect(m.get(3)?.deleted).toBe(true);
    expect(m.get(3)?.editedText).toBe("hi");
    expect(m.get(3)?.reactions.get("👍")?.size).toBe(1);
  });
});

/** One reaction op: explicit add or remove at a journal position. */
function react(
  m: Map<number, MessageOpState>,
  target: number,
  emoji: string,
  reactor: string,
  action: "add" | "remove",
  at: number,
): Map<number, MessageOpState> {
  return applyOp(m, { op: "reaction", target, emoji, reactor, action, at });
}

// The fold is the core's (`foldReactions`): reactors are 32-byte account
// keys, base64.
describe("applyOp — reactions through the core fold", () => {
  it("adds a reaction from a member", () => {
    const m = react(EMPTY(), 5, "❤️", ALICE, "add", 1);
    expect(m.get(5)?.reactions.get("❤️")).toEqual(new Set([ALICE]));
  });

  it("an explicit remove takes it off; the empty emoji is pruned", () => {
    const a = react(EMPTY(), 5, "❤️", ALICE, "add", 1);
    const b = react(a, 5, "❤️", ALICE, "remove", 2);
    expect(b.get(5)?.reactions.has("❤️")).toBe(false);
  });

  it("a second add of a held emoji is a no-op, not a toggle", () => {
    const a = react(EMPTY(), 5, "❤️", ALICE, "add", 1);
    const b = react(a, 5, "❤️", ALICE, "add", 2);
    expect(b.get(5)?.reactions.get("❤️")).toEqual(new Set([ALICE]));
  });

  it("aggregates distinct members on one emoji (count)", () => {
    let m = react(EMPTY(), 9, "😂", ALICE, "add", 1);
    m = react(m, 9, "😂", BOB, "add", 2);
    m = react(m, 9, "😂", CAROL, "add", 3);
    expect(m.get(9)?.reactions.get("😂")?.size).toBe(3);
  });

  it("one member taking theirs off leaves the others intact", () => {
    let m = react(EMPTY(), 9, "😂", ALICE, "add", 1);
    m = react(m, 9, "😂", BOB, "add", 2);
    m = react(m, 9, "😂", ALICE, "remove", 3);
    expect(m.get(9)?.reactions.get("😂")).toEqual(new Set([BOB]));
  });

  it("a person holds at most three; a fourth add is ignored", () => {
    let m = react(EMPTY(), 4, "👍", ALICE, "add", 1);
    m = react(m, 4, "🙏", ALICE, "add", 2);
    m = react(m, 4, "😮", ALICE, "add", 3);
    m = react(m, 4, "😢", ALICE, "add", 4);
    expect(m.get(4)?.held.filter((h) => h.person === ALICE).length).toBe(3);
    expect(m.get(4)?.reactions.has("😢")).toBe(false);
  });

  it("does not mutate a prior map's nested reaction set", () => {
    const a = react(EMPTY(), 5, "❤️", ALICE, "add", 1);
    const before = a.get(5)?.reactions.get("❤️")?.size;
    react(a, 5, "❤️", BOB, "add", 2);
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
