// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * 2026-11-20 (ADR-0074) — after a rotation a co-publisher proof is signed by
 * the account's lineage identity (the one the appointed binding names),
 * whichever account identity the caller passed, as on iOS and Android.
 */
import { afterEach, describe, expect, it } from "vitest";

import { coPublisherProofIdentity, installAccountIdentities, ownerProofIdentity } from "./api";
import type { Wasm } from "../wasm";

type Identity = InstanceType<Wasm["Identity"]>;

function identity(n: number): Identity {
  return { signaturePublicKey: new Uint8Array(32).fill(n) } as unknown as Identity;
}

afterEach(() => installAccountIdentities(null));

describe("coPublisherProofIdentity", () => {
  it("is the lineage identity for either of the account's identities", () => {
    const lineage = identity(1);
    const current = identity(2);
    installAccountIdentities(() => ({ current, lineage }));
    expect(coPublisherProofIdentity(current)).toBe(lineage);
    expect(coPublisherProofIdentity(lineage)).toBe(lineage);
  });

  it("uses another identity as passed, and the passed one with no session", () => {
    installAccountIdentities(() => ({ current: identity(2), lineage: identity(1) }));
    const stranger = identity(9);
    expect(coPublisherProofIdentity(stranger)).toBe(stranger);
    installAccountIdentities(null);
    const passed = identity(2);
    expect(coPublisherProofIdentity(passed)).toBe(passed);
  });
});

describe("ownerProofIdentity", () => {
  it("is the current identity for either of the account's identities", () => {
    const lineage = identity(1);
    const current = identity(2);
    installAccountIdentities(() => ({ current, lineage }));
    expect(ownerProofIdentity(lineage)).toBe(current);
    expect(ownerProofIdentity(current)).toBe(current);
    const stranger = identity(9);
    expect(ownerProofIdentity(stranger)).toBe(stranger);
    installAccountIdentities(null);
    expect(ownerProofIdentity(lineage)).toBe(lineage);
  });
});

