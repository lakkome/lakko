// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

// ADR-0074 section 0 and 2 — the client-side rules of "a session names a
// device", free of state:
//
//   * `LandingDevice`          what a session landing sends as `device_id_b64`;
//   * `SessionDeviceBinding`   whether the installed pair names this device,
//                              and what a `bind-device` refusal means;
//   * `IdentityKeyRecord`      whether the genesis mint still has to record
//                              the account key at auth (section 0.1);
//   * `IdentityKeyRecordMarks` the per-account "recorded" mark;
//   * `LogoutOwed`             the server half of Log out and its one retry.
//
// `AuthRepository` is the state around them.

// MARK: - What a landing sends

/// The device id a session landing asserts (ADR-0074 section 0.4).
///
/// The id is this device's conversation leaf signature key, the same 32
/// bytes that key its discovery directory row. On iOS the device identity is
/// kept per ACCOUNT (``DeviceIdentityStore``), so it exists before the finish
/// only where the account is known before the finish: signup (the client
/// generates the account id) and a sign-in of the account already signed in
/// (the proof login). Every other landing learns the account id from the
/// finish response and binds the session afterwards
/// (``SessionDeviceBinding``).
public enum LandingDevice {

    /// The fixed length of a device id.
    public static let length = 32

    /// `id` when it can name a device: 32 bytes and not all zero (the
    /// all-zero id is the server's "no device"). Anything else is `nil`.
    public static func usable(_ id: Data?) -> Data? {
        guard let id, id.count == length, id.contains(where: { $0 != 0 }) else { return nil }
        return id
    }

    /// What the finish of a landing carries.
    ///
    /// - Parameters:
    ///   - accountKnownBeforeFinish: the client holds the account id before
    ///     the finish request is built.
    ///   - ownDeviceId: this device's id for that account, when it could be
    ///     read or minted.
    /// - Returns: the id to send, or `nil` to send no field (an unbound
    ///   session, bound right after the landing).
    public static func asserted(accountKnownBeforeFinish: Bool, ownDeviceId: Data?) -> Data? {
        accountKnownBeforeFinish ? usable(ownDeviceId) : nil
    }
}

// MARK: - Is the session bound to this device

/// Whether the installed session pair names this device, and what to do when
/// it does not.
public enum SessionDeviceBinding {

    public enum Decision: Equatable {
        /// The pair names this device. Nothing to do.
        case bound
        /// The pair names no device: `POST /auth/session/bind-device` with
        /// this id.
        case bind(Data)
        /// This device has no device identity to name (its Keychain could
        /// not be read or written). The session stays unbound: it cannot
        /// publish a KeyPackage and cannot be revoked per device.
        case noDeviceIdentity
        /// The pair names ANOTHER device id (the device identity was
        /// replaced under a live session). The server would answer
        /// `409 device-already-bound`; a fresh sign-in is what binds again.
        case boundToAnother
    }

    public static func decide(pair: SessionPair, ownDeviceId: Data?) -> Decision {
        guard let own = LandingDevice.usable(ownDeviceId) else { return .noDeviceIdentity }
        guard let noted = pair.deviceIdB64 else { return .bind(own) }
        return noted == own.base64EncodedString() ? .bound : .boundToAnother
    }

    /// What `POST /auth/session/bind-device` refused with.
    public enum Refusal: Equatable {
        /// `401 device-revoked`: this device was removed. Wipe.
        case deviceRevoked
        /// `401 key-generation-superseded`: the keys were reset elsewhere.
        case superseded
        /// `401 refresh-invalid`: sign in again; nothing is wiped.
        case refreshInvalid
        /// `409 device-already-bound`: the session names another device.
        case alreadyBound
        /// `400`: a bad or all-zero id.
        case badDeviceId
        /// Anything else (offline, `5xx`): try again at the next landing.
        case other
    }

    public static func classify(status: Int, body: String) -> Refusal {
        let code = ((try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any])?["error"]
            as? String
        switch (status, code) {
        case (401, "device-revoked"): return .deviceRevoked
        case (401, "key-generation-superseded"): return .superseded
        case (401, "refresh-invalid"): return .refreshInvalid
        case (409, "device-already-bound"): return .alreadyBound
        case (400, _): return .badDeviceId
        default: return .other
        }
    }

    /// Read the refusal out of what the client threw.
    public static func refusal(of error: Error) -> Refusal {
        guard case .http(let status, _, let body) = error as? AuthClientError else { return .other }
        return classify(status: status, body: body)
    }
}

// MARK: - The recorded identity key (section 0.1)

/// Whether the account key still has to be recorded at auth.
///
/// `POST /auth/genesis/attest` records the account key on its FIRST call and
/// afterwards attests only that key. Without a recorded key a device removal
/// answers `account-key-required`, and whoever holds a session first could
/// record their own key (ADR-0074 "T1 as built" point 14). So the client
/// mints at signup and on every landing until the mint succeeded once for
/// the key it holds, and again after a key reset (the key changed).
public enum IdentityKeyRecord {

    /// - Parameters:
    ///   - recordedPubkeyB64: the mark of the last successful mint for this
    ///     account (standard base64 of the key), or `nil`.
    ///   - currentPubkey: the account key this device derives now.
    public static func needsMint(recordedPubkeyB64: String?, currentPubkey: Data) -> Bool {
        recordedPubkeyB64 != currentPubkey.base64EncodedString()
    }

    /// What the mint answered when it failed.
    public enum Refusal: Equatable {
        /// `409 identity-key-mismatch`: auth holds ANOTHER key for the
        /// account. This device's keys are not the account's (a reset
        /// elsewhere, which the landing rule reports), or its own reset is
        /// still being published. Not recorded; nothing is retried in a loop.
        case identityKeyMismatch
        /// `400`: the key is not a usable Ed25519 key.
        case unusableKey
        /// Offline, `401`, `5xx`: the next landing tries again.
        case retryLater
    }

