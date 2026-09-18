// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import {
  encryptionSheetSentence,
  safetyNumberGroups,
  safetyNumberSpokenLabel,
  showsEncryptionChip,
} from "./encryption-chip";

describe("showsEncryptionChip", () => {
  it("shows the chip for an SFU group call, which is what the old gate hid", () => {
    // `calls/sfu-call-ui.ts` publishes exactly this on `connected`. Under the
    // replaced `props.embedded !== true` gate this rendered on no live call,
    // because `Chat.tsx` always mounts the call embedded.
    expect(showsEncryptionChip({ active: true, shortFingerprint: "3701 9284 5510 6623" })).toBe(
      true,
    );
  });

  it("hides it for the 1x1 path, which has no SFrame and no number", () => {
    // `calls/CallHostContext.tsx` publishes exactly this. The old comment's
    // worry — a "secured" chip over a call with no safety number — is closed
    // here, by the data rather than by a layout flag.
    expect(showsEncryptionChip({ active: false, shortFingerprint: null })).toBe(false);
  });

  it("hides it while keying is armed but the number is not derived yet", () => {
    // The chip promises a code to compare; opening its sheet onto nothing is
    // worse than not offering it.
    expect(showsEncryptionChip({ active: true, shortFingerprint: null })).toBe(false);
    expect(showsEncryptionChip({ active: true, shortFingerprint: "   " })).toBe(false);
  });

  it("hides it when a number is present but encryption is not active", () => {
    // Should not occur; if it does, the honest answer is not to claim
    // protection.
    expect(showsEncryptionChip({ active: false, shortFingerprint: "3701 9284" })).toBe(false);
  });

  it("does not consult anything about the surface asking", () => {
    // The regression guard. The bug was a layout flag answering a
    // cryptographic question; this rule takes no such input, so it cannot
    // regress that way without changing the signature — a reviewable edit.
    expect(showsEncryptionChip.length).toBe(1);
  });
});

describe("encryptionSheetSentence", () => {
  it("names the other person", () => {
    expect(encryptionSheetSentence("Anna")).toBe("Only you and Anna can hear this call.");
  });

  it("trims a padded name rather than embedding the padding", () => {
    expect(encryptionSheetSentence("  Anna  ")).toBe("Only you and Anna can hear this call.");
  });

  it("says the same true thing without a name when there is none", () => {
    // Word for word what `callEncryptionSentence` says on Android and iOS.
    for (const missing of [null, undefined, "", "   "]) {
      expect(encryptionSheetSentence(missing)).toBe("Only the people on this call can hear it.");
    }
  });

  it("does not present the roster's unresolved placeholders as a person", () => {
    // `calls/CallHostContext.tsx` falls back to "Peer" and `routes/Call.tsx`
    // to "Participant N" when a peer cannot be resolved to a contact. Reading
    // "Only you and Peer can hear this call." as a name is exactly the kind of
    // invented fact the call surface must not state.
    expect(encryptionSheetSentence("Peer")).toBe("Only the people on this call can hear it.");
    expect(encryptionSheetSentence("Participant")).toBe(
      "Only the people on this call can hear it.",
    );
  });
});

describe("safetyNumberGroups", () => {
  it("splits the canonical four-groups-of-four the core emits", () => {
    // `lakko_calls::safety_number::call_safety_number` joins four four-digit
    // groups with single spaces.
    expect(safetyNumberGroups("3701 9284 5510 6623")).toEqual([
      "3701",
      "9284",
      "5510",
      "6623",
    ]);
  });

  it("survives irregular spacing instead of rendering one long run", () => {
    expect(safetyNumberGroups("  3701\t9284   5510 6623 ")).toEqual([
      "3701",
      "9284",
      "5510",
      "6623",
    ]);
  });

  it("re-groups an unspaced value rather than rendering one unreadable run", () => {
    // The failure this whole change exists to remove. Matches
    // `safetyNumberGroups` on Android, which re-chunks for the same reason.
    expect(safetyNumberGroups("3701928455106623")).toEqual([
      "3701",
      "9284",
      "5510",
      "6623",
    ]);
  });

  it("has nothing to show for an absent number", () => {
    expect(safetyNumberGroups(null)).toEqual([]);
    expect(safetyNumberGroups(undefined)).toEqual([]);
    expect(safetyNumberGroups("   ")).toEqual([]);
  });
});

describe("safetyNumberSpokenLabel", () => {
  it("pauses between groups the way a person reading it aloud would", () => {
    expect(safetyNumberSpokenLabel("3701 9284 5510 6623")).toBe(
      "Safety number: 3701, 9284, 5510, 6623",
    );
  });

  it("says so when there is no number, rather than reading an empty label", () => {
    expect(safetyNumberSpokenLabel(null)).toBe("Safety number unavailable");
  });
});
