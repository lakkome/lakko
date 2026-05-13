// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Single source of truth for "is the user authenticated on this
/// device?".
///
/// Mirror of:
///
///   * `apps/web/src/auth/AuthContext.tsx` (1.17d4)
///   * `apps/android/.../auth/AuthRepository.kt` (1.15d4)
///
/// Owns:
///
///   * an ``AuthClient`` (URLSession HTTP),
///   * a ``SessionStore`` (UserDefaults-backed `(accountId,
///     username)` marker),
///   * implicit access to ``DeviceKeyStore`` (Keychain-stored
///     device root key).
///
/// `ObservableObject` + two `@Published` slots drive the SwiftUI UI:
///
///   * ``state`` — discriminated ``AuthState`` (loading / anon /
///     session). The router observes this to decide which screen to
///     show.
///   * ``identitySecrets`` — in-memory `(accountId, exportKey)`
///     after a fresh authenticate; `nil` after a session-restore
///     from disk or after logout.
///
/// `@MainActor` because every mutation lands on
/// SwiftUI's main runloop; the async flows hop out to
/// `Task.detached` for the PoW solve + the URLSession call internally
/// (see ``AuthClient`` and ``solvePow(seedB64:effortBits:)``).
///
/// ## What this class deliberately doesn't do
///
/// The web `AuthContext` also handles username registration on the
/// discovery service, KeyPackage upload, MLS state rehydration, and
/// ChatContext fan-out. None of that surface exists on the iOS side
/// yet (discovery client lands in 1.14e, MLS chat surface in 1.14f).
/// When those crates show up we extend this repository — the shape
/// is intentional, not vestigial.
@MainActor
public final class AuthRepository: ObservableObject {

    // MARK: - Published state

    /// Discriminated union driving the UI router. Updated
    /// synchronously from the main actor — SwiftUI re-renders are
    /// scheduled atomically with the assignment.
    @Published public private(set) var state: AuthState = .loading

    /// In-memory identity secrets — the OPAQUE export key + the raw
    /// account id bytes for the active session.
    ///
    /// Mirror of `apps/web/src/auth/AuthContext.tsx::identitySecrets`.
    /// Populated whenever the user has **just authenticated**
    /// (signup, login, recover) — those flows have the export key
    /// in scope and can stash it for the duration of the current
    /// process lifetime. **Cleared on logout / forgetDevice**.
    ///
    /// **NOT populated on a session restore from disk**: the
    /// export key is not persisted (only the device root key,
    /// which is a one-way HKDF derivation — the export key cannot
    /// be recovered from the device key bytes). A returning user
    /// whose session rehydrates from ``SessionStore`` lands in a
    /// state where ``state`` is `.session` but ``identitySecrets``
    /// is `nil`. Operations that need to sign on behalf of the
    /// user (contact-request send, MLS commits, …) MUST check this
    /// and surface a "please re-enter your password" prompt when
    /// it's `nil`.
    @Published public private(set) var identitySecrets: IdentitySecrets?

    // MARK: - Wiring

    private let sessionStore: SessionStore
    private let authClientFactory: () -> AuthClient

    public init(
        sessionStore: SessionStore = SessionStore(),
        authClientFactory: @escaping () -> AuthClient = { AuthClient() }
    ) {
        self.sessionStore = sessionStore
        self.authClientFactory = authClientFactory
        self.state = rehydrate()
    }

    // MARK: - Lifecycle

    /// Inspect persisted state and resolve the initial ``AuthState``:
    ///
    ///   * No ``SessionStore`` record → fresh device, ``AuthState/anon(cachedUsername:)``.
    ///   * Record present but device key absent (Keychain entry
    ///     deleted, device reset, etc.) → ``AuthState/anon(cachedUsername:)``
    ///     with the cached username so the Login form can pre-fill.
    ///   * Record + device key both present → ``AuthState/session(_:)``.
    private func rehydrate() -> AuthState {
        guard let cached = sessionStore.load() else {
            return .anon(cachedUsername: nil)
        }
        // We don't *use* the device key bytes here; we just check
        // they exist. Keychain reads are cheap (no decryption until
        // the data is touched).
        let keyBytes: Data?
        do {
            keyBytes = try DeviceKeyStore.getDeviceKey(accountIdB64: cached.accountIdB64)
        } catch {
            // Keychain failure (revoked entry, device-reset
            // post-restore, etc.) — treat as "no key on this device"
            // so the user re-enters their password rather than
            // seeing the app crash.
            return .anon(cachedUsername: cached.username)
        }
        guard keyBytes != nil else {
            return .anon(cachedUsername: cached.username)
        }
        return .session(
            SessionAccount(
                accountIdB64: cached.accountIdB64,
                username: cached.username
            )
        )
    }

