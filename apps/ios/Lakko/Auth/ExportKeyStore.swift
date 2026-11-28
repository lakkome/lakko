// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed cache for the OPAQUE export key
/// (Phase 2a2c2-ios).
///
/// Mirror of:
///
///   * `apps/web/src/auth/export-key-store.ts` (IDB + AES-GCM
///     under the non-extractable device root key)
///   * `apps/android/.../auth/ExportKeyStore.kt` (Phase 2a2c2-
///     android, EncryptedSharedPreferences)
///
/// ## What's stored
///
/// The raw 64-byte OPAQUE export key — the seed every identity-
/// bearing operation derives from:
///
///   - Ed25519 + X25519 keypairs via `Identity.fromSeed(seed:)`
///   - The device root key via `deriveDeviceRootKey(exportKey:_:)`
///   - The MLS ratchet state via OpenMLS storage
///
/// Until Phase 2a2c2 the export key lived in the
/// ``AuthRepository.identitySecrets`` `@Published` slot only —
/// it survived in-memory across SwiftUI rebuilds but was gone on
/// process termination. The sign-in-with-passkey flow needs a
/// way to reconstruct it after a relaunch on a device that has
/// already seen this account; this store is that path.
///
/// ## Threat model
///
/// The cached export key is the **seed for the account's
/// private material on this device**. Leaking it equals full
/// identity compromise (until the user rotates via recovery
/// phrase + new password).
///
/// We rely on the same Keychain posture
/// ``DeviceKeyStore`` documents:
///
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` —
///     device-bound, no iCloud Keychain sync, background read
///     allowed once the device has been unlocked at least once
///     since boot.
///   * Hardware-isolated class key derived from the device
///     passcode + Secure Enclave; user-space cannot read the
///     payload even with root.
///
/// Compared to the session-token cache (1.13c, 30-day-expiring
/// HMAC token) the export key is materially more sensitive: it
/// grants every signing / decrypt / MLS-state-derivation
/// operation indefinitely. The trade-off is deliberate — see
/// the export-key-store docstring on the web side for the full
/// rationale.
///
/// ## Persistence layout
///
/// One Keychain item per account, generic-password class:
///
///   * `kSecAttrService = "me.lakko.export-key.v1"` — versioned
///   * `kSecAttrAccount = accountIdB64` — per-account namespace
///   * `kSecAttrAccessible = AfterFirstUnlockThisDeviceOnly`
///   * `kSecValueData = <64 raw bytes of the export key>`
public enum ExportKeyStore {

    /// Service tag — versioned so a future rotation (e.g. a
    /// move to a different KDF) can land alongside v1 without
    /// collision.
    public static let serviceTag: String = "me.lakko.export-key.v1"

    /// Fixed export-key length pinned by the OPAQUE ciphersuite.
    /// `EXPORT_KEY_LEN` in `crates/lakko-auth-client/src/cipher_suite.rs`
    /// (SHA-512). It read 32 until 2026-10-22, so no export key was ever
    /// stored on iOS and every write threw.
    public static let exportKeyLength: Int = 64

    /// Persist `exportKey` for `accountIdB64`. Existing record
    /// (if any) is overwritten — same delete-then-add pattern
    /// ``DeviceKeyStore.putDeviceKey`` uses.
    ///
    /// - Throws: ``ExportKeyStoreError`` on a wrong-length input
    ///   or a non-success `OSStatus` from `SecItem…`.
    public static func putExportKey(
        accountIdB64: String,
        exportKey: Data
    ) throws {
        guard exportKey.count == exportKeyLength else {
            throw ExportKeyStoreError.argument(
                "export key must be \(exportKeyLength) bytes (got \(exportKey.count))"
            )
        }

        deleteIgnoringMissing(accountIdB64: accountIdB64)

        let addQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: exportKey
        ]
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        if status != errSecSuccess {
            throw ExportKeyStoreError.keychain(
                status: status,
                operation: "SecItemAdd"
            )
        }
        // Every caller writes the export key of a password just proven
        // (sign-up, a password sign-in, a password change, a recall): the
        // account's password seal and its owned-channels password lock follow
        // it at the next look on a device that holds the keys (ADR-0101),
        // from memory only (``RunExportKey``, alignment item N).
        let key = exportKey
        if Thread.isMainThread {
            MainActor.assumeIsolated { RunExportKey.note(accountIdB64: accountIdB64, exportKey: key) }
        } else {
            Task { @MainActor in RunExportKey.note(accountIdB64: accountIdB64, exportKey: key) }
        }
    }

    /// Read the cached export key for `accountIdB64`. Returns
    /// `nil` when absent (fresh device / post-`forgetDevice` /
    /// account never seen here before).
    public static func getExportKey(
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
                throw ExportKeyStoreError.keychain(
                    status: status,
                    operation: "SecItemCopyMatching: unexpected payload type"
                )
            }
            // Belt-and-braces: a row written by a future build
            // could carry extra bytes; refuse it instead of
            // returning a malformed seed.
            guard data.count == exportKeyLength else {
                throw ExportKeyStoreError.argument(
                    "stored export key length \(data.count) != \(exportKeyLength)"
                )
            }
            return data
        case errSecItemNotFound:
            return nil
        default:
            throw ExportKeyStoreError.keychain(
                status: status,
                operation: "SecItemCopyMatching"
            )
        }
    }

    /// Erase the cached export key for `accountIdB64`. No-op
    /// when absent. Called from ``forgetDevice``.
    public static func deleteExportKey(accountIdB64: String) {
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

/// Error vocabulary for ``ExportKeyStore``.
public enum ExportKeyStoreError: Error, LocalizedError {
    case argument(String)
    case keychain(status: OSStatus, operation: String)

    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .keychain(let status, let operation):
            return "ExportKeyStore \(operation) failed: OSStatus \(status)"
        }
    }
}
