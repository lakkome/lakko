// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0101 section 7 - a channel taken back with its recovery key keeps
 *  every reader the service listed at the recovery; invitations, the
 *  returned-invitation list. */
import { describe, expect, it } from "vitest";

import {
  clearRecoveredChannel,
  highestSeq,
  markRecoveredChannel,
  readRecoveredChannels,
  recoveredVisibility,
  refoundAdmittedThrough,
} from "./recovered-channels";
import { dropReturnedInvitations, invitationsToShow, keepReturnedInvitation, readReturnedInvitations } from "./returned-invitations";

function memory(): { getItem(k: string): string | null; setItem(k: string, v: string): void; removeItem(k: string): void } {
  const m = new Map<string, string>();
  return { getItem: (k) => m.get(k) ?? null, setItem: (k, v) => void m.set(k, v), removeItem: (k) => void m.delete(k) };
}

describe("the recovery mark", () => {
  it("a re-founding keeps a recovered channel's readers, and judges a lost one from the start", () => {
    expect(refoundAdmittedThrough({ throughSeq: 42 })).toBe(42);
    expect(refoundAdmittedThrough(undefined)).toBe(0);
  });
  it("is the highest listed row, and only ever rises", () => {
    expect(highestSeq([{ seq: 3 }, { seq: 9 }, { seq: 1 }])).toBe(9);
    expect(highestSeq([], 5)).toBe(5);
    const s = memory();
    markRecoveredChannel("a", "c", 9, s);
    markRecoveredChannel("a", "c", 4, s);
    expect(readRecoveredChannels("a", s)).toEqual({ c: { throughSeq: 9 } });
    clearRecoveredChannel("a", "c", s);
    expect(readRecoveredChannels("a", s)).toEqual({});
  });
  it("the type comes from the newest record, none while none is held", () => {
    expect(recoveredVisibility(true)).toBe("public");
    expect(recoveredVisibility(false)).toBe("private");
    expect(recoveredVisibility(undefined)).toBeNull();
  });
});

describe("returned invitations", () => {
  const C1 = "11".repeat(16);
  const C2 = "22".repeat(16);
  it("are shown until the account reads the channel, never dropped otherwise", () => {
    const s = memory();
    keepReturnedInvitation("a", C2, "card2", s);
    keepReturnedInvitation("a", C1, "card1", s);
    keepReturnedInvitation("a", C1, "card1b", s);
    const held = readReturnedInvitations("a", s);
    expect(invitationsToShow(held, new Set())).toEqual([
      { channelIdHex: C1, cardB64: "card1b" },
      { channelIdHex: C2, cardB64: "card2" },
    ]);
    expect(invitationsToShow(held, new Set([C1])).map((i) => i.channelIdHex)).toEqual([C2]);
    dropReturnedInvitations("a", [C1], s);
    expect(Object.keys(readReturnedInvitations("a", s))).toEqual([C2]);
    expect(() => keepReturnedInvitation("a", C1, "x", undefined)).toThrow();
  });
});
