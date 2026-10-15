// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * ADR-0078 phase 1 - the session claims ONE mailbox per account identity: the
 * address of the account's MAILBOX key, claimed with `{"mailbox_pk_b64"}`.
 * No device mailbox is claimed or installed (the slot no longer exists), and
 * nothing names a mailbox after an identity key.
 */
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as wasm from "../../wasm/lakko_ffi_wasm";

vi.mock("../wasm", () => ({ loadWasm: async () => wasm }));

import { clearDsAccess, installMailboxAccess } from "./session-access";
import { dsAccess } from "./access";

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

afterEach(() => {
  clearDsAccess();
  vi.unstubAllGlobals();
});

async function settle(): Promise<void> {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
}

describe("the session's mailbox claim", () => {
  it("claims the mailbox key's address with the mailbox key, once", async () => {
    const calls: { url: string; method: string; body: string }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push({
          url: String(url),
          method: init?.method ?? "GET",
          body: new TextDecoder().decode(init?.body as ArrayBuffer),
        });
        return new Response(null, { status: 204 });
      }),
    );
    const account = wasm.Identity.fromSeed(new Uint8Array(64).fill(0x51));
    const key = wasm.MailboxKey.fromAccountIdentity(account);
    try {
      installMailboxAccess("account", account);
      await settle();
      const addressHex = Buffer.from(key.address()).toString("hex");
      expect(calls).toHaveLength(1);
      expect(calls[0]?.method).toBe("PUT");
      expect(calls[0]?.url).toBe(`/api/ds/ds/mailbox/${addressHex}/access`);
      expect(JSON.parse(calls[0]!.body)).toEqual({
        mailbox_pk_b64: Buffer.from(key.publicKey()).toString("base64"),
      });
      // The registry signs reads of exactly that address, and of no address
      // derived from the identity key.
      expect(dsAccess.mailboxSigner(key.address())).not.toBeNull();
      const identityDerived = wasm.IdentityPseudonym.fromIdentity(account.signaturePublicKey).asBytes();
      expect(dsAccess.mailboxSigner(identityDerived)).toBeNull();
    } finally {
      key.free();
      account.free();
    }
  });

  it("has no device slot: a device identity is never claimed (a type error, and nothing at runtime)", () => {
    // @ts-expect-error ADR-0078 removed the device mailbox slot.
    const slot: Parameters<typeof installMailboxAccess>[0] = "device";
    expect(slot).toBe("device");
  });
});
