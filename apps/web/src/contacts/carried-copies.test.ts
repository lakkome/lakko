// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  CARRIED_COPY_TTL_MS,
  decodeCarriedCopies,
  encodeCarriedCopies,
  matchArrivals,
  movedSlots,
  type CarriedCopy,
  type CarriedSlot,
} from "./carried-copies";

const NOW = 1_800_000_000_000;
const copy = (text: string, attachments = 0, heldAtMs = NOW - 1000): CarriedCopy => ({
  text,
  attachments: Array.from({ length: attachments }, () => ({ blobIdB64: "AA==" })),
  replyTo: null,
  heldAtMs,
});
const slot = (...copies: CarriedCopy[]): CarriedSlot => ({ copies, matched: [] });

describe("display-only copies are replaced by the real message", () => {
  it("an exact-text arrival removes one copy, the first in order", () => {
    const next = matchArrivals(slot(copy("hi"), copy("there"), copy("hi")), [
      { messageId: 10, text: "hi", attachmentCount: 0 },
    ], NOW);
    expect(next?.copies.map((c) => c.text)).toEqual(["there", "hi"]);
    expect(next?.matched).toEqual([10]);
  });

  it("a different text keeps the copy", () => {
    expect(matchArrivals(slot(copy("hi")), [{ messageId: 1, text: "hi!", attachmentCount: 0 }], NOW)).toBeNull();
  });

  it("a different number of attachments keeps the copy", () => {
    expect(matchArrivals(slot(copy("look", 1)), [{ messageId: 1, text: "look", attachmentCount: 0 }], NOW)).toBeNull();
    expect(
      matchArrivals(slot(copy("look", 1)), [{ messageId: 1, text: "look", attachmentCount: 1 }], NOW)?.copies,
    ).toEqual([]);
  });

  it("two identical texts match one copy each", () => {
    const first = matchArrivals(slot(copy("hi"), copy("hi")), [{ messageId: 1, text: "hi", attachmentCount: 0 }], NOW);
    expect(first?.copies).toHaveLength(1);
    const second = matchArrivals(first as CarriedSlot, [
      { messageId: 1, text: "hi", attachmentCount: 0 },
      { messageId: 2, text: "hi", attachmentCount: 0 },
    ], NOW);
    expect(second?.copies).toEqual([]);
    expect(second?.matched).toEqual([1, 2]);
  });

  it("an arrival already matched (a reload) never matches again", () => {
    const after = matchArrivals(slot(copy("hi"), copy("hi")), [{ messageId: 1, text: "hi", attachmentCount: 0 }], NOW);
    expect(matchArrivals(after as CarriedSlot, [{ messageId: 1, text: "hi", attachmentCount: 0 }], NOW)).toBeNull();
  });

  it("a copy unmatched for the lifetime is dropped", () => {
    const old = copy("stale", 0, NOW - CARRIED_COPY_TTL_MS);
    const next = matchArrivals(slot(old, copy("fresh")), [], NOW);
    expect(next?.copies.map((c) => c.text)).toEqual(["fresh"]);
  });

  it("the copies follow a pending contact into its conversation, in order", () => {
    const slots = { abcd: slot(copy("a"), copy("b")), ef: slot(copy("z")) };
    const moved = movedSlots(slots, "abcd", "ef");
    expect(moved?.["abcd"]).toBeUndefined();
    expect(moved?.["ef"]?.copies.map((c) => c.text)).toEqual(["z", "a", "b"]);
    expect(movedSlots(slots, "0000", "ef")).toBeNull();
  });

  it("the stored row round-trips and drops junk", () => {
    const slots = { abcd: { copies: [copy("a", 1)], matched: [3] } };
    expect(decodeCarriedCopies(encodeCarriedCopies(slots))).toEqual(slots);
    expect(decodeCarriedCopies(JSON.stringify({ v: 2, slots }))).toEqual({});
    expect(decodeCarriedCopies(JSON.stringify({ v: 1, slots: { "not hex": slot(copy("a")) } }))).toEqual({});
  });
});
