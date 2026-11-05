// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { conversationWithContact } from "./contact-conversation";

interface Conv {
  readonly groupIdB64: string;
  readonly people: readonly string[];
  readonly oneToOne: boolean;
}

const ME = "me";
const ANNA = "anna";

function find(contact: { signaturePubkeyB64: string; groupIdB64?: string }, conversations: Conv[], own: string | null = ME) {
  return conversationWithContact({
    contact,
    conversations,
    ownKeyB64: own,
    peopleOf: (c) => c.people,
    isOneToOne: (c) => c.oneToOne,
  });
}

describe("conversationWithContact", () => {
  it("answers the conversation linked on the contact while the registry holds it", () => {
    const linked: Conv = { groupIdB64: "g1", people: [ME, ANNA], oneToOne: true };
    expect(find({ signaturePubkeyB64: ANNA, groupIdB64: "g1" }, [linked])).toBe(linked);
  });

  it("does not treat a linked id whose chat is gone as an address", () => {
    expect(find({ signaturePubkeyB64: ANNA, groupIdB64: "gone" }, [])).toBeUndefined();
  });

  it("falls back to the one-to-one whose two people are this account and the contact", () => {
    const other: Conv = { groupIdB64: "g0", people: [ME, "boris"], oneToOne: true };
    const mine: Conv = { groupIdB64: "g2", people: [ANNA, ME], oneToOne: true };
    expect(find({ signaturePubkeyB64: ANNA, groupIdB64: "gone" }, [other, mine])).toBe(mine);
    expect(find({ signaturePubkeyB64: ANNA }, [other, mine])).toBe(mine);
  });

  it("never answers a group that happens to hold the same two people", () => {
    // Writing there is not writing to the contact.
    const group: Conv = { groupIdB64: "g3", people: [ME, ANNA], oneToOne: false };
    expect(find({ signaturePubkeyB64: ANNA }, [group])).toBeUndefined();
  });

  it("never answers a one-to-one with someone else, or one this account is not in", () => {
    expect(find({ signaturePubkeyB64: ANNA }, [{ groupIdB64: "g", people: [ME, "boris"], oneToOne: true }])).toBeUndefined();
    expect(find({ signaturePubkeyB64: ANNA }, [{ groupIdB64: "g", people: [ANNA, "boris"], oneToOne: true }])).toBeUndefined();
  });

  it("reads a torn conversation as no match", () => {
    expect(find({ signaturePubkeyB64: ANNA }, [{ groupIdB64: "g", people: [], oneToOne: true }])).toBeUndefined();
  });

  it("answers only the linked conversation before a session knows its own key", () => {
    const conv: Conv = { groupIdB64: "g1", people: [ME, ANNA], oneToOne: true };
    expect(find({ signaturePubkeyB64: ANNA }, [conv], null)).toBeUndefined();
    expect(find({ signaturePubkeyB64: ANNA, groupIdB64: "g1" }, [conv], null)).toBe(conv);
  });
});

describe("a stored chat id that is not the person's one-to-one does not count", () => {
  it("a link to a group (it held two people once) falls back to the real one-to-one, or to none", () => {
    const group: Conv = { groupIdB64: "walk", people: [ME, ANNA, "boris"], oneToOne: false };
    const real: Conv = { groupIdB64: "g2", people: [ME, ANNA], oneToOne: true };
    expect(find({ signaturePubkeyB64: ANNA, groupIdB64: "walk" }, [group])).toBeUndefined();
    expect(find({ signaturePubkeyB64: ANNA, groupIdB64: "walk" }, [group, real])).toBe(real);
    // Two people but a group: still not their chat.
    const pair: Conv = { groupIdB64: "pair", people: [ME, ANNA], oneToOne: false };
    expect(find({ signaturePubkeyB64: ANNA, groupIdB64: "pair" }, [pair])).toBeUndefined();
  });

  it("a link to a one-to-one with someone else does not count", () => {
    const boris: Conv = { groupIdB64: "gb", people: [ME, "boris"], oneToOne: true };
    expect(find({ signaturePubkeyB64: ANNA, groupIdB64: "gb" }, [boris])).toBeUndefined();
  });

  it("an older one-to-one under a key the contact rotated from is still theirs", () => {
    const old: Conv = { groupIdB64: "old", people: [ME, "anna-old"], oneToOne: true };
    expect(
      conversationWithContact({
        contact: { signaturePubkeyB64: ANNA, groupIdB64: "old", previousSignaturePubkeysB64: ["anna-old"] },
        conversations: [old],
        ownKeyB64: ME,
        peopleOf: (c) => c.people,
        isOneToOne: (c) => c.oneToOne,
      }),
    ).toBe(old);
  });
});

