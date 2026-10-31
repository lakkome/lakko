// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The two shapes of the device root key (ADR-0085 section 2): the WebCrypto
 * one writes exactly what a direct `crypto.subtle` call writes, and the host
 * one sends `iv || data` to the host and maps its answers.
 */
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  HOST_DEVICE_KEY_PATH,
  HostDeviceCipher,
  KeyStoreUnavailableError,
  WebDeviceCipher,
  hostDeriveDeviceKey,
  hostGetDeviceKey,
} from "./device-cipher";

async function aesKey(): Promise<CryptoKey> {
  return crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}

const IV = new Uint8Array(12).map((_, i) => i + 1);
const PLAIN = new TextEncoder().encode("device-root-key round trip");

describe("WebDeviceCipher", () => {
  it("round-trips", async () => {
    const cipher = new WebDeviceCipher(await aesKey());
    const ct = new Uint8Array(await cipher.encrypt(IV, PLAIN));
    expect(ct.length).toBe(PLAIN.length + 16);
    const pt = new Uint8Array(await cipher.decrypt(IV, ct));
    expect(pt).toEqual(PLAIN);
  });

  it("writes the same bytes as a direct crypto.subtle call with the same key and IV", async () => {
    const key = await aesKey();
    const viaCipher = new Uint8Array(await new WebDeviceCipher(key).encrypt(IV, PLAIN));
    const direct = new Uint8Array(
      await crypto.subtle.encrypt({ name: "AES-GCM", iv: IV.slice() }, key, PLAIN.slice()),
    );
    expect(viaCipher).toEqual(direct);
    // And a direct decrypt opens what the cipher wrote.
    const opened = new Uint8Array(
      await crypto.subtle.decrypt({ name: "AES-GCM", iv: IV.slice() }, key, viaCipher.slice()),
    );
    expect(opened).toEqual(PLAIN);
  });

  it("rejects a tampered ciphertext", async () => {
    const cipher = new WebDeviceCipher(await aesKey());
    const ct = new Uint8Array(await cipher.encrypt(IV, PLAIN));
    ct[0] = (ct[0] ?? 0) ^ 1;
    await expect(cipher.decrypt(IV, ct)).rejects.toBeDefined();
  });
});

describe("HostDeviceCipher", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function stubFetch(
    respond: (url: string, init: RequestInit) => Response,
  ): { calls: { url: string; init: RequestInit }[] } {
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        calls.push({ url, init });
        return respond(url, init);
      }),
    );
    return { calls };
  }

  it("sends iv || data to the host's encrypt route with the handle", async () => {
    const reply = new Uint8Array([9, 8, 7]);
    const { calls } = stubFetch(() => new Response(reply, { status: 200 }));
    const out = new Uint8Array(await new HostDeviceCipher("h/1 x").encrypt(IV, PLAIN));
    expect(out).toEqual(reply);
    expect(calls).toHaveLength(1);
    const call = calls[0];
    if (call === undefined) throw new Error("no call");
    expect(call.url).toBe(`${HOST_DEVICE_KEY_PATH}/encrypt?handle=h%2F1%20x`);
    expect(call.init.method).toBe("POST");
    const body = call.init.body as Uint8Array;
    const expected = new Uint8Array(IV.length + PLAIN.length);
    expected.set(IV, 0);
    expected.set(PLAIN, IV.length);
    expect(new Uint8Array(body)).toEqual(expected);
  });

  it("maps a 400 decrypt to the OperationError a WebCrypto tag failure raises", async () => {
    stubFetch(() => new Response(null, { status: 400 }));
    const err = await new HostDeviceCipher("h").decrypt(IV, PLAIN).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DOMException);
    expect((err as DOMException).name).toBe("OperationError");
  });

  it("maps a 503 to KeyStoreUnavailableError", async () => {
    stubFetch(() => Response.json({ error: "no-keyring" }, { status: 503 }));
    const err = await new HostDeviceCipher("h").decrypt(IV, PLAIN).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(KeyStoreUnavailableError);
    expect((err as KeyStoreUnavailableError).reason).toBe("no-keyring");
    const err2 = await new HostDeviceCipher("h").encrypt(IV, PLAIN).catch((e: unknown) => e);
    expect(err2).toBeInstanceOf(KeyStoreUnavailableError);
  });

  it("refuses an IV that is not 12 bytes without calling the host", async () => {
    const { calls } = stubFetch(() => new Response(null, { status: 200 }));
    await expect(new HostDeviceCipher("h").encrypt(new Uint8Array(8), PLAIN)).rejects.toThrow(/12 bytes/);
    expect(calls).toHaveLength(0);
  });

  it("derive returns the handle the host issued; get maps 404 to null", async () => {
    stubFetch((url) =>
      url.endsWith("/derive")
        ? Response.json({ handle: "abc" })
        : new Response(null, { status: 404 }),
    );
    const cipher = await hostDeriveDeviceKey("acct", "export-key", new Uint8Array([1, 2, 3]));
    expect(cipher.handle).toBe("abc");
    expect(await hostGetDeviceKey("acct")).toBeNull();
  });
});
