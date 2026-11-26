// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 3 - the member-card rules: what the own card says, when
 * it is uploaded again, what a card read from the list does to the kept
 * grants, and the Ed25519 flag-day checks.
 */
import { describe, expect, it } from "vitest";

import {
  cardContent,
  cardFingerprint,
  cardGrantDecision,
  cardUploadDue,
  isStaleSubscriberKey,
  memberRowName,
  ownCardPlan,
  sealEachSkippingInvalid,
} from "./member-card-rules";

describe("cardContent", () => {
  it("carries the username and the grant when the channel's members may find the owner", () => {
    expect(
      cardContent({ displayName: "Ada", username: "ada", channelAllowed: true, reachAvailable: true }),
    ).toEqual({ displayName: "Ada", username: "ada", includeGrant: true });
  });

  it("carries the name only when the setting does not allow this channel (UX 6.2)", () => {
    expect(
      cardContent({ displayName: "Ada", username: "ada", channelAllowed: false, reachAvailable: true }),
    ).toEqual({ displayName: "Ada", username: null, includeGrant: false });
  });

  it("carries no grant and no username without a reach session", () => {
    expect(
      cardContent({ displayName: "Ada", username: "ada", channelAllowed: true, reachAvailable: false }),
    ).toEqual({ displayName: "Ada", username: null, includeGrant: false });
  });

  it("reads a blank name as no name", () => {
    expect(
      cardContent({ displayName: "   ", username: null, channelAllowed: false, reachAvailable: true })
        .displayName,
    ).toBeNull();
  });
});

describe("cardUploadDue", () => {
  const content = cardContent({
    displayName: "Ada",
    username: "ada",
    channelAllowed: true,
    reachAvailable: true,
  });
  const fp = cardFingerprint(content, 100, 3);

  it("uploads when nothing was uploaded yet", () => {
    expect(cardUploadDue(undefined, 0, fp)).toBe(true);
  });

  it("uploads again after a rotation (a newcomer holds only current seeds)", () => {
    expect(cardUploadDue({ epoch: 2, fingerprint: fp }, 3, fp)).toBe(true);
  });

  it("does not upload the same card twice at the same epoch", () => {
    expect(cardUploadDue({ epoch: 3, fingerprint: fp }, 3, fp)).toBe(false);
  });

  it("uploads again at the same epoch when the content changed", () => {
    const renamed = cardFingerprint({ ...content, displayName: "Ada L." }, 200, 3);
    expect(cardUploadDue({ epoch: 3, fingerprint: fp }, 3, renamed)).toBe(true);
    const narrowed = cardFingerprint(
      cardContent({ displayName: "Ada", username: "ada", channelAllowed: false, reachAvailable: true }),
      100,
      null,
    );
    expect(cardUploadDue({ epoch: 3, fingerprint: fp }, 3, narrowed)).toBe(true);
  });

  it("never uploads for an epoch older than the one already uploaded", () => {
    const renamed = cardFingerprint({ ...content, displayName: "Ada L." }, 200, 3);
    expect(cardUploadDue({ epoch: 4, fingerprint: fp }, 3, renamed)).toBe(false);
  });

  it("a new grant version changes the fingerprint only when the grant rides", () => {
    expect(cardFingerprint(content, 100, 3)).not.toBe(cardFingerprint(content, 100, 4));
    const hidden = { ...content, username: null, includeGrant: false };
    expect(cardFingerprint(hidden, 100, 3)).toBe(cardFingerprint(hidden, 100, 4));
  });
});

