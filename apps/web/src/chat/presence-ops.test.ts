// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, it, expect } from "vitest";
import {
  foldTyping,
  activeTypers,
  pruneTyping,
  foldReadWatermark,
  isReadByEveryOther,
  shouldSendTyping,
  shouldSendReadReceipt,
  TYPING_EXPIRY_MS,
  TYPING_SEND_THROTTLE_MS,
  type TypingMap,
  type ReadWatermarkMap,
} from "./presence-ops";

const EMPTY_TYPING = (): TypingMap => new Map();
const EMPTY_READ = (): ReadWatermarkMap => new Map();

describe("foldTyping — receive (ADR-0024)", () => {
  it("marks a leaf typing on a fresh true, stamping the receive time", () => {
    const now = 1_000_000;
    const m = foldTyping(EMPTY_TYPING(), 3, true, now, now);
    expect(m.get(3)?.lastTrueAtMs).toBe(now);
  });

  it("clears a leaf on an explicit false", () => {
    const now = 1_000_000;
    let m = foldTyping(EMPTY_TYPING(), 3, true, now, now);
    m = foldTyping(m, 3, false, now, now);
    expect(m.has(3)).toBe(false);
  });

  it("false on an absent leaf is a no-op (=== input)", () => {
    const prev = EMPTY_TYPING();
    const next = foldTyping(prev, 3, false, 1, 1);
    expect(next).toBe(prev);
  });

  it("STALE-IGNORES a true whose createdAt is older than the window (re-sync guard)", () => {
    const now = 1_000_000;
    // createdAt is exactly the expiry window old → dropped.
    const prev = EMPTY_TYPING();
    const next = foldTyping(prev, 3, true, now - TYPING_EXPIRY_MS, now);
    expect(next).toBe(prev);
    expect(next.has(3)).toBe(false);
  });

  it("accepts a true that is just inside the window", () => {
    const now = 1_000_000;
    const m = foldTyping(EMPTY_TYPING(), 3, true, now - (TYPING_EXPIRY_MS - 1), now);
    expect(m.has(3)).toBe(true);
  });

  it("does not mutate the previous map (replace-to-rerender)", () => {
    const prev = EMPTY_TYPING();
    const next = foldTyping(prev, 1, true, 5, 5);
    expect(prev.has(1)).toBe(false);
    expect(next).not.toBe(prev);
  });

  it("treats leaf 0 as a real leaf", () => {
    const m = foldTyping(EMPTY_TYPING(), 0, true, 10, 10);
    expect(m.get(0)?.lastTrueAtMs).toBe(10);
  });
});

describe("activeTypers / pruneTyping — auto-expiry", () => {
  it("lists leaves whose last true is within the window", () => {
    const t0 = 1_000_000;
    let m = foldTyping(EMPTY_TYPING(), 2, true, t0, t0);
    m = foldTyping(m, 5, true, t0, t0);
    expect(activeTypers(m, t0 + 1).sort()).toEqual([2, 5]);
  });

  it("drops a leaf from activeTypers once its true ages out", () => {
    const t0 = 1_000_000;
    const m = foldTyping(EMPTY_TYPING(), 2, true, t0, t0);
    expect(activeTypers(m, t0 + TYPING_EXPIRY_MS)).toEqual([]);
  });

  it("pruneTyping removes expired entries, returning a new map", () => {
    const t0 = 1_000_000;
    let m = foldTyping(EMPTY_TYPING(), 2, true, t0, t0);
    m = foldTyping(m, 5, true, t0 + 5_000, t0 + 5_000);
    const pruned = pruneTyping(m, t0 + TYPING_EXPIRY_MS);
    // leaf 2 expired (t0 + 6000 >= t0 + 6000), leaf 5 still fresh.
    expect(pruned.has(2)).toBe(false);
    expect(pruned.has(5)).toBe(true);
    expect(pruned).not.toBe(m);
  });

  it("pruneTyping is === the input when nothing expired", () => {
    const t0 = 1_000_000;
    const m = foldTyping(EMPTY_TYPING(), 2, true, t0, t0);
    expect(pruneTyping(m, t0 + 1)).toBe(m);
  });
});

