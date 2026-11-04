// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The per-account device root key, seen only through what it does
 * (ADR-0085 section 2, decision 9.1).
 *
 * Every local store is AES-256-GCM under one key per account, with a 12-byte
 * IV and no additional data. In the browser that key is a non-extractable
 * WebCrypto `CryptoKey` kept in IndexedDB. In the desktop app the key never
 * enters the page: the Tauri host derives it, keeps it wrapped by the
 * operating system, and does the AES-GCM itself, so the page holds only a
 * handle.
 *
 * Both shapes answer the same two calls with the same bytes as
 * `crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, plaintext)` /
 * `decrypt`: the ciphertext is the AES-GCM output (ciphertext followed by the
 * 16-byte tag), so stores written by either read the same.
 */

/** AES-GCM IV length every caller uses. */
export const DEVICE_CIPHER_IV_LEN = 12;

/** The device root key of one account. */
export interface DeviceCipher {
  /** AES-256-GCM seal; returns ciphertext followed by the tag. */
  encrypt(iv: Uint8Array, plaintext: Uint8Array): Promise<ArrayBuffer>;
  /** AES-256-GCM open; rejects when the tag does not verify. */
  decrypt(iv: Uint8Array, ciphertext: Uint8Array): Promise<ArrayBuffer>;
}

/** The browser shape: a non-extractable WebCrypto key. */
export class WebDeviceCipher implements DeviceCipher {
  /** The key itself; only `auth/key-store.ts` reads it, to persist it. */
  readonly cryptoKey: CryptoKey;

  constructor(cryptoKey: CryptoKey) {
    this.cryptoKey = cryptoKey;
  }

  encrypt(iv: Uint8Array, plaintext: Uint8Array): Promise<ArrayBuffer> {
    return crypto.subtle.encrypt(
      { name: "AES-GCM", iv: iv.slice() },
      this.cryptoKey,
      plaintext.slice(),
    );
  }

  decrypt(iv: Uint8Array, ciphertext: Uint8Array): Promise<ArrayBuffer> {
    return crypto.subtle.decrypt(
      { name: "AES-GCM", iv: iv.slice() },
      this.cryptoKey,
      ciphertext.slice(),
    );
  }
}

/**
 * The host refused because it has no key store it can use: no keychain, a
 * locked one the person declined, or an OS the host does not support yet.
 * Sign-in stops on it (`docs/ux/desktop.md` 1.1); nothing falls back to
 * IndexedDB (invariant #7, fail-closed).
 */
export class KeyStoreUnavailableError extends Error {
  readonly reason: string;
  constructor(reason: string) {
    super(`device key store unavailable: ${reason}`);
    this.name = "KeyStoreUnavailableError";
    this.reason = reason;
  }
}

/** Where the host answers device-key requests (served by its URI handler). */
export const HOST_DEVICE_KEY_PATH = "/__host/device-key";

/** What may derive a device root key; the HKDF `info` differs per source. */
export type DeviceKeySource = "export-key" | "identity-seed";

function b64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

async function hostError(res: Response): Promise<Error> {
  let code = "";
  try {
    const body = (await res.json()) as { error?: unknown };
    if (typeof body.error === "string") code = body.error;
  } catch {
    // not JSON
  }
  if (res.status === 503) return new KeyStoreUnavailableError(code || "unavailable");
  return new Error(`host device key: ${res.status} ${code}`.trim());
}

async function postJson(op: string, body: unknown): Promise<Response> {
  return fetch(`${HOST_DEVICE_KEY_PATH}/${op}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

/** The desktop shape: a handle to a key held by the host. */
export class HostDeviceCipher implements DeviceCipher {
  /** Opaque, process-lifetime handle the host issued. */
  readonly handle: string;

  constructor(handle: string) {
    this.handle = handle;
  }

  private async op(name: "encrypt" | "decrypt", iv: Uint8Array, data: Uint8Array): Promise<ArrayBuffer> {
    if (iv.length !== DEVICE_CIPHER_IV_LEN) throw new Error("device cipher: IV must be 12 bytes");
    const body = new Uint8Array(iv.length + data.length);
    body.set(iv, 0);
    body.set(data, iv.length);
    const res = await fetch(
      `${HOST_DEVICE_KEY_PATH}/${name}?handle=${encodeURIComponent(this.handle)}`,
      { method: "POST", headers: { "content-type": "application/octet-stream" }, body },
    );
    if (!res.ok) {
      if (name === "decrypt" && res.status === 400) {
        // Same shape a WebCrypto tag failure has.
        throw new DOMException("decryption failed", "OperationError");
      }
      throw await hostError(res);
    }
    return res.arrayBuffer();
  }

  encrypt(iv: Uint8Array, plaintext: Uint8Array): Promise<ArrayBuffer> {
    return this.op("encrypt", iv, plaintext);
  }

  decrypt(iv: Uint8Array, ciphertext: Uint8Array): Promise<ArrayBuffer> {
    return this.op("decrypt", iv, ciphertext);
  }
}

/** Ask the host to derive an account's device root key from `ikm` (the
 *  same HKDF-SHA-512 as `auth/device-key.ts`). Held in host memory until
 *  [`hostPutDeviceKey`] or process exit. */
export async function hostDeriveDeviceKey(
  accountIdB64: string,
  source: DeviceKeySource,
  ikm: Uint8Array,
): Promise<HostDeviceCipher> {
  const res = await postJson("derive", { accountIdB64, source, ikmB64: b64(ikm) });
  if (!res.ok) throw await hostError(res);
  const body = (await res.json()) as { handle: string };
  return new HostDeviceCipher(body.handle);
}

/** Persist the key behind `cipher` as the account's device key, wrapped by
 *  the operating system. */
export async function hostPutDeviceKey(accountIdB64: string, cipher: HostDeviceCipher): Promise<void> {
  const res = await postJson("put", { accountIdB64, handle: cipher.handle });
  if (!res.ok) throw await hostError(res);
}

/** The account's stored device key, or `null` when there is none. */
export async function hostGetDeviceKey(accountIdB64: string): Promise<HostDeviceCipher | null> {
  const res = await postJson("get", { accountIdB64 });
  if (res.status === 404) return null;
  if (!res.ok) throw await hostError(res);
  const body = (await res.json()) as { handle: string };
  return new HostDeviceCipher(body.handle);
}

/** Erase the account's device key from the OS store and host memory. */
export async function hostDeleteDeviceKey(accountIdB64: string): Promise<void> {
  const res = await postJson("delete", { accountIdB64 });
  if (!res.ok && res.status !== 404) throw await hostError(res);
}
