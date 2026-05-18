// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import java.security.SecureRandom
import uniffi.lakko_ffi_uniffi.DecodedPairingQr
import uniffi.lakko_ffi_uniffi.buildPairingQr
import uniffi.lakko_ffi_uniffi.parsePairingQr

/**
 * Thin Kotlin wrappers over the UniFFI pairing-QR codec
 * (Phase 2a3b4 Android infrastructure).
 *
 * Mirror of:
 *
 *   * `apps/web/src/devices/pairing.ts`
 *   * `apps/ios/Lakko/Devices/PairingCodec.swift`
 *
 * Two free functions + a small random-pseudonym helper:
 *
 *   * [buildPairingQrPayload] — fresh-device side. Build the
 *     wire bytes the ZXing `MultiFormatWriter` turns into pixels.
 *   * [parsePairingQrPayload] — scanner side. Take the bytes the
 *     ML Kit `BarcodeScanner` handed us (base64url-decoded
 *     upstream), verify the embedded `account_id`, return the
 *     decoded contents.
 *
 * The UI sits on top of these in
 * `apps/android/.../ui/devices/DevicesScreen.kt`.
 */
public object PairingCodec {

    /** 32-byte width matching
     *  `lakko_mls::pairing::RENDEZVOUS_PSEUDONYM_LEN`. */
    public const val RENDEZVOUS_PSEUDONYM_LEN: Int = 32

    /** Generate a fresh random 32-byte rendezvous pseudonym for
     *  a single add-device pairing session. Uses
     *  [SecureRandom] — Android's `SecureRandom` is seeded from
     *  `/dev/urandom` (kernel CSPRNG) since API 26. */
    public fun generateRendezvousPseudonym(): ByteArray {
        val bytes = ByteArray(RENDEZVOUS_PSEUDONYM_LEN)
        SecureRandom().nextBytes(bytes)
        return bytes
    }

    /**
     * Build the wire-format pairing QR payload.
     *
     * `accountId` is the raw 16-byte UUIDv7. `keyPackage` is
     * the TLS-encoded `KeyPackage` from the joining device's
     * DeviceIdentity. `rendezvousPseudonym` is the 32-byte slot
     * from [generateRendezvousPseudonym].
     *
     * Throws the underlying `FfiException` on wrong-length
     * `accountId` / `rendezvousPseudonym` — these are
     * programmer errors that bubble straight to the caller.
     */
    public fun buildPairingQrPayload(
        accountId: ByteArray,
        keyPackage: ByteArray,
        rendezvousPseudonym: ByteArray,
    ): ByteArray = buildPairingQr(
        accountId = accountId,
        keyPackage = keyPackage,
        rendezvousPseudonym = rendezvousPseudonym,
    )

    /**
     * Parse a pairing QR payload + verify it's for the local
     * account. Throws `FfiException` on:
     *   * wrong-length `expectedAccountId`,
     *   * malformed wire bytes,
     *   * wrong embedded `account_id` (scanned a QR from a
     *     different account's session),
     *   * newer-version payload than this binary recognises.
     */
    public fun parsePairingQrPayload(
        wireBytes: ByteArray,
        expectedAccountId: ByteArray,
    ): DecodedPairingQr = parsePairingQr(
        wire = wireBytes,
        expectedAccountId = expectedAccountId,
    )
}