    // MARK: - Flows

    /// Drive the full OPAQUE-PAKE signup flow.
    ///
    /// Steps:
    ///   1. Validate the username (non-empty after trim).
    ///   2. Call ``AuthClient/signup(password:enrolRecovery:)`` with
    ///      `enrolRecovery = true`.
    ///   3. Save the `(accountId, username)` marker to ``SessionStore``.
    ///   4. Derive the device root key via
    ///      ``deriveDeviceRootKey(exportKey:accountIdB64:)``.
    ///   5. Stash via ``DeviceKeyStore/putDeviceKey(accountIdB64:rootKey:)``.
    ///   6. Flip state to ``AuthState/session(_:)``.
    ///   7. Return the 24-word recovery phrase for the UI to display
    ///      **once**.
    ///
    /// The order matters. We persist the marker BEFORE deriving the
    /// device key so a crash between (3) and (5) leaves the user
    /// with a recoverable state on next launch (the rehydrate gate
    /// detects "marker but no device key" and asks for the password
    /// again). The reverse ordering would leave a working device key
    /// in Keychain with no record of who it belongs to.
    public func signup(
        username: String,
        password: Data
    ) async throws -> SignupOutcome {
        let trimmed = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw AuthClientError.argument("username must be non-empty")
        }
        let client = authClientFactory()
        let result = try await client.signup(password: password, enrolRecovery: true)

