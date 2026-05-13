// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed storage for the device root key.
///
/// Mirror of:
///
///   * `apps/web/src/auth/device-key.ts` (1.17d5 — IndexedDB +
///     non-extractable WebCrypto)
///   * `apps/android/.../auth/DeviceKeyStore.kt` (1.15d3 —
///     Keystore-wrap + on-disk envelope)
///
/// ## Threat model
///
/// The device root key is the bottom of the local data-encryption
/// hierarchy: every MLS group state, every SQLCipher column, every
/// SwiftUI-side draft sits underneath it. If an attacker reads it,
/// the local secrets are over.
///
/// On iOS the Keychain is the only **hardware-isolated** option:
///
///   * The Keychain payload is encrypted by a class key derived
///     from the device passcode / Secure Enclave on every modern
///     iPhone (Secure Enclave processor since A7 / iPhone 5s — well
///     below our iOS 17 deployment target's hardware floor).
///   * The class key is **never readable** by user-space code,
///     even with root. Decryption happens inside the Secure Enclave
///     coprocessor.
///   * Setting `kSecAttrAccessible` to
///     ``kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`` pins
///     the entry to this device (no iCloud Keychain sync) and
///     requires the device to have been unlocked at least once
///     after boot (allows background reads for push-triggered MLS
///     decrypts, the iOS analogue of the Android Keystore
///     `setUserAuthenticationRequired(false)` posture).
///
/// ## Why no separate wrap-key dance?
///
/// The Android side hand-rolls an AES-GCM wrap key in Keystore +
/// stores an encrypted envelope under `getNoBackupFilesDir()`
/// because Android's Keystore has stricter size limits on direct
/// secret storage and needs explicit StrongBox vs. TEE branching.
/// iOS Keychain has neither limit; storing the 32-byte root key as
/// the Keychain item's `kSecValueData` directly is the canonical
/// shape and uses hardware encryption identical to what the wrap
/// key would provide.
///
/// ## Persistence layout
///
/// One Keychain item per account, under the generic-password class
/// with:
///
///   * `kSecAttrService = "me.lakko.device-key.v1"` — versioned
///     service tag so a future rotation can land alongside.
///   * `kSecAttrAccount = accountIdB64` — per-account namespacing.
///   * `kSecAttrAccessible = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`
///     — device-bound, available after first unlock.
///   * `kSecAttrSynchronizable = false` (Foundation default) — no
///     iCloud Keychain sync, mirror of Android's `noBackupFilesDir`.
public enum DeviceKeyStore {

    /// Service tag — versioned so a future rotation has somewhere
    /// to go without colliding with v1 entries on upgrade.
    public static let serviceTag: String = "me.lakko.device-key.v1"

    /// Persist `rootKey` for `accountIdB64`.
    ///
    /// Existing record (if any) is overwritten — same posture as
    /// `putDeviceKey` on the web and Android sides.
    ///
    /// - Throws: ``DeviceKeyStoreError/keychain(status:operation:)``
    ///   on a non-`errSecSuccess` Keychain result.
    public static func putDeviceKey(
        accountIdB64: String,
        rootKey: Data
    ) throws {
        guard rootKey.count == deviceRootKeyLength else {
            throw DeviceKeyStoreError.argument(
                "device root key must be \(deviceRootKeyLength) bytes (got \(rootKey.count))"
            )
        }

        // SecItemUpdate cannot create new entries; SecItemAdd
        // rejects existing ones. The Keychain API doesn't ship an
        // upsert primitive, so we delete-then-add. Race-free because
        // there's only ever one writer (the auth flow on the main
        // actor) per account.
        deleteIgnoringMissing(accountIdB64: accountIdB64)

        let addQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: rootKey
        ]
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        if status != errSecSuccess {
            throw DeviceKeyStoreError.keychain(
                status: status,
                operation: "SecItemAdd"
            )
        }
    }

    /// Read the device root key for `accountIdB64`.
    ///
    /// Returns `nil` when no entry exists — same shape as the web
    /// and Android `getDeviceKey` returning `null`. Keychain
    /// failures (e.g. `errSecAuthFailed` if the user has reset
    /// their device) surface as ``DeviceKeyStoreError/keychain(status:operation:)``.
    public static func getDeviceKey(
        accountIdB64: String
    ) throws -> Data? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data else {
                throw DeviceKeyStoreError.keychain(
                    status: status,
                    operation: "SecItemCopyMatching: unexpected payload type"
                )
            }
            return data
        case errSecItemNotFound:
            return nil
        default:
            throw DeviceKeyStoreError.keychain(
                status: status,
                operation: "SecItemCopyMatching"
            )
        }
    }

    /// Erase the device root key for `accountIdB64`. No-op if the
    /// entry is absent.
    public static func deleteDeviceKey(accountIdB64: String) {
        deleteIgnoringMissing(accountIdB64: accountIdB64)
    }

    /// Internal: best-effort delete, swallowing `errSecItemNotFound`.
    /// Used by both ``deleteDeviceKey(accountIdB64:)`` and the
    /// delete-then-add upsert in ``putDeviceKey(accountIdB64:rootKey:)``.
    private static func deleteIgnoringMissing(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64
        ]
        // Discarding the OSStatus on purpose — the caller has already
        // committed to forgetting this key, and a non-success status
        // here is at worst "nothing to delete". A genuine Keychain
        // failure surfaces from the follow-up SecItemAdd in the
        // upsert path.
        _ = SecItemDelete(query as CFDictionary)
    }
}

/// Error vocabulary for ``DeviceKeyStore``.
public enum DeviceKeyStoreError: Error {
    /// Caller-side wiring bug (wrong key length, etc.). Distinct
    /// from a Keychain failure so the UI can surface it as a
    /// developer-facing error rather than a transient device issue.
    case argument(String)
    /// A `SecItem…` call returned a non-success ``OSStatus``. The
    /// operation tag identifies which call so log lines stay legible.
    case keychain(status: OSStatus, operation: String)
}

extension DeviceKeyStoreError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .keychain(let status, let operation):
            return "DeviceKeyStore \(operation) failed: OSStatus \(status)"
        }
    }
}
