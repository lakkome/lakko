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

// Stand-in signature keys. Both registries key on the member's
// identity, never on its MLS leaf — see the module doc.
const OWN = "own-key";
const ALICE = "alice-key";
const BOB = "bob-key";

describe("foldTyping — receive (ADR-0024)", () => {
  it("marks a member typing on a fresh true, stamping the receive time", () => {
    const now = 1_000_000;
    const m = foldTyping(EMPTY_TYPING(), ALICE, true, now, now);
    expect(m.get(ALICE)?.lastTrueAtMs).toBe(now);
  });

  it("clears a member on an explicit false", () => {
    const now = 1_000_000;
    let m = foldTyping(EMPTY_TYPING(), ALICE, true, now, now);
    m = foldTyping(m, ALICE, false, now, now);
    expect(m.has(ALICE)).toBe(false);
  });

  it("false on an absent member is a no-op (=== input)", () => {
    const prev = EMPTY_TYPING();
    const next = foldTyping(prev, ALICE, false, 1, 1);
    expect(next).toBe(prev);
  });

  it("STALE-IGNORES a true whose createdAt is older than the window (re-sync guard)", () => {
    const now = 1_000_000;
    // createdAt is exactly the expiry window old → dropped.
    const prev = EMPTY_TYPING();
    const next = foldTyping(prev, ALICE, true, now - TYPING_EXPIRY_MS, now);
    expect(next).toBe(prev);
    expect(next.has(ALICE)).toBe(false);
  });

  it("accepts a true that is just inside the window", () => {
    const now = 1_000_000;
    const m = foldTyping(
      EMPTY_TYPING(),
      ALICE,
      true,
      now - (TYPING_EXPIRY_MS - 1),
      now,
    );
    expect(m.has(ALICE)).toBe(true);
  });

  it("does not mutate the previous map (replace-to-rerender)", () => {
    const prev = EMPTY_TYPING();
    const next = foldTyping(prev, ALICE, true, 5, 5);
    expect(prev.has(ALICE)).toBe(false);
    expect(next).not.toBe(prev);
  });

  it("tracks distinct members independently", () => {
    let m = foldTyping(EMPTY_TYPING(), ALICE, true, 10, 10);
    m = foldTyping(m, BOB, true, 10, 10);
    m = foldTyping(m, ALICE, false, 10, 10);
    expect(m.has(ALICE)).toBe(false);
    expect(m.has(BOB)).toBe(true);
  });
});

describe("activeTypers / pruneTyping — auto-expiry", () => {
  it("lists members whose last true is within the window", () => {
    const t0 = 1_000_000;
    let m = foldTyping(EMPTY_TYPING(), ALICE, true, t0, t0);
    m = foldTyping(m, BOB, true, t0, t0);
    expect(activeTypers(m, t0 + 1).sort()).toEqual([ALICE, BOB].sort());
  });

  it("drops a member from activeTypers once its true ages out", () => {
    const t0 = 1_000_000;
    const m = foldTyping(EMPTY_TYPING(), ALICE, true, t0, t0);
    expect(activeTypers(m, t0 + TYPING_EXPIRY_MS)).toEqual([]);
  });

  it("pruneTyping removes expired entries, returning a new map", () => {
    const t0 = 1_000_000;
    let m = foldTyping(EMPTY_TYPING(), ALICE, true, t0, t0);
    m = foldTyping(m, BOB, true, t0 + 5_000, t0 + 5_000);
    const pruned = pruneTyping(m, t0 + TYPING_EXPIRY_MS);
    // Alice expired (t0 + 6000 >= t0 + 6000), Bob still fresh.
    expect(pruned.has(ALICE)).toBe(false);
    expect(pruned.has(BOB)).toBe(true);
    expect(pruned).not.toBe(m);
  });

  it("pruneTyping is === the input when nothing expired", () => {
    const t0 = 1_000_000;
    const m = foldTyping(EMPTY_TYPING(), ALICE, true, t0, t0);
    expect(pruneTyping(m, t0 + 1)).toBe(m);
  });
});

describe("foldReadWatermark — monotone (ADR-0024)", () => {
  it("sets the watermark for a new member", () => {
    const m = foldReadWatermark(EMPTY_READ(), ALICE, 10);
    expect(m.get(ALICE)).toBe(10);
  });

  it("advances on a higher value", () => {
    let m = foldReadWatermark(EMPTY_READ(), ALICE, 10);
    m = foldReadWatermark(m, ALICE, 20);
    expect(m.get(ALICE)).toBe(20);
  });

  it("NEVER lowers on a smaller value (re-sync idempotent)", () => {
    let m = foldReadWatermark(EMPTY_READ(), ALICE, 20);
    const same = foldReadWatermark(m, ALICE, 10);
    expect(same).toBe(m);
    expect(same.get(ALICE)).toBe(20);
  });

  it("an equal value is a no-op (=== input)", () => {
    const m = foldReadWatermark(EMPTY_READ(), ALICE, 20);
    expect(foldReadWatermark(m, ALICE, 20)).toBe(m);
  });

  it("tracks distinct members independently", () => {
    let m = foldReadWatermark(EMPTY_READ(), ALICE, 20);
    m = foldReadWatermark(m, BOB, 5);
    expect(m.get(ALICE)).toBe(20);
    expect(m.get(BOB)).toBe(5);
  });
});

describe("isReadByEveryOther — own-message read state", () => {
  it("is false when no other member has reported a read", () => {
    expect(isReadByEveryOther(EMPTY_READ(), 5, OWN)).toBe(false);
  });

  it("1x1: true once the single other member's watermark >= the message", () => {
    const m = foldReadWatermark(EMPTY_READ(), ALICE, 5);
    expect(isReadByEveryOther(m, 5, OWN)).toBe(true);
    expect(isReadByEveryOther(m, 6, OWN)).toBe(false);
  });

  it("group: requires EVERY other member >= the message", () => {
    let m = foldReadWatermark(EMPTY_READ(), ALICE, 10);
    m = foldReadWatermark(m, BOB, 4);
    // Bob only read up to 4 → message 5 not read by all.
    expect(isReadByEveryOther(m, 5, OWN)).toBe(false);
    // once Bob catches up:
    m = foldReadWatermark(m, BOB, 5);
    expect(isReadByEveryOther(m, 5, OWN)).toBe(true);
  });

  it("excludes ourselves from the all-read check", () => {
    // Only our own watermark is present — no OTHER member → false.
    const m = foldReadWatermark(EMPTY_READ(), OWN, 99);
    expect(isReadByEveryOther(m, 5, OWN)).toBe(false);
  });

  it("an unresolvable own identity excludes nobody (stricter, not looser)", () => {
    const m = foldReadWatermark(EMPTY_READ(), OWN, 99);
    // Our own watermark now counts as "another member", which can
    // only ever make the answer harder to satisfy.
    expect(isReadByEveryOther(m, 100, undefined)).toBe(false);
    expect(isReadByEveryOther(m, 99, undefined)).toBe(true);
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
