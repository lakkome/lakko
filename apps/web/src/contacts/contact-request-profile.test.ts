// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0048 §2 — a contact request carries the sender's profile inside its
 * sealed envelope, end to end through the REAL core: `ContactsClient` composes
 * the request with the profile it is handed, the DS stand-in stores the opaque
 * bytes, and the recipient's `pollMailbox` opens them back into a profile bound
 * to the request's authenticated `fromPubkey`.
 *
 * ## Discrimination (verified by running the broken variant)
 *
 * Passing an empty profile to `ContactRequest.compose` (dropping
 * `profileEnvelopeForRequest`) fails "carries the sender's profile" with
 * `expected null to deeply equal { displayName: 'Alice Petrova', ... }`.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import initWasm, * as wasm from "../../wasm/lakko_ffi_wasm";
import { ContactsClient } from "./client";
import type { DsClient } from "../ds/client";
import type { IdentitySecrets } from "../auth/AuthContext";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await initWasm({ module_or_path: readFileSync(path) });
});

function secrets(fill: number): IdentitySecrets {
  return {
    accountId: new Uint8Array(16).fill(fill),
    exportKey: new Uint8Array(64).fill(fill),
    accountIdentitySeed: new Uint8Array(64).fill(fill),
    sessionToken: "token",
    sessionExpiresAtMs: Date.now() + 60_000,
  } as unknown as IdentitySecrets;
}

/** A DS that keeps every mailbox publish, per address, and replays it on
 *  fetch of that address. */
function memoryDs() {
  const rows = new Map<string, Array<{ id: number; payload: Uint8Array; createdAt: string }>>();
  const hex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  let next = 1;
  const ds = {
    publishMailbox: vi.fn(async (address: Uint8Array, payload: Uint8Array) => {
      const list = rows.get(hex(address)) ?? [];
      list.push({ id: next, payload, createdAt: "2026-09-13T00:00:00Z" });
      rows.set(hex(address), list);
      next += 1;
      return { id: next - 1, createdAt: "2026-09-13T00:00:00Z" };
    }),
    fetchMailbox: vi.fn(async (address: Uint8Array) => ({ messages: rows.get(hex(address)) ?? [], hasMore: false })),
  } as unknown as DsClient & { publishMailbox: ReturnType<typeof vi.fn> };
  return ds;
}

async function roundTrip(ownProfileEnvelope: (() => Promise<Uint8Array | null>) | undefined) {
  const alice = secrets(0x0a);
  const bob = secrets(0x0b);
  const bobIdentity = wasm.Identity.fromSeed(bob.accountIdentitySeed);
  const aliceIdentity = wasm.Identity.fromSeed(alice.accountIdentitySeed);
  const ds = memoryDs();

  const sender = new ContactsClient(undefined, ds, ownProfileEnvelope === undefined ? {} : { ownProfileEnvelope });
  // ADR-0078: Bob is in Everyone; the request goes unsigned to the mailbox
  // his public record's binding names, verified against his key.
  await sender.sendContactRequestToBundle(
    alice,
    bob.accountId,
    bobIdentity.signaturePublicKey,
    bobIdentity.x25519PublicKey,
    new TextEncoder().encode("hi bob"),
    { openBinding: wasm.signMailboxBinding(bobIdentity) },
  );

  const recipient = new ContactsClient(undefined, ds);
  const polled = await recipient.pollMailbox(bob, 0);
  const ev = polled.events[0];
  if (ev?.kind !== "contact-request") throw new Error("expected a contact request");
  return { request: ev.request, aliceKey: aliceIdentity.signaturePublicKey, ds, bobIdentity };
}

