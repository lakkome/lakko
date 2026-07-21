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
/// ## Division of labor
///
/// Username registration / KeyPackage upload live on
/// ``DiscoveryClient`` + ``ContactsRepository``, and MLS state
/// + chat fan-out on ``ChatRepository`` — both surfaces exist
/// on iOS (discovery 1.14e, MLS chat 1.14f). This repository
/// owns the authenticate flows (signup / login / passkey /
/// recover) and coordinates the post-auth bootstrap: it uploads
/// the identity bundle via ``DiscoveryClient`` and drives the
/// backup restore + ``ChatRepository/bootstrap(secrets:)``
/// hand-off, but does not itself own the discovery or MLS state.
@MainActor
public final class AuthRepository: ObservableObject {

    // MARK: - Published state

    /// Discriminated union driving the UI router. Updated
    /// synchronously from the main actor — SwiftUI re-renders are
    /// scheduled atomically with the assignment.
    @Published public private(set) var state: AuthState = .loading

    /// ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) —
    /// non-nil when this device's canonical AccountIdentity did NOT match
    /// the account's currently-published bundle at login (someone replaced
    /// it; the ACCESS-hijack signature). Surfaced as a dismissible banner.
    @Published public private(set) var identityAlert: IdentityAlert?

    /// ADR-0037 §6 (G-11) — non-nil when a rolled-back / tampered backup was
    /// REFUSED at restore. An unambiguous tamper the user must be told about
    /// (distinct from a stale-and-declined outcome, which is only logged).
    /// Surfaced as a user-visible alert.
    @Published public private(set) var backupRollbackAlarm: BackupRollbackAlarm?

    /// ADR-0037 §6 (G-11) — non-nil when a stale (older-than-expected) but
    /// authentic backup is pending the user's explicit override decision. Drives
    /// a blocking confirmation dialog; the restore flow suspends until
    /// ``resolveStaleBackupOverride(_:)`` is called from the dialog buttons.
    @Published public private(set) var pendingStaleBackupOverride: StaleBackupOverrideRequest?

    /// The continuation the restore flow parks on while
    /// ``pendingStaleBackupOverride`` awaits the user's decision. Resolved
    /// exactly once by ``resolveStaleBackupOverride(_:)``.
    private var staleOverrideContinuation: CheckedContinuation<Bool, Never>?

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

    /// ADR-0017 fresh-device fail-safe. When a password / passkey
    /// auth succeeds but no AccountIdentity (phrase) seed is cached on
    /// this device, everything needed to finish the session is stashed
    /// here and ``state`` flips to ``AuthState/needsRecoveryPhrase(_:)``.
    /// ``provideRecoveryPhrase(mnemonicPhrase:)`` reads this back.
    private var pendingRecovery: PendingRecovery?

    /// ADR-0017 lost-everything fallback — the minted fresh phrase +
    /// completion context, parked between ``mintFreshIdentity()`` and
    /// ``confirmFreshPhraseSaved()``.
    private var pendingFreshPhrase: PendingFreshPhrase?