describe("cardGrantDecision", () => {
  const base = {
    isOwnCard: false,
    senderAccountKeyHex: "aa",
    ownAccountKeyHex: "bb",
    carriesChannelGrant: true,
    grantVerified: true as boolean | null,
  };

  it("files a verified grant from another member", () => {
    expect(cardGrantDecision(base)).toBe("file");
  });

  it("never files a grant the reach check refused or could not answer", () => {
    expect(cardGrantDecision({ ...base, grantVerified: false })).toBe("skip");
    expect(cardGrantDecision({ ...base, grantVerified: null })).toBe("skip");
  });

  it("withdraws when the card no longer carries the grant", () => {
    expect(cardGrantDecision({ ...base, carriesChannelGrant: false, grantVerified: null })).toBe(
      "withdraw",
    );
  });

  it("skips the own card and a card of the own account", () => {
    expect(cardGrantDecision({ ...base, isOwnCard: true })).toBe("skip");
    expect(cardGrantDecision({ ...base, senderAccountKeyHex: "bb" })).toBe("skip");
  });

  it("skips a card whose author could not be resolved, grant or not", () => {
    expect(cardGrantDecision({ ...base, senderAccountKeyHex: null })).toBe("skip");
    expect(
      cardGrantDecision({ ...base, senderAccountKeyHex: null, carriesChannelGrant: false }),
    ).toBe("skip");
  });
});

describe("memberRowName", () => {
  it("labels the viewer, a named member and an unnamed one", () => {
    expect(memberRowName("Ada", true)).toBe("You");
    expect(memberRowName(" Ada ", false)).toBe("Ada");
    expect(memberRowName(undefined, false)).toBe("Member");
    expect(memberRowName("  ", false)).toBe("Member");
  });
});

describe("isStaleSubscriberKey", () => {
  it("accepts a pair whose public half is the Ed25519 key of its secret", () => {
    const pk = new Uint8Array(32).fill(7);
    expect(isStaleSubscriberKey(pk, pk.slice())).toBe(false);
  });

  it("flags a pre-flag-day pair and a malformed one", () => {
    const pk = new Uint8Array(32).fill(7);
    const other = pk.slice();
    other[31] = 8;
    expect(isStaleSubscriberKey(pk, other)).toBe(true);
    expect(isStaleSubscriberKey(pk.subarray(0, 31), pk)).toBe(true);
  });
});

describe("sealEachSkippingInvalid", () => {
  it("skips a key the sealer refuses and keeps the order of the rest", () => {
    const res = sealEachSkippingInvalid(["aa", "bb", "cc"], (hex) => {
      if (hex === "bb") throw new Error("not a valid point");
      return `sealed-${hex}`;
    });
    expect(res.sealed).toEqual([
      { subscriberHex: "aa", value: "sealed-aa" },
      { subscriberHex: "cc", value: "sealed-cc" },
    ]);
    expect(res.skipped).toEqual(["bb"]);
  });
});

describe("ownCardPlan (ADR-0101 walk 2026-11-26, D1)", () => {
  const base = {
    publishing: false,
    readerDevice: false,
    reading: true,
    readingKeyHex: "aa".repeat(32),
    registerKeyHex: null,
    registeredReaderKeyHex: null,
  } as const;

  it("a reading subscriber uploads under its own key and may register it", () => {
    expect(ownCardPlan(base)).toEqual({ keySource: "reading-key", reRegister: true });
  });

  it("a subscriber not let in yet uploads nothing", () => {
    expect(ownCardPlan({ ...base, reading: false })).toBeNull();
    expect(ownCardPlan({ ...base, readingKeyHex: null })).toBeNull();
  });

  it("a reader device of the owner uploads under the register key it reads with, never registering it", () => {
    expect(ownCardPlan({ ...base, readerDevice: true })).toEqual({ keySource: "reading-key", reRegister: false });
  });

  it("the owner's publishing device uploads under the registered register key", () => {
    const key = "bb".repeat(32);
    expect(
      ownCardPlan({ ...base, publishing: true, readingKeyHex: null, registerKeyHex: key, registeredReaderKeyHex: key }),
    ).toEqual({ keySource: "register-key", reRegister: false });
  });

  it("the publishing device waits until the service holds the register key", () => {
    expect(
      ownCardPlan({ ...base, publishing: true, registerKeyHex: "bb".repeat(32), registeredReaderKeyHex: null }),
    ).toBeNull();
    expect(
      ownCardPlan({
        ...base,
        publishing: true,
        registerKeyHex: "bb".repeat(32),
        registeredReaderKeyHex: "cc".repeat(32),
      }),
    ).toBeNull();
    expect(ownCardPlan({ ...base, publishing: true, registerKeyHex: null })).toBeNull();
  });
});
