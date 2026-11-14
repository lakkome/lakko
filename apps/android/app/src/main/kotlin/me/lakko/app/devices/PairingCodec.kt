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
 *     upstream), verify the embedded login handle against this
 *     account's own, return the decoded contents.
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
     * Build the wire-format pairing payload (v2, ADR-0056).
     *
     * `loginHandle` is the 32-byte `loginHandle(username)` of the name
     * the person typed (never the account id: nothing before a session
     * carries it). `keyPackage` is the TLS-encoded `KeyPackage` from the
     * joining device's DeviceIdentity. `rendezvousPseudonym` is the
     * 32-byte slot from [generateRendezvousPseudonym].
     * `deviceCardEnvelope` is the joining device's own ADR-0080 card
     * envelope ([DeviceCardCenter.pairingCardEnvelope]), so the signed-in
     * device can name it (UX second-device.md section 2.1); display only,
     * `null` for none.
     *
     * Throws the underlying `FfiException.Codec` on a wrong-length
     * `loginHandle` / `rendezvousPseudonym` (programmer errors).
     */
    public fun buildPairingQrPayload(
        loginHandle: ByteArray,
        keyPackage: ByteArray,
        rendezvousPseudonym: ByteArray,
        deviceCardEnvelope: ByteArray? = null,
    ): ByteArray = buildPairingQr(
        loginHandle = loginHandle,
        keyPackage = keyPackage,
        rendezvousPseudonym = rendezvousPseudonym,
        deviceCardEnvelope = deviceCardEnvelope,
    )

    /**
     * ADR-0081 amendment — a DeviceGroup rejoin knock: the pairing payload
     * plus, when the journal names an epoch, this device's self-removal proof
     * (signed with its old leaf key, which is its device identity's key) for
     * `keyPackage`; an empty proof for none.
     *
     * G-55: the row is sealed under a key derived from `lineageSeed`, the
     * 64-byte seed the DeviceGroup id derives from
     * ([DeviceGroupLifecycle.lineageSeed]), and padded, so the Delivery
     * Service reads neither the login handle nor the KeyPackage. A sibling
     * opens it with [openRejoinKnockPayload].
     */
    public fun buildRejoinKnockPayload(
        lineageSeed: ByteArray,
        loginHandle: ByteArray,
        keyPackage: ByteArray,
        rendezvousPseudonym: ByteArray,
        selfRemovalProof: ByteArray,
    ): ByteArray = uniffi.lakko_ffi_uniffi.Identity.fromSeed(lineageSeed).use { lineage ->
        uniffi.lakko_ffi_uniffi.buildRejoinKnock(
            accountIdentity = lineage,
            loginHandle = loginHandle,
            keyPackage = keyPackage,
            rendezvousPseudonym = rendezvousPseudonym,
            selfRemovalProof = selfRemovalProof,
        )
    }

    /**
     * A sibling's side of [buildRejoinKnockPayload] (G-55): open a row read
     * from the account's DeviceGroup knock queue under `lineageSeed`, then
     * check the payload names this account's own handle, as
     * [parsePairingQrPayload] does. Throws `FfiException` for a row that is
     * not a sealed knock or does not open under this account's key.
     */
    public fun openRejoinKnockPayload(
        lineageSeed: ByteArray,
        row: ByteArray,
        expectedLoginHandle: ByteArray,
    ): DecodedPairingQr = uniffi.lakko_ffi_uniffi.Identity.fromSeed(lineageSeed).use { lineage ->
        uniffi.lakko_ffi_uniffi.openRejoinKnock(
            accountIdentity = lineage,
            row = row,
            expectedLoginHandle = expectedLoginHandle,
        )
    }

    /**
     * Parse a pairing payload and check it names this account: the
     * embedded login handle must equal `expectedLoginHandle`, the
     * handle of this account's own username. Throws `FfiException`:
     *   * `Codec` - wrong-length handle, malformed wire bytes,
     *   * `Unsupported` - a payload version other than 2,
     *   * `Group` ("login_handle mismatch") - the code was made for
     *     another account's name.
     */
    public fun parsePairingQrPayload(
        wireBytes: ByteArray,
        expectedLoginHandle: ByteArray,
    ): DecodedPairingQr = parsePairingQr(
        wire = wireBytes,
        expectedLoginHandle = expectedLoginHandle,
    )
}