    /// ADR-0018 factor (d) — the resolved context for an in-flight
    /// phone / email OTP recovery. ``recoverWithContactStart(username:kind:contact:)``
    /// stashes the resolved `(accountId, username, kind)` here after
    /// sending the code so ``recoverWithContactFinish(code:)`` can
    /// submit the code without re-resolving. Cleared on finish (the
    /// code is single-use server-side; a retry must re-send).
    private var pendingContactRecover: PendingContactRecover?

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
        // Phase 2a2c2 — cache the OPAQUE export key under the
        // Keychain so a future sign-in-with-passkey on the SAME
        // device can reconstruct the full IdentitySecrets
        // without going through the password round-trip. Best-
        // effort: a persistence failure here just means the
        // user falls back to password login on this device.
        do {
            try ExportKeyStore.putExportKey(
                accountIdB64: accountIdB64,
                exportKey: result.exportKey
            )
        } catch {
            NSLog("export-key persist (signup) failed: \(error)")
        }
        // Phase 2a3b0 (ADR-0007) — ensure a random per-device
        // DeviceIdentity seed exists on this device. Distinct
        // from the deterministic AccountIdentity (the
        // phrase-seeded identity below); used as the MLS leaf in
        // the user's DeviceGroup (2a3a) once the add-device flow
        // lands. Best-effort.
        do {
            _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                accountIdB64: accountIdB64
            )
        } catch {
            NSLog("device-identity ensure (signup) failed: \(error)")
        }

        // ADR-0017: the canonical AccountIdentity is now derived from
        // the stable BIP39 recovery-phrase seed, NOT the password
        // export key — so a future recover / password change preserves
        // the identity. We need the phrase here, so fail loud BEFORE
        // building IdentitySecrets if it is absent (`enrolRecovery =
        // true` always enrols recovery; a missing phrase is a
        // code-path bypass we must not navigate the user past).
        guard let phrase = result.recoveryPhrase else {
            // Should be unreachable — we asked for `enrolRecovery =
            // true` so the AuthClient always returns a phrase.
            throw AuthClientError.argument(
                "internal: signup completed without a recovery phrase"
            )
        }
        let accountIdentitySeed = try RecoveryMnemonic
            .fromPhrase(phrase: phrase)
            .toSeed()
        // ADR-0017: cache the AccountIdentity (phrase) seed in the
        // Keychain — same hardware-backed posture as the export-key
        // cache — so a same-device login / passkey-resume can rebuild
        // the canonical identity without re-entering the phrase.
        // Best-effort: a persistence failure just means a future
        // fresh-resume on this device falls into the
        // `needsRecoveryPhrase` fail-safe.
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: accountIdB64,
                seed: accountIdentitySeed
            )
        } catch {
            NSLog("account-identity-seed persist (signup) failed: \(error)")
        }

        // Stash the export key + phrase seed + session token for
        // ContactsClient / ChatRepository / DiscoveryClient. The bytes
        // survive until logout / forgetDevice clears the publisher;
        // signup is a one-shot ceremony so we don't expect any
        // surprises here. ADR-0017: the canonical identity downstream
        // callers rebuild reads `accountIdentitySeed`.
        identitySecrets = IdentitySecrets(
            accountId: result.accountId,
            exportKey: result.exportKey,
            accountIdentitySeed: accountIdentitySeed,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs
        )
        state = .session(
            SessionAccount(
                accountIdB64: accountIdB64,
                username: trimmed
            )
        )
        // Phase 2a1c — post-signup backup bootstrap. Caches the
        // phrase in the Keychain + posts a baseline /backup
        // envelope. Best-effort end-to-end; errors are logged
        // and swallowed so a transient transport hiccup does
        // not block signup from completing.
        Task {
            await Self.bootstrapBackupBestEffort(
                accountId: result.accountId,
                accountIdB64: accountIdB64,
                accountIdentitySeed: accountIdentitySeed,
                sessionToken: result.sessionToken,
                phrase: phrase,
                context: "signup"
            )
        }
        // 1.20c — best-effort push subscribe. Same posture as the
        // web side: errors are logged and swallowed; the user
        // lands in a working session even without push.
        Task { await Self.subscribePushBestEffort(context: "signup") }
        return SignupOutcome(recoveryPhrase: phrase)
    }

    /// ADR-0032 guest join — create a FULL account with NO username and no
    /// user-chosen password, so an anonymous `/c/<slug>` visitor can join a
    /// call without a login.
    ///
    /// Reuses the audited ``signup(username:password:)`` machinery VERBATIM
    /// minus the username claim: the SAME PoW-gated OPAQUE registration
    /// (``AuthClient/signup(password:enrolRecovery:)``) — under a RANDOM
    /// 32-byte password the user NEVER sees (recovery is phrase-only, the
    /// ACCESS-only convention the recovery cascade already uses) — plus the
    /// device-root-key / export-key / device-identity / account-identity-seed
    /// derivation, and the first-ever identity-bundle publish (with the genesis
    /// `leaf_auth`, G-30 #2). SECURITY: guest creation MUST route through this
    /// same PoW-gated signup — there is no bypass.
    ///
    /// It then parks the recovery phrase + flips to ``AuthState/saveFreshPhrase(_:)``
    /// (mirroring ``mintFreshIdentity()``) instead of landing the session
    /// directly, so the existing `SaveFreshPhraseScreen` + ``confirmFreshPhraseSaved()``
    /// render + land it unchanged. Claiming a username (`registerUsername`) is a
    /// later onboarding step; the account is full from the start, just
    /// username-less. Returns the recovery phrase.
    public func signupGuest() async throws -> SignupOutcome {
        // A 32-byte random password the user never sees — the guest HAS a real,
        // random password, so the normal signup derivations apply. `SystemRandom-
        // NumberGenerator` is the OS CSPRNG on Apple platforms.
        var rng = SystemRandomNumberGenerator()
        let randomBytes = (0..<32).map { _ in UInt8.random(in: UInt8.min...UInt8.max, using: &rng) }
        let randomPassword = Data(Data(randomBytes).base64EncodedString().utf8)

        // The SAME PoW-gated OPAQUE registration `signup` uses — no bypass.
        let client = authClientFactory()
        let result = try await client.signup(password: randomPassword, enrolRecovery: true)
        let accountIdB64 = base64Encode(result.accountId)

        // NOTE: no username claim — the account stays username-less until a
        // later "claim your username" onboarding step (`registerUsername`).

        guard let phrase = result.recoveryPhrase else {
            throw AuthClientError.argument(
                "internal: guest signup completed without a recovery phrase"
            )
        }
        let accountIdentitySeed = try RecoveryMnemonic
            .fromPhrase(phrase: phrase)
            .toSeed()

        // Device root key from the OPAQUE export key + persist the key /
        // export key / device-identity / account-identity seed, exactly as
        // `signup` does. Best-effort mirrors (a persistence failure just
        // degrades a future warm resume on this device).
        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: accountIdB64
        )
        try DeviceKeyStore.putDeviceKey(
            accountIdB64: accountIdB64,
            rootKey: deviceRootKey
        )
        do {
            try ExportKeyStore.putExportKey(
                accountIdB64: accountIdB64,
                exportKey: result.exportKey
            )
        } catch {
            NSLog("export-key persist (signupGuest) failed: \(error)")
        }
        do {
            _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                accountIdB64: accountIdB64
            )
        } catch {
            NSLog("device-identity ensure (signupGuest) failed: \(error)")
        }
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: accountIdB64,
                seed: accountIdentitySeed
            )
        } catch {
            NSLog("account-identity-seed persist (signupGuest) failed: \(error)")
        }

        // G-30 #2 (ADR-0019): publish the FIRST-EVER bundle WITH the genesis
        // leaf self-signature so the transparency-log append accepts the new
        // binding — identical to the web `signupGuest`. Fail-loud: a publish
        // failure rejects the guest signup rather than parking an unverifiable
        // identity (mirrors `mintFreshIdentity`, which publishes before any
        // local state flip).
        try await publishFreshIdentityBundle(
            accountId: result.accountId,
            sessionToken: result.sessionToken,
            seed: accountIdentitySeed
        )

        // Park the completion context + flip to `.saveFreshPhrase` so the UI
        // shows the recovery phrase before the session lands (mirroring
        // `mintFreshIdentity`); `confirmFreshPhraseSaved` builds the canonical
        // session from the stash. Username EMPTY (unclaimed).
        pendingFreshPhrase = PendingFreshPhrase(
            seed: accountIdentitySeed,
            accountIdBytes: result.accountId,
            accountIdB64: accountIdB64,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs,
            username: "",
            phrase: phrase
        )
        state = .saveFreshPhrase(
            SessionAccount(accountIdB64: accountIdB64, username: "")
        )
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
        // Phase 2a2c2 — refresh the cached export key under the
        // Keychain. Best-effort.
        do {
            try ExportKeyStore.putExportKey(
                accountIdB64: cached.accountIdB64,
                exportKey: result.exportKey
            )
        } catch {
            NSLog("export-key persist (login) failed: \(error)")
        }
        // Phase 2a3b0 (ADR-0007) — ensure DeviceIdentity seed.
        // Idempotent: a returning user with a seed already on
        // file gets the cached one; first-login generates
        // fresh bytes.
        do {
            _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                accountIdB64: cached.accountIdB64
            )
        } catch {
            NSLog("device-identity ensure (login) failed: \(error)")
        }
        sessionStore.save(cached) // idempotent upsert — schema-versioned

        // ADR-0017: the canonical AccountIdentity is seeded from the
        // BIP39 phrase seed, NOT the export key. Read the cached seed
        // (warm same-device path). If present, finish the session with
        // it. If ABSENT (fresh device, or a device that only ever held
        // the export key), we MUST NOT (a) derive the identity from the
        // export key — that is the wrong identity now — nor (b)
        // silently mint a fresh one — that breaks every contact's pin.
        // Instead we flip to the `needsRecoveryPhrase` fail-safe state
        // and let the user supply their phrase.
        let accountIdentitySeed = Self.readCachedAccountIdentitySeed(
            accountIdB64: cached.accountIdB64,
            context: "login"
        )
        finishOrPromptForRecovery(
            accountIdentitySeed: accountIdentitySeed,
            accountIdBytes: accountIdBytes,
            accountIdB64: cached.accountIdB64,
            exportKey: result.exportKey,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs,
            username: cached.username
        )
        // 1.20c — best-effort push subscribe.
        Task { await Self.subscribePushBestEffort(context: "login") }
    }

    /// Phase 2a2c2-ios — sign in with a WebAuthn passkey on a
    /// device that has already been seen by this account at
    /// least once (signup / login / recover ran here in the past
    /// + populated the export-key cache).
    ///
    /// Drives the WebAuthn discoverable-authentication flow via
    /// ``WebauthnClient`` + ``WebauthnAuthentication`` (no
    /// password / username typed in the UI). On a successful
    /// assertion, resolves the cached export key from the
    /// Keychain and reconstructs the full ``IdentitySecrets``
    /// pipeline — same shape as ``login(username:password:)``
    /// minus the OPAQUE round-trip.
    ///
    /// Fresh-device UX is **unsupported** by design: without a
    /// locally-cached export key the passkey gives us only a
    /// session token, but no path to derive Identity / device
    /// key / MLS bootstrap. The recovery phrase remains the only
    /// fresh-device entry route. The Login screen gates the
    /// "Sign in with passkey" button on the cached marker so
    /// this error path is rare in practice.
    public func loginWithPasskey() async throws {
        let webauthn = WebauthnClient()
        let assertion = try await webauthn.loginWithPasskey()
        // Resolve the cached account marker. We only support
        // passkey-login for the account this device most
        // recently signed in to — multi-account on the same
        // device isn't a supported flow yet.
        guard let cached = sessionStore.load() else {
            throw AuthClientError.argument(
                "No cached account on this device. Sign in with " +
                "password or recover with your phrase first."
            )
        }
        // Compare via raw bytes: the cached `accountIdB64` rides
        // as standard padded base64 (Foundation's
        // `Data.base64EncodedString()`), while the server's
        // `auth_finish` response carries URL-safe base64
        // without padding (`URL_SAFE_NO_PAD.encode`). A string
        // comparison would always mismatch.
        guard
            let cachedAccountBytes = base64Decode(cached.accountIdB64),
            let assertionAccountBytes = base64UrlDecodeNoPad(assertion.accountIdB64)
        else {
            throw AuthClientError.argument(
                "Malformed account_id in passkey assertion."
            )
        }
        guard cachedAccountBytes == assertionAccountBytes else {
            throw AuthClientError.argument(
                "This passkey belongs to a different account than the " +
                "one cached on this device. Sign in with password first " +
                "to enable passkey on this device."
            )
        }

        // Read the cached export key from the Keychain. A miss
        // (fresh device, post-`forgetDevice`, or a key-store
        // failure) sends the user back to password / recovery.
        let exportKey: Data
        do {
            guard let cachedKey = try ExportKeyStore.getExportKey(
                accountIdB64: cached.accountIdB64
            ) else {
                throw AuthClientError.argument(
                    "No cached credentials for passkey sign-in on this " +
                    "device. Sign in with password or recover with your " +
                    "phrase first."
                )
            }
            exportKey = cachedKey
        } catch let storeError as ExportKeyStoreError {
            NSLog("export-key cache read failed: \(storeError)")
            throw AuthClientError.argument(
                "Could not read the local credential store. Sign in " +
                "with password or recover with your phrase first."
            )
        }

        // From here on we have everything we need to
        // reconstruct a real IdentitySecrets-driven session.
        // OPAQUE export key is deterministic for the same
        // password, so re-deriving the device root key here
        // yields identical bytes — the upsert is a no-op but
        // keeps the Keychain freshness clock moving.
        let deviceRootKey = deriveDeviceRootKey(
            exportKey: exportKey,
            accountIdB64: cached.accountIdB64
        )
        try DeviceKeyStore.putDeviceKey(
            accountIdB64: cached.accountIdB64,
            rootKey: deviceRootKey
        )
        // Re-upsert the export key cache too — keeps the row's
        // last-write timestamp moving for any future eviction
        // policy.
        do {
            try ExportKeyStore.putExportKey(
                accountIdB64: cached.accountIdB64,
                exportKey: exportKey
            )
        } catch {
            NSLog("export-key persist (passkey login) failed: \(error)")
        }
        sessionStore.save(cached)

        // ADR-0017: build the canonical Identity from the cached
        // AccountIdentity (phrase) seed, NOT the export key. Passkey
        // resume only runs on a device that already cached the export
        // key, so the phrase seed is normally present too — but a
        // device that last logged in before this cache existed may lack
        // it, in which case we fail closed into the
        // `needsRecoveryPhrase` state (never derive from exportKey,
        // never mint fresh).
        let accountIdentitySeed = Self.readCachedAccountIdentitySeed(
            accountIdB64: cached.accountIdB64,
            context: "passkey login"
        )
        finishOrPromptForRecovery(
            accountIdentitySeed: accountIdentitySeed,
            accountIdBytes: cachedAccountBytes,
            accountIdB64: cached.accountIdB64,
            exportKey: exportKey,
            sessionToken: assertion.sessionToken,
            sessionExpiresAtMs: assertion.sessionExpiresAtMs,
            username: cached.username
        )
        Task { await Self.subscribePushBestEffort(context: "passkey-login") }
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
    ///
    /// ADR-0017: the canonical AccountIdentity is derived from the
    /// (unchanged) BIP39 phrase seed, so a routine recover preserves
    /// the identity — the supersession proof emit is SKIPPED because
    /// the old and new account pubkeys are equal. The `username`
    /// resolves the account_id like ``login(username:password:)`` does
    /// (cache hit, else a `DiscoveryClient.lookup`) so recover works
    /// on a FRESH device given the username + phrase. It is required
    /// only when no account is cached on this device; a cached account
    /// wins.
    public func recover(
        mnemonicPhrase: String,
        newPassword: Data,
        username: String? = nil
    ) async throws {
        let trimmedPhrase = mnemonicPhrase.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedPhrase.isEmpty else {
            throw AuthClientError.argument("recovery phrase must be non-empty")
        }
        guard !newPassword.isEmpty else {
            throw AuthClientError.argument("new password must be non-empty")
        }

        // ADR-0017: resolve the account_id the same way `login` does so
        // recover works on a FRESH device: a local cache hit wins;
        // otherwise a `DiscoveryClient.lookup` on the supplied username
        // binds it. Without either we cannot target the recover. The
        // branch logic lives in the pure ``resolveRecoverAccount`` helper
        // (G-29) so it can be unit-tested without a network.
        let cached = sessionStore.load().map {
            CachedAccount(accountIdB64: $0.accountIdB64, username: $0.username)
        }
        let resolved = try await resolveRecoverAccount(
            cached: cached,
            username: username,
            lookup: {
                let lookup = try await DiscoveryClient().lookup(username: $0)
                return RecoverAccountLookup(
                    account: lookup.account,
                    normalisedUsername: lookup.normalisedUsername
                )
            },
            encodeAccountId: base64Encode
        )
        let accountIdB64 = resolved.accountIdB64
        let resolvedUsername = resolved.resolvedUsername
        guard let accountIdBytes = base64Decode(accountIdB64) else {
            throw AuthClientError.argument(
                "resolved account id is malformed; please try again"
            )
        }

        // Parse the mnemonic up front. `fromPhrase` collapses
        // unknown-word and bad-checksum into one error by design
        // (preserves BIP39's no-probing property).
        let mnemonic = try RecoveryMnemonic.fromPhrase(phrase: trimmedPhrase)
        let seed = mnemonic.toSeed()
        // ADR-0017: the canonical AccountIdentity is derived from this
        // BIP39 phrase seed. `seed` doubles as the OPAQUE recovery
        // input below; keep a private copy for the identity cache + the
        // in-memory secrets, which must outlive this call.
        let accountIdentitySeed = seed

        // G-4 (ADR-0016 / ADR-0017) — capture the OLD AccountIdentity
        // signature pubkey BEFORE the recover rotates anything, so the
        // emit path below can compare old-vs-new and SKIP the
        // supersession proof when they are equal (the ADR-0017 routine
        // recover case — the phrase is unchanged, so the new account
        // pubkey equals the old one). The OLD identity is itself
        // phrase-seeded now, so the old pubkey is exactly
        // `Identity.fromSeed(accountIdentitySeed).signaturePublicKey()`
        // — which is what the new identity derives too. We snapshot it
        // here for an explicit equality assertion rather than assuming.
        let oldAccountPubkey = try Identity
            .fromSeed(seed: accountIdentitySeed)
            .signaturePublicKey()

        let client = authClientFactory()
        let result = try await client.recover(
            accountId: accountIdBytes,
            mnemonicSeed: seed,
            newPassword: newPassword
        )

        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: accountIdB64
        )
        try DeviceKeyStore.putDeviceKey(
            accountIdB64: accountIdB64,
            rootKey: deviceRootKey
        )
        // Phase 2a2c2 — refresh the cached export key under the
        // freshly-rotated device key. Any prior cached value was
        // bound to the OLD password; the upsert here overwrites
        // it cleanly. Best-effort.
        do {
            try ExportKeyStore.putExportKey(
                accountIdB64: accountIdB64,
                exportKey: result.exportKey
            )
        } catch {
            NSLog("export-key persist (recover) failed: \(error)")
        }
        // ADR-0017: cache the AccountIdentity (phrase) seed under the
        // freshly-rotated device. This is what makes the warm
        // same-device path (and a later passkey resume) rebuild the
        // SAME canonical identity without re-entering the phrase.
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: accountIdB64,
                seed: accountIdentitySeed
            )
        } catch {
            NSLog("account-identity-seed persist (recover) failed: \(error)")
        }

        // G-4 (ADR-0016) emit path — now DORMANT per ADR-0017. With a
        // phrase-seeded AccountIdentity, a routine recover does NOT
        // rotate the account key: the phrase is unchanged, so the new
        // account signature pubkey EQUALS the old one. We re-derive the
        // NEW canonical identity from the (unchanged) phrase seed,
        // compare it to the OLD pubkey captured above, and — when equal
        // (the only case on a routine recover) — SKIP the supersession
        // proof mint + upload entirely (emitting a proof for a key
        // nobody's pin changed would be spurious). The bundle is still
        // re-published WITHOUT a proof so the directory's KeyPackage
        // upserts current + any stale server-side proof column clears.
        // A mismatch can NEVER happen on a phrase-seeded recover (same
        // seed ⇒ same key); if it ever does we fail loud rather than
        // publish a changed key with no proof (which would break every
        // contact's pin — a self-inflicted downgrade). See ADR-0017
        // "old == new account pubkey ⇒ skip" — the load-bearing emit
        // invariant.
        await Self.republishBundleSkippingSupersessionProof(
            accountId: accountIdBytes,
            accountIdentitySeed: accountIdentitySeed,
            oldAccountPubkey: oldAccountPubkey,
            sessionToken: result.sessionToken
        )
        // Phase 2a3b0 (ADR-0007) — DeviceIdentity seed is
        // decoupled from the OPAQUE password, so recovery does
        // NOT rotate it. `ensureDeviceIdentitySeed` returns
        // the existing seed when present and only generates
        // fresh bytes if the Keychain row is missing.
        do {
            _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                accountIdB64: accountIdB64
            )
        } catch {
            NSLog("device-identity ensure (recover) failed: \(error)")
        }
        // Persist the session marker — important on the fresh-device
        // recover path where there was no cached row (resolved via the
        // discovery lookup above).
        sessionStore.save(
            PersistedSession(
                accountIdB64: accountIdB64,
                username: resolvedUsername
            )
        )

        // Phase 2a1d / 2a3d7 — restore-then-bootstrap on recover.
        //
        // The user has just gone through recovery, which lands
        // on a device that may or may not have been used by
        // them before. If a prior backup envelope sits on the
        // server, we fetch + decrypt + parse it before posting
        // a fresh baseline — otherwise the bootstrap-side
        // upload nukes the prior backup with an empty payload.
        //
        // **Ordering matters:** the restore + snapshot-write to
        // `IdentitySnapshotStore` happen BEFORE the `state`
        // flip below. SwiftUI's `SessionRoot.task(id:
        // identitySecrets?.accountId)` modifier wakes on the
        // flip and immediately drives
        // `chatRepository.bootstrap(secrets:)`, which reads
        // `snapshotStore.getSnapshot(...)` for the MLS state.
        // If we restored from server AFTER the flip, that read
        // would race against the write and the new device would
        // land with no MLS state. Doing it before is the only
        // way to make the rehydrate path see the restored
        // snapshot on the very first ChatRepository.bootstrap
        // call.
        //
        // The applying-restored-contacts-into-ContactsRepository
        // wire is not built yet on iOS — the contacts list is
        // in-memory only and there's no persistent target to
        // upsert into (mirror of the Android situation). For
        // now restore logs the outcome; the merge layer lands
        // alongside the iOS contact-persistence sub-sprint.
        let restoredSnapshot = await restoreFromServerBackupBestEffort(
            accountId: accountIdBytes,
            accountIdB64: accountIdB64,
            sessionToken: result.sessionToken,
            phrase: trimmedPhrase,
            context: "recover"
        )
        if !restoredSnapshot.isEmpty {
            // Drop the restored MLS blob into the local snapshot
            // store. `ChatRepository.bootstrap`'s rehydrate path
            // picks it up automatically — no wiring change
            // needed beyond this single putSnapshot call.
            IdentitySnapshotStore().putSnapshot(
                accountIdB64: accountIdB64,
                blob: restoredSnapshot
            )
        }

        // ADR-0017: also carry the phrase seed (the canonical identity
        // seed downstream callers rebuild from). Cleared on logout.
        identitySecrets = IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: result.exportKey,
            accountIdentitySeed: accountIdentitySeed,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs
        )
        // Clear any pending fresh-device fail-safe — recover just
        // restored the identity directly.
        pendingRecovery = nil
        state = .session(
            SessionAccount(
                accountIdB64: accountIdB64,
                username: resolvedUsername
            )
        )

        // Phase 2a1c — post-recover backup bootstrap: re-cache
        // the phrase under the freshly-rotated device root key
        // and post a baseline /backup envelope that re-publishes
        // the restored snapshot (so the next device-recover
        // sees the same state we just rebuilt).
        //
        // G-10 (known-gaps): AWAITED, not a detached `Task`. The
        // baseline re-seals the backup envelope under the
        // freshly-rotated export-key material; until it lands the
        // server still holds the pre-recover envelope. Awaiting
        // folds the rewrite into the recover round-trip so the
        // "stale envelope" window collapses to the upload latency.
        // `bootstrapBackupBestEffort` is non-throwing (it logs +
        // continues on any internal failure), so awaiting it cannot
        // fail the recover itself.
        await Self.bootstrapBackupBestEffort(
            accountId: accountIdBytes,
            accountIdB64: accountIdB64,
            accountIdentitySeed: accountIdentitySeed,
            sessionToken: result.sessionToken,
            phrase: trimmedPhrase,
            context: "recover",
            wholeStorageSnapshot: restoredSnapshot
        )
        // 1.20c — best-effort push subscribe.
        Task { await Self.subscribePushBestEffort(context: "recover") }
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
        pendingRecovery = nil
        pendingFreshPhrase = nil
        identityAlert = nil
        state = .anon(cachedUsername: cached?.username)
        // 1.20c — fire-and-forget unsubscribe so the next page
        // reload doesn't silently keep accepting pushes against
        // a logged-out session.
        Task { await PushSubscriber.shared.unsubscribe() }
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
            // Phase 2a1c — drop the cached recovery phrase too.
            // Best-effort; a Keychain failure here is logged and
            // ignored (the row is unreadable without the device
            // key just wiped, but the clean wipe matches
            // `forgetDevice`'s "leave nothing per-account behind"
            // contract).
            try? PhraseStore().deletePhrase(accountIdB64: cached.accountIdB64)
            // ADR-0037 (G-11) — drop the backup-head version floor. Same
            // clean-wipe rationale; the next recover starts from the
            // zero-anchor freshness-window path.
            BackupFloorStore.deleteFloor(accountIdB64: cached.accountIdB64)
            // Phase 2a2c2 — drop the cached export key too. Same
            // clean-wipe rationale.
            ExportKeyStore.deleteExportKey(accountIdB64: cached.accountIdB64)
            // ADR-0017 — drop the cached AccountIdentity (phrase) seed.
            // It is the highest-sensitivity on-device secret. After this
            // wipe the only way back into the account is the recovery
            // phrase (or a paired device). Same clean-wipe rationale as
            // the export-key / device-key rows.
            AccountIdentitySeedStore.deleteAccountIdentitySeed(
                accountIdB64: cached.accountIdB64
            )
            // Phase 2a3b0 — drop the cached DeviceIdentity
            // seed. The leaf in the user's DeviceGroup that
            // this seed signed for is orphaned; sibling
            // devices clean it up via 2a3c's remove-device
            // flow.
            DeviceIdentityStore.deleteDeviceIdentitySeed(
                accountIdB64: cached.accountIdB64
            )
            // Phase 2a3b3 — drop the cached DeviceGroup MLS
            // snapshot. Same clean-wipe rationale as the
            // identity-seed / phrase / device-key rows above.
            DeviceGroupSnapshotStore().deleteSnapshot(
                accountIdB64: cached.accountIdB64
            )
        }
        sessionStore.clear()
        identitySecrets = nil
        // ADR-0017/0018 — drop any parked recovery context too (mirror of
        // logout + the Android forgetDevice), so an abandoned recover /
        // lost-everything mint leaves no in-memory seed behind.
        pendingRecovery = nil
        pendingFreshPhrase = nil
        identityAlert = nil
        state = .anon(cachedUsername: nil)
        // 1.20c — full wipe of push state too. The contract is
        // "leave NOTHING per-account behind on this device", so
        // we both unregister server-side and clear the Keychain
        // pseudonym. Best-effort fire-and-forget — the local
        // clear inside `unsubscribe` runs regardless of the
        // server leg outcome.
        Task { await PushSubscriber.shared.unsubscribe() }
    }

    // MARK: - Push integration helpers

    /// 1.20c — fire ``PushSubscriber/subscribe()`` on the
    /// MainActor with errors logged and swallowed. Called from
    /// signup / login / recover after the state flip so the
    /// user lands in a working session even when push fails.
    @MainActor
    private static func subscribePushBestEffort(context: String) async {
        do {
            _ = try await PushSubscriber.shared.subscribe()
        } catch {
            NSLog("push subscribe (\(context)) failed: \(error)")
        }
    }

    // MARK: - ADR-0017 session-completion helpers

    /// ADR-0017 — read the cached AccountIdentity (phrase) seed for
    /// `accountIdB64` from the Keychain. Returns `nil` on a miss
    /// (fresh device / never-cached) OR on a Keychain read failure
    /// (device key rotated, ciphertext tampered) — both fold into the
    /// `needsRecoveryPhrase` fail-safe rather than the wrong-identity
    /// branch. A `context` label disambiguates the log line.
    private static func readCachedAccountIdentitySeed(
        accountIdB64: String,
        context: String
    ) -> Data? {
        do {
            return try AccountIdentitySeedStore.getAccountIdentitySeed(
                accountIdB64: accountIdB64
            )
        } catch {
            NSLog("account-identity-seed read failed (\(context)): \(error)")
            return nil
        }
    }

    /// ADR-0017 — branch on the presence of the cached AccountIdentity
    /// (phrase) seed at the tail of `login` / `loginWithPasskey`.
    ///
    ///   * seed present → ``finishCanonicalSession`` (set the
    ///     in-memory secrets carrying the seed + flip to `.session`).
    ///   * seed absent → stash a ``PendingRecovery`` and flip to
    ///     `.needsRecoveryPhrase`. We MUST NOT (a) derive the identity
    ///     from the export key (the wrong identity now) nor (b)
    ///     silently mint a fresh one (breaks every contact's pin) —
    ///     the user supplies the phrase via
    ///     ``provideRecoveryPhrase(mnemonicPhrase:)``.
    private func finishOrPromptForRecovery(
        accountIdentitySeed: Data?,
        accountIdBytes: Data,
        accountIdB64: String,
        exportKey: Data,
        sessionToken: String,
        sessionExpiresAtMs: Int64,
        username: String
    ) {
        guard let accountIdentitySeed else {
            // Fresh-device fail-safe (ADR-0017). Auth succeeded; the
            // identity restore is pending the phrase. Stash the context
            // and flip — `provideRecoveryPhrase` finishes from here.
            pendingRecovery = PendingRecovery(
                accountIdBytes: accountIdBytes,
                accountIdB64: accountIdB64,
                exportKey: exportKey,
                sessionToken: sessionToken,
                sessionExpiresAtMs: sessionExpiresAtMs,
                username: username
            )
            state = .needsRecoveryPhrase(
                SessionAccount(accountIdB64: accountIdB64, username: username)
            )
            return
        }
        finishCanonicalSession(
            accountIdentitySeed: accountIdentitySeed,
            accountIdBytes: accountIdBytes,
            exportKey: exportKey,
            sessionToken: sessionToken,
            sessionExpiresAtMs: sessionExpiresAtMs,
            username: username
        )
    }

    /// ADR-0017 — finish a warm session once the canonical
    /// AccountIdentity (phrase) seed is in hand: set the in-memory
    /// secrets (carrying the seed downstream callers rebuild the
    /// canonical identity from) and flip to `.session`. Shared by the
    /// warm same-device login / passkey paths and the fresh-device
    /// `provideRecoveryPhrase` completion.
    private func finishCanonicalSession(
        accountIdentitySeed: Data,
        accountIdBytes: Data,
        exportKey: Data,
        sessionToken: String,
        sessionExpiresAtMs: Int64,
        username: String
    ) {
        let accountIdB64 = base64Encode(accountIdBytes)
        identitySecrets = IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: exportKey,
            accountIdentitySeed: accountIdentitySeed,
            sessionToken: sessionToken,
            sessionExpiresAtMs: sessionExpiresAtMs
        )
        pendingRecovery = nil
        state = .session(
            SessionAccount(accountIdB64: accountIdB64, username: username)
        )
        // ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i))
        // — fire-and-forget self-check: is the account's published identity
        // still OURS? A mismatch means someone replaced it (the ACCESS-hijack
        // signature). iOS does not re-publish on a warm login, so this is
        // race-free; best-effort (a 404 / fetch error never alerts).
        Task { [weak self] in
            await self?.checkPublishedIdentityConsistency(
                accountIdBytes: accountIdBytes,
                accountIdentitySeed: accountIdentitySeed,
                sessionToken: sessionToken
            )
        }
    }

    /// ADR-0017 defence-in-depth — set ``identityAlert`` when the account's
    /// published AccountIdentity differs from this device's. Best-effort:
    /// a 404 (fresh signup) or a fetch error never alerts. Resets the alert
    /// each call so a clean login clears a stale one.
    private func checkPublishedIdentityConsistency(
        accountIdBytes: Data,
        accountIdentitySeed: Data,
        sessionToken: String
    ) async {
        identityAlert = nil
        do {
            let derivedPubkey = try Identity
                .fromSeed(seed: accountIdentitySeed)
                .signaturePublicKey()
            let discovery = DiscoveryClient(sessionToken: sessionToken)
            guard let bundle = try await discovery.identityFetch(
                accountId: accountIdBytes
            ) else {
                return // 404 — nothing published yet; do not alert.
            }
            if base64Decode(bundle.signaturePubkey) != derivedPubkey {
                identityAlert = .publishedIdentityChanged
            }
        } catch {
            NSLog("self-identity consistency check failed: \(error)")
        }
    }

    /// Dismiss the identity-changed security banner.
    public func dismissIdentityAlert() {
        identityAlert = nil
    }

    // MARK: - ADR-0037 backup-rollback gate (G-11)

    /// Dismiss the backup-rollback alarm. Idempotent.
    public func dismissBackupRollbackAlarm() {
        backupRollbackAlarm = nil
    }

    /// Resolve a pending stale-backup override from the confirmation dialog's
    /// buttons — `true` = "Restore anyway", `false` = "Cancel". Idempotent: a
    /// second call after the continuation has already been resumed (e.g. the
    /// dialog's dismiss binding firing after a button) is a no-op.
    public func resolveStaleBackupOverride(_ accepted: Bool) {
        pendingStaleBackupOverride = nil
        let cont = staleOverrideContinuation
        staleOverrideContinuation = nil
        cont?.resume(returning: accepted)
    }

    /// The async confirmation the restore gate awaits. Publishes the request
    /// (which drives the SwiftUI confirmation dialog) and suspends until
    /// ``resolveStaleBackupOverride(_:)`` resumes it. Fail-closed: if a request
    /// is already in flight (should not happen — restore is serialized), returns
    /// `false` (never a silent downgrade).
    private func awaitStaleBackupOverride(reason: String) async -> Bool {
        if staleOverrideContinuation != nil {
            return false
        }
        return await withCheckedContinuation { cont in
            staleOverrideContinuation = cont
            pendingStaleBackupOverride = StaleBackupOverrideRequest(reason: reason)
        }
    }

    /// ADR-0017 fresh-device fail-safe completion. Called only while
    /// the state is `.needsRecoveryPhrase`: the user typed their
    /// recovery phrase, we derive the BIP39 seed, HARD-BLOCK verify it
    /// against the account's published bundle, cache it under the
    /// device, and flip to a full `.session`.
    ///
    /// FAIL CLOSED (ADR-0017 + lakko-security finding #1): the supplied
    /// phrase MUST reproduce the account's PUBLISHED AccountIdentity
    /// key. This path only runs on a fresh-device login of an ALREADY-
    /// ESTABLISHED account, so the bundle must exist — a missing bundle
    /// (404) or an unreachable directory is an anomaly, NOT a
    /// first-publish. We therefore HARD-BLOCK on any failure-to-verify
    /// (mismatch, 404, or network) rather than trust-on-first-use a
    /// possibly-wrong phrase: a server-forced 404 must not let a
    /// socially-engineered wrong phrase overwrite the published
    /// identity. Throws ``PhraseMismatch`` on a mismatch,
    /// ``IdentityVerificationUnavailable`` on a 404 / fetch failure.
    public func provideRecoveryPhrase(mnemonicPhrase: String) async throws {
        guard let pending = pendingRecovery else {
            throw AuthClientError.argument(
                "no pending identity recovery — sign in first"
            )
        }
        // Parse the phrase → derive the BIP39 seed → HARD-BLOCK verify
        // against the published bundle (shared gate).
        let mnemonic = try RecoveryMnemonic.fromPhrase(phrase: mnemonicPhrase)
        var accountIdentitySeed = mnemonic.toSeed()
        do {
            try await verifySeedAgainstPublishedIdentity(
                accountIdBytes: pending.accountIdBytes,
                sessionToken: pending.sessionToken,
                accountIdentitySeed: accountIdentitySeed
            )
        } catch {
            // On every refusal (mismatch, 404, network) the seed never
            // reached the live IdentitySecrets — zeroize it.
            Self.zeroize(&accountIdentitySeed)
            throw error
        }

        // ADR-0018 factor (c): a TOTP-recovered fresh device parked
        // with NO device root key (no password => no export key to
        // derive one from). Bootstrap it from the VERIFIED identity
        // seed — exactly what `completeFromDeviceRestore` does — and
        // ONLY here, strictly AFTER the fail-closed verification
        // above: a refusal must leave this device untouched.
        bootstrapDeviceKeyFromVerifiedSeedIfAbsent(
            accountIdB64: pending.accountIdB64,
            accountIdentitySeed: accountIdentitySeed,
            context: "provideRecoveryPhrase"
        )

        // Cache the phrase seed under the device so future warm logins
        // skip the prompt. Best-effort.
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: pending.accountIdB64,
                seed: accountIdentitySeed
            )
        } catch {
            NSLog(
                "account-identity-seed persist (provideRecoveryPhrase) " +
                "failed: \(error)"
            )
        }

        finishCanonicalSession(
            accountIdentitySeed: accountIdentitySeed,
            accountIdBytes: pending.accountIdBytes,
            exportKey: pending.exportKey,
            sessionToken: pending.sessionToken,
            sessionExpiresAtMs: pending.sessionExpiresAtMs,
            username: pending.username
        )
    }

    /// Shared HARD-BLOCK verification (ADR-0017 + lakko-security
    /// finding #1): the supplied AccountIdentity seed MUST reproduce
    /// the account's PUBLISHED identity key. 404 (nil bundle) or a
    /// fetch error refuses (``IdentityVerificationUnavailable``); a
    /// mismatch refuses (``PhraseMismatch``). Never trust-on-first-use.
    /// Used by ``provideRecoveryPhrase(mnemonicPhrase:)``,
    /// ``completeFromPairing(phraseSeed:sessionToken:sessionExpiresAtMs:)``
    /// and
    /// ``completeFromDeviceRestore(accountIdBytes:accountIdB64:username:phraseSeed:sessionToken:sessionExpiresAtMs:)``;
    /// the CALLER owns zeroizing the seed on refusal.
    private func verifySeedAgainstPublishedIdentity(
        accountIdBytes: Data,
        sessionToken: String?,
        accountIdentitySeed: Data
    ) async throws {
        let derivedPubkey = try Identity
            .fromSeed(seed: accountIdentitySeed)
            .signaturePublicKey()

        let bundle: IdentityFetchResponse?
        do {
            let discovery = DiscoveryClient(sessionToken: sessionToken)
            bundle = try await discovery.identityFetch(
                accountId: accountIdBytes
            )
        } catch {
            // Could not reach the directory to verify (network/parse).
            // We cannot confirm the seed reproduces the published key,
            // so we refuse rather than cache a possibly-wrong identity.
            NSLog("identity-seed verification fetch failed: \(error)")
            throw IdentityVerificationUnavailable(
                "Could not reach the directory to verify the restored " +
                "identity. Check your connection and try again."
            )
        }
        guard let bundle else {
            // 404 — no published identity to verify against. On an
            // established account this is an anomaly; HARD-BLOCK.
            throw IdentityVerificationUnavailable(
                "No published identity was found to verify against. " +
                "Try again, or restore with your recovery phrase."
            )
        }
        guard let published = base64Decode(bundle.signaturePubkey),
              published == derivedPubkey else {
            throw PhraseMismatch()
        }
    }

    /// ADR-0018 factor (a), path (a) — complete a pending
    /// `needsRecoveryPhrase` login using the identity seed received
    /// from ANOTHER DEVICE over the E2EE pairing channel (instead of
    /// typing the 24 words). Same fail-closed gate as
    /// ``provideRecoveryPhrase(mnemonicPhrase:)``; prefers the freshly
    /// REDEEMED session token when the joiner's grant redeem succeeded.
    ///
    /// Takes ownership of `phraseSeed` (zeroized on every refusal —
    /// best-effort under Swift `Data` value semantics).
    ///
    /// - Throws: ``PhraseMismatch`` / ``IdentityVerificationUnavailable``
    ///   — same semantics as ``provideRecoveryPhrase(mnemonicPhrase:)``;
    ///   ``AuthClientError/argument(_:)`` when there is no pending
    ///   recovery.
    public func completeFromPairing(
        phraseSeed: Data,
        sessionToken: String? = nil,
        sessionExpiresAtMs: Int64? = nil
    ) async throws {
        guard let pending = pendingRecovery else {
            throw AuthClientError.argument(
                "no pending identity recovery — sign in first"
            )
        }
        var seed = phraseSeed
        do {
            try await verifySeedAgainstPublishedIdentity(
                accountIdBytes: pending.accountIdBytes,
                sessionToken: sessionToken ?? pending.sessionToken,
                accountIdentitySeed: seed
            )
        } catch {
            Self.zeroize(&seed)
            throw error
        }
        // ADR-0018 factor (c): a TOTP-recovered fresh device parked
        // with NO device root key. Bootstrap it from the VERIFIED
        // transferred seed (the `completeFromDeviceRestore` recipe),
        // strictly AFTER the fail-closed verification above — a
        // refusal must leave this device untouched.
        bootstrapDeviceKeyFromVerifiedSeedIfAbsent(
            accountIdB64: pending.accountIdB64,
            accountIdentitySeed: seed,
            context: "completeFromPairing"
        )
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: pending.accountIdB64,
                seed: seed
            )
        } catch {
            NSLog(
                "account-identity-seed persist (completeFromPairing) " +
                "failed: \(error)"
            )
        }
        identitySecrets = IdentitySecrets(
            accountId: pending.accountIdBytes,
            exportKey: pending.exportKey,
            accountIdentitySeed: seed,
            sessionToken: sessionToken ?? pending.sessionToken,
            sessionExpiresAtMs: sessionExpiresAtMs
                ?? pending.sessionExpiresAtMs
        )
        pendingRecovery = nil
        state = .session(
            SessionAccount(
                accountIdB64: pending.accountIdB64,
                username: pending.username
            )
        )
    }

    /// ADR-0018 factor (a), path (b) — NO-PASSWORD restore: the fresh
    /// device ran the anonymous pairing join and holds the transferred
    /// identity seed + the REDEEMED session token (the only ACCESS on
    /// this device — there is no password, hence no OPAQUE export key).
    ///
    /// FAIL CLOSED FIRST — the seed is verified against the published
    /// identity BEFORE any persistence; a mismatched / unverifiable
    /// seed leaves this device untouched.
    ///
    /// The device root key (the ``rehydrate()`` existence-gate) is
    /// derived from the identity seed via
    /// `deriveDeviceRootKeyFromIdentitySeed` — distinct HKDF domain
    /// from the exportKey-derived variant. The iOS per-account stores
    /// (`AccountIdentitySeedStore` / `DeviceIdentityStore` / …) are
    /// direct Keychain rows, NOT wrapped under the device root key, so
    /// a later password login overwriting it with the exportKey-derived
    /// value is harmless — same posture as Android.
    ///
    /// `IdentitySecrets.exportKey` is set EMPTY on this path — no
    /// consumer outside this repository reads it (grep-verified; the
    /// canonical identity + backup signing are phrase-seeded per
    /// ADR-0017), and a later password login replaces it.
    ///
    /// Takes ownership of `phraseSeed` (zeroized on every refusal —
    /// best-effort under Swift `Data` value semantics).
    public func completeFromDeviceRestore(
        accountIdBytes: Data,
        accountIdB64: String,
        username: String,
        phraseSeed: Data,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) async throws {
        var seed = phraseSeed
        do {
            try await verifySeedAgainstPublishedIdentity(
                accountIdBytes: accountIdBytes,
                sessionToken: sessionToken,
                accountIdentitySeed: seed
            )
        } catch {
            Self.zeroize(&seed)
            throw error
        }
        // Bootstrap the device root key from the identity seed so the
        // next-launch rehydrate gate sees a key on this device.
        // Best-effort: a Keychain failure leaves the in-memory session
        // working; the next password login re-derives + persists.
        var deviceRootKey = deriveDeviceRootKeyFromIdentitySeed(
            accountIdentitySeed: seed,
            accountIdB64: accountIdB64
        )
        do {
            try DeviceKeyStore.putDeviceKey(
                accountIdB64: accountIdB64,
                rootKey: deviceRootKey
            )
        } catch {
            NSLog(
                "device-key persist (completeFromDeviceRestore) " +
                "failed: \(error)"
            )
        }
        Self.zeroize(&deviceRootKey)
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: accountIdB64,
                seed: seed
            )
        } catch {
            NSLog(
                "account-identity-seed persist (completeFromDeviceRestore) " +
                "failed: \(error)"
            )
        }
        // Persist the account marker — there is no prior session row
        // on this fresh device.
        sessionStore.save(
            PersistedSession(
                accountIdB64: accountIdB64,
                username: username
            )
        )
        identitySecrets = IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: Data(),
            accountIdentitySeed: seed,
            sessionToken: sessionToken,
            sessionExpiresAtMs: sessionExpiresAtMs
        )
        pendingRecovery = nil
        state = .session(
            SessionAccount(
                accountIdB64: accountIdB64,
                username: username
            )
        )
    }

    /// ADR-0018 factor (c) — recover ACCESS with an authenticator
    /// (TOTP) code. Mirror of the web `recoverWithTotp`.
    ///
    /// ACCESS only: the server verifies the code and issues the
    /// session triple; identity acquisition runs the ADR-0017 state
    /// machine below. NO password is involved, so there is NO OPAQUE
    /// export key on this path — `exportKey` is empty `Data()` (the
    /// ``completeFromDeviceRestore(accountIdBytes:accountIdB64:username:phraseSeed:sessionToken:sessionExpiresAtMs:)``
    /// convention).
    ///
    ///   * **Warm case** — a cached AccountIdentity (phrase) seed on
    ///     this device rebuilds the canonical identity directly. If
    ///     the device root key is missing (e.g. post-Keychain-reset),
    ///     it is bootstrapped from the cached (already-verified) seed
    ///     so the ``rehydrate()`` existence-gate passes on the next
    ///     launch.
    ///   * **Cold case** — park a ``PendingRecovery`` (empty
    ///     exportKey) and flip to `.needsRecoveryPhrase`; the
    ///     completion paths (`provideRecoveryPhrase` /
    ///     `completeFromPairing`) verify the supplied seed fail-closed
    ///     and bootstrap the device key from it.
    public func recoverWithTotp(username: String, code: String) async throws {
        let trimmed = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw AuthClientError.argument(
                "enter the username of the account to recover"
            )
        }
        // Resolve the username -> account_id exactly like `login` /
        // `recover`: a local cache hit wins, else a discovery lookup
        // round-trip binds it.
        let cached = sessionStore.load()
        let accountIdB64: String
        let resolvedUsername: String
        if let cached, cached.username == trimmed {
            accountIdB64 = cached.accountIdB64
            resolvedUsername = cached.username
        } else {
            let discovery = DiscoveryClient()
            let lookup = try await discovery.lookup(username: trimmed)
            guard let accountBytes = lookup.account else {
                throw AuthClientError.argument("username not found")
            }
            accountIdB64 = base64Encode(accountBytes)
            resolvedUsername = lookup.normalisedUsername
        }
        guard let accountIdBytes = base64Decode(accountIdB64) else {
            throw AuthClientError.argument(
                "resolved account id is malformed; please try again"
            )
        }

        let client = authClientFactory()
        let result = try await client.totpRecover(
            accountId: accountIdBytes,
            code: code
        )

        // Shared ADR-0018 ACCESS-only landing (warm cached-seed finish
        // OR cold fail-safe park) — identical to the cascade's other
        // ACCESS factor (factor (d) phone / email OTP).
        landAfterAccessRecovery(
            username: resolvedUsername,
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs,
            context: "totp recover",
            pushContext: "totp-recover"
        )
    }

    /// ADR-0018 — the shared post-ACCESS landing for the two ACCESS-
    /// only recovery factors (factor (c) TOTP + factor (d) phone /
    /// email OTP). Both factors verify a code server-side and receive
    /// the session triple, carry NO password (so `exportKey` is empty),
    /// and then run the IDENTICAL ADR-0017 identity-acquisition state
    /// machine:
    ///
    ///   * **Warm case** — a cached AccountIdentity (phrase) seed on
    ///     this device rebuilds the canonical identity directly. The
    ///     device root key is bootstrapped from the cached (already-
    ///     verified) seed when absent so the ``rehydrate()``
    ///     existence-gate passes on the next launch, then
    ///     ``finishCanonicalSession`` flips to `.session`.
    ///   * **Cold case** — park a ``PendingRecovery`` (empty exportKey)
    ///     and flip to `.needsRecoveryPhrase`; the completion paths
    ///     (`provideRecoveryPhrase` / `completeFromPairing`) verify the
    ///     supplied seed fail-closed and bootstrap the device key from
    ///     it.
    ///
    /// Extracted VERBATIM from the original `recoverWithTotp` tail (the
    /// session-marker save + the warm / cold branch are byte-for-byte
    /// the prior behaviour) so factor (d) reuses it without drift.
    private func landAfterAccessRecovery(
        username: String,
        accountIdBytes: Data,
        accountIdB64: String,
        sessionToken: String,
        sessionExpiresAtMs: Int64,
        context: String,
        pushContext: String
    ) {
        // Persist the session marker — important on a fresh device
        // where no row existed (resolved via the discovery lookup).
        sessionStore.save(
            PersistedSession(
                accountIdB64: accountIdB64,
                username: username
            )
        )

        // ADR-0017 identity acquisition. These factors carried NO
        // password, so there is no OPAQUE export key — the only
        // same-device shortcut is an already-cached AccountIdentity
        // seed.
        let accountIdentitySeed = Self.readCachedAccountIdentitySeed(
            accountIdB64: accountIdB64,
            context: context
        )
        if let accountIdentitySeed {
            // Warm case. The cached seed was verified against the
            // published bundle when it was stored, so bootstrapping
            // the device root key from it (when absent — the
            // rehydrate existence-gate) is safe here.
            bootstrapDeviceKeyFromVerifiedSeedIfAbsent(
                accountIdB64: accountIdB64,
                accountIdentitySeed: accountIdentitySeed,
                context: context
            )
            finishCanonicalSession(
                accountIdentitySeed: accountIdentitySeed,
                accountIdBytes: accountIdBytes,
                exportKey: Data(),
                sessionToken: sessionToken,
                sessionExpiresAtMs: sessionExpiresAtMs,
                username: username
            )
            Task { await Self.subscribePushBestEffort(context: pushContext) }
            return
        }

        // Cold case — fresh-device fail-safe (ADR-0017): ACCESS
        // succeeded, identity restore is pending. exportKey EMPTY (no
        // password); the completion paths bootstrap the device key
        // from the VERIFIED seed.
        pendingRecovery = PendingRecovery(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            exportKey: Data(),
            sessionToken: sessionToken,
            sessionExpiresAtMs: sessionExpiresAtMs,
            username: username
        )
        state = .needsRecoveryPhrase(
            SessionAccount(
                accountIdB64: accountIdB64,
                username: username
            )
        )
    }

    /// The just-minted fresh recovery phrase, for the
    /// ``AuthState/saveFreshPhrase(_:)`` screen to display. `nil` outside
    /// that flow. Kept off the ``AuthState`` so it is not threaded through
    /// the router (mirror of the web `freshPhrase()` accessor).
    public func freshPhrase() -> String? { pendingFreshPhrase?.phrase }

    /// ADR-0017 lost-everything fallback. Mint a brand-new recovery phrase
    /// + AccountIdentity for the SAME account when the user recovered
    /// ACCESS via an ADR-0018 factor but has NO recovery phrase and NO
    /// other device.
    ///
    /// This is the ONLY sanctioned path that publishes a CHANGED
    /// AccountIdentity key WITHOUT a supersession proof — correct because
    /// there is genuinely no old seed to sign with. The user's contacts
    /// then re-verify out of band (the accepted, documented degradation).
    ///
    /// Reachable ONLY while a ``PendingRecovery`` is parked (post-ACCESS,
    /// from ``AuthState/needsRecoveryPhrase(_:)``) and ONLY via the
    /// explicit UI button. Does NOT land the session — it flips to
    /// ``AuthState/saveFreshPhrase(_:)`` and returns the new phrase;
    /// ``confirmFreshPhraseSaved()`` lands the session after the user
    /// acknowledges. Mirror of the web `mintFreshIdentity`.
    public func mintFreshIdentity() async throws -> String {
        guard let pending = pendingRecovery else {
            throw AuthClientError.argument("no pending identity recovery — sign in first")
        }

        // Generate a fresh 24-word phrase + seed (same client-side
        // generation signup uses).
        let mnemonic = RecoveryMnemonic.generate()
        let phrase = mnemonic.phrase()
        let seed = mnemonic.toSeed()

        // Publish the NEW bundle WITHOUT a supersession proof FIRST, and
        // FAIL the flow if it does not land — otherwise a transient network
        // error would leave the device with a cached fresh seed while the
        // server still serves the OLD bundle, a torn state the user cannot
        // self-heal (provideRecoveryPhrase with the new phrase would then
        // hard-block against the stale published key). Publishing before any
        // local mutation keeps a failure fully recoverable: the device is
        // untouched and the user can retry.
        try await publishFreshIdentityBundle(
            accountId: pending.accountIdBytes,
            sessionToken: pending.sessionToken,
            seed: seed
        )

        // Bootstrap the device-key existence gate from the fresh seed
        // (no password on this path); no-op if a key already exists.
        bootstrapDeviceKeyFromVerifiedSeedIfAbsent(
            accountIdB64: pending.accountIdB64,
            accountIdentitySeed: seed,
            context: "mintFreshIdentity"
        )

        // Cache the fresh seed locally (best-effort).
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: pending.accountIdB64,
                seed: seed
            )
        } catch {
            NSLog("fresh-identity seed persist failed: \(error)")
        }

        // Stash the completion + phrase; flip to the save-phrase state.
        pendingFreshPhrase = PendingFreshPhrase(
            seed: seed,
            accountIdBytes: pending.accountIdBytes,
            accountIdB64: pending.accountIdB64,
            sessionToken: pending.sessionToken,
            sessionExpiresAtMs: pending.sessionExpiresAtMs,
            username: pending.username,
            phrase: phrase
        )
        pendingRecovery = nil
        state = .saveFreshPhrase(
            SessionAccount(
                accountIdB64: pending.accountIdB64,
                username: pending.username
            )
        )
        return phrase
    }

    /// ADR-0017 lost-everything fallback — finish the session once the user
    /// acknowledged the new recovery phrase. Valid ONLY while a
    /// ``PendingFreshPhrase`` is parked.
    public func confirmFreshPhraseSaved() throws {
        guard let pending = pendingFreshPhrase else {
            throw AuthClientError.argument("no fresh identity to confirm")
        }
        pendingFreshPhrase = nil
        sessionStore.save(
            PersistedSession(
                accountIdB64: pending.accountIdB64,
                username: pending.username
            )
        )
        finishCanonicalSession(
            accountIdentitySeed: pending.seed,
            accountIdBytes: pending.accountIdBytes,
            exportKey: Data(),
            sessionToken: pending.sessionToken,
            sessionExpiresAtMs: pending.sessionExpiresAtMs,
            username: pending.username
        )
    }

    /// ADR-0036 (S6) — mint an auth-attested genesis proof binding the
    /// OPAQUE-authenticated account to `identityPubkey`, so the KT-log genesis
    /// entry is account-authenticated (anti-squatting), not merely
    /// self-consistent. `POST /auth/genesis/attest`, bearer-authed with the
    /// live session token.
    ///
    /// FAIL-SAFE: a mint failure logs + returns `nil` so the caller still
    /// publishes the (self-signed) genesis bundle — the transparency operator
    /// gate is default-OFF, so an un-attested genesis is accepted; the
    /// attestation is attached only when the mint succeeds.
    private static func mintGenesisAttestation(
        accountId: Data,
        identityPubkey: Data,
        sessionToken: String
    ) async -> Data? {
        do {
            return try await AuthClient().mintGenesisAttestation(
                sessionToken: sessionToken,
                identityPubkey: identityPubkey
            )
        } catch {
            NSLog(
                "genesis-attest (ADR-0036): mint failed, publishing bundle " +
                "without attestation: \(error)"
            )
            return nil
        }
    }

    /// Publish the fresh-identity bundle (+ KeyPackage) for `accountId`
    /// WITHOUT a supersession proof (ADR-0017 lost-everything). THROWS on
    /// failure — the caller publishes before any local mutation, so a throw
    /// leaves the device untouched and the user can retry.
    private func publishFreshIdentityBundle(
        accountId: Data,
        sessionToken: String,
        seed: Data
    ) async throws {
        let discovery = DiscoveryClient(sessionToken: sessionToken)
        let identity = try Identity.fromSeed(seed: seed)
        // G-30 #2 (ADR-0019): a fresh-start mint publishes a NEW key with no
        // supersession proof, so authorise the log entry with a genesis
        // self-signature over the (account_key, pubkey) binding.
        let leafAuth = try signLeafGenesis(
            ed25519Seed: identity.ed25519Seed(),
            usernameHash: transparencyAccountKey(accountId: accountId)
        )
        // ADR-0036: additionally attest the genesis binding via auth (fail-safe
        // — a mint failure attaches nothing, never blocks the reset publish).
        let genesisAttest = await Self.mintGenesisAttestation(
            accountId: accountId,
            identityPubkey: identity.signaturePublicKey(),
            sessionToken: sessionToken
        )
        try await discovery.uploadIdentityBundle(
            accountId: accountId,
            signaturePubkey: identity.signaturePublicKey(),
            x25519Pubkey: identity.x25519PublicKey(),
            leafAuth: leafAuth,
            genesisAttest: genesisAttest
        )
        try await discovery.uploadKeyPackage(
            accountId: accountId,
            keyPackageBytes: identity.createKeyPackage()
        )
    }

    /// ADR-0018 factor (d), step 1 — send a one-time code to a
    /// previously-enrolled phone / email. ACCESS only: the finish step
    /// verifies the code and issues the session triple; identity
    /// acquisition runs the ADR-0017 state machine afterward. Mirror of
    /// the web `AuthContext.recoverWithContactStart`.
    ///
    /// Resolves the username → account_id exactly like `login` /
    /// `recover` / `recoverWithTotp` (a local cache hit wins, else a
    /// discovery lookup binds it), sends the code, and stashes the
    /// resolved `(accountId, username, kind)` so
    /// ``recoverWithContactFinish(code:)`` can submit without
    /// re-resolving.
    ///
    /// ENUMERATION-SAFE: the server returns the same `{ sent: true }`
    /// whether or not the contact matches a confirmed row, so a
    /// non-throwing return only means "advance to the code step" — it
    /// reveals nothing about account / contact existence.
    public func recoverWithContactStart(
        username: String,
        kind: ContactKind,
        contact: String
    ) async throws {
        let trimmed = username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw AuthClientError.argument(
                "enter the username of the account to recover"
            )
        }
        // Resolve the username -> account_id exactly like `login` /
        // `recover` / `recoverWithTotp`: a local cache hit wins, else a
        // discovery lookup round-trip binds it.
        let cached = sessionStore.load()
        let accountIdB64: String
        let resolvedUsername: String
        if let cached, cached.username == trimmed {
            accountIdB64 = cached.accountIdB64
            resolvedUsername = cached.username
        } else {
            let discovery = DiscoveryClient()
            let lookup = try await discovery.lookup(username: trimmed)
            guard let accountBytes = lookup.account else {
                throw AuthClientError.argument("username not found")
            }
            accountIdB64 = base64Encode(accountBytes)
            resolvedUsername = lookup.normalisedUsername
        }
        guard let accountIdBytes = base64Decode(accountIdB64) else {
            throw AuthClientError.argument(
                "resolved account id is malformed; please try again"
            )
        }

        // Send the code. ENUMERATION-SAFE server-side: a non-throwing
        // return here only means "advance to the code step".
        let client = authClientFactory()
        try await client.recoveryContactRecoverStart(
            accountId: accountIdBytes,
            kind: kind,
            contact: contact
        )

        // Stash the resolved context so `…Finish` can submit the code
        // without re-resolving. The kind is carried verbatim so the
        // finish targets the exact (account, kind) the code was sent
        // for.
        pendingContactRecover = PendingContactRecover(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            username: resolvedUsername,
            kind: kind
        )
    }

    /// ADR-0018 factor (d), step 2 — submit the delivered code. Mirror
    /// of the web `AuthContext.recoverWithContactFinish`.
    ///
    /// Reads back the stash from ``recoverWithContactStart(username:kind:contact:)``,
    /// verifies the code server-side for the session triple, clears the
    /// stash (the code is single-use; a retry must re-send), and runs
    /// the shared ACCESS-only landing — identical to the cascade's
    /// other ACCESS factor (factor (c) TOTP).
    public func recoverWithContactFinish(code: String) async throws {
        guard let pending = pendingContactRecover else {
            throw AuthClientError.argument(
                "request a code first (no pending phone / email recovery)"
            )
        }
        let client = authClientFactory()
        let result = try await client.recoveryContactRecoverFinish(
            accountId: pending.accountIdBytes,
            kind: pending.kind,
            code: code
        )
        // Consume the stash — the code is single-use server-side; a
        // retry must re-send.
        pendingContactRecover = nil
        // Shared ADR-0018 ACCESS-only landing — identical to the
        // cascade's other ACCESS factor (factor (c) TOTP).
        landAfterAccessRecovery(
            username: pending.username,
            accountIdBytes: pending.accountIdBytes,
            accountIdB64: pending.accountIdB64,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs,
            context: "contact recover",
            pushContext: "contact-recover"
        )
    }

    /// ADR-0018 factor (c) — bootstrap the device root key from a
    /// VERIFIED AccountIdentity seed when the Keychain has none (the
    /// ``rehydrate()`` existence-gate would otherwise bounce the next
    /// launch to the login form). Mirror of the web's nullable-
    /// deviceKey bootstrap in `provideRecoveryPhrase` /
    /// `completeFromPairing`.
    ///
    /// SECURITY: the caller MUST only invoke this with a seed that
    /// already passed ``verifySeedAgainstPublishedIdentity`` (or was
    /// cached AFTER passing it) — never bootstrap before
    /// verification; a refusal must leave this device untouched.
    ///
    /// Best-effort: a Keychain failure leaves the in-memory session
    /// working; a later password login re-derives + persists. A probe
    /// failure is treated as "no key" (same as ``rehydrate()``) — the
    /// iOS per-account stores are direct Keychain rows, NOT wrapped
    /// under the device root key, so an overwrite is harmless.
    private func bootstrapDeviceKeyFromVerifiedSeedIfAbsent(
        accountIdB64: String,
        accountIdentitySeed: Data,
        context: String
    ) {
        do {
            if try DeviceKeyStore.getDeviceKey(accountIdB64: accountIdB64) != nil {
                // A stored device key exists (e.g. a prior password
                // login on this device) — keep it.
                return
            }
        } catch {
            NSLog("device-key probe failed (\(context)): \(error)")
            // Fall through — same "treat as no key" posture as
            // rehydrate().
        }
        var deviceRootKey = deriveDeviceRootKeyFromIdentitySeed(
            accountIdentitySeed: accountIdentitySeed,
            accountIdB64: accountIdB64
        )
        do {
            try DeviceKeyStore.putDeviceKey(
                accountIdB64: accountIdB64,
                rootKey: deviceRootKey
            )
        } catch {
            NSLog("device-key bootstrap (\(context)) failed: \(error)")
        }
        Self.zeroize(&deviceRootKey)
    }

    /// Best-effort wipe of a seed buffer we own — zero it now so the
    /// next GC / copy cannot hand it to a leaked closure.
    private static func zeroize(_ data: inout Data) {
        data.resetBytes(in: 0..<data.count)
    }

    // MARK: - G-4 (ADR-0016 / ADR-0017) identity-supersession emit

    /// ADR-0017 — re-publish the identity bundle on recover, SKIPPING
    /// the G-4 supersession proof (now DORMANT).
    ///
    /// With a phrase-seeded AccountIdentity, a routine recover does NOT
    /// rotate the account key: the phrase is unchanged, so the NEW
    /// account signature pubkey EQUALS `oldAccountPubkey` (captured
    /// before the recover). We re-derive the new canonical identity
    /// from the (unchanged) phrase seed and assert the equality:
    ///
    ///   * equal (the only case on a routine recover) → re-publish the
    ///     bundle WITHOUT a proof. The public keys are identical but the
    ///     fresh KeyPackage's init key is new, so the upsert keeps the
    ///     directory current + a proof-less upload clears any stale
    ///     server-side proof column.
    ///   * NOT equal → this can NEVER happen on a phrase-seeded recover
    ///     (same seed ⇒ same key). If it ever does, the phrase that
    ///     authenticated the OPAQUE recover differs from the one seeding
    ///     the identity — a logic bug. We FAIL LOUD (skip the upload +
    ///     log loudly) rather than silently publish a changed key with
    ///     no proof, which would break every contact's pin
    ///     (self-inflicted downgrade). A genuine identity rotation is a
    ///     separate explicit action (ADR-0017 "rotate my identity"),
    ///     out of scope for Part 1.
    ///
    /// The upload itself is best-effort beyond the equality gate: a
    /// network failure does not fail the recover.
    private static func republishBundleSkippingSupersessionProof(
        accountId: Data,
        accountIdentitySeed: Data,
        oldAccountPubkey: Data,
        sessionToken: String
    ) async {
        let identity: Identity
        do {
            identity = try Identity.fromSeed(seed: accountIdentitySeed)
        } catch {
            NSLog("supersession (ADR-0017): new identity derive (recover) failed: \(error)")
            return
        }
        let newAccountPubkey = identity.signaturePublicKey()
        guard newAccountPubkey == oldAccountPubkey else {
            // ADR-0017 emit invariant violated. Do NOT publish a changed
            // key with no proof — fail loud and leave the old bundle
            // live.
            NSLog(
                "supersession (ADR-0017): recover produced a DIFFERENT " +
                "AccountIdentity pubkey than expected (invariant: same " +
                "phrase => same identity). SKIPPING bundle re-publish to " +
                "avoid publishing a changed key with no proof."
            )
            return
        }

        let discovery = DiscoveryClient(sessionToken: sessionToken)
        do {
            // G-30 #2 (ADR-0019): carry a genesis self-signature over the
            // (account_key, pubkey) binding. On a TRUE re-publish (the key is
            // already logged) discovery Skips the transparency append, so this
            // is ignored; but on this platform `signup` does not itself publish
            // the bundle, so a recover can be the FIRST time discovery sees the
            // account — then this self-sig authorises the genesis log entry.
            // The key is unchanged, so the self-sig is valid.
            let leafAuth = try signLeafGenesis(
                ed25519Seed: identity.ed25519Seed(),
                usernameHash: transparencyAccountKey(accountId: accountId)
            )
            // ADR-0036: attest the genesis binding via auth when this recover is
            // the FIRST time discovery sees the account (fail-safe — a mint
            // failure attaches nothing, never blocks the re-publish). On a TRUE
            // re-publish (already logged) discovery skips the append + the
            // attestation is ignored.
            let genesisAttest = await mintGenesisAttestation(
                accountId: accountId,
                identityPubkey: identity.signaturePublicKey(),
                sessionToken: sessionToken
            )
            // Proof-less re-publish (account key unchanged). Refreshes
            // the KeyPackage + clears any stale server proof column.
            try await discovery.uploadIdentityBundle(
                accountId: accountId,
                signaturePubkey: identity.signaturePublicKey(),
                x25519Pubkey: identity.x25519PublicKey(),
                leafAuth: leafAuth,
                genesisAttest: genesisAttest
            )
        } catch {
            NSLog("supersession (ADR-0017): proof-less bundle re-publish (recover) failed: \(error)")
        }
    }

    // MARK: - Backup bootstrap helpers (Phase 2a1c)

    /// Phase 2a1d — fetch + decrypt + parse the prior backup
    /// envelope for this account, if any. Best-effort; logs
    /// every non-`restored` outcome and continues.
    ///
    /// Called from the recover flow BEFORE the bootstrap step
    /// so the new baseline upload can carry the merged set
    /// instead of overwriting the server blob with an empty
    /// list. On iOS the application of restored contacts into
    /// ``ContactsRepository`` is not yet wired — the contacts
    /// list is in-memory only and there's no persistent target
    /// to upsert into; this helper logs the parsed-contact
    /// count so the wire-decryption end of the flow is
    /// exercised today, and the merge-into-state layer lands
    /// alongside the iOS contact-persistence sub-sprint.
    /// Returns the prior MLS whole-storage snapshot (if any) so
    /// the bootstrap helper can re-upload it instead of nuking
    /// the server-side snapshot with an empty one. Empty `Data`
    /// when there is no prior backup or the prior backup
    /// pre-dates 2a3d.
    ///
    /// ADR-0037 §6 (G-11): the restore runs behind the rollback gate — a
    /// tampered / rolled-back head refuses outright and raises
    /// ``backupRollbackAlarm``; a stale head prompts the user via
    /// ``pendingStaleBackupOverride`` (fail-closed on decline). An instance
    /// method (was `static`) so it can arm the gate's confirmation callback +
    /// set the published alarm.
    private func restoreFromServerBackupBestEffort(
        accountId: Data,
        accountIdB64: String,
        sessionToken: String,
        phrase: String,
        context: String
    ) async -> Data {
        let phraseBytes = Data(phrase.utf8)
        let client = BackupClient()
        client.setSessionToken(sessionToken)

        // ADR-0037 §6: arm the rollback gate — verify the auth-signed backup
        // head against the pinned attest key before trusting the served
        // envelope. A stale head suspends on the confirmation dialog; declining
        // aborts the restore (fail-closed — never a silent downgrade).
        let gate = RestoreGateContext(
            accountId: accountId,
            accountIdB64: accountIdB64,
            confirmStaleOverride: { [weak self] reason in
                guard let self else { return false }
                return await self.awaitStaleBackupOverride(reason: reason)
            }
        )
        let outcome = await client.restore(phraseBytes: phraseBytes, gate: gate)

        let parsed: ParsedBackupPayload
        switch outcome {
        case .rollbackBlocked(let hard, let reason):
            if hard {
                // Refuse + alarm. We do NOT load the served state (invariant #4).
                NSLog(
                    "backup restore (\(context)) BLOCKED a rolled-back / tampered "
                    + "backup: \(reason)"
                )
                backupRollbackAlarm = BackupRollbackAlarm(reason: reason)
            } else {
                NSLog("backup restore (\(context)): user declined a stale backup: \(reason)")
            }
            return Data()
        case .noBackup:
            NSLog("backup restore (\(context)): no prior backup")
            return Data()
        case .wrongPhraseOrTampered:
            NSLog("backup restore (\(context)) failed: wrong phrase or tampered blob")
            return Data()
        case .transportFailed(let error):
            NSLog("backup restore (\(context)) failed: \(error)")
            return Data()
        case .restored(let payload):
            parsed = payload
        }

        NSLog(
            "backup restore (\(context)): restored \(parsed.acceptedContacts.count) contact(s) "
            + "from snapshot at \(parsed.createdAtMs), "
            + "MLS snapshot \(parsed.wholeStorageSnapshot.count) bytes"
        )
        // Contact-in-backup restore (ADR-0021): reconstruct the accepted
        // contacts (with their account_id) and MERGE into the persistent
        // store — skip a pubkey already present so a contact accepted after
        // the backup is not clobbered. The backup does not carry the inbox
        // metadata, so reconstruct an IncomingContactRequest with sentinels:
        // a negative messageId (never collides with a real DS id), a zero
        // pseudonym, no intro.
        let existingContacts = AcceptedContactsStore.load()
        let restoredContacts = reconstructRestoredContacts(
            existing: existingContacts,
            restored: parsed.acceptedContacts,
            createdAtMs: parsed.createdAtMs
        )
        if !restoredContacts.isEmpty {
            AcceptedContactsStore.save(existingContacts + restoredContacts)
            NSLog("backup restore (\(context)): merged \(restoredContacts.count) contact(s)")
        }
        return parsed.wholeStorageSnapshot
    }

    /// Cache the recovery phrase under the Keychain + post a
    /// baseline backup envelope to /backup. Called from
    /// ``signup`` and ``recover`` right after the state flip.
    /// Best-effort end-to-end; every step logs + continues on
    /// failure so a transient transport hiccup does not block
    /// the user from landing in a working session.
    ///
    /// ADR-0017: `ed25519Seed` is derived from the canonical
    /// AccountIdentity (phrase) seed — NOT the OPAQUE export key.
    /// The backup envelope STORES this account-identity Ed25519 seed
    /// so a fresh device can restore the SAME identity from the
    /// phrase; it must therefore be the phrase-seeded identity's seed
    /// (matching the web `bootstrap` which signs with
    /// `canonical.ed25519Seed`). Re-derive here rather than threading
    /// a handle through the public surface so this helper stays
    /// self-contained.
    private static func bootstrapBackupBestEffort(
        accountId: Data,
        accountIdB64: String,
        accountIdentitySeed: Data,
        sessionToken: String,
        phrase: String,
        context: String,
        wholeStorageSnapshot: Data = Data()
    ) async {
        let phraseBytes = Data(phrase.utf8)
        let backupClient = BackupClient()
        backupClient.setSessionToken(sessionToken)

        // Cache the phrase first — even if the network leg
        // fails, future auto-backups (post-2a3) can re-try from
        // the cached value.
        do {
            try backupClient.cachePhrase(
                accountIdB64: accountIdB64,
                phraseBytes: phraseBytes
            )
        } catch {
            NSLog("backup phrase cache (\(context)) failed: \(error)")
            // Continue — we can still ship a one-shot baseline
            // backup without the cache. The next signup / recover
            // re-tries the cache write.
        }

        // ADR-0017: derive the ed25519 seed from the canonical
        // AccountIdentity (phrase) seed — the same `Identity.fromSeed`
        // path every other canonical-identity operation now uses.
        let identity: Identity
        do {
            identity = try Identity.fromSeed(seed: accountIdentitySeed)
        } catch {
            NSLog("backup identity-derive (\(context)) failed: \(error)")
            return
        }
        let seedBytes = identity.ed25519Seed()

        // Contact-in-backup (ADR-0021): include the accepted contacts so a
        // restore on a fresh device rebuilds the list + resolves group-call
        // roster names. The native AcceptedContact carries no X25519 pubkey,
        // so zero-fill it (matches web's legacy-contact handling); the backup
        // carries ed25519 + display name + the 16-byte account_id. displayName
        // stays "" when unset so a restore does not promote the fingerprint
        // fallback into a "chosen" name.
        let contactRecords = AcceptedContactsStore.load().map { c in
            BackupContactRecord(
                ed25519Pubkey: c.request.fromPubkey,
                x25519Pubkey: Data(count: 32),
                displayName: c.displayName ?? "",
                accountId: c.request.fromAccountId
            )
        }

        do {
            try await backupClient.uploadNow(
                accountId: accountId,
                accountIdB64: accountIdB64,
                ed25519Seed: seedBytes,
                phraseBytes: phraseBytes,
                contacts: contactRecords,
                wholeStorageSnapshot: wholeStorageSnapshot
            )
        } catch {
            NSLog("backup baseline upload (\(context)) failed: \(error)")
        }
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
    /// ADR-0017 fresh-device fail-safe. The user authenticated
    /// successfully (password or passkey ACCESS) but this device has
    /// NO cached AccountIdentity (phrase) seed, so we cannot rebuild
    /// the canonical identity. We MUST NOT derive it from the export
    /// key (the wrong identity now) nor silently mint a fresh one
    /// (which would break every contact's pin). The UI renders a
    /// recovery-phrase prompt
    /// (``RestoreIdentityPromptScreen``); ``provideRecoveryPhrase(mnemonicPhrase:)``
    /// completes the session once the user supplies their phrase.
    case needsRecoveryPhrase(SessionAccount)
    /// ADR-0017 lost-everything fallback. The user recovered ACCESS via
    /// an ADR-0018 factor but had NO recovery phrase and NO other device,
    /// and chose to mint a brand-new identity (already published, without
    /// a supersession proof). The UI must show the new phrase to save
    /// before the session lands; ``confirmFreshPhraseSaved()`` finishes it.
    case saveFreshPhrase(SessionAccount)
}

/// ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) — a
/// security alert raised when this device's canonical AccountIdentity did
/// not match the account's currently-published bundle at login. Mirror of
/// the web `IdentityAlert`.
public enum IdentityAlert: Equatable {
    /// Someone published a DIFFERENT identity for this account (the
    /// ACCESS-hijack signature — an attacker minted a fresh identity via
    /// the lost-everything path).
    case publishedIdentityChanged
}

/// ADR-0037 §6 (G-11) — a rolled-back / tampered backup was REFUSED at restore.
/// Carries the human-readable reason from the ``restoreVerdict(_:)`` hard-fail.
/// Surfaced as a user-visible alert; the served (older / altered) state is NOT
/// loaded.
public struct BackupRollbackAlarm: Equatable {
    public let reason: String

    public init(reason: String) {
        self.reason = reason
    }
}

/// ADR-0037 §6 (G-11) — a stale (older-than-expected) but authentic backup is
/// pending the user's explicit override decision. Drives the blocking
/// confirmation dialog; `Identifiable` so the SwiftUI dialog can key on it.
public struct StaleBackupOverrideRequest: Equatable, Identifiable {
    public let id: UUID
    public let reason: String

    public init(reason: String) {
        self.id = UUID()
        self.reason = reason
    }
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
    ///
    /// ADR-0017: NO LONGER the seed for the canonical
    /// AccountIdentity — it is retained only as an auth factor and
    /// as the device-root-key seed
    /// (``deriveDeviceRootKey(exportKey:accountIdB64:)``). Build the
    /// canonical identity from ``accountIdentitySeed`` instead.
    public let exportKey: Data
    /// ADR-0017: the BIP39 recovery-phrase seed
    /// (`RecoveryMnemonic.toSeed()`, 64 bytes). This is the seed for
    /// the canonical AccountIdentity (`Identity.fromSeed(seed:)`) —
    /// stable across password changes and recovery, so the pubkey
    /// contacts pin never churns on a routine recover. Every
    /// `Identity.fromSeed` site that builds the canonical identity
    /// reads THIS, not ``exportKey``.
    public let accountIdentitySeed: Data
    /// Phase 1.5b: bearer token issued by the auth service on the
    /// most recent signup/login/recover. Threaded into
    /// ``DiscoveryClient/setSessionToken(_:)`` by
    /// ``ContactsRepository`` so every protected write carries
    /// the `Authorization: Bearer <token>` header.
    public let sessionToken: String
    /// Token expiry as a unix-millisecond timestamp.
    public let sessionExpiresAtMs: Int64

