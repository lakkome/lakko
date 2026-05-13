// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Per-device cache of the `(accountId, username)` marker.
///
/// Mirror of:
///
///   * `apps/web/src/auth/AuthContext.tsx`'s `localStorage` pair
///     (1.17d4)
///   * `apps/android/.../auth/SessionStore.kt` (1.15d3)
///
/// Neither field is secret on its own:
///
///   * `accountIdB64` — base64 of the 16-byte UUIDv7 the client
///     picked at signup; the server uses it on every subsequent
///     login. Knowing it doesn't help an attacker derive any key
///     (OPAQUE is password-blinded) and doesn't reveal anything
///     about the user that isn't already on a captured device.
///   * `username` — human-visible handle. Routinely sent to the
///     discovery service in plaintext (or rather, OPRF-blinded once
///     the discovery surface lands in 1.14e).
///
/// Plain ``UserDefaults`` is the right tool here: small, synchronous,
/// no encryption overhead. The Keychain-backed material lives in
/// ``DeviceKeyStore``, not here.
///
/// Versioned via the `schemaVersionKey` so a future shape change can
/// detect + drop old rows instead of mis-parsing them.
public final class SessionStore {

    // MARK: - Storage backend

    /// Backing ``UserDefaults`` suite. Constructor parameter so tests
    /// can pass a transient instance — the production path uses the
    /// shared `.standard` suite which is process-scoped + per-app.
    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    // MARK: - Public surface

    /// Load the persisted session marker, or `nil` when:
    ///
    ///   * no record exists,
    ///   * the schema version doesn't match (the row is dropped on
    ///     the next ``save(_:)``), or
    ///   * one of the fields is missing (corrupted defaults).
    public func load() -> PersistedSession? {
        let version = defaults.integer(forKey: schemaVersionKey)
        guard version == schemaVersion else { return nil }
        guard let accountIdB64 = defaults.string(forKey: accountIdKey) else {
            return nil
        }
        guard let username = defaults.string(forKey: usernameKey) else {
            return nil
        }
        return PersistedSession(
            accountIdB64: accountIdB64,
            username: username
        )
    }

    /// Replace the cached record. ``UserDefaults`` writes are
    /// synchronously visible to subsequent reads within the same
    /// process; the kernel commits to disk lazily (the
    /// `.standard` suite's `synchronize()` is deprecated since iOS
    /// 12). For our use case (next-launch rehydrate) the in-process
    /// visibility is what matters.
    public func save(_ session: PersistedSession) {
        defaults.set(schemaVersion, forKey: schemaVersionKey)
        defaults.set(session.accountIdB64, forKey: accountIdKey)
        defaults.set(session.username, forKey: usernameKey)
    }

    /// Drop the cached record. Used by ``AuthRepository/forgetDevice()``.
    ///
    /// Note: this does NOT delete the Keychain-stored device key —
    /// ``DeviceKeyStore/deleteDeviceKey(accountIdB64:)`` must be
    /// called separately. The two layers are intentionally
    /// independent so `logout()` (drop in-memory session, keep
    /// persistence) and `forgetDevice()` (wipe everything) have
    /// clean primitives to build on.
    public func clear() {
        defaults.removeObject(forKey: schemaVersionKey)
        defaults.removeObject(forKey: accountIdKey)
        defaults.removeObject(forKey: usernameKey)
    }

    // MARK: - Versioning

    /// Schema-version slot key. Bump ``schemaVersion`` when the
    /// shape changes; rows under the old version are treated as
    /// absent + overwritten on the next save.
    private let schemaVersionKey: String = "me.lakko.session.v1.schema"
    private let schemaVersion: Int = 1

    private let accountIdKey: String = "me.lakko.session.v1.account_id_b64"
    private let usernameKey: String = "me.lakko.session.v1.username"
}

/// Snapshot of the per-device session marker.
public struct PersistedSession: Equatable {
    /// Base64-encoded raw 16-byte UUIDv7 account id. Same shape as
    /// the web's `localStorage` `accountIdB64` field.
    public let accountIdB64: String
    /// The human-visible handle the user picked at signup, e.g.
    /// `alice` (no `@`). Until 1.14e wires discovery in, this is
    /// set from the typed input verbatim — there's no normalisation
    /// step yet.
    public let username: String

    public init(accountIdB64: String, username: String) {
        self.accountIdB64 = accountIdB64
        self.username = username
    }
}
