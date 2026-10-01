// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed cache for the **AccountIdentity seed**
/// (ADR-0017, Part 1 — phrase-seeded identity).
///
/// Mirror of:
///
///   * `apps/web/src/auth/account-identity-seed-store.ts` (IDB +
///     AES-GCM under the non-extractable device root key)
///   * `apps/android/.../auth/AccountIdentitySeedStore.kt`
///     (EncryptedSharedPreferences)
///
/// Parallel of ``ExportKeyStore`` — same Keychain posture, same
/// per-account namespacing — but it holds a different secret with a
/// different lifecycle.
///
/// ## What's stored
///
/// A single **64-byte** seed: the BIP39 recovery-phrase PBKDF2 seed
/// (`RecoveryMnemonic.toSeed()`). Per ADR-0017 the canonical
/// **AccountIdentity** — the account-level Ed25519 + X25519 keypair
/// published in the discovery identity bundle / KT log / recovery
/// proofs, the key contacts pin — is now derived from THIS seed
/// (`Identity.fromSeed(seed:)`), NOT the password-derived OPAQUE
/// `exportKey`. Because the recovery phrase does not change on a
/// routine recover / password-change, the derived pubkey never
/// churns, so contacts never re-verify on those paths.
///
/// `Identity.fromSeed` HKDF-expands arbitrary input bytes under a
/// pinned salt, so it is seed-agnostic (ADR-0017 "Scope" section) —
/// feeding it the 64-byte BIP39 seed instead of the 32-byte OPAQUE
/// export key requires no `lakko-mls` / Rust-core change.
///
/// ## Threat model
///
/// This is the **highest-sensitivity** on-device secret: leaking the
/// AccountIdentity seed equals full account-identity compromise (it
/// is the recovery factor itself). It relies on the same Keychain
/// posture ``ExportKeyStore`` / ``DeviceKeyStore`` document:
///
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` —
///     device-bound, no iCloud Keychain sync, background read
///     allowed once the device has been unlocked at least once
///     since boot.
///   * Hardware-isolated class key derived from the device passcode
///     + Secure Enclave; user-space cannot read the payload even
///     with root (invariant #7).
///
/// Compared to the export-key cache: the export key can be rotated by
/// a password change; this seed only changes if the user deliberately
/// rotates their recovery phrase (a "rotate my identity" action,
/// out of scope for ADR-0017 Part 1). It is wiped on `forgetDevice`
/// alongside the export key, the DeviceIdentity seed, and the device
/// root key.
///
/// ## Persistence layout
///
/// One Keychain item per account, generic-password class:
///
///   * `kSecAttrService = "me.lakko.account-identity-seed.v1"`
///   * `kSecAttrAccount = accountIdB64` — per-account namespace
///   * `kSecAttrAccessible = AfterFirstUnlockThisDeviceOnly`
///   * `kSecValueData = <64 raw bytes of the AccountIdentity seed>`
public enum AccountIdentitySeedStore {

    /// Service tag — versioned so a future rotation (e.g. a move to
    /// a different KDF) can land alongside v1 without collision.
    public static let serviceTag: String = "me.lakko.account-identity-seed.v1"

    /// Fixed seed length — pinned by the BIP39 PBKDF2 seed contract
    /// (`RecoveryMnemonic.toSeed` returns 64 bytes).
    public static let seedLength: Int = 64

    /// Persist the 64-byte `seed` for `accountIdB64`. Existing record
    /// (if any) is overwritten — same delete-then-add pattern
    /// ``ExportKeyStore/putExportKey(accountIdB64:exportKey:)`` uses.
    ///
    /// ADR-0041 Phase 1 — the seed is written into the **shared Keychain
    /// access group** (``SharedStorage/keychainAccessGroup``) so the NSE
    /// process can read it. If the shared access group is unavailable for
    /// this process (an unprovisioned simulator build, an entitlement
    /// that has not resolved), the write **falls back to the app-private
    /// item** so the crown-jewel seed is *never* left unpersisted. A
    /// pre-existing app-private seed is **left in place** (copy-not-move);
    /// a future cleanup phase removes it.
    ///
    /// - Throws: ``AccountIdentitySeedStoreError`` on a wrong-length
    ///   input or a non-success `OSStatus` from the fallback `SecItemAdd`.
    public static func putAccountIdentitySeed(
        accountIdB64: String,
        seed: Data
    ) throws {
        guard seed.count == seedLength else {
            throw AccountIdentitySeedStoreError.argument(
                "AccountIdentity seed must be \(seedLength) bytes (got \(seed.count))"
            )
        }

        // Prefer the shared access group (NSE-readable). Overwrites any
        // existing shared item, leaves the app-private item untouched.
        if addSeed(accountIdB64: accountIdB64, seed: seed,
                   accessGroup: SharedStorage.keychainAccessGroup) == errSecSuccess {
            return
        }

        // Fallback: the app-private item (today's behaviour). This path
        // is reached only when the shared access group is unavailable
        // (e.g. an unprovisioned build) — in which case no shared item
        // exists to strand. Must succeed or throw: the seed is never left
        // unpersisted.
        let status = addSeed(accountIdB64: accountIdB64, seed: seed, accessGroup: nil)
        if status != errSecSuccess {
            throw AccountIdentitySeedStoreError.keychain(
                status: status,
                operation: "SecItemAdd"
            )
        }
    }

    /// Read the cached AccountIdentity seed for `accountIdB64`.
    /// Returns `nil` when absent (fresh device / post-`forgetDevice`
    /// / account never seen here / a device that logged in with only
    /// a password before this seed was cached).
    ///
    /// ADR-0041 Phase 1 — prefers the shared access group, falls back to
    /// the old app-private item, so an un-migrated seed still reads.
    ///
    /// The caller MUST NOT silently fall back to deriving the
    /// identity from the export key or minting a fresh identity on a
    /// `nil` — per ADR-0017 the fresh-device fail-safe surfaces a
    /// "needs recovery phrase" state instead.
    public static func getAccountIdentitySeed(
        accountIdB64: String
    ) throws -> Data? {
        // Prefer the shared item; a shared read swallows every error to
        // `nil` so the fallback runs. The app-private read surfaces a
        // genuine Keychain fault (unchanged contract from before ADR-0041).
        if let shared = readSeedSafe(
            accountIdB64: accountIdB64,
            accessGroup: SharedStorage.keychainAccessGroup
        ) {
            return shared
        }
        return try readSeed(accountIdB64: accountIdB64, accessGroup: nil)
    }

    /// ADR-0041 Phase 1 — one-time, idempotent, **copy-not-move** copy of
    /// the seed into the shared Keychain access group so the NSE can read
    /// it. No-op when the shared item already exists (idempotent), the old
    /// item is absent (nothing to copy), or the shared group is
    /// unavailable (the copy fails → the old item stays authoritative).
    /// The old app-private item is **never deleted** here.
    public static func migrateToSharedAccessGroup(accountIdB64: String) {
        let sharedPresent = readSeedSafe(
            accountIdB64: accountIdB64,
            accessGroup: SharedStorage.keychainAccessGroup
        ) != nil
        let old = readSeedSafe(accountIdB64: accountIdB64, accessGroup: nil)
        guard SharedStorage.shouldCopy(sharedPresent: sharedPresent, oldPresent: old != nil),
              let seed = old else {
            return
        }
        _ = addSeed(accountIdB64: accountIdB64, seed: seed,
                    accessGroup: SharedStorage.keychainAccessGroup)
    }

    /// Erase the cached AccountIdentity seed for `accountIdB64` from
    /// **both** the shared access group and the old app-private item.
    /// No-op when absent. Called from ``AccountLocalDataWipe`` — a
    /// deliberate full wipe (distinct from the never-delete-old migration
    /// posture), so the crown-jewel seed is not left behind.
    public static func deleteAccountIdentitySeed(accountIdB64: String) {
        deleteSeed(accountIdB64: accountIdB64, accessGroup: SharedStorage.keychainAccessGroup)
        deleteSeed(accountIdB64: accountIdB64, accessGroup: nil)
    }

    // MARK: - Access-group-scoped Keychain primitives

    /// ``readSeed`` collapsed to a plain `Data?` — a genuine Keychain
    /// fault is swallowed to `nil`. Used for the shared-group read
    /// (which must degrade to the app-private fallback, never throw) and
    /// for the migration presence checks.
    private static func readSeedSafe(accountIdB64: String, accessGroup: String?) -> Data? {
        (try? readSeed(accountIdB64: accountIdB64, accessGroup: accessGroup)) ?? nil
    }

    /// Read the seed from a specific access group (shared or app-private
    /// when `nil`). Returns `nil` on not-found / missing-entitlement, and
    /// validates the length. Throws only a genuine Keychain fault.
    private static func readSeed(
        accountIdB64: String,
        accessGroup: String?
    ) throws -> Data? {
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
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
                throw AccountIdentitySeedStoreError.keychain(
                    status: status,
                    operation: "SecItemCopyMatching: unexpected payload type"
                )
            }
            // Belt-and-braces: refuse a row a future build might have
            // written with a different length rather than returning a
            // malformed seed.
            guard data.count == seedLength else {
                throw AccountIdentitySeedStoreError.argument(
                    "stored AccountIdentity seed length \(data.count) != \(seedLength)"
                )
            }
            return data
        case errSecItemNotFound, errSecMissingEntitlement:
            // Absent here, or the access group is not provisioned for this
            // process — the caller falls back to the other location.
            return nil
        default:
            throw AccountIdentitySeedStoreError.keychain(
                status: status,
                operation: "SecItemCopyMatching"
            )
        }
    }

    /// Overwrite-add the seed into a specific access group (delete-then-add,
    /// scoped to that group so it never touches the other group's item).
    @discardableResult
    private static func addSeed(
        accountIdB64: String,
        seed: Data,
        accessGroup: String?
    ) -> OSStatus {
        deleteSeed(accountIdB64: accountIdB64, accessGroup: accessGroup)
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: seed
        ]
        if let accessGroup { query[kSecAttrAccessGroup] = accessGroup }
        return SecItemAdd(query as CFDictionary, nil)
    }

    private static func deleteSeed(accountIdB64: String, accessGroup: String?) {
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64
        ]
        if let accessGroup { query[kSecAttrAccessGroup] = accessGroup }
        _ = SecItemDelete(query as CFDictionary)
    }
}

/// Error vocabulary for ``AccountIdentitySeedStore``.
public enum AccountIdentitySeedStoreError: Error, LocalizedError {
    case argument(String)
    case keychain(status: OSStatus, operation: String)

    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .keychain(let status, let operation):
            return "AccountIdentitySeedStore \(operation) failed: OSStatus \(status)"
        }
    }
}