    public init(
        accountId: Data,
        exportKey: Data,
        accountIdentitySeed: Data,
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountId = accountId
        self.exportKey = exportKey
        self.accountIdentitySeed = accountIdentitySeed
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
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

// MARK: - ADR-0017 fresh-device fail-safe types

/// ADR-0017 fresh-device fail-safe: everything needed to finish a
/// session once the user supplies their recovery phrase. The auth
/// factor (password / passkey) already succeeded; only the
/// AccountIdentity (phrase) seed is missing on this device.
struct PendingRecovery {
    let accountIdBytes: Data
    let accountIdB64: String
    /// OPAQUE export key from the just-completed auth — retained for
    /// the in-memory `exportKey` (device key already derived from it).
    /// EMPTY `Data()` on the ADR-0018 factor (c) TOTP path: no
    /// password was involved, so there is no export key and no device
    /// root key yet — the completion paths bootstrap one from the
    /// verified identity seed (`bootstrapDeviceKeyFromVerifiedSeedIfAbsent`).
    let exportKey: Data
    let sessionToken: String
    let sessionExpiresAtMs: Int64
    let username: String
}

/// ADR-0017 lost-everything fallback — the completion context stashed by
/// ``AuthRepository/mintFreshIdentity()`` between minting the fresh identity
/// (already published) and the user acknowledging the new phrase. Holds no
/// password material; `seed` is the just-minted fresh AccountIdentity seed.
struct PendingFreshPhrase {
    let seed: Data
    let accountIdBytes: Data
    let accountIdB64: String
    let sessionToken: String
    let sessionExpiresAtMs: Int64
    let username: String
    let phrase: String
}

/// ADR-0018 factor (d) — the resolved context for an in-flight phone /
/// email OTP recovery, stashed between
/// ``AuthRepository/recoverWithContactStart(username:kind:contact:)``
/// (which resolves the username + sends the code) and
/// ``AuthRepository/recoverWithContactFinish(code:)`` (which submits
/// the delivered code). The `kind` is carried verbatim so the finish
/// targets the exact `(account, kind)` the code was sent for. Mirror of
/// the web `PendingContactRecover` signal.
struct PendingContactRecover {
    let accountIdBytes: Data
    let accountIdB64: String
    let username: String
    let kind: ContactKind
}

/// ADR-0017 — thrown by
/// ``AuthRepository/provideRecoveryPhrase(mnemonicPhrase:)`` when the
/// typed recovery phrase derives an AccountIdentity pubkey that does
/// NOT match the account's published bundle. Surfaced to the UI as
/// "that phrase doesn't match this account" — we refuse rather than
/// cache a forked identity that would break every contact's pin.
public struct PhraseMismatch: Error, LocalizedError {
    public init() {}
    public var errorDescription: String? {
        "That recovery phrase doesn't match this account's identity. " +
        "Check the phrase, or pair an existing device instead."
    }
}

/// ADR-0017 (lakko-security finding #1) — thrown by
/// ``AuthRepository/provideRecoveryPhrase(mnemonicPhrase:)`` when the
/// account's published bundle could NOT be fetched (404 / network) so
/// the supplied phrase cannot be verified against it. We HARD-BLOCK
/// rather than trust-on-first-use a possibly-wrong phrase: this path
/// only runs for an already-established account, so a missing bundle
/// is an anomaly (a withholding or unreachable directory), and a
/// server-forced 404 must not be able to push a socially-engineered
/// wrong phrase into the published identity. Surfaced as a retryable
/// error, not a dead end.
public struct IdentityVerificationUnavailable: Error, LocalizedError {
    private let message: String
    public init(_ message: String) {
        self.message = message
    }
    public var errorDescription: String? { message }
}
