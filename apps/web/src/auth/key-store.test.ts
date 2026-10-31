// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The device-key store on both platforms (ADR-0085 section 2): the web keeps
 * the bare `CryptoKey` record the service worker reads; the desktop asks the
 * store gate first and then the host, and a key-store refusal propagates.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { FakeIndexedDb, MemoryStorage } from "../test/fake-storage";
import {
  resetAccountStoreGateForTests,
  sealAccountStoresForWipe,
  takeWipeMark,
} from "./account-store-gate";
import { HostDeviceCipher, KeyStoreUnavailableError, WebDeviceCipher } from "./device-cipher";

const platform = vi.hoisted(() => ({ desktop: false }));
vi.mock("../platform", () => ({ isDesktop: (): boolean => platform.desktop }));

import { deleteDeviceKey, getDeviceKey, putDeviceKey } from "./key-store";
import { deriveDeviceRootKey, deriveDeviceRootKeyFromIdentitySeed } from "./device-key";

const A = "QUFBQUFBQUFBQUFBQUFBQQ==";

let idb: FakeIndexedDb;

beforeEach(() => {
  idb = new FakeIndexedDb();
  const g = globalThis as unknown as Record<string, unknown>;
  g["localStorage"] = new MemoryStorage();
  g["indexedDB"] = idb;
  resetAccountStoreGateForTests();
});

afterEach(() => {
  const g = globalThis as unknown as Record<string, unknown>;
  delete g["localStorage"];
  delete g["indexedDB"];
  resetAccountStoreGateForTests();
  vi.unstubAllGlobals();
  platform.desktop = false;
});

async function aesKey(): Promise<CryptoKey> {
  return crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}

describe("web store", () => {
  it("round-trips a WebDeviceCipher, storing the bare CryptoKey", async () => {
    const key = await aesKey();
    expect(await putDeviceKey(A, new WebDeviceCipher(key))).toBe(true);
    const back = await getDeviceKey(A);
    expect(back).toBeInstanceOf(WebDeviceCipher);
    // The record holds a CryptoKey (the service worker's format) and it is
    // the same key: what the original sealed, the stored one opens.
    expect((back as WebDeviceCipher).cryptoKey).toBeInstanceOf(CryptoKey);
    const iv = new Uint8Array(12);
    const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, new Uint8Array([5]));
    expect(new Uint8Array(await (back as WebDeviceCipher).decrypt(iv, new Uint8Array(ct)))).toEqual(
      new Uint8Array([5]),
    );
    await deleteDeviceKey(A);
    expect(await getDeviceKey(A)).toBeNull();
  });

  it("refuses a host cipher", async () => {
    await expect(putDeviceKey(A, new HostDeviceCipher("h"))).rejects.toThrow(/WebCrypto/);
  });

  it("refuses a write to a sealed account", async () => {
    sealAccountStoresForWipe([A]);
    expect(await putDeviceKey(A, new WebDeviceCipher(await aesKey()))).toBe(false);
  });
});

describe("desktop store", () => {
  function stubHost(respond: (op: string, body: unknown) => Response): string[] {
    const ops: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        const op = url.split("/").pop() ?? "";
        ops.push(op);
        return respond(op, JSON.parse(init.body as string) as unknown);
      }),
    );
    return ops;
  }

  beforeEach(() => {
    platform.desktop = true;
  });

  it("puts, gets and deletes through the host", async () => {
    const bodies: unknown[] = [];
    const ops = stubHost((op, body) => {
      bodies.push(body);
      if (op === "get") return Response.json({ handle: "stored" });
      return new Response(null, { status: 204 });
    });
    expect(await putDeviceKey(A, new HostDeviceCipher("fresh"))).toBe(true);
    const got = await getDeviceKey(A);
    expect(got).toBeInstanceOf(HostDeviceCipher);
    expect((got as HostDeviceCipher).handle).toBe("stored");
    await deleteDeviceKey(A);
    expect(ops).toEqual(["put", "get", "delete"]);
    expect(bodies[0]).toEqual({ accountIdB64: A, handle: "fresh" });
  });

  it("asks the store gate before the host and does not call it when refused", async () => {
    const ops = stubHost(() => new Response(null, { status: 204 }));
    sealAccountStoresForWipe([A]);
    expect(await putDeviceKey(A, new HostDeviceCipher("h"))).toBe(false);
    expect(ops).toEqual([]);
  });

  it("honours a wipe mark taken before the wipe", async () => {
    const ops = stubHost(() => new Response(null, { status: 204 }));
    const mark = takeWipeMark();
    sealAccountStoresForWipe([A]);
    expect(await putDeviceKey(A, new HostDeviceCipher("h"), mark)).toBe(false);
    expect(ops).toEqual([]);
  });

  it("refuses a WebCrypto cipher", async () => {
    stubHost(() => new Response(null, { status: 204 }));
    await expect(putDeviceKey(A, new WebDeviceCipher(await aesKey()))).rejects.toThrow(/host cipher/);
  });

  it("propagates a key-store refusal from get and put; it is never read as no key", async () => {
    stubHost(() => Response.json({ error: "locked" }, { status: 503 }));
    await expect(getDeviceKey(A)).rejects.toBeInstanceOf(KeyStoreUnavailableError);
    await expect(putDeviceKey(A, new HostDeviceCipher("h"))).rejects.toBeInstanceOf(
      KeyStoreUnavailableError,
    );
  });

  it("reads a host 404 as no key", async () => {
    stubHost(() => new Response(null, { status: 404 }));
    expect(await getDeviceKey(A)).toBeNull();
  });
});

describe("device-key derivation", () => {
  it("on the web returns a WebDeviceCipher that round-trips", async () => {
    const cipher = await deriveDeviceRootKey(new Uint8Array(64).fill(7), A);
    expect(cipher).toBeInstanceOf(WebDeviceCipher);
    const iv = new Uint8Array(12);
    const ct = new Uint8Array(await cipher.encrypt(iv, new Uint8Array([1, 2, 3])));
    expect(new Uint8Array(await cipher.decrypt(iv, ct))).toEqual(new Uint8Array([1, 2, 3]));
  });

  it("on the desktop derives nothing in the page and asks the host per source", async () => {
    platform.desktop = true;
    const bodies: unknown[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        expect(url).toBe("/__host/device-key/derive");
        bodies.push(JSON.parse(init.body as string));
        return Response.json({ handle: `h${bodies.length}` });
      }),
    );
    const subtle = vi.spyOn(crypto.subtle, "deriveKey");
    const a = await deriveDeviceRootKey(new Uint8Array([1, 2]), A);
    const b = await deriveDeviceRootKeyFromIdentitySeed(new Uint8Array([3]), A);
    expect(subtle).not.toHaveBeenCalled();
    subtle.mockRestore();
    expect(a).toBeInstanceOf(HostDeviceCipher);
    expect((b as HostDeviceCipher).handle).toBe("h2");
    expect(bodies).toEqual([
      { accountIdB64: A, source: "export-key", ikmB64: "AQI=" },
      { accountIdB64: A, source: "identity-seed", ikmB64: "Aw==" },
    ]);
  });
});