    public static func classify(status: Int, body: String) -> Refusal {
        let code = ((try? JSONSerialization.jsonObject(with: Data(body.utf8))) as? [String: Any])?["error"]
            as? String
        switch (status, code) {
        case (409, "identity-key-mismatch"): return .identityKeyMismatch
        case (400, _): return .unusableKey
        default: return .retryLater
        }
    }

    public static func refusal(of error: Error) -> Refusal {
        guard case .http(let status, _, let body) = error as? AuthClientError else { return .retryLater }
        return classify(status: status, body: body)
    }
}

/// The per-account "account key recorded at auth" mark, in `UserDefaults`.
/// It holds a public key only. Cleared by the account wipe.
public struct IdentityKeyRecordMarks {
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    static func key(accountIdB64: String) -> String {
        "lakko.identity-key-recorded.v1.\(accountIdB64)"
    }

    public func recorded(accountIdB64: String) -> String? {
        defaults.string(forKey: Self.key(accountIdB64: accountIdB64))
    }

    public func markRecorded(pubkey: Data, accountIdB64: String) {
        guard let gate = AccountStoreGate.enterUnlessWiped(accountIdB64) else { return }
        defer { gate.leave() }
        defaults.set(pubkey.base64EncodedString(), forKey: Self.key(accountIdB64: accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64: accountIdB64))
    }
}

// MARK: - Log out at the server (section 2.1, 2.3)

/// The server half of Log out: `POST /auth/session/logout`, best effort,
/// with one retry at the next launch from a small owed record.
///
/// The local sign-out never waits for it. The record holds the session pair
/// the logout has to present (the access token lives 15 minutes, so the
/// retry may have to refresh first), which is why it sits in the Keychain
/// under the same class as the live pair, and why it is dropped after the
/// one retry whatever that retry came to.
public enum LogoutOwed {

    /// What one attempt came to.
    public enum Attempt: Equatable {
        /// `204`.
        case done
        /// The server answered with a refusal (`401`: the session was
        /// already dead, which is what a logout wants).
        case refused
        /// No answer (offline).
        case unreachable
    }

    public static func attempt(of error: Error?) -> Attempt {
        guard let error else { return .done }
        if case .http = error as? AuthClientError { return .refused }
        return .unreachable
    }

    /// Whether the owed record stays after an attempt. Only the FIRST
    /// attempt (at Log out) may leave it, and only when the server was not
    /// reached; the retry at the next launch always drops it.
    public static func keepsRecord(after attempt: Attempt, isLaunchRetry: Bool) -> Bool {
        !isLaunchRetry && attempt == .unreachable
    }

    /// What the launch retry presents.
    public enum RetryPlan: Equatable {
        /// The access token is still live: log out with it.
        case logout(sessionToken: String)
        /// It expired and the refresh credential is live: refresh, then log
        /// out with the new access token.
        case refreshThenLogout(refreshToken: String)
        /// Both expired: every credential the logout would revoke is dead
        /// already. Drop the record.
        case drop
    }

    public static func retryPlan(pair: SessionPair, nowMs: Int64) -> RetryPlan {
        if !pair.sessionToken.isEmpty, pair.sessionExpiresAtMs > nowMs {
            return .logout(sessionToken: pair.sessionToken)
        }
        if pair.refreshUsable(nowMs: nowMs) {
            return .refreshThenLogout(refreshToken: pair.refreshToken)
        }
        return .drop
    }
}

/// Where the owed logout is kept. One record per device: a second Log out
/// before the first was confirmed replaces it.
public protocol LogoutOwedStoring {
    func load() -> SessionPair?
    func store(_ pair: SessionPair)
    func clear()
}

/// The Keychain store of the owed logout
/// (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, app-private).
public struct KeychainLogoutOwedStore: LogoutOwedStoring {
    static let serviceTag = "me.lakko.logout-owed.v1"
    static let account = "owed"

    public init() {}

    public func load() -> SessionPair? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.serviceTag,
            kSecAttrAccount: Self.account,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data,
              let stored = try? JSONDecoder().decode(PersistedSessionToken.self, from: data) else {
            return nil
        }
        return stored.pair
    }

    public func store(_ pair: SessionPair) {
        guard let payload = try? JSONEncoder().encode(PersistedSessionToken(pair: pair)) else { return }
        clear()
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.serviceTag,
            kSecAttrAccount: Self.account,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: payload
        ]
        let status = SecItemAdd(query as CFDictionary, nil)
        if status != errSecSuccess {
            NSLog("[auth] WARN the owed logout was not stored: OSStatus \(status)")
        }
    }

    public func clear() {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.serviceTag,
            kSecAttrAccount: Self.account
        ]
        _ = SecItemDelete(query as CFDictionary)
    }
}

/// An in-memory store, for tests and previews.
public final class InMemoryLogoutOwedStore: LogoutOwedStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var pair: SessionPair?

    public init() {}

    public func load() -> SessionPair? {
        lock.lock(); defer { lock.unlock() }
        return pair
    }

    public func store(_ pair: SessionPair) {
        lock.lock(); self.pair = pair; lock.unlock()
    }

    public func clear() {
        lock.lock(); pair = nil; lock.unlock()
    }
}

/// Why this device shows the signed-out screen and wipes itself.
public enum DeviceSignOutCause: Equatable, Sendable {
    /// The account keys were reset on another device (ADR-0073 section 3.5).
    case keysReset
    /// This device was removed from the account (ADR-0074 section 2.3).
    case deviceRemoved
}
