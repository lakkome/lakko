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
///     us, verify the embedded login handle names this device's
///     own account (ADR-0056), return the decoded contents.
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

    /// Build the wire-format pairing QR payload (ADR-0056 v2).
    ///
    /// `loginHandle` is the 32-byte handle of the username the person
    /// typed (or the signed-in account's own name), from the core's
    /// `loginHandle`: the payload names the account by name, never by
    /// account id, so a fresh device needs no lookup before a session.
    /// `keyPackage` is the TLS-encoded `KeyPackage` from the joining
    /// device's DeviceIdentity. `rendezvousPseudonym` is the 32-byte slot
    /// from [`generateRendezvousPseudonym`].
    public static func buildPairingQrPayload(
        loginHandle: Data,
        keyPackage: Data,
        rendezvousPseudonym: Data
    ) throws -> Data {
        return try buildPairingQr(
            loginHandle: loginHandle,
            keyPackage: keyPackage,
            rendezvousPseudonym: rendezvousPseudonym
        )
    }

    /// Parse a pairing QR payload and check it names this device's own
    /// account. `expectedLoginHandle` is the confirming device's handle,
    /// computed from its own username. Throws on:
    ///   * a wrong-length handle or malformed wire bytes (`FfiError.Codec`),
    ///   * a payload naming a different account's name (`FfiError.Group`,
    ///     "login_handle mismatch"),
    ///   * a payload version this binary does not read (`FfiError.Unsupported`).
    /// ``PairingScanFailure/userFacing(_:)`` turns the last two into copy.
    public static func parsePairingQrPayload(
        wireBytes: Data,
        expectedLoginHandle: Data
    ) throws -> DecodedPairingQr {
        return try parsePairingQr(
            wire: wireBytes,
            expectedLoginHandle: expectedLoginHandle
        )
    }
}

/// What a confirming device shows when a scanned pairing code is refused.
public enum PairingScanFailure {

    /// The payload names another account (the name typed on the new device
    /// is not this account's username).
    public static let wrongAccountCopy =
        "This code is for a different account. Check that the new device uses the same username as this one."

    /// The payload comes from a Lakko version this one cannot read.
    public static let unsupportedCopy =
        "This code comes from a different version of Lakko. Update Lakko on both devices and try again."

    /// A signed-in account without a username (a guest) has no login
    /// handle, so no pairing code can name it.
    public static let needsUsernameCopy = "Set a username first to add another device."

    /// Whether `error` is the core's refusal of a payload naming another
    /// account's handle.
    public static func isWrongAccount(_ error: Error) -> Bool {
        if case .Group(let message) = error as? FfiError {
            return message.contains("login_handle mismatch")
        }
        return false
    }

    /// Copy for a failed scan: the wrong-account and version refusals get
    /// their own sentence, anything else keeps its description.
    public static func userFacing(_ error: Error) -> String {
        if isWrongAccount(error) { return wrongAccountCopy }
        if case .Unsupported = error as? FfiError { return unsupportedCopy }
        return (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
    }
}

/// The login handle of a signed-in account's own username, or `nil` when
/// the account has no username (a guest) or the name does not pass the
/// username rule. The confirming device passes it to
/// ``PairingCodec/parsePairingQrPayload(wireBytes:expectedLoginHandle:)``
/// and a signed-in joining device builds its payload with it.
func ownLoginHandle(
    username: String,
    derive: (String) throws -> Data = { try loginHandle(username: $0) }
) -> Data? {
    let trimmed = username.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !trimmed.isEmpty else { return nil }
    let name = try? LoginName.make(
        trimmed,
        emptyError: AuthClientError.argument("empty username"),
        derive: derive
    )
    return name?.handle
}
