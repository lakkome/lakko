// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0073 R.4 - the continuity key's window (`continuity.ts`), and that the
 *  wasm core derives one key from public values. ADR-0078: the resetting
 *  device no longer registers it (no peer can address its mailbox), so there
 *  is no marker any more. */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import {
  CONTINUITY_BINDING_LEN,
  CONTINUITY_KEY_TTL_MS,
  continuityBindingExpiryMs,
  continuityExpiryMs,
  continuityKeyUsable,
  continuityTargetLive,
  newIdentityReachableVerdict,
  peerContinuityTarget,
} from "./continuity";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

describe("the continuity window (D2: 30 days from the reset's signed_at)", () => {
  it("is the DS journal retention", () => {
    expect(CONTINUITY_KEY_TTL_MS).toBe(30 * 24 * 60 * 60 * 1000);
    expect(continuityExpiryMs(1000)).toBe(1000 + CONTINUITY_KEY_TTL_MS);
  });
  it("a peer uses it inside the same window only", () => {
    expect(continuityKeyUsable(1000, 1000 + CONTINUITY_KEY_TTL_MS - 1)).toBe(true);
    expect(continuityKeyUsable(1000, 1000 + CONTINUITY_KEY_TTL_MS)).toBe(false);
  });
});

describe("both sides derive one key (the wasm core)", () => {
  const ACCOUNT = new Uint8Array(16).fill(5);
  it("the seed is a function of the account id and the generation only", () => {
    const a = wasm.continuityReachSeed(ACCOUNT, 2n);
    const b = wasm.continuityReachSeed(ACCOUNT, 2n);
    expect(Array.from(a)).toEqual(Array.from(b));
    expect(a.length).toBe(32);
    expect(Array.from(wasm.continuityReachSeed(ACCOUNT, 3n))).not.toEqual(Array.from(a));
    expect(Array.from(wasm.continuityReachSeed(new Uint8Array(16).fill(6), 2n))).not.toEqual(Array.from(a));
  });
  it("the reach verifying key is the one a signer over the seed proves under", () => {
    const seed = wasm.continuityReachSeed(ACCOUNT, 2n);
    const registered = wasm.reachVerifyingKey(seed);
    const signer = wasm.DsProofSigner.forReach(wasm.continuityReachSeed(ACCOUNT, 2n));
    try {
      expect(Array.from(signer.verifyingKey())).toEqual(Array.from(registered));
    } finally {
      signer.free();
    }
  });
});

const b64 = (b: Uint8Array): string => Buffer.from(b).toString("base64");
const unb64 = (s: string): Uint8Array => new Uint8Array(Buffer.from(s, "base64"));

describe("ADR-0101: the peer verifies the continuity binding under the attested key", () => {
  it("the window equals the core's", () => {
    expect(BigInt(CONTINUITY_KEY_TTL_MS)).toBe(wasm.continuityWindowMs());
    expect(CONTINUITY_BINDING_LEN).toBe(wasm.continuityBindingLen());
  });

  it("a binding signed by the new key yields its continuity mailbox and expiry", () => {
    const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(9));
    const now = 1_700_000_000_000;
    const binding = wasm.signContinuityBinding(id, BigInt(now + 1000));
    const mailbox = wasm.MailboxKey.continuityFromAccountIdentity(id);
    try {
      expect(continuityBindingExpiryMs(binding)).toBe(now + 1000);
      const target = peerContinuityTarget(b64(binding), id.signaturePublicKey, now, wasm.verifyContinuityBinding, unb64, b64);
      expect(target).toEqual({ mailboxPkB64: b64(mailbox.publicKey()), expiresAtMs: now + 1000 });
      // Never the real mailbox.
      const real = wasm.MailboxKey.fromAccountIdentity(id);
      expect(target?.mailboxPkB64).not.toBe(b64(real.publicKey()));
      real.free();
    } finally {
      mailbox.free();
      id.free();
    }
  });

  it("refuses another key, an expired binding, garbage and absence", () => {
    const id = wasm.Identity.fromSeed(new Uint8Array(32).fill(9));
    const other = wasm.Identity.fromSeed(new Uint8Array(32).fill(8));
    const now = 1_700_000_000_000;
    const binding = wasm.signContinuityBinding(id, BigInt(now + 1000));
    try {
      const v = wasm.verifyContinuityBinding;
      expect(peerContinuityTarget(b64(binding), other.signaturePublicKey, now, v, unb64, b64)).toBeNull();
      expect(peerContinuityTarget(b64(binding), id.signaturePublicKey, now + 1000, v, unb64, b64)).toBeNull();
      expect(peerContinuityTarget("AAAA", id.signaturePublicKey, now, v, unb64, b64)).toBeNull();
      expect(peerContinuityTarget(undefined, id.signaturePublicKey, now, v, unb64, b64)).toBeNull();
      const flipped = binding.slice();
      flipped[104] = (flipped[104] ?? 0) ^ 1;
      expect(peerContinuityTarget(b64(flipped), id.signaturePublicKey, now, v, unb64, b64)).toBeNull();
    } finally {
      id.free();
      other.free();
    }
  });
});

describe("ADR-0101 section 4: newIdentityReachable", () => {
  const live = { mailboxPkB64: "x", expiresAtMs: 2000 };
  it("an ordinary candidate is enough", () => {
    expect(newIdentityReachableVerdict({ ordinaryCandidates: 1, continuity: null, resetSignedAtMs: null, nowMs: 0 })).toBe(true);
  });
  it("otherwise a live binding AND a usable continuity key", () => {
    expect(newIdentityReachableVerdict({ ordinaryCandidates: 0, continuity: live, resetSignedAtMs: 0, nowMs: 1000 })).toBe(true);
    expect(newIdentityReachableVerdict({ ordinaryCandidates: 0, continuity: live, resetSignedAtMs: 0, nowMs: 2000 })).toBe(false);
    expect(newIdentityReachableVerdict({ ordinaryCandidates: 0, continuity: null, resetSignedAtMs: 0, nowMs: 1000 })).toBe(false);
    expect(newIdentityReachableVerdict({ ordinaryCandidates: 0, continuity: live, resetSignedAtMs: null, nowMs: 1000 })).toBe(false);
    expect(
      newIdentityReachableVerdict({
        ordinaryCandidates: 0,
        continuity: { mailboxPkB64: "x", expiresAtMs: Number.MAX_SAFE_INTEGER },
        resetSignedAtMs: 0,
        nowMs: CONTINUITY_KEY_TTL_MS,
      }),
    ).toBe(false);
  });
  it("a target is live strictly before its expiry", () => {
    expect(continuityTargetLive(live, 1999)).toBe(true);
    expect(continuityTargetLive(live, 2000)).toBe(false);
    expect(continuityTargetLive(null, 0)).toBe(false);
  });
});
