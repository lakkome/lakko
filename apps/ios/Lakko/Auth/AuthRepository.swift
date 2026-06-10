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
        // binds it. Without either we cannot target the recover.
        let cached = sessionStore.load()
        let trimmedUsername = username?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let accountIdB64: String
        let resolvedUsername: String
        if let cached,
           trimmedUsername.isEmpty || cached.username == trimmedUsername {
            accountIdB64 = cached.accountIdB64
            resolvedUsername = cached.username
        } else if !trimmedUsername.isEmpty {
            let discovery = DiscoveryClient()
            let lookup = try await discovery.lookup(username: trimmedUsername)
            guard let accountBytes = lookup.account else {
                // Collapsed with "bad phrase" downstream by the OPAQUE
                // rejection — we don't help probe the username space.
                throw AuthClientError.argument("invalid recovery")
            }
            accountIdB64 = base64Encode(accountBytes)
            resolvedUsername = lookup.normalisedUsername
        } else {
            throw AuthClientError.argument(
                "no account on this device — enter your username so we " +
                "can look up the account to recover"
            )
        }
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
        let restoredSnapshot = await Self.restoreFromServerBackupBestEffort(
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
            // Proof-less re-publish (account key unchanged). Refreshes
            // the KeyPackage + clears any stale server proof column.
            try await discovery.uploadIdentityBundle(
                accountId: accountId,
                signaturePubkey: identity.signaturePublicKey(),
                x25519Pubkey: identity.x25519PublicKey()
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
    private static func restoreFromServerBackupBestEffort(
        sessionToken: String,
        phrase: String,
        context: String
    ) async -> Data {
        let phraseBytes = Data(phrase.utf8)
        let client = BackupClient()
        client.setSessionToken(sessionToken)
        let parsed: ParsedBackupPayload?
        do {
            parsed = try await client.restore(phraseBytes: phraseBytes)
        } catch {
            // Wrong phrase / tampered blob / transport failure —
            // fold into a single log line. The user can still
            // continue; the bootstrap step will overwrite.
            NSLog("backup restore (\(context)) failed: \(error)")
            return Data()
        }
        guard let parsed else {
            NSLog("backup restore (\(context)): no prior backup")
            return Data()
        }
        NSLog(
            "backup restore (\(context)): restored \(parsed.acceptedContacts.count) contact(s) "
            + "from snapshot at \(parsed.createdAtMs), "
            + "MLS snapshot \(parsed.wholeStorageSnapshot.count) bytes"
        )
        // TODO(2a1d-followup): once the iOS persistent contact
        // store lands, apply `parsed.acceptedContacts` here via a
        // contacts-merge hook.
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

        do {
            try await backupClient.uploadNow(
                accountId: accountId,
                ed25519Seed: seedBytes,
                phraseBytes: phraseBytes,
                contacts: [],  // 2a3 wires the cross-device-synced contact list
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
    let exportKey: Data
    let sessionToken: String
    let sessionExpiresAtMs: Int64
    let username: String
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
