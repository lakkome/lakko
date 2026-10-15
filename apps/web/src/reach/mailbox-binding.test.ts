// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0078 phase 1 - the mailbox binding is verified before every write and
 * before a grant is kept:
 *
 *   - a write under a held grant goes to the mailbox its binding names, only
 *     after the binding verified against the person's pinned key; a binding
 *     that does not verify refuses the write (nothing is written);
 *   - the unsigned write to a person in Everyone needs their public record's
 *     binding, verified the same way; with none, nothing is written (no
 *     address is ever derived from an identity key);
 *   - profile grants and join reply grants are kept only when their binding
 *     verifies against the sender's / joiner's account key;
 *   - a `/c/` link grant is checked against the owner's account key.
 *
 * Driven through the REAL wasm core.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

import {
  MailboxUnreachableError,
  verifiedTargetOfBinding,
  verifiedTargetOfGrant,
  writeToPerson,
} from "./mailbox-write";
import { grantsWithVerifiedBindings } from "./grant-intake";
import { joinReplyBindingVerifies } from "./join-replies";
import { callLinkGrantVerifies, callLinkOwnerAccountKey } from "../calls/link-grant";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

const b64 = (bytes: Uint8Array): string => Buffer.from(bytes).toString("base64");
const hex = (bytes: Uint8Array): string => Buffer.from(bytes).toString("hex");

function person(fill: number): wasm.Identity {
  return wasm.Identity.fromSeed(new Uint8Array(64).fill(fill));
}

function contactGrantOf(owner: wasm.Identity, version = 0): wasm.ReachGrant {
  return wasm.deriveReachGrant(owner.ed25519Seed, wasm.ReachClass.Contact, new Uint8Array(0), version);
}

function addressOf(owner: wasm.Identity): Uint8Array {
  const key = wasm.MailboxKey.fromAccountIdentity(owner);
  try {
    return key.address();
  } finally {
    key.free();
  }
}

describe("a grant's target", () => {
  it("is the owner's mailbox when the binding verifies under the owner's key, and nothing otherwise", async () => {
    const bob = person(0x41);
    const mallory = person(0x42);
    const grant = contactGrantOf(bob);
    try {
      const target = await verifiedTargetOfGrant(grant, [bob.signaturePublicKey]);
      expect(target).not.toBeNull();
      expect(hex(target!.address)).toBe(hex(addressOf(bob)));
      // Under someone else's key: refused.
      expect(await verifiedTargetOfGrant(grant, [mallory.signaturePublicKey])).toBeNull();
      // A kept wire works the same; a wire that does not decode is refused.
      expect(await verifiedTargetOfGrant(b64(wasm.encodeReachGrant(grant)), [bob.signaturePublicKey])).not.toBeNull();
      expect(await verifiedTargetOfGrant("AAAA", [bob.signaturePublicKey])).toBeNull();
      // Any key of the person's proven chain is tried.
      expect(
        await verifiedTargetOfGrant(grant, [mallory.signaturePublicKey, bob.signaturePublicKey]),
      ).not.toBeNull();
    } finally {
      grant.free();
      bob.free();
      mallory.free();
    }
  });

  it("a bare binding (a public record's) is verified the same way", async () => {
    const bob = person(0x43);
    const mallory = person(0x44);
    try {
      const binding = wasm.signMailboxBinding(bob);
      expect(hex((await verifiedTargetOfBinding(binding, [bob.signaturePublicKey]))!.address)).toBe(
        hex(addressOf(bob)),
      );
      expect(await verifiedTargetOfBinding(binding, [mallory.signaturePublicKey])).toBeNull();
      expect(await verifiedTargetOfBinding(new Uint8Array(97), [bob.signaturePublicKey])).toBeNull();
    } finally {
      bob.free();
      mallory.free();
    }
  });
});

