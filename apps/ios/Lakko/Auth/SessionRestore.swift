// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The on-device material a relaunch needs to restore a session without
/// a password, behind a seam so the restore decision can be driven by a
/// fake in tests. Production is ``KeychainSessionPersistence``.
///
/// Every read collapses a store fault to "absent": the restore must fail
/// CLOSED to the logged-out state, never land half a session.
public protocol SessionPersistence {
    /// Is the device root key present for this account? A Keychain fault
    /// reads as `false`.
    func hasDeviceKey(accountIdB64: String) -> Bool
    /// The ADR-0017 AccountIdentity (phrase) seed, or `nil`.
    func accountIdentitySeed(accountIdB64: String) -> Data?
    /// The cached OPAQUE export key, or `nil`. Optional input: the
    /// no-password paths already run sessions with an empty export key.
    func exportKey(accountIdB64: String) -> Data?
    /// The persisted session token, or `nil`.
    func sessionToken(accountIdB64: String) -> PersistedSessionToken?
    /// Persist the live session token so the next launch can restore.
    func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws
    /// Drop the persisted session token.
    func deleteSessionToken(accountIdB64: String)
}

/// Production ``SessionPersistence`` over the Keychain stores.
public struct KeychainSessionPersistence: SessionPersistence {
    public init() {}

    public func hasDeviceKey(accountIdB64: String) -> Bool {
        do {
            return try DeviceKeyStore.getDeviceKey(accountIdB64: accountIdB64) != nil
        } catch {
            NSLog("session restore: device-key read failed: \(error)")
            return false
        }
    }

    public func accountIdentitySeed(accountIdB64: String) -> Data? {
        do {
            return try AccountIdentitySeedStore.getAccountIdentitySeed(accountIdB64: accountIdB64)
        } catch {
            NSLog("session restore: account-identity-seed read failed: \(error)")
            return nil
        }
    }

    public func exportKey(accountIdB64: String) -> Data? {
        do {
            return try ExportKeyStore.getExportKey(accountIdB64: accountIdB64)
        } catch {
            NSLog("session restore: export-key read failed: \(error)")
            return nil
        }
    }

    public func sessionToken(accountIdB64: String) -> PersistedSessionToken? {
        do {
            return try SessionTokenStore.getSessionToken(accountIdB64: accountIdB64)
        } catch {
            NSLog("session restore: session-token read failed: \(error)")
            return nil
        }
    }

    public func putSessionToken(_ token: PersistedSessionToken, accountIdB64: String) throws {
        try SessionTokenStore.putSessionToken(accountIdB64: accountIdB64, token: token)
    }

    public func deleteSessionToken(accountIdB64: String) {
        SessionTokenStore.deleteSessionToken(accountIdB64: accountIdB64)
    }
}

/// Where a relaunch lands.
///
/// A relaunch lands in exactly one of two states — logged out, or a full
/// session with its secrets — and that is the point: before this, a marker
/// plus a device key produced ``AuthState/session(_:)`` with no
/// ``IdentitySecrets``, a session in name only.
public enum SessionRestoreDecision: Equatable {
    /// Logged out. `cachedUsername` pre-fills the Login form when a
    /// marker existed.
    case anon(cachedUsername: String?)
    /// A full session: the state flip and the secrets land together.
    case session(account: SessionAccount, secrets: IdentitySecrets)
    /// ``anon(cachedUsername:)`` AND the persisted token must be discarded
    /// (it is expired or unusable), so a later launch does not re-read it.
    case anonDiscardingToken(cachedUsername: String?)
}

public enum SessionRestore {

    /// Raw account id length (UUIDv7).
    static let accountIdLength = 16

    /// Decide the launch state from what is on the device.
    ///
    /// Every input is REQUIRED except the export key:
    ///
    ///   * no marker → logged out, no cached username;
    ///   * a marker whose account id does not decode to 16 bytes, no
    ///     device key, or no 64-byte AccountIdentity seed → logged out
    ///     with the cached username (the seed gap is exactly the
    ///     ADR-0017 fresh-device case; the password login re-prompts for
    ///     the phrase, which is the only correct way to fill it);
    ///   * no token → logged out;
    ///   * an empty access token, or a pair whose refresh credential AND
    ///     access token are both expired → logged out AND discard the token.
    ///
    /// ADR-0073 section 3.3: the REFRESH credential is what restores the
    /// session. The access token may already be expired — the silent restore
    /// refreshes instead of reusing it. A pair with no refresh credential (a
    /// keys-only pairing outcome, a fixture) restores only while its access
    /// token is live. Expiry is judged against the device clock with no
    /// grace: the server re-checks the credential on every use, so this gate
    /// only stops a known-dead session from being restored, it is not the
    /// authority.
    ///
    /// Security trade-off, stated plainly: the reload barrier becomes the
    /// device key + the Keychain item class
    /// (`AfterFirstUnlockThisDeviceOnly`). That is the same boundary the
    /// NotificationServiceExtension already relies on to read this seed
    /// without a password (ADR-0041, C-59), and the same material a warm
    /// password or passkey login on this device already reads without
    /// re-entering the phrase. Anyone who can read the seed can already act
    /// as the account; the token adds ACCESS, not identity.
    public static func decide(
        marker: PersistedSession?,
        persistence: SessionPersistence,
        nowMs: Int64
    ) -> SessionRestoreDecision {
        guard let marker else {
            return .anon(cachedUsername: nil)
        }
        let username = marker.username
        guard let accountId = base64Decode(marker.accountIdB64),
              accountId.count == accountIdLength else {
            return .anon(cachedUsername: username)
        }
        guard persistence.hasDeviceKey(accountIdB64: marker.accountIdB64) else {
            return .anon(cachedUsername: username)
        }
        guard let seed = persistence.accountIdentitySeed(accountIdB64: marker.accountIdB64),
              seed.count == AccountIdentitySeedStore.seedLength else {
            return .anon(cachedUsername: username)
        }
        guard let token = persistence.sessionToken(accountIdB64: marker.accountIdB64) else {
            return .anon(cachedUsername: username)
        }
        guard !token.sessionToken.isEmpty,
              token.pair.refreshUsable(nowMs: nowMs) || token.sessionExpiresAtMs > nowMs else {
            return .anonDiscardingToken(cachedUsername: username)
        }
        let secrets = IdentitySecrets(
            accountId: accountId,
            exportKey: persistence.exportKey(accountIdB64: marker.accountIdB64) ?? Data(),
            accountIdentitySeed: seed,
            session: token.pair
        )
        return .session(
            account: SessionAccount(accountIdB64: marker.accountIdB64, username: username),
            secrets: secrets
        )
    }

    /// Current wall-clock time in unix milliseconds.
    public static func nowMs() -> Int64 {
        Int64(Date().timeIntervalSince1970 * 1000)
    }
}
