// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";
import type { AcceptedContact } from "../contacts/local-store";
import { channelInviteCandidates, inChannelKeys } from "./invite-candidates";

function contact(key: string, previous?: string[]): AcceptedContact {
  return {
    signaturePubkeyB64: key,
    ...(previous === undefined ? {} : { previousSignaturePubkeysB64: previous }),
  } as unknown as AcceptedContact;
}

describe("channelInviteCandidates (UX 14.1, 15.2)", () => {
  it("does not offer a person who publishes the channel", () => {
    // Base walk 2026-11-13: the owner's Invite people listed a publisher.
    const accepted = [contact("pub"), contact("anna")];
    const inChannel = inChannelKeys({ ownKeyB64: "me", publisherKeysB64: ["me", "pub"] });
    expect(channelInviteCandidates(accepted, inChannel).map((c) => c.signaturePubkeyB64)).toEqual(["anna"]);
  });

  it("does not offer a subscriber known by their card", () => {
    const inChannel = inChannelKeys({ ownKeyB64: null, publisherKeysB64: [], memberKeysB64: ["sub", undefined, "self"] });
    expect(channelInviteCandidates([contact("sub"), contact("x")], inChannel).map((c) => c.signaturePubkeyB64)).toEqual(["x"]);
    expect(inChannel.has("self")).toBe(false);
  });

  it("matches a contact by an earlier key too", () => {
    const inChannel = inChannelKeys({ ownKeyB64: null, publisherKeysB64: ["old"] });
    expect(channelInviteCandidates([contact("new", ["old"])], inChannel)).toEqual([]);
  });

  it("offers every contact when nobody is known to be in", () => {
    const accepted = [contact("a"), contact("b")];
    expect(channelInviteCandidates(accepted, new Set())).toEqual(accepted);
  });
});