describe("a write under a grant", () => {
  it("goes to the mailbox the grant's binding names, after it verified", async () => {
    const bob = person(0x45);
    const grant = contactGrantOf(bob);
    try {
      const write = vi.fn(async (target: { address: Uint8Array }) => hex(target.address));
      const out = await writeToPerson(bob.signaturePublicKey, { explicit: grant, recheck: null }, write);
      expect(out).toBe(hex(addressOf(bob)));
      expect(write).toHaveBeenCalledTimes(1);
      // Signed with the grant.
      const auth = (write.mock.calls[0] as unknown as [unknown, { signer: unknown }])[1];
      expect(auth.signer).not.toBeNull();
    } finally {
      grant.free();
      bob.free();
    }
  });

  it("is REFUSED when the binding does not verify against the person's key: nothing is written", async () => {
    const bob = person(0x46);
    const mallory = person(0x47);
    // Mallory's grant presented as if it were Bob's.
    const forged = contactGrantOf(mallory);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const write = vi.fn(async () => 1);
      await expect(
        writeToPerson(bob.signaturePublicKey, { explicit: forged, recheck: null }, write),
      ).rejects.toBeInstanceOf(MailboxUnreachableError);
      expect(write).not.toHaveBeenCalled();
      // Logged, without identifiers.
      const lines = warn.mock.calls.map((c) => String(c[0]));
      expect(lines.some((l) => l.includes("ADR-0078"))).toBe(true);
      for (const l of lines) {
        expect(l).not.toContain(hex(bob.signaturePublicKey));
        expect(l).not.toContain(hex(addressOf(mallory)));
      }
    } finally {
      warn.mockRestore();
      forged.free();
      bob.free();
      mallory.free();
    }
  });

  it("an unsigned write to a person in Everyone needs their verified binding; without one nothing is written", async () => {
    const bob = person(0x48);
    const mallory = person(0x49);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const write = vi.fn(async (target: { address: Uint8Array }, auth: { signer?: unknown }) => {
        expect(auth.signer ?? null).toBeNull();
        return hex(target.address);
      });
      const out = await writeToPerson(
        bob.signaturePublicKey,
        { openBinding: wasm.signMailboxBinding(bob), recheck: null },
        write,
      );
      expect(out).toBe(hex(addressOf(bob)));
      // No binding at all: no candidate, nothing written.
      const none = vi.fn(async () => 1);
      await expect(writeToPerson(bob.signaturePublicKey, { recheck: null }, none)).rejects.toBeInstanceOf(
        MailboxUnreachableError,
      );
      expect(none).not.toHaveBeenCalled();
      // Mallory's binding under Bob's name: refused.
      const forged = vi.fn(async () => 1);
      await expect(
        writeToPerson(bob.signaturePublicKey, { openBinding: wasm.signMailboxBinding(mallory), recheck: null }, forged),
      ).rejects.toBeInstanceOf(MailboxUnreachableError);
      expect(forged).not.toHaveBeenCalled();
      // A Welcome is never written unsigned, even with a binding in hand.
      const welcome = vi.fn(async () => 1);
      await expect(
        writeToPerson(
          bob.signaturePublicKey,
          { openBinding: wasm.signMailboxBinding(bob), neverUnsigned: true, recheck: null },
          welcome,
        ),
      ).rejects.toBeInstanceOf(MailboxUnreachableError);
      expect(welcome).not.toHaveBeenCalled();
    } finally {
      warn.mockRestore();
      bob.free();
      mallory.free();
    }
  });
});

describe("grants kept at receipt", () => {
  it("a profile's grants are kept only when their binding verifies against the sender's key", () => {
    const sender = person(0x4a);
    const other = person(0x4b);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const own = contactGrantOf(sender);
    const foreign = contactGrantOf(other);
    try {
      const wires = [
        { classByte: wasm.ReachClass.Contact, version: 0, wireB64: b64(wasm.encodeReachGrant(own)) },
        { classByte: wasm.ReachClass.Contact, version: 0, wireB64: b64(wasm.encodeReachGrant(foreign)) },
        { classByte: wasm.ReachClass.Contact, version: 0, wireB64: "AAAA" },
      ];
      const kept = grantsWithVerifiedBindings(wasm, wires, [sender.signaturePublicKey]);
      expect(kept.map((g) => g.wireB64)).toEqual([wires[0]!.wireB64]);
      expect(warn.mock.calls.some((c) => String(c[0]).includes("ADR-0078"))).toBe(true);
    } finally {
      warn.mockRestore();
      own.free();
      foreign.free();
      sender.free();
      other.free();
    }
  });

  it("a join reply grant is kept only when it names the joiner's own mailbox", () => {
    const joiner = person(0x4c);
    const member = person(0x4d);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const reply = wasm.deriveReachGrant(joiner.ed25519Seed, wasm.ReachClass.Reply, new Uint8Array(32).fill(1), 0);
    const swapped = wasm.deriveReachGrant(member.ed25519Seed, wasm.ReachClass.Reply, new Uint8Array(32).fill(1), 0);
    try {
      expect(joinReplyBindingVerifies(reply, joiner.signaturePublicKey)).toBe(true);
      // A member handing out its own mailbox under the joiner's name.
      expect(joinReplyBindingVerifies(swapped, joiner.signaturePublicKey)).toBe(false);
      // An account key that could not be resolved: not kept.
      expect(joinReplyBindingVerifies(reply, undefined)).toBe(false);
    } finally {
      warn.mockRestore();
      reply.free();
      swapped.free();
      joiner.free();
      member.free();
    }
  });
});

describe("a /c/ call link's grant", () => {
  it("is checked against the owner's ACCOUNT key (the KeyPackage's binding first, else the signing key)", () => {
    const owner = person(0x4e);
    const mallory = person(0x4f);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    try {
      const ownGrant = (): wasm.ReachGrant =>
        wasm.deriveReachGrant(owner.ed25519Seed, wasm.ReachClass.Link, new Uint8Array(16).fill(2), 0);
      const fakeWasm = { keyPackageAccountKey: (): Uint8Array | undefined => undefined };
      const cap = (grant: wasm.ReachGrant, signer: Uint8Array) => ({
        oneTimeKeyPackage: new Uint8Array([1]),
        ownerSignaturePubkey: signer,
        linkGrant: grant,
      });
      expect(callLinkGrantVerifies(fakeWasm, cap(ownGrant(), owner.signaturePublicKey))).toBe(true);
      // The owner key does not match the grant's binding: refused.
      expect(callLinkGrantVerifies(fakeWasm, cap(ownGrant(), mallory.signaturePublicKey))).toBe(false);
      // A bound KeyPackage names the account key, which is what is checked.
      const bound = { keyPackageAccountKey: (): Uint8Array | undefined => owner.signaturePublicKey };
      expect(
        hex(callLinkOwnerAccountKey(bound, cap(ownGrant(), mallory.signaturePublicKey))),
      ).toBe(hex(owner.signaturePublicKey));
      expect(callLinkGrantVerifies(bound, cap(ownGrant(), mallory.signaturePublicKey))).toBe(true);
    } finally {
      warn.mockRestore();
      owner.free();
      mallory.free();
    }
  });
});