describe("foldReadWatermark — monotone (ADR-0024)", () => {
  it("sets the watermark for a new leaf", () => {
    const m = foldReadWatermark(EMPTY_READ(), 4, 10);
    expect(m.get(4)).toBe(10);
  });

  it("advances on a higher value", () => {
    let m = foldReadWatermark(EMPTY_READ(), 4, 10);
    m = foldReadWatermark(m, 4, 20);
    expect(m.get(4)).toBe(20);
  });

  it("NEVER lowers on a smaller value (re-sync idempotent)", () => {
    let m = foldReadWatermark(EMPTY_READ(), 4, 20);
    const same = foldReadWatermark(m, 4, 10);
    expect(same).toBe(m);
    expect(same.get(4)).toBe(20);
  });

  it("an equal value is a no-op (=== input)", () => {
    const m = foldReadWatermark(EMPTY_READ(), 4, 20);
    expect(foldReadWatermark(m, 4, 20)).toBe(m);
  });

  it("tracks distinct leaves independently", () => {
    let m = foldReadWatermark(EMPTY_READ(), 4, 20);
    m = foldReadWatermark(m, 7, 5);
    expect(m.get(4)).toBe(20);
    expect(m.get(7)).toBe(5);
  });
});

describe("isReadByEveryOther — own-message read state", () => {
  it("is false when no other leaf has reported a read", () => {
    expect(isReadByEveryOther(EMPTY_READ(), 5, 1)).toBe(false);
  });

  it("1x1: true once the single other leaf's watermark >= the message", () => {
    const m = foldReadWatermark(EMPTY_READ(), 2, 5);
    expect(isReadByEveryOther(m, 5, 1)).toBe(true);
    expect(isReadByEveryOther(m, 6, 1)).toBe(false);
  });

  it("group: requires EVERY other leaf >= the message", () => {
    let m = foldReadWatermark(EMPTY_READ(), 2, 10);
    m = foldReadWatermark(m, 3, 4);
    // leaf 3 only read up to 4 → message 5 not read by all.
    expect(isReadByEveryOther(m, 5, 1)).toBe(false);
    // once leaf 3 catches up:
    m = foldReadWatermark(m, 3, 5);
    expect(isReadByEveryOther(m, 5, 1)).toBe(true);
  });

  it("excludes our own leaf from the all-read check", () => {
    // Only our own leaf (1) has a watermark — no OTHER leaf → false.
    const m = foldReadWatermark(EMPTY_READ(), 1, 99);
    expect(isReadByEveryOther(m, 5, 1)).toBe(false);
  });
});

describe("shouldSendTyping — send throttle", () => {
  it("sends the first true while editing", () => {
    expect(shouldSendTyping(true, null, 1000)).toBe(true);
  });

  it("does not send while not editing", () => {
    expect(shouldSendTyping(false, null, 1000)).toBe(false);
  });

  it("throttles a second true within the window", () => {
    const last = 1000;
    expect(shouldSendTyping(true, last, last + TYPING_SEND_THROTTLE_MS - 1)).toBe(
      false,
    );
  });

  it("sends again once the throttle window elapses", () => {
    const last = 1000;
    expect(shouldSendTyping(true, last, last + TYPING_SEND_THROTTLE_MS)).toBe(true);
  });
});

describe("shouldSendReadReceipt — monotone send", () => {
  it("null when there is no server-assigned message", () => {
    expect(shouldSendReadReceipt(null, null)).toBe(null);
  });

  it("sends the highest id when never published before", () => {
    expect(shouldSendReadReceipt(12, null)).toBe(12);
  });

  it("sends when the highest id advances past the last published", () => {
    expect(shouldSendReadReceipt(20, 12)).toBe(20);
  });

  it("does NOT resend when the highest id has not advanced", () => {
    expect(shouldSendReadReceipt(12, 12)).toBe(null);
    expect(shouldSendReadReceipt(8, 12)).toBe(null);
  });
});