        let accountIdB64 = base64Encode(result.accountId)
        sessionStore.save(
            PersistedSession(
                accountIdB64: accountIdB64,
                username: trimmed
            )
        )
        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: accountIdB64
        )
        try DeviceKeyStore.putDeviceKey(
            accountIdB64: accountIdB64,
            rootKey: deviceRootKey
        )

        // Stash the export key for ContactsClient / future ChatClient
        // and friends. The bytes survive until logout / forgetDevice
        // clears the publisher; signup is a one-shot ceremony so we
        // don't expect any surprises here.
        identitySecrets = IdentitySecrets(
            accountId: result.accountId,
            exportKey: result.exportKey
        )
        state = .session(
            SessionAccount(
                accountIdB64: accountIdB64,
                username: trimmed
            )
        )

        guard let phrase = result.recoveryPhrase else {
            // Should be unreachable — we asked for `enrolRecovery =
            // true` so the AuthClient always returns a phrase.
            throw AuthClientError.argument(
                "internal: signup completed without a recovery phrase"
            )
        }
        return SignupOutcome(recoveryPhrase: phrase)
    }

    /// Drive the full OPAQUE-PAKE login flow.
    ///
    /// Mirror of `apps/android`'s `AuthRepository.login`. Steps:
    ///
    ///   1. Resolve the typed username to an `accountId`. In 1.14d4
    ///      the only resolver is the on-device ``SessionStore``
    ///      cache — if the typed username matches the cached entry,
    ///      we use the cached `accountId` and skip a round-trip; if
    ///      it doesn't, we throw `invalid credentials` (the same
    ///      error as a wrong password — indistinguishable failure
    ///      preserves OPAQUE's no-probing property).
    ///      Cross-device login via the discovery service is 1.14e's
    ///      job; until then a fresh device requires running signup
    ///      first.
    ///   2. Call ``AuthClient/login(accountId:password:)``.
    ///   3. Re-derive the device root key from the OPAQUE export
    ///      key. The derivation is deterministic for the same
    ///      password, so re-running it on every login produces the
    ///      same bytes the original signup wrote — the
    ///      ``DeviceKeyStore`` upsert is harmless.
    ///   4. Re-save the ``SessionStore`` marker (idempotent — same
    ///      `accountId`, same `username`).
    ///   5. Flip state to ``AuthState/session(_:)``.
    public func login(
        username: String,
        password: Data
    ) async throws {
        let trimmed = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw AuthClientError.argument("username must be non-empty")
        }
        guard !password.isEmpty else {
            throw AuthClientError.argument("password must be non-empty")
        }
        guard let cached = sessionStore.load() else {
            throw AuthClientError.http(
                status: 401,
                endpoint: "/login/start",
                body: "no account on this device — sign up first"
            )
        }
        guard cached.username == trimmed else {
            throw AuthClientError.http(
                status: 401,
                endpoint: "/login/start",
                body: "invalid credentials"
            )
        }
        guard let accountIdBytes = base64Decode(cached.accountIdB64) else {
            // Corrupted session row — fail safe by wiping it +
            // surfacing as anon.
            sessionStore.clear()
            throw AuthClientError.argument(
                "stored account id is malformed; please sign up again"
            )
        }

        let client = authClientFactory()
        let result = try await client.login(
            accountId: accountIdBytes,
            password: password
        )

        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: cached.accountIdB64
        )
        try DeviceKeyStore.putDeviceKey(
            accountIdB64: cached.accountIdB64,
            rootKey: deviceRootKey
        )
        sessionStore.save(cached) // idempotent upsert — schema-versioned

        identitySecrets = IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: result.exportKey
        )
        state = .session(
            SessionAccount(
                accountIdB64: cached.accountIdB64,
                username: cached.username
            )
        )
    }

    /// Drive the full recover-from-mnemonic flow.
    ///
    /// Mirror of `apps/android`'s `AuthRepository.recover`. Drives
    /// `lakko-svc-auth`'s `/password/reset/{start,finish}` endpoint
    /// pair via ``AuthClient/recover(accountId:mnemonicSeed:newPassword:)``:
    ///
    ///   1. Validate caller-side preconditions (cached account
    ///      present, mnemonic + new password non-empty).
    ///   2. Parse the typed phrase via `RecoveryMnemonic.fromPhrase`
    ///      to validate the BIP39 checksum and derive the 64-byte
    ///      seed.
    ///   3. Call ``AuthClient/recover(accountId:mnemonicSeed:newPassword:)``.
    ///   4. Re-derive the device root key from the **new** export
    ///      key. The previous key (if any) is no longer reachable
    ///      from the new password; the upsert overwrites the
    ///      Keychain entry cleanly.
    ///   5. Idempotent re-save of the ``SessionStore`` marker
    ///      (`accountId` unchanged, `username` unchanged).
    ///   6. Flip state to ``AuthState/session(_:)``.
    ///
    /// A wrong mnemonic surfaces as an ``AuthClientError/http(status:endpoint:body:)``
    /// from ``AuthClient/recover(accountId:mnemonicSeed:newPassword:)``
    /// — by design the failure is indistinguishable from a
    /// transport-mangled response (OPAQUE AKE refuses to agree on a
    /// session key, the server returns a generic error code). We
    /// let it bubble unchanged so the UI form can render "invalid
    /// recovery phrase" without leaking further.
    public func recover(
        mnemonicPhrase: String,
        newPassword: Data
    ) async throws {
        let trimmedPhrase = mnemonicPhrase.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedPhrase.isEmpty else {
            throw AuthClientError.argument("recovery phrase must be non-empty")
        }
        guard !newPassword.isEmpty else {
            throw AuthClientError.argument("new password must be non-empty")
        }
        guard let cached = sessionStore.load() else {
            throw AuthClientError.argument(
                "no account on this device — cross-device recovery " +
                "via the discovery service arrives in 1.14e"
            )
        }
        guard let accountIdBytes = base64Decode(cached.accountIdB64) else {
            sessionStore.clear()
            throw AuthClientError.argument(
                "stored account id is malformed; please sign up again"
            )
        }

        // Parse the mnemonic up front. `fromPhrase` collapses
        // unknown-word and bad-checksum into one error by design
        // (preserves BIP39's no-probing property).
        let mnemonic = try RecoveryMnemonic.fromPhrase(phrase: trimmedPhrase)
        let seed = mnemonic.toSeed()

        let client = authClientFactory()
        let result = try await client.recover(
            accountId: accountIdBytes,
            mnemonicSeed: seed,
            newPassword: newPassword
        )

        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: cached.accountIdB64
        )
        try DeviceKeyStore.putDeviceKey(
            accountIdB64: cached.accountIdB64,
            rootKey: deviceRootKey
        )
        sessionStore.save(cached) // idempotent upsert

        identitySecrets = IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: result.exportKey
        )
        state = .session(
            SessionAccount(
                accountIdB64: cached.accountIdB64,
                username: cached.username
            )
        )
    }

    /// Drop the in-memory session. Keeps ``SessionStore`` and the
    /// Keychain-stored device key intact — re-login on the same
    /// device stays cheap.
    ///
    /// Mirror of the web's `logout()`. The cached username is
    /// surfaced in ``AuthState/anon(cachedUsername:)`` so the next
    /// Login form pre-fills it.
    public func logout() {
        let cached = sessionStore.load()
        identitySecrets = nil
        state = .anon(cachedUsername: cached?.username)
    }

    /// Drop the in-memory session **and** the on-device material.
    ///
    /// Mirror of the web's `forgetDevice` semantics. Distinct from
    /// ``logout()``: this wipes both the ``SessionStore`` record and
    /// the Keychain-stored device key. After `forgetDevice` the
    /// only way back into the account is via the recovery passphrase
    /// (``recover(mnemonicPhrase:newPassword:)``).
    public func forgetDevice() {
        if let cached = sessionStore.load() {
            DeviceKeyStore.deleteDeviceKey(accountIdB64: cached.accountIdB64)
        }
        sessionStore.clear()
        identitySecrets = nil
        state = .anon(cachedUsername: nil)
    }
}

