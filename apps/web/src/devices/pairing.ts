// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Thin TS wrappers over the wasm pairing-QR codec
 * (Phase 2a3b2 web infrastructure).
 *
 * Two halves:
 *
 *   * [`buildPairingQrPayload`] — fresh-device side. Build the
 *     wire bytes a QR-rendering library then turns into pixels
 *     on screen.
 *   * [`parsePairingQrPayload`] — scanner side. Take wire bytes
 *     a QR-scanner library handed us and produce the typed
 *     `{keyPackage, rendezvousPseudonym}` pair the rest of the
 *     add-device flow consumes.
 *
 * The QR-rendering and QR-scanning libraries land in the
 * per-route UI sub-sprint (2a3b2-ui); this module is the
 * stateless codec layer on top of the wasm FFI.
 */
import { loadWasm } from "../wasm";

/** Length of the rendezvous pseudonym in bytes. Pinned at 32 —
 *  matches `lakko_mls::pairing::RENDEZVOUS_PSEUDONYM_LEN` on
 *  the Rust side, the routing-pseudonym width in
 *  `wire/v1.proto`, and every other pseudonym in Lakko. */
export const RENDEZVOUS_PSEUDONYM_LEN = 32;

/**
 * Generate a fresh random 32-byte rendezvous pseudonym for an
 * add-device pairing session.
 *
 * Called on the joining-device side once per pairing. The
 * pseudonym names the DS-mailbox slot the scanner PUTs the
 * Welcome to. Single-use — never reused across pairings.
 */
export function generateRendezvousPseudonym(): Uint8Array {
  return crypto.getRandomValues(new Uint8Array(RENDEZVOUS_PSEUDONYM_LEN));
}

/**
 * Build the wire-format pairing QR payload.
 *
 * `accountId` is the raw 16-byte UUIDv7. `keyPackage` is the
 * TLS-encoded `KeyPackage` from the joining device's
 * DeviceIdentity (Phase 2a3b0 / ADR-0007). `rendezvousPseudonym`
 * is the 32-byte slot from
 * [`generateRendezvousPseudonym`].
 *
 * Throws on wrong-length inputs — caller bugs surface
 * synchronously, not deep inside the QR-render call.
 */
export async function buildPairingQrPayload(
  accountId: Uint8Array,
  keyPackage: Uint8Array,
  rendezvousPseudonym: Uint8Array,
): Promise<Uint8Array> {
  if (accountId.length !== 16) {
    throw new Error(`accountId must be 16 bytes (got ${accountId.length})`);
  }
  if (rendezvousPseudonym.length !== RENDEZVOUS_PSEUDONYM_LEN) {
    throw new Error(
      `rendezvousPseudonym must be ${RENDEZVOUS_PSEUDONYM_LEN} bytes ` +
        `(got ${rendezvousPseudonym.length})`,
    );
  }
  const wasm = await loadWasm();
  return wasm.encodePairingQr(accountId, keyPackage, rendezvousPseudonym);
}

/** Result of [`parsePairingQrPayload`]. */
export interface DecodedPairingQr {
  /** TLS-encoded `KeyPackage` from the joining device's
   *  DeviceIdentity. Feed to `DeviceGroup.addDeviceFromWire`. */
  readonly keyPackage: Uint8Array;
  /** 32-byte rendezvous pseudonym. The DS-mailbox slot the
   *  scanner PUTs the resulting Welcome to. */
  readonly rendezvousPseudonym: Uint8Array;
}

/**
 * Parse a pairing QR payload + verify it's for the local
 * account. Throws on:
 *
 *   - wrong-length `expectedAccountId`,
 *   - malformed wire bytes (proto-decode failure),
 *   - wrong embedded `account_id` (the user scanned a QR
 *     from a different account's session),
 *   - newer-version payload than this binary recognises.
 *
 * UI should branch on the thrown message string and surface
 * one of: "not a pairing QR", "this code is for a different
 * account", "this code is for a newer Lakko version". The
 * exact strings come from `lakko_mls::pairing::decode` and
 * are stable.
 */
export async function parsePairingQrPayload(
  wireBytes: Uint8Array,
  expectedAccountId: Uint8Array,
): Promise<DecodedPairingQr> {
  if (expectedAccountId.length !== 16) {
    throw new Error(
      `expectedAccountId must be 16 bytes (got ${expectedAccountId.length})`,
    );
  }
  const wasm = await loadWasm();
  const decoded = wasm.decodePairingQr(wireBytes, expectedAccountId);
  return {
    keyPackage: decoded.keyPackage,
    rendezvousPseudonym: decoded.rendezvousPseudonym,
  };
}