describe("contact request carries a profile", () => {
  it("carries the sender's profile, bound to the authenticated sender key", async () => {
    const { request, aliceKey } = await roundTrip(async () =>
      wasm.composeProfileEnvelope("Alice Petrova", "alice", 1_234n),
    );
    expect(request.profile).toEqual({
      displayName: "Alice Petrova",
      username: "alice",
      updatedAtMs: 1_234n,
      accountId: null,
    });
    expect(Array.from(request.fromPubkey)).toEqual(Array.from(aliceKey));
    expect(request.intro).toBe("hi bob");
  });

  it("carries the sender's claimed account id when the envelope has one (ADR-0048 v2)", async () => {
    const id = new Uint8Array(16).fill(0x5a);
    const { request } = await roundTrip(async () =>
      wasm.composeProfileEnvelope("Alice Petrova", "alice", 1_234n, id),
    );
    expect(request.profile?.accountId).toEqual(id);
    expect(request.profile?.displayName).toBe("Alice Petrova");
  });

  it("without a profile the request still arrives, with none", async () => {
    const { request } = await roundTrip(undefined);
    expect(request.profile).toBeNull();
    expect(request.intro).toBe("hi bob");
  });

  it("ADR-0078: the row is SEALED to Bob's mailbox key and addressed to its address, with no clear kind byte", async () => {
    const { ds, bobIdentity } = await roundTrip(undefined);
    const call = ds.publishMailbox.mock.calls[0] as [Uint8Array, Uint8Array];
    const key = wasm.MailboxKey.fromAccountIdentity(bobIdentity);
    try {
      expect(Array.from(call[0])).toEqual(Array.from(key.address()));
      expect(wasm.isWellFormedMailboxRow(call[1])).toBe(true);
      const opened = wasm.openMailboxRow(key, call[1]);
      try {
        expect(opened.kind).toBe(wasm.MailboxKind.ContactRequest);
        // The body is the signed ContactRequest wire, unchanged.
        expect(() => wasm.ContactRequest.decodeAndVerify(opened.body).free()).not.toThrow();
      } finally {
        opened.free();
      }
    } finally {
      key.free();
    }
  });

  it("ADR-0078: a binding that does not verify against the recipient's key refuses the write", async () => {
    const bob = secrets(0x0b);
    const bobIdentity = wasm.Identity.fromSeed(bob.accountIdentitySeed);
    const mallory = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x0c));
    const ds = memoryDs();
    const sender = new ContactsClient(undefined, ds, {});
    await expect(
      sender.sendContactRequestToBundle(
        secrets(0x0a),
        bob.accountId,
        bobIdentity.signaturePublicKey,
        bobIdentity.x25519PublicKey,
        new TextEncoder().encode("hi bob"),
        // Mallory's mailbox presented under Bob's name.
        { openBinding: wasm.signMailboxBinding(mallory) },
      ),
    ).rejects.toThrow();
    expect(ds.publishMailbox).not.toHaveBeenCalled();
  });

  it("ADR-0078: with no grant and no binding nothing is written (no identity-derived address)", async () => {
    const bob = secrets(0x0b);
    const bobIdentity = wasm.Identity.fromSeed(bob.accountIdentitySeed);
    const ds = memoryDs();
    const sender = new ContactsClient(undefined, ds, {});
    await expect(
      sender.sendContactRequestToBundle(
        secrets(0x0a),
        bob.accountId,
        bobIdentity.signaturePublicKey,
        bobIdentity.x25519PublicKey,
        new TextEncoder().encode("hi bob"),
      ),
    ).rejects.toThrow();
    expect(ds.publishMailbox).not.toHaveBeenCalled();
  });

  it("ADR-0078: `from_pseudonym` is fresh random bytes per request, not the identity pseudonym", async () => {
    const first = await roundTrip(undefined);
    const second = await roundTrip(undefined);
    const identityPseudonym = wasm.IdentityPseudonym.fromIdentity(first.aliceKey).asBytes();
    expect(first.request.fromPseudonym.length).toBe(32);
    expect(Array.from(first.request.fromPseudonym)).not.toEqual(Array.from(identityPseudonym));
    expect(Array.from(second.request.fromPseudonym)).not.toEqual(Array.from(identityPseudonym));
    expect(Array.from(first.request.fromPseudonym)).not.toEqual(Array.from(second.request.fromPseudonym));
  });

  it("a failing profile source never stops the request", async () => {
    const { request } = await roundTrip(async () => {
      throw new Error("no profile");
    });
    expect(request.profile).toBeNull();
    expect(request.intro).toBe("hi bob");
  });
});
