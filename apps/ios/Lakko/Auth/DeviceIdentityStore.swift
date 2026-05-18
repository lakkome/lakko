// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed cache for the per-device **DeviceIdentity
/// seed** (Phase 2a3b0, ADR-0007).
///
/// Mirror of:
///
///   * `apps/web/src/auth/device-identity-store.ts` (IDB AES-GCM
///     under the non-extractable device root key)
///   * `apps/android/.../auth/DeviceIdentityStore.kt`
///     (EncryptedSharedPreferences)
///
/// ## What's stored
///
/// A single 32-byte random seed. The runtime DeviceIdentity is
/// rebuilt deterministically by `Identity.fromSeed(seed:)` —
/// same shape as the existing `Identity.fromSeed(exportKey)`
/// path Phase 1 already uses, just with random input bytes.
///
/// Distinct from the **AccountIdentity** seed (=
/// `IdentitySecrets.exportKey`, deterministic from the OPAQUE
/// password) which is shared across all of the user's devices
/// and persisted separately under ``ExportKeyStore`` for the
/// sign-in-with-passkey flow.
///
/// See `docs/adr/0007-multi-device-identity.md` for the full
/// rationale.
///
/// ## Threat model
///
/// Compared to the export-key cache (2a2c2):
///
///   - **Export key** leak = account-wide compromise.
///   - **DeviceIdentity seed** leak = THIS device's leaf
///     material only. Surgical via DeviceGroup `Remove` (2a3c).
///
/// Same Keychain posture ``DeviceKeyStore`` documents —
/// `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, no iCloud
/// Keychain sync, hardware-isolated class key.
///
/// ## Persistence layout
///
/// One Keychain item per account, generic-password class:
///
///   * `kSecAttrService = "me.lakko.device-identity.v1"`
///   * `kSecAttrAccount = accountIdB64`
///   * `kSecAttrAccessible = AfterFirstUnlockThisDeviceOnly`
///   * `kSecValueData = <32 raw seed bytes>`
public enum DeviceIdentityStore {

    /// Service tag — versioned so a future rotation (e.g. a
    /// move to a different keypair format) can land alongside
    /// v1 without collision.
    public static let serviceTag: String = "me.lakko.device-identity.v1"

    /// Fixed seed length — pinned by `Identity.fromSeed`'s
    /// 32-byte Ed25519 seed contract.
    public static let seedLength: Int = 32

    /// Persist `seed` for `accountIdB64`. Existing record (if
    /// any) is overwritten — same delete-then-add pattern
    /// ``DeviceKeyStore.putDeviceKey`` uses.
    ///
    /// - Throws: ``DeviceIdentityStoreError`` on a wrong-length
    ///   input or a non-success `OSStatus` from `SecItem…`.
    public static func putDeviceIdentitySeed(
        accountIdB64: String,
        seed: Data
    ) throws {
        guard seed.count == seedLength else {
            throw DeviceIdentityStoreError.argument(
                "DeviceIdentity seed must be \(seedLength) bytes (got \(seed.count))"
            )
        }

        deleteIgnoringMissing(accountIdB64: accountIdB64)

        let addQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: seed
        ]
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        if status != errSecSuccess {
            throw DeviceIdentityStoreError.keychain(
                status: status,
                operation: "SecItemAdd"
            )
        }
    }

    /// Read the cached DeviceIdentity seed for `accountIdB64`.
    /// Returns `nil` when absent.
    public static func getDeviceIdentitySeed(
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
                throw DeviceIdentityStoreError.keychain(
                    status: status,
                    operation: "SecItemCopyMatching: unexpected payload type"
                )
            }
            guard data.count == seedLength else {
                throw DeviceIdentityStoreError.argument(
                    "stored DeviceIdentity seed length \(data.count) != \(seedLength)"
                )
            }
            return data
        case errSecItemNotFound:
            return nil
        default:
            throw DeviceIdentityStoreError.keychain(
                status: status,
                operation: "SecItemCopyMatching"
            )
        }
    }

    /// Erase the cached DeviceIdentity seed for `accountIdB64`.
    /// No-op when absent. Called from ``forgetDevice``.
    ///
    /// Note: deleting the seed alone does NOT remove the
    /// device's leaf from the user's DeviceGroup — the other
    /// devices need to see an MLS `Remove` commit to ratchet
    /// past the gone leaf. The full revoke flow lands in 2a3c.
    public static func deleteDeviceIdentitySeed(accountIdB64: String) {
        deleteIgnoringMissing(accountIdB64: accountIdB64)
    }

    /// Convenience: read the cached seed if present, otherwise
    /// generate a fresh random 32-byte seed and persist it.
    /// Returns the seed bytes in both branches. Idempotent.
    public static func ensureDeviceIdentitySeed(
        accountIdB64: String
    ) throws -> Data {
        if let existing = try? getDeviceIdentitySeed(accountIdB64: accountIdB64) {
            return existing
        }
        var bytes = Data(count: seedLength)
        let status = bytes.withUnsafeMutableBytes { ptr -> OSStatus in
            guard let base = ptr.baseAddress else {
                return errSecAllocate
            }
            return SecRandomCopyBytes(kSecRandomDefault, seedLength, base)
        }
        guard status == errSecSuccess else {
            throw DeviceIdentityStoreError.keychain(
                status: status,
                operation: "SecRandomCopyBytes"
            )
        }
        try putDeviceIdentitySeed(
            accountIdB64: accountIdB64,
            seed: bytes
        )
        return bytes
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

/// Error vocabulary for ``DeviceIdentityStore``.
public enum DeviceIdentityStoreError: Error, LocalizedError {
    case argument(String)
    case keychain(status: OSStatus, operation: String)

    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .keychain(let status, let operation):
            return "DeviceIdentityStore \(operation) failed: OSStatus \(status)"
        }
    }
}
