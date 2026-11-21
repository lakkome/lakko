// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Security review 2026-11-21 - a contact request is never sealed to an
 * account X25519 key from the directory whose binding to the account's
 * signature key does not verify. Drives the real `ContactsClient` with a fake
 * directory; the seal itself is spied on so nothing is sent anywhere.
 */
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import init, * as realWasm from "../../wasm/lakko_ffi_wasm";
import type { LogVerifyResult } from "../transparency/verify";
import type { IdentityFetchResponse } from "../discovery/wire";
import type { DiscoveryClient } from "../discovery/client";

vi.mock("../transparency/verify", () => ({
  verifyIdentityInLog: async (): Promise<LogVerifyResult> => ({ status: "off", isReset: null }),
  verifyRotationChain: vi.fn(),
}));

const { ContactsClient, ContactsError } = await import("./client");
const { bindContactsAccount } = await import("./local-store");

class MemoryStorage {
  private map = new Map<string, string>();
  getItem(k: string): string | null {
    return this.map.get(k) ?? null;
  }
  setItem(k: string, v: string): void {
    this.map.set(k, String(v));
  }
  removeItem(k: string): void {
    this.map.delete(k);
  }
  clear(): void {
    this.map.clear();
  }
  get length(): number {
    return this.map.size;
  }
  key(i: number): string | null {
    return [...this.map.keys()][i] ?? null;
  }
}

function b64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

const ACCOUNT_ID = new Uint8Array(16).fill(5);

beforeAll(async () => {
  const path = fileURLToPath(new URL("../../wasm/lakko_ffi_wasm_bg.wasm", import.meta.url));
  await init({ module_or_path: readFileSync(path) });
});

beforeEach(() => {
  (globalThis as unknown as { localStorage: Storage }).localStorage = new MemoryStorage() as unknown as Storage;
  bindContactsAccount("dmlld2VyLWFjY291bnQ=");
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
  vi.spyOn(console, "info").mockImplementation(() => undefined);
});

afterEach(() => {
  bindContactsAccount(null);
  delete (globalThis as unknown as { localStorage?: Storage }).localStorage;
  vi.restoreAllMocks();
});

/** A person and the bundle a directory serves for them. */
function bundles(): { bound: IdentityFetchResponse; unbound: IdentityFetchResponse; swapped: IdentityFetchResponse; x: Uint8Array } {
  const person = realWasm.Identity.fromSeed(new Uint8Array(32).fill(0x21));
  const intruder = realWasm.Identity.fromSeed(new Uint8Array(32).fill(0x22));
  const base = {
    signature_pubkey: b64(person.signaturePublicKey),
    x25519_pubkey: b64(person.x25519PublicKey),
    key_generation: 1,
  };
  return {
    bound: { ...base, x25519_binding: b64(realWasm.signX25519Binding(person)) },
    unbound: base,
    swapped: {
      ...base,
      x25519_pubkey: b64(intruder.x25519PublicKey),
      x25519_binding: b64(realWasm.signX25519Binding(intruder)),
    },
    x: person.x25519PublicKey,
  };
}

function client(bundle: IdentityFetchResponse) {
  const discovery = {
    identityFetch: async () => bundle,
    lookup: async () => ({ account: ACCOUNT_ID, normalisedUsername: "someone", mailboxBinding: null }),
  } as unknown as DiscoveryClient;
  const c = new ContactsClient(discovery);
  const sealed: Uint8Array[] = [];
  vi.spyOn(
    c as unknown as { composeAndPublishContactRequest: (...a: unknown[]) => Promise<unknown> },
    "composeAndPublishContactRequest",
  ).mockImplementation(async (_identity, _sig, x) => {
    sealed.push(x as Uint8Array);
    return { messageId: 1, createdAt: "" };
  });
  return { c, sealed };
}

describe("a contact request to a fetched bundle", () => {
  it("is sealed to the bundle's X25519 key when the binding verifies", async () => {
    const b = bundles();
    const { c, sealed } = client(b.bound);
    await c.sendContactRequest(null, "someone", new Uint8Array(0));
    expect(sealed.map((x) => [...x])).toEqual([[...b.x]]);
  });

  it("refuses, sealing nothing, when the binding is absent or names another key", async () => {
    for (const bundle of [bundles().unbound, bundles().swapped]) {
      const { c, sealed } = client(bundle);
      const err = await c.sendContactRequest(null, "someone", new Uint8Array(0)).catch((e: unknown) => e);
      expect(err).toBeInstanceOf(ContactsError);
      expect((err as InstanceType<typeof ContactsError>).variant.kind).toBe("no-identity-bundle");
      expect(sealed).toEqual([]);
    }
  });

  it("the re-send after a rotation sends nothing to an unbound key", async () => {
    const { c, sealed } = client(bundles().unbound);
    expect(await c.resendContactRequestAfterRotation(null, ACCOUNT_ID)).toBe("skipped");
    expect(sealed).toEqual([]);
  });
});
