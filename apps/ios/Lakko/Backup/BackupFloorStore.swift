// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Backup-head version-floor store (ADR-0037 §6, G-11).
///
/// Swift mirror of `apps/web/src/backup/floor-store.ts` (which encrypts the
/// floor under the non-extractable device root key before persisting to IDB).
/// On iOS the **Keychain itself is the hardware-backed store** — the class key
/// is derived from the device passcode + Secure Enclave (invariant #7) — so the
/// floor rides directly in a generic-password item rather than needing an
/// application-layer AES-GCM wrap. Same Keychain posture + per-account
/// namespacing as ``AccountIdentitySeedStore`` / ``PhraseStore``.
///
/// ## What the floor is
///
/// The monotonic rollback floor: the highest `backup_version` this device has
/// ever minted or restored, plus the envelope hash it committed. On a later
/// restore a **returning** client refuses any head below this floor outright
/// (hard, no freshness window) — the auth-assigned version never regresses, so
/// the floor is trustworthy against a storage-tier replay of an older head.
///
/// ## Why integrity-critical
///
/// An attacker who could lower the floor would defeat the returning-client
/// rollback defence. The Keychain data-protection class encrypts + authenticates
/// the row at rest; a user-space attacker (even with root) cannot read or tamper
/// it without also breaking the Secure Enclave isolation boundary.
///
/// ## Persistence layout
///
/// One Keychain item per account, generic-password class:
///
///   * `kSecAttrService = "me.lakko.backup-floor.v1"`
///   * `kSecAttrAccount = accountIdB64` — per-account namespace
///   * `kSecAttrAccessible = AfterFirstUnlockThisDeviceOnly` — device-bound,
///     no iCloud sync, background read after first unlock
///   * `kSecValueData = version(8 BE) ‖ envelopeHash(32)` — fixed 40-byte layout
public enum BackupFloorStore {

    /// Keychain `kSecAttrService` tag — versioned so a future layout change can
    /// land alongside v1 without collision.
    public static let serviceTag: String = "me.lakko.backup-floor.v1"

    /// Fixed envelope-hash length (`BLAKE3` digest).
    public static let envelopeHashLength: Int = 32

    /// Fixed persisted record length: `version(8) ‖ envelopeHash(32)`.
    private static let recordLength: Int = 8 + envelopeHashLength

    /// Persist (or advance) the version floor for `accountIdB64`.
    ///
    /// **Monotone.** If a higher (or equal) floor is already stored, this is a
    /// no-op — the floor never regresses, mirroring the auth-side counter. A
    /// deliberate stale-override restore therefore never lowers a returning
    /// client's protection.
    ///
    /// - Throws: ``BackupFloorStoreError`` on a wrong-length hash or a
    ///   non-success `OSStatus` from `SecItemAdd`.
    public static func putFloor(accountIdB64: String, floor: BackupFloor) throws {
        guard floor.envelopeHash.count == envelopeHashLength else {
            throw BackupFloorStoreError.argument(
                "envelope hash must be \(envelopeHashLength) bytes "
                    + "(got \(floor.envelopeHash.count))"
            )
        }

        // Never lower an existing (readable) floor. A corrupt / absent existing
        // floor is overwritten by this fresh, valid one — the write heals the
        // anchor (only a `.present` floor at or above the new version no-ops).
        if case .present(let existing) = getFloor(accountIdB64: accountIdB64),
            existing.version >= floor.version {
            return
        }

        var record = Data(capacity: recordLength)
        var beVersion = floor.version.bigEndian
        withUnsafeBytes(of: &beVersion) { record.append(contentsOf: $0) }
        record.append(floor.envelopeHash)

        deleteIgnoringMissing(accountIdB64: accountIdB64)

        let addQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: record
        ]
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        if status != errSecSuccess {
            throw BackupFloorStoreError.keychain(status: status, operation: "SecItemAdd")
        }
    }

    /// Read the version floor for `accountIdB64`.
    ///
    /// Returns a discriminated ``FloorRead`` (F5a):
    /// - `.absent` — no Keychain item (`errSecItemNotFound`): a fresh device, the
    ///   zero-anchor path. **Fail-safe:** a first-ever restore is NEVER `.corrupt`.
    /// - `.present(floor)` — a Keychain item exists and decodes to the fixed
    ///   40-byte `version(8 BE) ‖ envelopeHash(32)` layout.
    /// - `.corrupt` — a Keychain item EXISTS but its bytes are the wrong length /
    ///   undecodable (a corrupt write or an active tamper). Distinct from
    ///   `.absent`: the anchor once existed and is now untrustworthy, so the
    ///   restore gate must NOT silently fall to the weaker freshness path — it
    ///   routes a corrupt anchor to `staleOverride`.
    public static func getFloor(accountIdB64: String) -> FloorRead {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        // No row at all → genuinely absent (fresh device). Any OTHER non-success
        // status leaves us unable to read a row we cannot prove absent; treat it
        // fail-safe as absent too (only a PRESENT-but-broken row is corrupt).
        if status == errSecItemNotFound {
            return .absent
        }
        guard status == errSecSuccess, let data = item as? Data else {
            return .absent
        }
        // The item is present. A wrong-length payload is an untrustworthy anchor,
        // NOT an absent one — surface it as corrupt so the gate fails closed.
        guard data.count == recordLength else {
            return .corrupt
        }
        let versionBytes = data.prefix(8)
        var version: UInt64 = 0
        for byte in versionBytes {
            version = (version << 8) | UInt64(byte)
        }
        let hash = Data(data.suffix(from: data.startIndex + 8))
        return .present(BackupFloor(version: version, envelopeHash: hash))
    }

    /// Erase the floor for `accountIdB64`. No-op when absent — used by
    /// ``AccountLocalDataWipe`` cleanup alongside the phrase / seed wipes.
    public static func deleteFloor(accountIdB64: String) {
        deleteIgnoringMissing(accountIdB64: accountIdB64)
    }

    private static func deleteIgnoringMissing(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64
        ]
        _ = SecItemDelete(query as CFDictionary)
    }
}

/// The decrypted floor a returning client compares against. Mirror of the web
/// `BackupFloor`.
public struct BackupFloor: Equatable {
    /// Highest `backup_version` this device has minted / restored.
    public let version: UInt64
    /// The 32-byte `BLAKE3` envelope hash the floor's head committed.
    public let envelopeHash: Data

    public init(version: UInt64, envelopeHash: Data) {
        self.version = version
        self.envelopeHash = envelopeHash
    }
}

/// The outcome of reading the floor (F5a). Distinguishes a genuinely absent
/// anchor (fresh device) from an anchor that EXISTS but is untrustworthy (a
/// present-but-undecodable Keychain row — a corrupt write or an active tamper).
/// The restore gate must never silently demote a corrupt anchor to the weaker
/// fresh-install path — it routes it to `staleOverride` instead. Mirror of the
/// web `FloorRead`.
public enum FloorRead: Equatable {
    /// No Keychain item — a first-ever restore on this device (zero-anchor path).
    case absent
    /// A valid, decoded floor.
    case present(BackupFloor)
    /// A Keychain item EXISTS but its bytes are the wrong length / undecodable.
    case corrupt
}

/// Error vocabulary for ``BackupFloorStore``.
public enum BackupFloorStoreError: Error, LocalizedError {
    case argument(String)
    case keychain(status: OSStatus, operation: String)

    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .keychain(let status, let operation):
            return "BackupFloorStore \(operation) failed: OSStatus \(status)"
        }
    }
}
