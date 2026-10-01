// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed cache for the auth **session token** — the bearer
/// token `services/auth` issues on every signup / login / recover /
/// passkey / ACCESS-factor finish, plus its server-stated expiry.
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
/// The token grants ACCESS (bearer-authed `services/discovery` and
/// `services/auth` writes for this account until its expiry), not
/// identity: it cannot decrypt a message or sign as the account. It is
/// strictly less sensitive than the AccountIdentity seed stored beside it
/// under the same Keychain class, so persisting it does not move the
/// device's security boundary:
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
///   * `kSecAttrService = "me.lakko.session-token.v1"`
///   * `kSecAttrAccount = accountIdB64`
///   * `kSecValueData = JSON { "token": String, "expires_at_ms": Int64 }`
public enum SessionTokenStore {

    /// Service tag — versioned so a future shape change lands beside v1.
    public static let serviceTag: String = "me.lakko.session-token.v1"

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
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64
        ]
        _ = SecItemDelete(query as CFDictionary)
    }
}

/// A persisted session token + its server-stated expiry (unix ms).
public struct PersistedSessionToken: Equatable, Codable {
    public let sessionToken: String
    public let sessionExpiresAtMs: Int64

    public init(sessionToken: String, sessionExpiresAtMs: Int64) {
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
    }

    enum CodingKeys: String, CodingKey {
        case sessionToken = "token"
        case sessionExpiresAtMs = "expires_at_ms"
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
