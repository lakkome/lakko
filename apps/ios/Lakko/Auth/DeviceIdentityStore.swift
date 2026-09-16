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
///
/// ## ADR-0050 — the device identity is the conversation leaf
///
/// Since ADR-0050 this seed is the leaf of every conversation group this
/// device is in, not only of the DeviceGroup, so the MLS snapshot the
/// notification service extension restores (ADR-0041) was written under it.
/// The seed therefore lives in the shared Keychain access group, exactly as
/// ``AccountIdentitySeedStore`` does (prefer the shared group, fall back to
/// the app-private item on write and on read). The account-key binding over
/// this device's key (public bytes) is kept beside it so its `issued_at`
/// stays stable across launches.
public enum DeviceIdentityStore {

    /// Service tag — versioned so a future rotation (e.g. a
    /// move to a different keypair format) can land alongside
    /// v1 without collision.
    public static let serviceTag: String = "me.lakko.device-identity.v1"

    /// ADR-0050 — service tag of the persisted device binding (public).
    public static let bindingServiceTag: String = "me.lakko.device-binding.v1"

    /// Fixed seed length — pinned by `Identity.fromSeed`'s
    /// 32-byte Ed25519 seed contract.
    public static let seedLength: Int = 32

    /// Persist `seed` for `accountIdB64`. Existing record (if
    /// any) is overwritten. Prefers the shared access group; falls back to
    /// the app-private item so the seed is never left unpersisted.
    ///
    /// - Throws: ``DeviceIdentityStoreError`` on a wrong-length
    ///   input or a non-success `OSStatus` from the fallback `SecItemAdd`.
    public static func putDeviceIdentitySeed(
        accountIdB64: String,
        seed: Data
    ) throws {
        guard seed.count == seedLength else {
            throw DeviceIdentityStoreError.argument(
                "DeviceIdentity seed must be \(seedLength) bytes (got \(seed.count))"
            )
        }
        try putItem(service: serviceTag, accountIdB64: accountIdB64, value: seed)
    }

    /// Read the cached DeviceIdentity seed for `accountIdB64`.
    /// Returns `nil` when absent. Prefers the shared access group, falls
    /// back to the app-private item.
    public static func getDeviceIdentitySeed(
        accountIdB64: String
    ) throws -> Data? {
        guard let data = try getItem(service: serviceTag, accountIdB64: accountIdB64) else {
            return nil
        }
        guard data.count == seedLength else {
            throw DeviceIdentityStoreError.argument(
                "stored DeviceIdentity seed length \(data.count) != \(seedLength)"
            )
        }
        return data
    }

    /// Erase the cached DeviceIdentity seed (and its binding) for
    /// `accountIdB64` from both locations. No-op when absent. Called from
    /// ``forgetDevice``.
    ///
    /// Note: deleting the seed alone does NOT remove the
    /// device's leaf from the user's DeviceGroup — the other
    /// devices need to see an MLS `Remove` commit to ratchet
    /// past the gone leaf. That is the separate revoke flow in
    /// ``DeviceRemoval/removeDeviceAt`` (2a3c).
    public static func deleteDeviceIdentitySeed(accountIdB64: String) {
        for group in [SharedStorage.keychainAccessGroup, nil] as [String?] {
            deleteItem(service: serviceTag, accountIdB64: accountIdB64, accessGroup: group)
            deleteItem(service: bindingServiceTag, accountIdB64: accountIdB64, accessGroup: group)
        }
    }

    /// Convenience: read the cached seed if present, otherwise
    /// generate a fresh random 32-byte seed and persist it.
    /// Returns the seed bytes in both branches. Idempotent.
    public static func ensureDeviceIdentitySeed(
        accountIdB64: String
    ) throws -> Data {
        copyToSharedIfMissing(service: serviceTag, accountIdB64: accountIdB64)
        copyToSharedIfMissing(service: bindingServiceTag, accountIdB64: accountIdB64)
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

    // MARK: - ADR-0050 device binding

    /// Read the persisted device binding wire bytes, or `nil` when this
    /// device has not been bound yet (or on a Keychain fault).
    public static func getDeviceBinding(accountIdB64: String) -> Data? {
        (try? getItem(service: bindingServiceTag, accountIdB64: accountIdB64)) ?? nil
    }

    /// Persist the device binding wire bytes beside the seed. Public bytes
    /// (the account key's signature over this device's key).
    public static func putDeviceBinding(accountIdB64: String, binding: Data) throws {
        guard !binding.isEmpty else {
            throw DeviceIdentityStoreError.argument("device binding must be non-empty")
        }
        try putItem(service: bindingServiceTag, accountIdB64: accountIdB64, value: binding)
    }

    // MARK: - Access-group-scoped Keychain primitives

    private static func putItem(service: String, accountIdB64: String, value: Data) throws {
        if addItem(service: service, accountIdB64: accountIdB64, value: value,
                   accessGroup: SharedStorage.keychainAccessGroup) == errSecSuccess {
            return
        }
        let status = addItem(service: service, accountIdB64: accountIdB64, value: value,
                             accessGroup: nil)
        if status != errSecSuccess {
            throw DeviceIdentityStoreError.keychain(status: status, operation: "SecItemAdd")
        }
    }

    private static func getItem(service: String, accountIdB64: String) throws -> Data? {
        if let shared = (try? readItem(service: service, accountIdB64: accountIdB64,
                                       accessGroup: SharedStorage.keychainAccessGroup)) ?? nil {
            return shared
        }
        return try readItem(service: service, accountIdB64: accountIdB64, accessGroup: nil)
    }

    /// Copy an app-private item into the shared access group when only the
    /// app-private one exists (a seed written before it moved), never deleting
    /// the old item. Called from the app's ``ensureDeviceIdentitySeed``
    /// only, so the extension process never writes to the Keychain.
    private static func copyToSharedIfMissing(service: String, accountIdB64: String) {
        let shared = (try? readItem(service: service, accountIdB64: accountIdB64,
                                    accessGroup: SharedStorage.keychainAccessGroup)) ?? nil
        guard shared == nil,
              let old = (try? readItem(service: service, accountIdB64: accountIdB64,
                                       accessGroup: nil)) ?? nil
        else { return }
        _ = addItem(service: service, accountIdB64: accountIdB64, value: old,
                    accessGroup: SharedStorage.keychainAccessGroup)
    }

    private static func readItem(
        service: String,
        accountIdB64: String,
        accessGroup: String?
    ) throws -> Data? {
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: accountIdB64,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        if let accessGroup { query[kSecAttrAccessGroup] = accessGroup }
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
            return data
        case errSecItemNotFound, errSecMissingEntitlement:
            return nil
        default:
            throw DeviceIdentityStoreError.keychain(
                status: status,
                operation: "SecItemCopyMatching"
            )
        }
    }

    @discardableResult
    private static func addItem(
        service: String,
        accountIdB64: String,
        value: Data,
        accessGroup: String?
    ) -> OSStatus {
        deleteItem(service: service, accountIdB64: accountIdB64, accessGroup: accessGroup)
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: value
        ]
        if let accessGroup { query[kSecAttrAccessGroup] = accessGroup }
        return SecItemAdd(query as CFDictionary, nil)
    }

    private static func deleteItem(service: String, accountIdB64: String, accessGroup: String?) {
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: accountIdB64
        ]
        if let accessGroup { query[kSecAttrAccessGroup] = accessGroup }
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
