// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed cache for the auth **session pair** (ADR-0073 section 3):
/// the access token `services/auth` issues on every signup / login / recover /
/// passkey / ACCESS-factor finish, the refresh credential beside it, both
/// expiries, and the key generation they were issued under.
///
/// Mirror of `apps/web/src/auth/session-token-store.ts` (IndexedDB,
/// AES-GCM under the non-extractable device root key). iOS has no
/// device-root-key wrap for its per-account rows — ``ExportKeyStore`` /
/// ``AccountIdentitySeedStore`` are direct Keychain items — so this store
/// follows that same posture rather than inventing a second wrap.
///
/// ## Why it exists
///
/// Without it, a relaunch restored the `(accountId, username)` marker and
/// found the device key, flipped to ``AuthState/session(_:)``, and left
/// ``AuthRepository/identitySecrets`` `nil` — chat and contacts dead
/// behind a "log out and log back in" hint. The account identity seed is
/// already on this device (ADR-0017) and readable without the password;
/// the session token was the only missing input. See
/// ``AuthRepository`` `rehydrate(nowMs:)` for the restore itself.
///
/// ## Threat model
///
/// The pair grants ACCESS (bearer-authed `services/discovery` and
/// `services/auth` writes for this account; the refresh credential renews
/// that access for up to 30 days), not identity: neither token can decrypt
/// a message or sign as the account. Both are strictly less sensitive than
/// the AccountIdentity seed stored beside them under the same Keychain
/// class, so persisting them does not move the device's security boundary:
///
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — device-bound,
///     no iCloud Keychain sync, hardware-isolated class key (invariant #7).
///   * App-private: deliberately NOT written to the shared access group.
///     The NotificationServiceExtension reads the DS without a bearer
///     token, so there is no reason to widen this item's read scope.
///
/// Deleted on ``AuthRepository/logout()`` (so a relaunch after logout
/// lands logged out) and on ``AccountLocalDataWipe``.
///
/// ## Persistence layout
///
/// One Keychain item per account, generic-password class:
///
///   * `kSecAttrService = "me.lakko.session-token.v2"`
///   * `kSecAttrAccount = accountIdB64`
///   * `kSecValueData = JSON { "token", "expires_at_ms", "refresh_token",
///     "refresh_expires_at_ms", "key_generation", "device_id"? }`
///
/// v2 is a flag day (ADR-0073: no users, no v1 token to keep valid): a v1
/// row under the old service tag is never read and is deleted beside the
/// v2 row, so a device that upgraded lands logged out once.
public enum SessionTokenStore {

    /// Service tag — versioned so a future shape change lands beside v2.
    public static let serviceTag: String = "me.lakko.session-token.v2"

    /// The pre-ADR-0073 tag, deleted on every delete so no row outlives it.
    static let legacyServiceTag: String = "me.lakko.session-token.v1"

    /// Persist `token` for `accountIdB64`, replacing any existing row.
    ///
    /// - Throws: ``SessionTokenStoreError`` on an empty token, an encode
    ///   failure, or a non-success `OSStatus` from `SecItemAdd`.
    public static func putSessionToken(
        accountIdB64: String,
        token: PersistedSessionToken
    ) throws {
        guard !token.sessionToken.isEmpty else {
            throw SessionTokenStoreError.argument("session token must be non-empty")
        }
        let payload: Data
        do {
            payload = try JSONEncoder().encode(token)
        } catch {
            throw SessionTokenStoreError.argument("session token encode failed: \(error)")
        }
        deleteSessionToken(accountIdB64: accountIdB64)
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: payload
        ]
        let status = SecItemAdd(query as CFDictionary, nil)
        if status != errSecSuccess {
            throw SessionTokenStoreError.keychain(status: status, operation: "SecItemAdd")
        }
    }

    /// Read the cached token for `accountIdB64`. `nil` when absent.
    /// Does NOT judge expiry — that is the restore decision's job, so the
    /// rule lives in one testable place.
    ///
    /// - Throws: ``SessionTokenStoreError`` on a Keychain fault or an
    ///   undecodable row.
    public static func getSessionToken(accountIdB64: String) throws -> PersistedSessionToken? {
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
                throw SessionTokenStoreError.keychain(
                    status: status,
                    operation: "SecItemCopyMatching: unexpected payload type"
                )
            }
            do {
                return try JSONDecoder().decode(PersistedSessionToken.self, from: data)
            } catch {
                throw SessionTokenStoreError.argument("stored session token is undecodable")
            }
        case errSecItemNotFound:
            return nil
        default:
            throw SessionTokenStoreError.keychain(status: status, operation: "SecItemCopyMatching")
        }
    }

    /// Erase the cached token for `accountIdB64`. No-op when absent.
    public static func deleteSessionToken(accountIdB64: String) {
        for tag in [serviceTag, legacyServiceTag] {
            let query: [CFString: Any] = [
                kSecClass: kSecClassGenericPassword,
                kSecAttrService: tag,
                kSecAttrAccount: accountIdB64
            ]
            _ = SecItemDelete(query as CFDictionary)
        }
    }
}

/// A persisted session pair (ADR-0073): the access token and its expiry,
/// the refresh credential and its expiry, the key generation.
public struct PersistedSessionToken: Equatable, Codable {
    public let sessionToken: String
    public let sessionExpiresAtMs: Int64
    public let refreshToken: String
    public let refreshExpiresAtMs: Int64
    public let keyGeneration: UInt64
    /// ADR-0074 — the device the pair was issued for, as this client
    /// asserted it (``SessionPair/deviceIdB64``); absent for an unbound pair.
    public let deviceIdB64: String?

    public init(
        sessionToken: String,
        sessionExpiresAtMs: Int64,
        refreshToken: String,
        refreshExpiresAtMs: Int64,
        keyGeneration: UInt64,
        deviceIdB64: String? = nil
    ) {
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
        self.refreshToken = refreshToken
        self.refreshExpiresAtMs = refreshExpiresAtMs
        self.keyGeneration = keyGeneration
        self.deviceIdB64 = deviceIdB64
    }

    public init(pair: SessionPair) {
        self.init(
            sessionToken: pair.sessionToken,
            sessionExpiresAtMs: pair.sessionExpiresAtMs,
            refreshToken: pair.refreshToken,
            refreshExpiresAtMs: pair.refreshExpiresAtMs,
            keyGeneration: pair.keyGeneration,
            deviceIdB64: pair.deviceIdB64
        )
    }

    /// The pair as the provider installs it.
    public var pair: SessionPair {
        SessionPair(
            sessionToken: sessionToken,
            sessionExpiresAtMs: sessionExpiresAtMs,
            refreshToken: refreshToken,
            refreshExpiresAtMs: refreshExpiresAtMs,
            keyGeneration: keyGeneration
        ).naming(deviceIdB64: deviceIdB64)
    }

    enum CodingKeys: String, CodingKey {
        case sessionToken = "token"
        case sessionExpiresAtMs = "expires_at_ms"
        case refreshToken = "refresh_token"
        case refreshExpiresAtMs = "refresh_expires_at_ms"
        case keyGeneration = "key_generation"
        case deviceIdB64 = "device_id"
    }
}

/// Error vocabulary for ``SessionTokenStore``.
public enum SessionTokenStoreError: Error, LocalizedError {
    case argument(String)
    case keychain(status: OSStatus, operation: String)

    public var errorDescription: String? {
        switch self {
        case .argument(let message):
            return message
        case .keychain(let status, let operation):
            return "SessionTokenStore \(operation) failed: OSStatus \(status)"
        }
    }
}
