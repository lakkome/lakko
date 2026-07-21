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

        // Never lower an existing floor.
        if let existing = getFloor(accountIdB64: accountIdB64),
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
    /// Returns `nil` when no floor exists (fresh device — the zero-anchor path)
    /// OR when the stored row cannot be read / parsed. Treating a broken floor as
    /// "absent" is fail-safe: the restore then falls back to the freshness-window
    /// path rather than throwing — the floor is a rollback OPTIMISATION, never
    /// the confidentiality boundary.
    public static func getFloor(accountIdB64: String) -> BackupFloor? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data,
            data.count == recordLength else {
            return nil
        }
        let versionBytes = data.prefix(8)
        var version: UInt64 = 0
        for byte in versionBytes {
            version = (version << 8) | UInt64(byte)
        }
        let hash = Data(data.suffix(from: data.startIndex + 8))
        return BackupFloor(version: version, envelopeHash: hash)
    }

    /// Erase the floor for `accountIdB64`. No-op when absent — used by
    /// ``AuthRepository/forgetDevice`` cleanup alongside the phrase / seed wipes.
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
