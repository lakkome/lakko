// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Per-device push pseudonym — 32 random bytes generated on first
 * push registration and persisted in `localStorage` for the
 * lifetime of the device.
 *
 * ## Why a fresh random value, not a derivation from identity
 *
 * The push server sees `(device_pseudonym, provider) → token`.
 * Anyone who knew the pseudonym could post `/push/notify` with it
 * (gated by PoW; ADR-0054 deletes that route, no client ever called
 * it). To minimise what the server can link:
 *
 *   * `device_pseudonym = BLAKE3("lakko/push/device" || identity_pubkey)`
 *     would let the server correlate identity uploads with push
 *     registrations the moment a peer publishes a contact request.
 *
 *   * `device_pseudonym = SHA-256(device_root_key)` is better but
 *     the device root key lives in non-extractable WebCrypto and
 *     cannot leave the browser; we'd have to derive a separate
 *     KEK just for this, and even then the value would be stable
 *     across logouts on the same device.
 *
 *   * **Fresh random 32 bytes, persisted in localStorage**: server
 *     sees an opaque handle; clearing localStorage rotates the
 *     pseudonym (the user can do this from browser settings if
 *     they want a fresh push identity). `forgetDevice` clears the
 *     key alongside the rest of the per-device material.
 *
 * The pseudonym is **not** privacy-critical against a passive
 * server adversary — it's opaque randomness. It IS critical that
 * peers who message us know our current pseudonym so they can ask
 * the DS to fan out a wake. That cross-device pseudonym
 * distribution is server-side fan-out plumbing (out of scope for
 * 1.20b; see ROADMAP §1.20 unfinished items).
 *
 * ## Storage layout
 *
 * `localStorage["lakko.push.device.v1"]` = base64-encoded 32-byte
 * pseudonym. Same key format as the auth / contacts local-store
 * conventions for grep-ability.
 */

import { base64Decode, base64Encode } from "../auth/account-id";

const STORAGE_KEY = "lakko.push.device.v1";
const PSEUDONYM_LEN = 32;

/**
 * Return the per-device push pseudonym, generating one and
 * persisting it on first call. Subsequent calls return the same
 * bytes for the lifetime of the localStorage entry.
 *
 * Safe to call from any code path — generates lazily, no async.
 */
export function getOrCreateDevicePushId(): Uint8Array {
  const cached = readPersisted();
  if (cached !== null) return cached;
  const fresh = crypto.getRandomValues(new Uint8Array(PSEUDONYM_LEN));
  try {
    localStorage.setItem(STORAGE_KEY, base64Encode(fresh));
  } catch {
    // Quota / private-mode failures: we still hand back the
    // value, but subsequent calls won't be able to retrieve it.
    // That's OK — the registration flow tolerates a re-generated
    // pseudonym (it just shows up as a fresh registration row
    // server-side).
  }
  return fresh;
}

/** Drop the persisted pseudonym (called by `forgetDevice`). */
export function clearDevicePushId(): void {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    // Silent — same as above, the read path will just regenerate.
  }
}

/** Read-without-create accessor, for paths that want to query
 *  whether a pseudonym exists without forcing one. */
export function peekDevicePushId(): Uint8Array | null {
  return readPersisted();
}

function readPersisted(): Uint8Array | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) return null;
    const bytes = base64Decode(raw);
    if (bytes.length !== PSEUDONYM_LEN) {
      // Corrupted / wrong-shape entry — discard.
      localStorage.removeItem(STORAGE_KEY);
      return null;
    }
    return bytes;
  } catch {
    return null;
  }
}
