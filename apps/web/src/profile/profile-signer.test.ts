// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/** ADR-0101 section 8 - a received profile is filed only under its signer;
 *  this account's own, returned after its reset, comes back as its own. */
import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";
import { profileVerifiesUnder, routeReceivedProfile } from "./profile-signer";
import { adoptsReturnedProfile } from "./returned-profile";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const b64 = (b: Uint8Array): string => Buffer.from(b).toString("base64");
const unb64 = (s: string): Uint8Array => new Uint8Array(Buffer.from(s, "base64"));

describe("the route", () => {
  it("files only what the sender signed", () => {
    expect(routeReceivedProfile({ verifiesUnderSender: true, verifiesUnderPreviousOwnKey: true })).toBe("file");
    expect(routeReceivedProfile({ verifiesUnderSender: false, verifiesUnderPreviousOwnKey: true })).toBe("returned-own");
    expect(routeReceivedProfile({ verifiesUnderSender: false, verifiesUnderPreviousOwnKey: false })).toBe("drop");
  });
});

describe("through the core", () => {
  it("a profile a peer carries back verifies under the person's old key, not the peer's", () => {
    const person = wasm.Identity.fromSeed(new Uint8Array(32).fill(1));
    const peer = wasm.Identity.fromSeed(new Uint8Array(32).fill(2));
    try {
      const env = wasm.composeProfileEnvelope(person, "Anna", undefined, 7n);
      const verify = (e: Uint8Array, k: Uint8Array): { free(): void } => wasm.verifyProfileEnvelope(e, k);
      const underPeer = profileVerifiesUnder(verify, env, b64(peer.signaturePublicKey), unb64);
      const underPerson = profileVerifiesUnder(verify, env, b64(person.signaturePublicKey), unb64);
      expect(underPeer).toBe(false);
      expect(underPerson).toBe(true);
      expect(routeReceivedProfile({ verifiesUnderSender: underPeer, verifiesUnderPreviousOwnKey: underPerson })).toBe(
        "returned-own",
      );
      expect(profileVerifiesUnder(verify, env, null, unb64)).toBe(false);
    } finally {
      person.free();
      peer.free();
    }
  });
});

describe("adopting a returned own profile", () => {
  const p = (t: bigint): { displayName: string; updatedAtMs: bigint; avatar: null } => ({ displayName: "A", updatedAtMs: t, avatar: null });
  it("only a strictly newer, non-zero stamp", () => {
    expect(adoptsReturnedProfile(p(5n), 4n)).toBe(true);
    expect(adoptsReturnedProfile(p(5n), 5n)).toBe(false);
    expect(adoptsReturnedProfile(p(0n), 0n)).toBe(false);
  });
});

import { newestHeldEnvelope } from "./returned-profile";

describe("the newest profile held for a reset person", () => {
  it("across their retired keys, by stamp", () => {
    const held: Record<string, { envelope: Uint8Array; updatedAtMs: bigint }> = {
      A: { envelope: new Uint8Array([1]), updatedAtMs: 5n },
      B: { envelope: new Uint8Array([2]), updatedAtMs: 9n },
    };
    expect(Array.from(newestHeldEnvelope(["A", "B", "C"], (k) => held[k] ?? null) ?? [])).toEqual([2]);
    expect(newestHeldEnvelope(["C"], () => null)).toBeNull();
  });
});
