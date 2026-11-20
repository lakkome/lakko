// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0074 section 3.7 (2026-11-20) - an owner appoints a member that rotated
 * its account keys, and that member opens what the owner sealed. Driven
 * through the real wasm core: the link is signed and verified by
 * `signConversationLink` / `verifyConversationLink`, and the envelope sealed
 * and opened by the registrar-seed and knock-seed functions.
 */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { bundleServesRosterKey, openSealedToOwnAccount } from "./rotated-member-seal";

beforeAll(async () => {
  const wasmPath = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(wasmPath) });
});

const verifyLink = (l: Uint8Array, k: Uint8Array): Uint8Array => wasm.verifyConversationLink(l, k);

function person(): { conversation: wasm.Identity; current: wasm.Identity; other: wasm.Identity; link: Uint8Array } {
  const conversation = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x31));
  const current = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x32));
  const other = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x33));
  const link = wasm.signConversationLink(conversation.ed25519Seed, current.ed25519Seed, 1_700_000_000n);
  return { conversation, current, other, link };
}

describe("bundleServesRosterKey", () => {
  it("accepts an unrotated member on the served key alone", () => {
    const { conversation } = person();
    const key = conversation.signaturePublicKey;
    expect(bundleServesRosterKey({ servedKey: key, conversationLink: null, rosterKey: key, verifyLink })).toBe(
      true,
    );
  });

  it("accepts a rotated member whose link names the roster key", () => {
    const { conversation, current, link } = person();
    expect(
      bundleServesRosterKey({
        servedKey: current.signaturePublicKey,
        conversationLink: link,
        rosterKey: conversation.signaturePublicKey,
        verifyLink,
      }),
    ).toBe(true);
  });

  it("refuses a rotated member with no link", () => {
    const { conversation, current } = person();
    for (const conversationLink of [null, new Uint8Array(0)]) {
      expect(
        bundleServesRosterKey({
          servedKey: current.signaturePublicKey,
          conversationLink,
          rosterKey: conversation.signaturePublicKey,
          verifyLink,
        }),
      ).toBe(false);
    }
  });

  it("refuses a link that does not verify against the served key", () => {
    const { conversation, other, link } = person();
    expect(
      bundleServesRosterKey({
        servedKey: other.signaturePublicKey,
        conversationLink: link,
        rosterKey: conversation.signaturePublicKey,
        verifyLink,
      }),
    ).toBe(false);
    const damaged = link.slice();
    damaged[damaged.length - 1] = (damaged[damaged.length - 1] ?? 0) ^ 1;
    const { current } = person();
    expect(
      bundleServesRosterKey({
        servedKey: current.signaturePublicKey,
        conversationLink: damaged,
        rosterKey: conversation.signaturePublicKey,
        verifyLink,
      }),
    ).toBe(false);
  });

  it("refuses a link naming another conversation key", () => {
    const { current, other, link } = person();
    expect(
      bundleServesRosterKey({
        servedKey: current.signaturePublicKey,
        conversationLink: link,
        rosterKey: other.signaturePublicKey,
        verifyLink,
      }),
    ).toBe(false);
  });
});

describe("openSealedToOwnAccount", () => {
  it("opens a registrar seed addressed to the conversation key and sealed to the current key", () => {
    const { conversation, current } = person();
    const rosterKey = conversation.signaturePublicKey;
    const seed = wasm.dsJournalRegistrarSeed(new Uint8Array(32).fill(0x51), rosterKey);
    const envelope = wasm.composeRegistrarSeedEnvelope(
      new Uint8Array(32).fill(0x45),
      rosterKey,
      current.x25519PublicKey,
      1,
      seed,
    );
    const opened = openSealedToOwnAccount({
      conversation,
      current,
      openOwn: (a) => wasm.openRegistrarSeedEnvelope(envelope, a),
      openSealedTo: (a, c) => wasm.openRegistrarSeedEnvelopeSealedTo(envelope, a, c),
    });
    expect(opened).toEqual(seed);
    // An unrotated receiver (no current identity) cannot open it.
    expect(
      openSealedToOwnAccount({
        conversation,
        current: null,
        openOwn: (a) => wasm.openRegistrarSeedEnvelope(envelope, a),
        openSealedTo: (a, c) => wasm.openRegistrarSeedEnvelopeSealedTo(envelope, a, c),
      }),
    ).toBeNull();
  });

  it("still opens an envelope sealed to the conversation identity's own key", () => {
    const { conversation, current } = person();
    const rosterKey = conversation.signaturePublicKey;
    const seed = wasm.dsJournalRegistrarSeed(new Uint8Array(32).fill(0x52), rosterKey);
    const envelope = wasm.composeRegistrarSeedEnvelope(
      new Uint8Array(32).fill(0x46),
      rosterKey,
      conversation.x25519PublicKey,
      2,
      seed,
    );
    expect(
      openSealedToOwnAccount({
        conversation,
        current,
        openOwn: (a) => wasm.openRegistrarSeedEnvelope(envelope, a),
        openSealedTo: (a, c) => wasm.openRegistrarSeedEnvelopeSealedTo(envelope, a, c),
      }),
    ).toEqual(seed);
  });

  it("opens a knock seed addressed to the conversation key and sealed to the current key", () => {
    const { conversation, current, other } = person();
    const seed = wasm.groupKnockSeed(new Uint8Array(32).fill(0x11), 1);
    const envelope = wasm.composeKnockSeedEnvelope(
      new Uint8Array(32).fill(0x4a),
      conversation.signaturePublicKey,
      current.x25519PublicKey,
      1,
      seed,
    );
    expect(
      openSealedToOwnAccount({
        conversation,
        current,
        openOwn: (a) => wasm.openKnockSeedEnvelope(envelope, a),
        openSealedTo: (a, c) => wasm.openKnockSeedEnvelopeSealedTo(envelope, a, c),
      }),
    ).toEqual(seed);
    // Addressed to the conversation key: another identity is not the addressee.
    expect(
      openSealedToOwnAccount({
        conversation: other,
        current,
        openOwn: (a) => wasm.openKnockSeedEnvelope(envelope, a),
        openSealedTo: (a, c) => wasm.openKnockSeedEnvelopeSealedTo(envelope, a, c),
      }),
    ).toBeNull();
  });
});
