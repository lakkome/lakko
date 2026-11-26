// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0060 section 3 against the real wasm core: the header name this client
 * sends is the core's, a card sealed for one subscriber opens only for it,
 * and the flag-day check tells an Ed25519 pair from any other.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, {
  ChannelSubscriberKeypair,
  composeProfileEnvelope,
  decodeProfileEnvelope,
  Identity,
  memberCardEpoch,
  memberCardSigHeaderName,
  openMemberCard,
  sealMemberCard,
} from "../../wasm/lakko_ffi_wasm";
import { MEMBER_CARD_SIG_HEADER } from "./api";
import { isStaleSubscriberKey } from "./member-card-rules";

/** ADR-0101 section 8: a v6 profile envelope is signed by an account
 *  identity; this test's profiles are signed by one fixed identity. */
let profileSignerHeld: Identity | undefined;
function profileSigner(): Identity {
  return (profileSignerHeld ??= Identity.fromSeed(new Uint8Array(64).fill(0x5a)));
}

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const channelId = new Uint8Array(16).fill(0x11);
const seed = new Uint8Array(32).fill(0x42);

describe("member cards through the core", () => {
  it("sends the header the core names", () => {
    expect(MEMBER_CARD_SIG_HEADER).toBe(memberCardSigHeaderName());
  });

  it("round-trips a card for its subscriber and refuses it for another", () => {
    const me = ChannelSubscriberKeypair.generate();
    const other = ChannelSubscriberKeypair.generate();
    try {
      const profile = composeProfileEnvelope(profileSigner(), "Ada", undefined, 5n, new Uint8Array(16).fill(1));
      const accountSeed = new Uint8Array(32).fill(0x07);
      const card = sealMemberCard(seed, 3, channelId, me.public(), accountSeed, profile);
      expect(memberCardEpoch(card)).toBe(3);
      const openedCard = openMemberCard(seed, card, channelId, me.public());
      const identityPk = openedCard.identityPk;
      const opened = decodeProfileEnvelope(openedCard.profile);
      openedCard.free();
      try {
        // The binding names the account whose seed sealed the card.
        expect(identityPk).toHaveLength(32);
        expect(opened.displayName).toBe("Ada");
        expect(opened.username).toBeUndefined();
        expect(opened.grants).toHaveLength(0);
      } finally {
        opened.free();
      }
      expect(() => openMemberCard(seed, card, channelId, other.public())).toThrow();
      const sig = me.memberCardRequestSignature(channelId, card);
      expect(sig).toMatch(/^[A-Za-z0-9_-]{86}$/);
    } finally {
      me.free();
      other.free();
    }
  });

  it("tells a current Ed25519 pair from a pre-flag-day one", () => {
    const kp = ChannelSubscriberKeypair.generate();
    try {
      const secret = kp.secret();
      const rebuilt = new ChannelSubscriberKeypair(secret);
      try {
        expect(isStaleSubscriberKey(kp.public(), rebuilt.public())).toBe(false);
        // An X25519 pair stored its X25519 public half, which is not the
        // Ed25519 public key of the same secret.
        const x25519Like = kp.public().slice();
        x25519Like[0] = (x25519Like[0] ?? 0) ^ 0xff;
        expect(isStaleSubscriberKey(x25519Like, rebuilt.public())).toBe(true);
      } finally {
        rebuilt.free();
      }
    } finally {
      kp.free();
    }
  });
});

import { verifyProfileEnvelope } from "../../wasm/lakko_ffi_wasm";
import { unnamedCardRow } from "./member-card-rules";

describe("ADR-0101 section 8: a card's profile counts only when its bound account signed it", () => {
  it("a profile signed by the card's account verifies; one signed by another account is refused", () => {
    const me = ChannelSubscriberKeypair.generate();
    const account = Identity.fromSeed(new Uint8Array(32).fill(0x07));
    const stranger = Identity.fromSeed(new Uint8Array(32).fill(0x08));
    try {
      const own = composeProfileEnvelope(account, "Ada", undefined, 5n);
      const forged = composeProfileEnvelope(stranger, "Eve", undefined, 5n);
      for (const [profile, ok] of [
        [own, true],
        [forged, false],
      ] as const) {
        const card = sealMemberCard(seed, 1, channelId, me.public(), account.ed25519Seed, profile);
        const opened = openMemberCard(seed, card, channelId, me.public());
        try {
          if (ok) verifyProfileEnvelope(opened.profile, opened.identityPk).free();
          else expect(() => verifyProfileEnvelope(opened.profile, opened.identityPk)).toThrow();
        } finally {
          opened.free();
        }
      }
    } finally {
      me.free();
      account.free();
      stranger.free();
    }
  });

  it("a member with a refused profile is an unnamed row under its bound key", () => {
    expect(unnamedCardRow({ identityPkHex: "ab", isOwnCard: false, ownAccountKeyHex: "cd" })).toMatchObject({
      key: "ab",
      username: null,
      isSelf: false,
      named: false,
    });
    expect(unnamedCardRow({ identityPkHex: "cd", isOwnCard: false, ownAccountKeyHex: "cd" }).key).toBe("self");
  });
});
