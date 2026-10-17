// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0078 phase 1 - every mailbox row is sealed to the owner's mailbox key,
 * with the kind inside the seal; the owner opens each row and dispatches by
 * kind, and a row that does not open is dropped. Driven through the REAL wasm
 * core (`sealMailboxRow` / `openMailboxRow`).
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

import { openOwnRow, sealRowFor, type MailboxRowKind, type MailboxTarget } from "./mailbox-row";
import { dispatchMailboxRows } from "./client";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

/** The verified target of `owner`'s mailbox, as a writer reaches it: the
 *  owner's binding verified against the owner's key. */
function targetOf(owner: wasm.Identity): MailboxTarget {
  const mailboxPk = wasm.verifyMailboxBinding(wasm.signMailboxBinding(owner), owner.signaturePublicKey);
  return { mailboxPk, address: wasm.mailboxAddress(mailboxPk) };
}

describe("sealing and opening one row", () => {
  it("every send kind opens to its own kind and the unchanged body; the clear bytes are a sealed row", () => {
    const bob = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x31));
    const key = wasm.MailboxKey.fromAccountIdentity(bob);
    try {
      const target = targetOf(bob);
      expect(Array.from(target.address)).toEqual(Array.from(key.address()));
      const kinds: MailboxRowKind[] = ["contact-request", "welcome", "contact-ack", "welcome-rendezvous"];
      for (const kind of kinds) {
        const body = new TextEncoder().encode(`body of ${kind}`);
        const row = sealRowFor(wasm, target, kind, body);
        expect(wasm.isWellFormedMailboxRow(row), kind).toBe(true);
        // The body does not appear in the clear, and no clear kind byte leads.
        expect(Buffer.from(row).includes(Buffer.from(body)), kind).toBe(false);
        const opened = openOwnRow(wasm, key, row);
        expect(opened?.kind, kind).toBe(kind);
        expect(Array.from(opened?.body ?? []), kind).toEqual(Array.from(body));
      }
    } finally {
      key.free();
      bob.free();
    }
  });

  it("a row sealed for another mailbox, a damaged row and garbage do not open", () => {
    const bob = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x32));
    const carol = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x33));
    const bobKey = wasm.MailboxKey.fromAccountIdentity(bob);
    try {
      const forCarol = sealRowFor(wasm, targetOf(carol), "welcome", new Uint8Array([1, 2, 3]));
      expect(openOwnRow(wasm, bobKey, forCarol)).toBeNull();
      const damaged = sealRowFor(wasm, targetOf(bob), "welcome", new Uint8Array([1, 2, 3]));
      damaged[damaged.length - 1] = (damaged[damaged.length - 1] ?? 0) ^ 0xff;
      expect(openOwnRow(wasm, bobKey, damaged)).toBeNull();
      expect(openOwnRow(wasm, bobKey, new Uint8Array([0x01, 2, 3]))).toBeNull();
      expect(openOwnRow(wasm, bobKey, new Uint8Array(0))).toBeNull();
    } finally {
      bobKey.free();
      bob.free();
      carol.free();
    }
  });
});

describe("dispatching the rows of one mailbox read", () => {
  it("opens each row, dispatches by kind, drops what does not open, and advances the cursor past all", () => {
    const alice = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x34));
    const bob = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x35));
    const mallory = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x36));
    const bobKey = wasm.MailboxKey.fromAccountIdentity(bob);
    try {
      const toBob = targetOf(bob);
      const welcome = new Uint8Array([9, 8, 7]);
      const ackWire = wasm.ContactAck.compose(
        alice.ed25519Seed,
        new Uint8Array(16).fill(1),
        alice.x25519PublicKey,
        new Uint8Array(32).fill(2),
        1n,
      );
      const messages = [
        { id: 11, payload: sealRowFor(wasm, toBob, "welcome", welcome), createdAt: "t1" },
        // Sealed for someone else: dropped.
        { id: 12, payload: sealRowFor(wasm, targetOf(mallory), "welcome", welcome), createdAt: "t2" },
        // Garbage: dropped.
        { id: 13, payload: new Uint8Array([1, 2, 3, 4]), createdAt: "t3" },
        { id: 14, payload: sealRowFor(wasm, toBob, "contact-ack", ackWire), createdAt: "t4" },
        // A Welcome rendezvous is the co-publisher flow's, never surfaced here.
        { id: 15, payload: sealRowFor(wasm, toBob, "welcome-rendezvous", welcome), createdAt: "t5" },
      ];
      const out = dispatchMailboxRows(wasm, bobKey, messages, 10, bob.x25519SecretKey);
      expect(out.cursor).toBe(15);
      expect(out.events.map((e) => e.kind)).toEqual(["welcome", "contact-ack"]);
      const first = out.events[0];
      if (first?.kind !== "welcome") throw new Error("expected a welcome");
      expect(Array.from(first.welcome.welcomeBytes)).toEqual(Array.from(welcome));
      expect(first.welcome.messageId).toBe(11);
      const second = out.events[1];
      if (second?.kind !== "contact-ack") throw new Error("expected an ack");
      expect(Array.from(second.ack.accepterPubkey)).toEqual(Array.from(alice.signaturePublicKey));

      // A predecessor mailbox delivers Welcomes only.
      const welcomesOnly = dispatchMailboxRows(wasm, bobKey, messages, 10, bob.x25519SecretKey, {
        welcomesOnly: true,
      });
      expect(welcomesOnly.events.map((e) => e.kind)).toEqual(["welcome"]);
      expect(welcomesOnly.cursor).toBe(15);
    } finally {
      bobKey.free();
      alice.free();
      bob.free();
      mallory.free();
    }
  });

  it("a contact-request row keeps its OPENED body for the pending inbox", () => {
    const alice = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x37));
    const bob = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x38));
    const bobKey = wasm.MailboxKey.fromAccountIdentity(bob);
    try {
      const wire = wasm.ContactRequest.compose(
        alice.ed25519Seed,
        new Uint8Array(16).fill(5),
        bob.x25519PublicKey,
        new Uint8Array(32).fill(6),
        BigInt(Date.now()),
        new Uint8Array(0),
        new Uint8Array(0),
        undefined,
        // ADR-0079 — no link answer.
        new Uint8Array(0),
        new Uint8Array(0),
        new TextEncoder().encode("hi"),
      );
      const row = sealRowFor(wasm, targetOf(bob), "contact-request", wire);
      const out = dispatchMailboxRows(wasm, bobKey, [{ id: 3, payload: row, createdAt: "t" }], 0, bob.x25519SecretKey);
      const ev = out.events[0];
      if (ev?.kind !== "contact-request") throw new Error("expected a contact request");
      expect(ev.request.intro).toBe("hi");
      // The kept row is the inner wire, not the sealed mailbox row.
      expect(Array.from(ev.row.payload)).toEqual(Array.from(wire));
    } finally {
      bobKey.free();
      alice.free();
      bob.free();
    }
  });
});
