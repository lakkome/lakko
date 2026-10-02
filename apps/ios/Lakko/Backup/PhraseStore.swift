// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Encrypted-recovery-phrase Keychain cache for the auto-backup
/// flow (Phase 2a1c).
///
/// Mirror of `apps/web/src/backup/phrase-store.ts` and the
/// Android `PhraseStore.kt`. Stores the canonical recovery
/// phrase bytes (UTF-8, NFKD-normalised, single-space-separated)
/// in the iOS Keychain under
/// `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — same
/// data-protection class the device-root wrap key in
/// ``IdentitySnapshotStore`` already uses.
///
/// ## Why direct Keychain (vs wrap-key + file like the MLS
/// snapshot store)
///
/// The phrase is ~80 bytes for a 12-word BIP39 mnemonic — well
/// within Keychain's per-item `kSecValueData` ceiling
/// (around 4-8 KB). The Keychain hands us hardware-isolated
/// storage for free at this size; the file-with-wrap-key
/// indirection only matters for the larger MLS snapshot blobs.
///
/// ## Per-account isolation
///
/// One Keychain item per `accountIdB64`. The account id sits
/// in `kSecAttrAccount`, the service tag is
/// [`Self.service`]. Deletion is per-account too —
/// `forgetDevice` calls [`deletePhrase`] alongside the existing
/// session-token / MLS-snapshot wipes.
///
/// ## Threat model
///
/// - **At-rest confidentiality**: Keychain class-key encryption.
///   The bytes never leave the data-protection sandbox; the
///   device unlocks the key on first unlock-after-boot and
///   keeps it available through the session.
/// - **Process-memory exposure**: same posture as
///   ``IdentitySnapshotStore`` — the bytes briefly hit the JS
///   heap when wrapped / unwrapped. The Swift API exposes the
///   phrase as `Data`; callers `zero()` it after use where
///   possible.
/// - **Cross-device migration via iCloud Keychain**:
///   `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` keeps
///   the row local to this device. Recovery on a fresh device
///   uses the recovery-phrase entry flow — that's the only way
///   the phrase moves between devices.
public final class PhraseStore {

    /// Keychain `kSecAttrService` tag. Distinct from
    /// `IdentitySnapshotStore.wrapKeyService` so a Keychain
    /// dump can tell the two apart at a glance.
    public static let service: String = "me.lakko.backup-phrase.v1"

    public init() {}

    /// Persist `phraseBytes` for `accountIdB64`. Overwrites any
    /// prior row. Caller is responsible for zeroing the input
    /// `Data` after this resolves; the function does not retain
    /// a reference past return.
    public func putPhrase(accountIdB64: String, phraseBytes: Data) throws {
        // Delete first — `kSecAttrAccessible` cannot be updated
        // via `SecItemUpdate`; an overwrite has to drop + re-add.
        try? deletePhrase(accountIdB64: accountIdB64)

        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.service,
            kSecAttrAccount as String: accountIdB64,
            kSecAttrAccessible as String:
                kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData as String: phraseBytes,
        ]
        let status = SecItemAdd(query as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw PhraseStoreError.keychain(status: status, op: "SecItemAdd")
        }
    }

    /// Read the phrase for `accountIdB64`, or `nil` when absent.
    /// Returns the canonical UTF-8 phrase bytes the caller hands
    /// straight to `sealBackup`.
    public func getPhrase(accountIdB64: String) throws -> Data? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.service,
            kSecAttrAccount as String: accountIdB64,
            kSecReturnData as String: kCFBooleanTrue!,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound {
            return nil
        }
        guard status == errSecSuccess else {
            throw PhraseStoreError.keychain(status: status, op: "SecItemCopyMatching")
        }
        return result as? Data
    }

    /// Erase the phrase row for `accountIdB64`. No-op when
    /// absent. Used by the `forgetDevice` cleanup.
    public func deletePhrase(accountIdB64: String) throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.service,
            kSecAttrAccount as String: accountIdB64,
        ]
        let status = SecItemDelete(query as CFDictionary)
        if status == errSecSuccess || status == errSecItemNotFound {
            return
        }
        throw PhraseStoreError.keychain(status: status, op: "SecItemDelete")
    }
}

public enum PhraseStoreError: Error {
    case keychain(status: OSStatus, op: String)
}
