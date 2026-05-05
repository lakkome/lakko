// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Device root key derivation.
 *
 * Inputs and outputs:
 *
 *   * **Input** — the OPAQUE password export key (64 bytes, fresh
 *     out of `OpaqueRegister.finish` / `OpaqueLogin.finish`).
 *   * **Output** — a non-extractable AES-256-GCM `CryptoKey`
 *     bound to a single account id and pinned to the
 *     `lakko/device-root/v1` HKDF label.
 *
 * The derivation is HKDF-SHA-512:
 *
 * ```text
 *   IKM    = export_key            // 64 bytes
 *   salt   = "lakko-device-root-v1" // ASCII-only, fixed
 *   info   = "lakko/device-root/v1|" || account_id_b64
 *   length = 32 bytes (AES-256)
 * ```
 *
 * The derived key is wrapped as a `CryptoKey` with
 * `extractable: false` — once it lands in WebCrypto, the bytes
 * never leave the keystore. JavaScript can use the key (encrypt,
 * decrypt, derive new keys via HKDF chaining) but cannot read
 * the raw bytes — this is what makes `localStorage`-stuffing
 * attacks impossible.
 *
 * IndexedDB persistence is layered on top: a `CryptoKey` with
 * `extractable: false` can still be stored in IDB via the
 * structured-clone algorithm (Chrome / Firefox / Safari all
 * support this since 2017). The browser keeps the key material
 * inside the WebCrypto sandbox; only the opaque handle is
 * persisted.
 *
 * The OPAQUE export key bytes themselves drop out of memory the
 * moment this function returns. Callers MUST NOT cache the
 * `exportKey` argument for any longer than necessary.
 */

const HKDF_SALT = new TextEncoder().encode("lakko-device-root-v1");

/** Fixed HKDF info-prefix; the account_id_b64 string is appended. */
const HKDF_INFO_PREFIX = "lakko/device-root/v1|";

/**
 * Derive the non-extractable device root key for a given account.
 *
 * @param exportKey       OPAQUE password export key bytes (64 bytes
 *                        for `LakkoCipherSuite`). Caller is
 *                        responsible for wiping after this call —
 *                        this function does not retain a reference.
 * @param accountIdB64    Base64-encoded raw 16-byte UUIDv7 account
 *                        id. Mixed into the HKDF `info` so two
 *                        accounts on the same device derive
 *                        different keys.
 * @returns               Non-extractable AES-256-GCM `CryptoKey`.
 */
export async function deriveDeviceRootKey(
  exportKey: Uint8Array,
  accountIdB64: string,
): Promise<CryptoKey> {
  // 1. Import the export key as an HKDF base. `extractable: false`
  //    here means the base key is never extractable either —
  //    we only ever use it to derive the device root key below.
  //    We `.slice()` the input to guarantee an `ArrayBuffer`
  //    (not `SharedArrayBuffer`) backing — TS strict mode
  //    requires the narrower type for `BufferSource`.
  const baseKey = await crypto.subtle.importKey(
    "raw",
    exportKey.slice(),
    { name: "HKDF" },
    false,
    ["deriveKey"],
  );

  // 2. Derive the AES-256-GCM device root key. `extractable: false`
  //    so the bytes stay inside WebCrypto. `usages: ["encrypt",
  //    "decrypt"]` — the consumer is `lakko-storage`'s eventual
  //    IDB-backed `EncryptedStore` (1.13c).
  const info = new TextEncoder().encode(HKDF_INFO_PREFIX + accountIdB64);
  return crypto.subtle.deriveKey(
    {
      name: "HKDF",
      hash: "SHA-512",
      salt: HKDF_SALT.slice(),
      info: info.slice(),
    },
    baseKey,
    { name: "AES-GCM", length: 256 },
    /* extractable */ false,
    ["encrypt", "decrypt"],
  );
}