// MARK: - State types

/// Discriminated union for the auth state — drives the UI router.
public enum AuthState: Equatable {
    /// Initial state before ``AuthRepository`` finishes rehydrating
    /// from disk. Practically invisible because rehydration is
    /// synchronous, but kept for symmetry with the web side.
    case loading
    /// No on-device session. Optionally carries the cached username
    /// so the Login form (1.14d4) can pre-fill it.
    case anon(cachedUsername: String?)
    /// Active session.
    case session(SessionAccount)
}

/// Account record exposed to the UI when a session is active.
/// Mirror of `apps/web`'s `SessionAccount`.
public struct SessionAccount: Equatable {
    /// Base64 of the raw 16-byte UUIDv7. The canonical id we send
    /// to the server on every subsequent request.
    public let accountIdB64: String
    /// Human-visible handle. Local-only until 1.14e.
    public let username: String

    public init(accountIdB64: String, username: String) {
        self.accountIdB64 = accountIdB64
        self.username = username
    }
}

/// Identity secrets held in memory for the current session.
///
/// Mirror of `apps/web/src/auth/AuthContext.tsx::IdentitySecrets`.
/// Populated whenever the user has just authenticated (signup,
/// login, recover) — those flows have the OPAQUE export key in
/// scope. Empty after a session restore from disk: the export key
/// is not persisted, so the secrets cannot be re-derived without a
/// password.
///
/// Consumers that need to sign on behalf of the user
/// (`ContactsClient.sendContactRequest`, MLS commits) MUST check
/// ``AuthRepository/identitySecrets`` and surface a "please
/// re-enter your password" prompt when it's `nil`.
///
/// The bytes themselves are held — not a UniFFI `Identity` instance
/// — because:
///
///   * they're small (16 + 64 = 80 bytes) and clone-friendly;
///   * a UniFFI handle carries lifetime baggage and tying it to a
///     `@Published` property would complicate `Equatable` synthesis;
///   * downstream callers reconstruct an `Identity` on demand via
///     `Identity.fromSeed(seed:)` — each reconstruction is
///     sub-millisecond (HKDF + Ed25519 + X25519 derivation).
public struct IdentitySecrets: Equatable {
    /// Raw 16-byte UUIDv7.
    public let accountId: Data
    /// OPAQUE password export key (64 bytes for `LakkoCipherSuite`).
    /// Use as the seed for `Identity.fromSeed(seed:)`.
    public let exportKey: Data

    public init(accountId: Data, exportKey: Data) {
        self.accountId = accountId
        self.exportKey = exportKey
    }
}

/// Result handed back to the signup ViewModel so it can show the
/// freshly-generated 24-word recovery passphrase exactly once.
public struct SignupOutcome: Equatable {
    /// The 24-word BIP39 recovery phrase. The UI MUST display this
    /// to the user and require an explicit "I have written it down"
    /// acknowledgement before navigating away. Re-display is
    /// impossible — the words live in memory only and drop the
    /// moment the user moves on.
    public let recoveryPhrase: String

    public init(recoveryPhrase: String) {
        self.recoveryPhrase = recoveryPhrase
    }
}
