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
 *
 * ## Desktop (ADR-0085 section 2)
 *
 * In the desktop build (`isDesktop()`) nothing is derived in the page: the
 * input bytes go to the Tauri host (`hostDeriveDeviceKey`), which derives
 * the key with the IDENTICAL HKDF-SHA-512 parameters below (the same salt
 * bytes, the same `info` prefix per source followed by the account id
 * base64 string, 32-byte output) and returns a handle. The host must use
 * these exact strings, or a store written by one shape will not open under
 * the other:
 *
 * ```text
 *   salt                       = "lakko-device-root-v1"
 *   info  (source export-key)    = "lakko/device-root/v1|" || account_id_b64
 *   info  (source identity-seed) = "lakko/device-root/from-identity-seed/v1|" || account_id_b64
 * ```
 *
 * Both functions return a {@link DeviceCipher}; the web shape wraps the
 * non-extractable `CryptoKey` in a {@link WebDeviceCipher}.
 */

import { isDesktop } from "../platform";
import {
  type DeviceCipher,
  WebDeviceCipher,
  hostDeriveDeviceKey,
} from "./device-cipher";

/** HKDF salt (ASCII). The desktop host uses the same bytes. */
export const HKDF_SALT_STRING = "lakko-device-root-v1";

const HKDF_SALT = new TextEncoder().encode(HKDF_SALT_STRING);

/** Fixed HKDF info-prefix; the account_id_b64 string is appended. */
export const HKDF_INFO_PREFIX = "lakko/device-root/v1|";

/** ADR-0018 factor (a), path (b): distinct HKDF info for a device
 *  root key derived from the 64-byte AccountIdentity (phrase) seed
 *  instead of the OPAQUE export key — used to bootstrap a device
 *  that was restored via another device and has NO password (so no
 *  export key) at bootstrap time. A DISTINCT domain string keeps
 *  the two derivations cryptographically separated even though the
 *  inputs differ anyway. */
export const HKDF_INFO_PREFIX_IDENTITY_SEED =
  "lakko/device-root/from-identity-seed/v1|";

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
 * @returns               The account's device cipher: a
 *                        non-extractable AES-256-GCM `CryptoKey` on
 *                        the web, a host handle on the desktop.
 */
export async function deriveDeviceRootKey(
  exportKey: Uint8Array,
  accountIdB64: string,
): Promise<DeviceCipher> {
  if (isDesktop()) {
    return hostDeriveDeviceKey(accountIdB64, "export-key", exportKey);
  }
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
  const key = await crypto.subtle.deriveKey(
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
  return new WebDeviceCipher(key);
}

/**
 * ADR-0018 factor (a), path (b) — derive the device root key from
 * the 64-byte AccountIdentity (BIP39 phrase) seed.
 *
 * Same HKDF-SHA-512 shape as [`deriveDeviceRootKey`], with a
 * DISTINCT `info` domain string
 * (`lakko/device-root/from-identity-seed/v1`). Used exactly once
 * per device lifetime: bootstrapping a fresh device restored via
 * another device, where no password (hence no OPAQUE export key)
 * exists yet. The resulting non-extractable key is `putDeviceKey`'d
 * and every later login on the device PREFERS the stored key over
 * re-deriving, so the export-key path never clobbers it.
 *
 * @param accountIdentitySeed  64-byte BIP39 phrase seed (ADR-0017).
 *                             Caller wipes its copy after this call.
 * @param accountIdB64         Base64 16-byte account id, mixed into
 *                             the HKDF `info`.
 */
export async function deriveDeviceRootKeyFromIdentitySeed(
  accountIdentitySeed: Uint8Array,
  accountIdB64: string,
): Promise<DeviceCipher> {
  if (isDesktop()) {
    return hostDeriveDeviceKey(accountIdB64, "identity-seed", accountIdentitySeed);
  }
  const baseKey = await crypto.subtle.importKey(
    "raw",
    accountIdentitySeed.slice(),
    { name: "HKDF" },
    false,
    ["deriveKey"],
  );
  const info = new TextEncoder().encode(
    HKDF_INFO_PREFIX_IDENTITY_SEED + accountIdB64,
  );
  const key = await crypto.subtle.deriveKey(
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
  return new WebDeviceCipher(key);
}
