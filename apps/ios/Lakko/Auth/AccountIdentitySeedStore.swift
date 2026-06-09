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
    /// - Throws: ``AccountIdentitySeedStoreError`` on a wrong-length
    ///   input or a non-success `OSStatus` from `SecItem…`.
    public static func putAccountIdentitySeed(
        accountIdB64: String,
        seed: Data
    ) throws {
        guard seed.count == seedLength else {
            throw AccountIdentitySeedStoreError.argument(
                "AccountIdentity seed must be \(seedLength) bytes (got \(seed.count))"
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
    /// The caller MUST NOT silently fall back to deriving the
    /// identity from the export key or minting a fresh identity on a
    /// `nil` — per ADR-0017 the fresh-device fail-safe surfaces a
    /// "needs recovery phrase" state instead.
    public static func getAccountIdentitySeed(
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
        case errSecItemNotFound:
            return nil
        default:
            throw AccountIdentitySeedStoreError.keychain(
                status: status,
                operation: "SecItemCopyMatching"
            )
        }
    }

    /// Erase the cached AccountIdentity seed for `accountIdB64`.
    /// No-op when absent. Called from ``AuthRepository/forgetDevice``.
    public static func deleteAccountIdentitySeed(accountIdB64: String) {
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
