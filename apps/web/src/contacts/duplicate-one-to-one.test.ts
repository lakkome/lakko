// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { duplicateOneToOneFolds, type HeldConversationGroup } from "./duplicate-one-to-one";

const ME = "me-key";
const VG3 = "vg3-key";
const ANNA = "anna-key";

function chat(groupHex: string, founder: string | null, peer = VG3, conversationHex = groupHex): HeldConversationGroup {
  return { groupHex, conversationHex, isOneToOne: true, people: [ME, peer], founderKeyB64: founder };
}

describe("duplicateOneToOneFolds", () => {
  it("continues the one-to-one this account founded with the one the person founded", () => {
    // The desktop founded aa (VG3 never opened it); VG3 founded bb in answer
    // to the desktop's link answer (seen live 2026-11-07).
    expect(duplicateOneToOneFolds([chat("aa", ME), chat("bb", VG3)], ME)).toEqual([
      { successorHex: "bb", rootHex: "aa" },
    ]);
  });

  it("is order-independent and writes nothing once folded", () => {
    expect(duplicateOneToOneFolds([chat("bb", VG3), chat("aa", ME)], ME)).toEqual([
      { successorHex: "bb", rootHex: "aa" },
    ]);
    expect(duplicateOneToOneFolds([chat("aa", ME), chat("bb", VG3, VG3, "aa")], ME)).toEqual([]);
  });

  it("both sides head the group the other founded when both founded at once", () => {
    // Side ME holds aa (own) and bb (VG3's); side VG3 holds the same pair.
    expect(duplicateOneToOneFolds([chat("aa", ME), chat("bb", VG3)], ME)).toEqual([
      { successorHex: "bb", rootHex: "aa" },
    ]);
    const theirs = (g: string, f: string): HeldConversationGroup => ({
      groupHex: g,
      conversationHex: g,
      isOneToOne: true,
      people: [VG3, ME],
      founderKeyB64: f,
    });
    expect(duplicateOneToOneFolds([theirs("aa", ME), theirs("bb", VG3)], VG3)).toEqual([
      { successorHex: "aa", rootHex: "bb" },
    ]);
  });

  it("leaves one-to-ones with different people apart", () => {
    expect(duplicateOneToOneFolds([chat("aa", ME, ANNA), chat("bb", VG3)], ME)).toEqual([]);
  });

  it("does nothing when both were founded by the same side, or the founder is unknown", () => {
    expect(duplicateOneToOneFolds([chat("aa", ME), chat("bb", ME)], ME)).toEqual([]);
    expect(duplicateOneToOneFolds([chat("aa", VG3), chat("bb", VG3)], ME)).toEqual([]);
    expect(duplicateOneToOneFolds([chat("aa", null), chat("bb", VG3)], ME)).toEqual([]);
    expect(duplicateOneToOneFolds([chat("aa", ME), chat("bb", VG3)], null)).toEqual([]);
  });

  it("never folds a group, nor into one", () => {
    const group: HeldConversationGroup = {
      groupHex: "aa",
      conversationHex: "aa",
      isOneToOne: false,
      people: [ME, VG3],
      founderKeyB64: ME,
    };
    expect(duplicateOneToOneFolds([group, chat("bb", VG3)], ME)).toEqual([]);
  });

  it("does not fold a group that already continues or is continued", () => {
    expect(duplicateOneToOneFolds([chat("aa", ME), chat("bb", VG3, VG3, "cc"), chat("cc", VG3)], ME)).toEqual([]);
  });

  it("picks the same root on every device when several own one-to-ones exist", () => {
    expect(duplicateOneToOneFolds([chat("cc", ME), chat("aa", ME), chat("bb", VG3)], ME)).toEqual([
      { successorHex: "bb", rootHex: "aa" },
    ]);
  });
});
