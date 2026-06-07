// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// DeviceGroup journal envelope (G-13, ADR-0015 sprint 5).
///
/// Every DeviceGroup DS-journal entry is wrapped so a Remove commit can carry
/// the peer-verification attestation alongside the MLS message it belongs to.
/// Peer devices unwrap it and feed `(message, attestation)` to
/// `DeviceGroup.processIncomingVerifyingRemovals`, which refuses a removal the
/// group's policy requires to be attested but isn't.
///
/// Layout (big-endian):
///
///     version          : UInt8 = 1
///     attestation_len  : UInt32
///     attestation      : attestation_len bytes   (0 = none)
///     message          : the remaining bytes      (MLS wire message)
///
/// Mirror of `apps/web/src/devices/journal-envelope.ts`. Non-remove entries
/// carry `attestation_len = 0`. No raw fallback — the journal is pre-launch.
enum DeviceGroupJournalEnvelope {
    static let version: UInt8 = 1
    private static let headerLen = 5

    enum EnvelopeError: Error {
        case tooShort
        case badVersion(UInt8)
        case attestationOverflow
    }

    /// Wrap an MLS message plus an optional removal attestation.
    static func encode(message: Data, attestation: Data?) -> Data {
        let att = attestation ?? Data()
        let attLen = UInt32(att.count)
        var out = Data()
        out.append(version)
        out.append(UInt8((attLen >> 24) & 0xff))
        out.append(UInt8((attLen >> 16) & 0xff))
        out.append(UInt8((attLen >> 8) & 0xff))
        out.append(UInt8(attLen & 0xff))
        out.append(att)
        out.append(message)
        return out
    }

    /// Unwrap an envelope produced by ``encode(message:attestation:)``.
    static func decode(_ data: Data) throws -> (message: Data, attestation: Data?) {
        let b = [UInt8](data)
        guard b.count >= headerLen else { throw EnvelopeError.tooShort }
        guard b[0] == version else { throw EnvelopeError.badVersion(b[0]) }
        let attLen =
            (UInt32(b[1]) << 24) | (UInt32(b[2]) << 16) | (UInt32(b[3]) << 8) | UInt32(b[4])
        let total = headerLen + Int(attLen)
        guard total <= b.count else { throw EnvelopeError.attestationOverflow }
        let attestation = attLen > 0 ? Data(b[headerLen..<total]) : nil
        let message = Data(b[total...])
        return (message, attestation)
    }
}
