// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Thin Swift wrappers over the UniFFI pairing-QR codec
/// (Phase 2a3b3 iOS infrastructure).
///
/// Mirror of `apps/web/src/devices/pairing.ts`. Two free
/// functions + a small random-pseudonym helper:
///
///   * [`buildPairingQrPayload`] — fresh-device side. Build
///     the wire bytes the CoreImage `CIFilter.qrCodeGenerator`
///     turns into pixels.
///   * [`parsePairingQrPayload`] — scanner side. Take the
///     bytes the `AVCaptureMetadataOutput` QR pipeline handed
///     us, verify the embedded account_id, return the
///     decoded contents.
///
/// The actual UI sits on top of these in
/// `apps/ios/Lakko/UI/Devices/DevicesScreen.swift`
/// (2a3b3-ui).
public enum PairingCodec {

    /// 32-byte width matching `lakko_mls::pairing::RENDEZVOUS_PSEUDONYM_LEN`.
    public static let rendezvousPseudonymLength: Int = 32

    /// Generate a fresh random 32-byte rendezvous pseudonym
    /// for a single add-device pairing session.
    public static func generateRendezvousPseudonym() -> Data {
        var bytes = Data(count: rendezvousPseudonymLength)
        _ = bytes.withUnsafeMutableBytes { ptr -> OSStatus in
            guard let base = ptr.baseAddress else { return -1 }
            return SecRandomCopyBytes(
                kSecRandomDefault,
                rendezvousPseudonymLength,
                base
            )
        }
        return bytes
    }

    /// Build the wire-format pairing QR payload.
    ///
    /// `accountId` is the raw 16-byte UUIDv7. `keyPackage` is
    /// the TLS-encoded `KeyPackage` from the joining device's
    /// DeviceIdentity. `rendezvousPseudonym` is the 32-byte
    /// slot from [`generateRendezvousPseudonym`].
    public static func buildPairingQrPayload(
        accountId: Data,
        keyPackage: Data,
        rendezvousPseudonym: Data
    ) throws -> Data {
        return try buildPairingQr(
            accountId: accountId,
            keyPackage: keyPackage,
            rendezvousPseudonym: rendezvousPseudonym
        )
    }

    /// Parse a pairing QR payload + verify it's for the
    /// local account. Throws on:
    ///   * wrong-length `expectedAccountId`,
    ///   * malformed wire bytes,
    ///   * wrong embedded `account_id` (scanned a QR from a
    ///     different account's session),
    ///   * newer-version payload than this binary recognises.
    public static func parsePairingQrPayload(
        wireBytes: Data,
        expectedAccountId: Data
    ) throws -> DecodedPairingQr {
        return try parsePairingQr(
            wire: wireBytes,
            expectedAccountId: expectedAccountId
        )
    }
}
