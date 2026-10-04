// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import UserNotifications

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
///   * ``identitySecrets`` — the live session's secrets, after a
///     fresh authenticate or a relaunch restore from the Keychain;
///     `nil` whenever there is no session (logout, logged out).
///
/// `@MainActor` because every mutation lands on
/// SwiftUI's main runloop; the async flows hop out to
/// `Task.detached` for the signup PoW solve + the URLSession calls internally
/// (see ``AuthClient`` and ``solvePow(seedB64:effortBits:)``).
///
/// ## Division of labor
///
/// The discovery wire lives on ``DiscoveryClient``, KeyPackage
/// publishing on ``ChatRepository``, and MLS state + chat fan-out
/// on ``ChatRepository``. This repository owns the authenticate
/// flows (signup / login / passkey / the recovery routes) and coordinates the
/// post-auth bootstrap: it claims the username at signup (the auth login
/// handle first, then the discovery name, ADR-0054 S5), names the account
/// by its login handle at sign-in and recovery (``LoginName``; no discovery
/// lookup before a session), uploads the
/// identity bundle via ``DiscoveryClient`` (signup, sign-in,
/// recover), and drives the
/// backup restore + ``ChatRepository/bootstrap(secrets:)``
/// hand-off, but does not itself own the discovery or MLS state.
@MainActor
public final class AuthRepository: ObservableObject {

    // MARK: - Published state

    /// Discriminated union driving the UI router. Updated
    /// synchronously from the main actor — SwiftUI re-renders are
    /// scheduled atomically with the assignment.
    ///
    /// ADR-0054 S5: every landing on `.session` also re-binds the account's
    /// login handle, once per `(session, username)` per process
    /// (``ensureLoginHandleBound(username:sessionToken:context:)``). Every
    /// site that lands a session installs ``identitySecrets`` first and then
    /// sets this, so the session token is in hand here - and this one hook
    /// covers the relaunch restore, passkey, pairing, the another-device
    /// confirmation and the phrase completion alike, none of which names the
    /// account by its handle.
    @Published public private(set) var state: AuthState = .loading {
        didSet {
            if case .session(let account) = state, let token = identitySecrets?.sessionToken {
                ensureLoginHandleBound(username: account.username, sessionToken: token, context: "session")
            }
        }
    }

    /// ADR-0065 section 3.5 — this device's keys are not the account's keys
    /// any more: the keys were reset on another device (the automatic wipe
    /// runs), or the published identity differs at the same generation (the
    /// wipe runs on the tap). Replaces the whole app while set; `nil`
    /// otherwise. Cleared by "Sign in" on the screen.
    @Published public private(set) var keysSignOut: KeysSignOutState?

    /// The username the signed-out screen names (`@name`), captured when
    /// ``keysSignOut`` is set: the session record it would read is gone
    /// once the wipe ran. Empty for a guest account.
    public private(set) var keysSignOutUsername: String = ""

    /// UX 15.3 — the keys were reset on THIS device in this session: the
    /// Devices and Passkeys screens show their one-session note. In memory
    /// only.
    @Published public private(set) var keysResetThisSession: Bool = false

    /// ADR-0065 section 3.3 — the refresh credential was refused as invalid:
    /// the device landed logged out and the sign-in screen says why. Cleared
    /// by the next sign-in attempt.
    @Published public private(set) var sessionExpiredNotice: Bool = false

    /// Why the restore prompt is showing after a passkey sign-in, or `nil`:
    /// its first line. Cleared when the keys land.
    @Published public private(set) var restoreNotice: RestoreNotice?

    /// A forgot-password route succeeded (three words, an authenticator
    /// code, a passkey, another device), so the "Choose a new password" step
    /// is owed once this device holds the keys. In memory only: a relaunch
    /// drops the offer, and Settings > Security keeps the same form.
    @Published public private(set) var newPasswordOwed: Bool = false

    /// `true` while the account keys were reset on this device but the new
    /// keys are not published yet (the directory upload, this device's
    /// KeyPackage or the DeviceGroup journal is still owed; ADR-0065 section
    /// 2.3, UX 15.4). The phrase screen then shows an error and "Try again"
    /// instead of the words; ``retryFreshPublish()`` clears it.
    @Published public private(set) var freshPublishPending: Bool = false

    /// `true` when the owed publish may not run until the person signs in
    /// again: the stored attestation is older than the 30-day bound
    /// (ADR-0065 section 4.2, UX 15.4).
    @Published public private(set) var freshPublishNeedsSignIn: Bool = false

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
    /// **Also populated on a relaunch** (``rehydrate(nowMs:)``): the
    /// AccountIdentity seed and the session token are read back from
    /// the Keychain (``SessionRestore/decide(marker:persistence:nowMs:)``).
    /// The relaunch lands either a FULL session — ``state`` is
    /// `.session` AND this is non-nil, set together — or the
    /// logged-out state; it never lands `.session` with `nil` secrets.
    /// Operations that need to sign on behalf of the user still
    /// guard on `nil` defensively.
    ///
    /// Every change is mirrored into ``AmbientSessionToken`` (the bearer
    /// source a few repositories read) and into ``AnonTokenPool/shared``
    /// (ADR-0054 S3): the pool draws anonymous tokens with this session and
    /// holds the bound account's only; logout and an account switch drop
    /// them from memory and keep them stored for that account's next
    /// sign-in (Forget device / Delete account delete them through
    /// ``AccountLocalDataWipe``). It also binds the
    /// account the contact list and its identity pins are read for
    /// (``AcceptedContactsStore``, ``PeerPinStore``): no session, no contacts.
    @Published public private(set) var identitySecrets: IdentitySecrets? {
        didSet {
            AcceptedContactsStore.bind(accountIdB64: identitySecrets.map { base64Encode($0.accountId) })
            AmbientSessionToken.shared.set(identitySecrets?.sessionToken)
            AnonTokenPool.shared.configure(
                accountKey: identitySecrets.map { base64Encode($0.accountId) },
                sessionToken: identitySecrets?.sessionToken
            )
        }
    }

    // MARK: - Wiring

    private let sessionStore: SessionStore
    private let authClientFactory: () -> AuthClient
    /// Keychain seam for the relaunch restore + the session-token
    /// persistence that feeds it. Injectable so the restore can be driven
    /// by a fake in tests.
    private let persistence: SessionPersistence

    /// ADR-0017 fresh-device fail-safe. When a password / passkey
    /// auth succeeds but no AccountIdentity (phrase) seed is cached on
    /// this device, everything needed to finish the session is stashed
    /// here and ``state`` flips to ``AuthState/needsRecoveryPhrase(_:)``.
    /// ``provideRecoveryPhrase(mnemonicPhrase:)`` reads this back.
    private var pendingRecovery: PendingRecovery?

    /// The fresh phrase + completion context, parked between
    /// ``mintFreshIdentity()`` (or a guest signup) and
    /// ``confirmFreshPhraseSaved()``.
    private var pendingFreshPhrase: PendingFreshPhrase?

    /// The username directory the signup claim and the identity publish go
    /// through. Injectable so both can be driven by a fake in tests.
    private let accountDirectory: AccountDirectory

    /// The discovery reads of the identity-seed verification and of the
    /// ADR-0056 typed-name check. Injectable for tests.
    private let restoreDirectory: DeviceRestoreDirectory

    /// Which `(session, username)` pairs already had their login handle
    /// bound in this process. ``LoginHandleBindLedger/shared`` in the app.
    private let loginHandleBinds: LoginHandleBindLedger

    /// ADR-0042 (amended 2026-09-30) — the `(device pseudonym, account)`
    /// binding the background push paths gate every wake on. Log out and
    /// Forget device clear it before anything else they do.
    private let pushBinding: PushAccountBindingStore

    /// Removes this app's delivered notifications. Log out and Forget device
    /// call it, so nothing an account received stays on the lock screen after
    /// it signs out. Injectable for tests.
    private let removeDeliveredNotifications: () -> Void

    /// Runs the passkey ceremony: discoverable when the argument is `nil`,
    /// else limited to that account's passkeys (URL-safe base64 of the id).
    /// Asks for the PRF evaluation that protects the keys. Injectable so
    /// the flows around it can be driven by a test without a system sheet.
    public typealias PasskeyCeremony = @MainActor (_ accountIdB64Url: String?) async throws -> PasskeyLoginResult

    private let passkeyCeremony: PasskeyCeremony

    /// The system-sheet ceremony against the auth service.
    public nonisolated static let livePasskeyCeremony: PasskeyCeremony = { accountIdB64Url in
        try await WebauthnClient().loginWithPasskey(
            accountIdB64: accountIdB64Url, prfSalt: PasskeyVault.prfSalt)
    }

    /// The owed markers of a fresh identity (``RecoveryMarkers``).
    private let markers: RecoveryMarkers

    /// ADR-0065 section 3.3 — the one token provider every REST client's
    /// request passes through. The repository installs the pair on every
    /// landing, clears it on sign-out, and answers its three callbacks.
    private let tokenProvider: SessionTokenProvider

    /// The server step of a key reset (`sessionToken`, the reset grant,
    /// `seed`, the new account key):
    /// ``AuthClient/resetAccountKeys(sessionToken:resetGrant:seed:newSignaturePubkey:)``
    /// in the app; injectable because the OPAQUE exchange cannot be
    /// answered by a scripted server.
    typealias KeyResetOperation = (
        _ sessionToken: String, _ resetGrant: String, _ seed: Data, _ newSignaturePubkey: Data
    ) async throws -> KeyResetResult

    private let keyResetOperation: KeyResetOperation

    /// The password login the Reset account keys screen runs to obtain a
    /// reset grant (`handle`, `password`): ``AuthClient/login(handle:password:)``
    /// in the app; injectable for the same reason as ``KeyResetOperation``.
    typealias ResetLoginOperation = (_ handle: Data, _ password: Data) async throws -> LoginResult

    private let resetLoginOperation: ResetLoginOperation

    /// The reset grant of the last password sign-in (ADR-0065, amended
    /// 2026-10-04). In memory only: never persisted, never logged.
    private var resetGrants = ResetGrantHolder()

    /// The clock the reset grant's expiry is read against.
    private let grantNowMs: () -> Int64

    /// The attestation check against the pinned attest key.
    private let attestationVerifier: KeyResetAttestationVerifying

    /// The two publish steps of a reset that live outside this repository
    /// (ADR-0065 section 2.3 steps 4 and 5). The composition root wires the
    /// production ones (``AppServices``); a test injects recorders.
    struct KeyResetHooks {
        /// Re-publish this device's KeyPackage with a binding signed by the
        /// NEW account key. Throws when it did not land.
        var republishOwnKeyPackage: @MainActor (IdentitySecrets) async throws -> Void
        /// Re-found the DeviceGroup and re-root its journal with the
        /// attestation. Throws when the reset did not land.
        var refoundDeviceGroup: @MainActor (
            _ accountIdBytes: Data, _ accountIdB64: String, _ attestation: Data
        ) async throws -> Void

        /// The defaults: the device binding is re-minted under the new
        /// account key so the chat bootstrap's republish carries it (the
        /// upload itself needs the chat repository, which the composition
        /// root adds), and the DS reset runs over a live client.
        static let production = KeyResetHooks(
            republishOwnKeyPackage: { secrets in
                let account = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
                guard ChatRepository.installDeviceIdentity(
                    accountIdB64: base64Encode(secrets.accountId), account: account) != nil else {
                    throw KeyResetPublishFailed(
                        step: KeyResetPublish.Step.keyPackage.rawValue,
                        underlying: AuthClientError.argument("device binding could not be re-minted"))
                }
            },
            refoundDeviceGroup: { accountIdBytes, accountIdB64, attestation in
                try await DeviceGroupDsAccess.refoundAfterKeyReset(
                    accountIdBytes: accountIdBytes,
                    accountIdB64: accountIdB64,
                    attestation: attestation,
                    ds: DsClient())
            }
        )
    }

    var keyResetHooks: KeyResetHooks

    /// The full forget-device wipe the signed-out screen runs by itself
    /// (ADR-0065 section 3.5): every local store of the account, then the
    /// session ends. The composition root replaces it with
    /// ``AppServices/wipeAccountLocalData(accountIdB64:)``, which also lets
    /// the runtimes go first; the default wipes the stores and ends the
    /// session here. Nothing is published first and no call needs the
    /// superseded token.
    var wipeAfterKeyReset: @MainActor (_ accountIdB64: String) async -> Void

    /// The live `UNUserNotificationCenter` removal.
    nonisolated static func removeAllDeliveredNotifications() {
        UNUserNotificationCenter.current().removeAllDeliveredNotifications()
    }

    public convenience init(
        sessionStore: SessionStore = SessionStore(),
        authClientFactory: @escaping () -> AuthClient = { AuthClient() },
        persistence: SessionPersistence = KeychainSessionPersistence(),
        nowMs: () -> Int64 = SessionRestore.nowMs
    ) {
        self.init(
            sessionStore: sessionStore,
            authClientFactory: authClientFactory,
            persistence: persistence,
            nowMs: nowMs,
            accountDirectory: DiscoveryAccountDirectory()
        )
    }

    init(
        sessionStore: SessionStore,
        authClientFactory: @escaping () -> AuthClient,
        persistence: SessionPersistence,
        nowMs: () -> Int64,
        accountDirectory: AccountDirectory,
        loginHandleBinds: LoginHandleBindLedger = .shared,
        restoreDirectory: DeviceRestoreDirectory = DiscoveryDeviceRestoreDirectory(),
        pushBinding: PushAccountBindingStore = .live,
        removeDeliveredNotifications: @escaping () -> Void = AuthRepository.removeAllDeliveredNotifications,
        passkeyCeremony: @escaping PasskeyCeremony = AuthRepository.livePasskeyCeremony,
        markers: RecoveryMarkers = RecoveryMarkers(),
        tokenProvider: SessionTokenProvider = .shared,
        keyResetOperation: KeyResetOperation? = nil,
        resetLoginOperation: ResetLoginOperation? = nil,
        grantNowMs: @escaping () -> Int64 = SessionRestore.nowMs,
        attestationVerifier: KeyResetAttestationVerifying = PinnedKeyResetAttestationVerifier(),
        keyResetHooks: KeyResetHooks = .production,
        wipeAfterKeyReset: (@MainActor (String) async -> Void)? = nil
    ) {
        self.sessionStore = sessionStore
        self.authClientFactory = authClientFactory
        self.persistence = persistence
        self.accountDirectory = accountDirectory
        self.restoreDirectory = restoreDirectory
        self.loginHandleBinds = loginHandleBinds
        self.pushBinding = pushBinding
        self.removeDeliveredNotifications = removeDeliveredNotifications
        self.passkeyCeremony = passkeyCeremony
        self.markers = markers
        self.tokenProvider = tokenProvider
        self.keyResetOperation = keyResetOperation ?? { sessionToken, resetGrant, seed, newKey in
            try await authClientFactory().resetAccountKeys(
                sessionToken: sessionToken, resetGrant: resetGrant, seed: seed, newSignaturePubkey: newKey)
        }
        self.resetLoginOperation = resetLoginOperation ?? { handle, password in
            try await authClientFactory().login(handle: handle, password: password)
        }
        self.grantNowMs = grantNowMs
        self.attestationVerifier = attestationVerifier
        self.keyResetHooks = keyResetHooks
        self.wipeAfterKeyReset = wipeAfterKeyReset ?? { _ in }
        if wipeAfterKeyReset == nil {
            self.wipeAfterKeyReset = { [weak self] accountIdB64 in
                AccountLocalDataWipe.wipe(accountIdB64: accountIdB64)
                self?.endSessionAfterLocalWipe()
            }
        }
        wireTokenProvider()
        rehydrate(nowMs: nowMs())
    }

    /// The provider's refresher and its three callbacks (ADR-0065 section
    /// 3.3). Each callback hops to the main actor and is honoured only for
    /// the account still signed in.
    private func wireTokenProvider() {
        let factory = authClientFactory
        tokenProvider.refresher = { refreshToken in
            try await factory().refreshSession(refreshToken: refreshToken)
        }
        tokenProvider.onPairRefreshed = { [weak self] pair, accountIdB64 in
            Task { @MainActor in self?.sessionPairRefreshed(pair, accountIdB64: accountIdB64) }
        }
        tokenProvider.onSuperseded = { [weak self] accountIdB64 in
            Task { @MainActor in self?.handleKeysSuperseded(accountIdB64: accountIdB64) }
        }
        tokenProvider.onRefreshInvalid = { [weak self] accountIdB64 in
            Task { @MainActor in self?.handleRefreshInvalid(accountIdB64: accountIdB64) }
        }
    }

    // MARK: - Lifecycle

    /// Inspect persisted state and resolve the launch state. The decision
    /// itself is ``SessionRestore/decide(marker:persistence:nowMs:)``; this
    /// applies it.
    ///
    ///   * A marker + device key + AccountIdentity seed + unexpired session
    ///     token → ``AuthState/session(_:)`` with ``identitySecrets``
    ///     restored in the same step, so the chat bootstrap (keyed on the
    ///     secrets) runs exactly as it does after a login.
    ///   * Anything missing, undecodable or expired → logged out
    ///     (``AuthState/anon(cachedUsername:)``), never a `.session` with no
    ///     secrets behind it. An expired token is also deleted.
    ///
    /// Trade-off: the relaunch barrier is the device key + the Keychain item
    /// class, the same one the NSE already relies on to read the seed
    /// (ADR-0041). See ``SessionRestore/decide(marker:persistence:nowMs:)``.
    private func rehydrate(nowMs: Int64) {
        let marker = sessionStore.load()
        switch SessionRestore.decide(marker: marker, persistence: persistence, nowMs: nowMs) {
        case .anon(let cachedUsername):
            identitySecrets = nil
            state = .anon(cachedUsername: cachedUsername)
        case .anonDiscardingToken(let cachedUsername):
            if let marker {
                persistence.deleteSessionToken(accountIdB64: marker.accountIdB64)
            }
            identitySecrets = nil
            state = .anon(cachedUsername: cachedUsername)
        case .session(let account, let secrets):
            // The pair is installed before anything can make a bearer call.
            tokenProvider.install(secrets.session, accountIdB64: account.accountIdB64)
            // A fresh identity whose phrase was never confirmed: the phrase
            // screen comes before the session.
            if parkOwedFreshPhrase(
                accountIdBytes: secrets.accountId,
                accountIdB64: account.accountIdB64,
                username: account.username,
                seed: secrets.accountIdentitySeed,
                session: secrets.session
            ) {
                return
            }
            identitySecrets = secrets
            state = .session(account)
            // ADR-0065 sections 3.3 and 4.2 — the silent restore refreshes
            // instead of reusing the persisted access token (a reset
            // elsewhere answers `key-generation-superseded` here, at
            // launch), then checks the published identity before any
            // upload. No upload on a silent restore.
            Task { [weak self] in
                guard let self else { return }
                if case .superseded = await self.tokenProvider.refresh() { return }
                await self.checkPublishedIdentity(
                    accountIdBytes: secrets.accountId,
                    accountIdentitySeed: secrets.accountIdentitySeed,
                    uploads: false
                )
            }
            settleOwedKeyResetPublish(
                accountIdB64: account.accountIdB64,
                accountIdBytes: secrets.accountId,
                accountIdentitySeed: secrets.accountIdentitySeed,
                requireFresh: true
            )
        }
    }

    /// A refresh installed a new pair: the live secrets and the persisted
    /// token follow it, for the account still signed in.
    private func sessionPairRefreshed(_ pair: SessionPair, accountIdB64: String) {
        guard let secrets = identitySecrets, base64Encode(secrets.accountId) == accountIdB64 else {
            // A parked state (keys not here, or the fresh phrase) holds its
            // own copy of the pair; the provider serves the current token on
            // every request regardless, so only the persisted copy moves.
            do {
                try persistence.putSessionToken(PersistedSessionToken(pair: pair), accountIdB64: accountIdB64)
            } catch {
                NSLog("session-token persist (refresh) failed: \(error)")
            }
            return
        }
        adoptIdentitySecrets(
            IdentitySecrets(
                accountId: secrets.accountId,
                exportKey: secrets.exportKey,
                accountIdentitySeed: secrets.accountIdentitySeed,
                session: pair
            ),
            context: "refresh"
        )
    }

    /// The refresh credential was refused as invalid (ADR-0065 section 3.3):
    /// the device goes to the sign-in screen with the session-expired line.
    /// Nothing is wiped: the keys are still this account's.
    private func handleRefreshInvalid(accountIdB64: String) {
        guard signedInAccountIdB64 == accountIdB64 else { return }
        NSLog("[auth] the session refresh was refused as invalid; signing out without a wipe")
        logout()
        sessionExpiredNotice = true
    }

    /// The next sign-in attempt clears the session-expired line.
    public func clearSessionExpiredNotice() {
        sessionExpiredNotice = false
    }

    /// Install the live session's secrets AND persist the session token so
    /// the next launch can restore the session (``rehydrate(nowMs:)``).
    ///
    /// Every site that lands a session goes through here, so there is one
    /// place where "the session is live" and "the next launch can find it"
    /// are kept together. The persist is best-effort: a Keychain failure
    /// costs the relaunch restore (the next launch lands logged out), never
    /// the current session.
    private func adoptIdentitySecrets(_ secrets: IdentitySecrets, context: String) {
        identitySecrets = secrets
        adoptSessionPair(secrets.session, accountIdB64: base64Encode(secrets.accountId), context: context)
    }

    /// Install a session pair in the token provider and persist it, so every
    /// bearer request from now on carries it (refreshed ahead of expiry) and
    /// the next launch can restore the session. Also for the states that
    /// hold a pair before ``identitySecrets`` exists (a sign-in parked
    /// without keys, the fresh phrase). Best-effort persist, as above.
    private func adoptSessionPair(_ pair: SessionPair, accountIdB64: String, context: String) {
        tokenProvider.install(pair, accountIdB64: accountIdB64)
        do {
            try persistence.putSessionToken(PersistedSessionToken(pair: pair), accountIdB64: accountIdB64)
        } catch {
            NSLog("session-token persist (\(context)) failed: \(error)")
        }
    }

    // MARK: - Flows

    /// Drive the full OPAQUE-PAKE signup flow.
    ///
    /// Steps:
    ///   1. Validate the username (non-empty after the discovery
    ///      normalisation — `@` alone is empty).
    ///   2. Call ``AuthClient/signup(password:enrolRecovery:)`` with
    ///      `enrolRecovery = true`.
    ///   3. ``completeSignup(result:username:)`` — claim the username in
    ///      discovery, persist the on-device material, publish the identity
    ///      bundle, land the session.
    ///   4. Return the 12-word recovery phrase for the UI to display
    ///      **once**.
    ///
    /// ADR-0054 S4: cancellable (the task that runs it) until
    /// `onCommitting`, which fires right before `/signup/finish`. From there
    /// the rest runs to completion regardless of cancellation - an account
    /// the finish created must not be left without its username claim and
    /// device material because the user tapped Cancel a moment too late.
    public func signup(
        username: String,
        password: Data,
        onCommitting: (@MainActor () -> Void)? = nil
    ) async throws -> SignupOutcome {
        // ADR-0048 §4 — refuse a name that fails the username rule BEFORE the
        // OPAQUE registration, so a refused name never leaves an unreachable
        // account behind. The claim sends the canonical form.
        let canonical = try requireValidUsername(
            username,
            emptyError: AuthClientError.argument("username must be non-empty")
        )
        let client = authClientFactory()
        let result = try await client.signup(
            password: password, enrolRecovery: true, onCommitting: onCommitting)
        // A fresh unstructured task does not inherit the caller's cancellation.
        return try await Task { @MainActor in
            try await self.completeSignup(result: result, username: canonical)
        }.value
    }

    /// Everything signup does once the OPAQUE registration committed.
    ///
    /// Mirror of the post-registration half of the web `signup`
    /// (`apps/web/src/auth/AuthContext.tsx`). Split out so the ordering below
    /// can be driven by a test without an OPAQUE server.
    ///
    /// Order, and why:
    ///
    ///   1. Derive the AccountIdentity seed from the recovery phrase. Pure; a
    ///      missing phrase fails here, before any server-visible side effect
    ///      beyond the registration itself.
    ///   2. Claim the username (``claimSignupUsername(accountId:username:sessionToken:directory:binder:derive:)``):
    ///      the login handle at auth first, then the name in discovery (ADR-0054
    ///      S5 claim order). BEFORE anything touches the device: a taken name
    ///      (409 at either) or any other
    ///      claim failure is thrown to the form with the device untouched, so
    ///      the user is never signed in to an account nobody can find by name.
    ///      The server-side account the registration created is left behind,
    ///      unreachable — the web client accepts the same cost, since the claim
    ///      needs that account's id and session token to exist.
    ///   3. Persist the `(accountId, username)` marker with the CANONICAL name
    ///      the server bound, then the device key. The marker goes first so a
    ///      crash between the two leaves a recoverable "marker but no key"
    ///      state rather than a key with no record of whose it is.
    ///   4. Publish the identity bundle (signature + X25519 public keys, the
    ///      genesis `leaf_auth`, the ADR-0036 attestation when mintable). A
    ///      contact request and a fresh-device phrase check both need it.
    ///      Fail-loud, BEFORE the session lands — the web decision: an account
    ///      that is signed in but cannot be contacted is the half-broken state
    ///      this step exists to prevent. The marker and seed are already on the
    ///      device, so a later password sign-in re-publishes
    ///      (``finishCanonicalSession(accountIdentitySeed:accountIdBytes:exportKey:session:username:)``).
    ///      The KeyPackage is NOT published here — it must come from the
    ///      long-lived identity (``ChatRepository/republishKeyPackage()``).
    ///   5. Land the session.
    func completeSignup(
        result: SignupResult,
        username: String
    ) async throws -> SignupOutcome {
        // ADR-0017: the canonical AccountIdentity is derived from the stable
        // BIP39 recovery-phrase seed, NOT the password export key — so a
        // future recover / password change preserves the identity.
        // `enrolRecovery = true` always enrols recovery; a missing phrase is
        // a code-path bypass we must not navigate the user past.
        guard let phrase = result.recoveryPhrase else {
            throw AuthClientError.argument(
                "internal: signup completed without a recovery phrase"
            )
        }
        let accountIdentitySeed = try RecoveryMnemonic
            .fromPhrase(phrase: phrase)
            .toSeed()

        // Claim the username before any on-device state exists.
        // ADR-0054 S3 — the claim's OPRF step spends a `discovery-lookup`
        // token, drawn with the session signup just minted: the session has
        // not landed yet, so the pool is bound to it here, and put back the
        // way ``identitySecrets`` has it if the claim fails.
        AnonTokenPool.shared.configure(
            accountKey: base64Encode(result.accountId), sessionToken: result.sessionToken)
        tokenProvider.install(result.session, accountIdB64: base64Encode(result.accountId))
        let boundUsername: String
        do {
            boundUsername = try await claimSignupUsername(
                accountId: result.accountId,
                username: username,
                sessionToken: result.sessionToken,
                directory: accountDirectory,
                binder: authClientFactory()
            )
        } catch {
            // The account never lands on this device: nothing it drew stays.
            AnonTokenPool.shared.forget(accountKey: base64Encode(result.accountId))
            AnonTokenPool.shared.configure(
                accountKey: identitySecrets.map { base64Encode($0.accountId) },
                sessionToken: identitySecrets?.sessionToken)
            if let secrets = identitySecrets {
                tokenProvider.install(secrets.session, accountIdB64: base64Encode(secrets.accountId))
            } else {
                tokenProvider.clear()
            }
            throw error
        }
        // The claim just bound the handle with this session: the landing
        // below must not bind it a second time.
        _ = loginHandleBinds.claim(sessionToken: result.sessionToken, canonical: boundUsername)

        let accountIdB64 = base64Encode(result.accountId)
        sessionStore.save(
            PersistedSession(
                accountIdB64: accountIdB64,
                username: boundUsername
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
        // phrase-seeded identity); used as the MLS leaf in
        // the user's DeviceGroup (2a3a) once the add-device flow
        // lands. Best-effort.
        do {
            _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                accountIdB64: accountIdB64
            )
        } catch {
            NSLog("device-identity ensure (signup) failed: \(error)")
        }
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

        // The FIRST-EVER bundle publish, with the genesis leaf self-signature
        // (G-30 #2, ADR-0019) so the transparency append accepts the new
        // binding. Fail-loud — see the ordering note above.
        try await publishFreshIdentityBundle(
            accountId: result.accountId,
            sessionToken: result.sessionToken,
            seed: accountIdentitySeed
        )
        // The device holds the keys, so it holds the phrase (the local
        // three-word check, the hand-over to a new device, "Show recovery
        // phrase").
        DevicePhrase.store(phrase, accountIdB64: accountIdB64, context: "signup")

        // Stash the export key + phrase seed + session token for
        // ContactsClient / ChatRepository / DiscoveryClient. The bytes
        // survive until logout / forgetDevice clears the publisher.
        // ADR-0017: the canonical identity downstream callers rebuild
        // reads `accountIdentitySeed`.
        adoptIdentitySecrets(
            IdentitySecrets(
                accountId: result.accountId,
                exportKey: result.exportKey,
                accountIdentitySeed: accountIdentitySeed,
                session: result.session
            ),
            context: "signup"
        )
        state = .session(
            SessionAccount(
                accountIdB64: accountIdB64,
                username: boundUsername
            )
        )
        // Phase 2a1c — post-signup backup bootstrap: posts a baseline
        // /backup envelope (and re-writes the phrase store). Best-effort end-to-end; errors are logged
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
        Task { await self.subscribePushBestEffort(context: "signup", accountIdB64: accountIdB64) }
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
    public func signupGuest(
        onCommitting: (@MainActor () -> Void)? = nil
    ) async throws -> SignupOutcome {
        // A 32-byte random password the user never sees — the guest HAS a real,
        // random password, so the normal signup derivations apply. `SystemRandom-
        // NumberGenerator` is the OS CSPRNG on Apple platforms.
        var rng = SystemRandomNumberGenerator()
        let randomBytes = (0..<32).map { _ in UInt8.random(in: UInt8.min...UInt8.max, using: &rng) }
        let randomPassword = Data(Data(randomBytes).base64EncodedString().utf8)

        // The SAME PoW-gated OPAQUE registration `signup` uses — no bypass
        // (ADR-0054 S4: the one heavy proof, cancellable until `onCommitting`).
        let client = authClientFactory()
        let result = try await client.signup(
            password: randomPassword, enrolRecovery: true, onCommitting: onCommitting)
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

        tokenProvider.install(result.session, accountIdB64: accountIdB64)

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
        // identity (a guest has no attested reset to retry from, unlike
        // `mintFreshIdentity`).
        try await publishFreshIdentityBundle(
            accountId: result.accountId,
            sessionToken: result.sessionToken,
            seed: accountIdentitySeed
        )

        DevicePhrase.store(phrase, accountIdB64: accountIdB64, context: "signup-guest")

        // Park the completion context + flip to `.saveFreshPhrase` so the UI
        // shows the recovery phrase before the session lands (mirroring
        // `mintFreshIdentity`); `confirmFreshPhraseSaved` builds the canonical
        // session from the stash. Username EMPTY (unclaimed).
        adoptSessionPair(result.session, accountIdB64: accountIdB64, context: "signupGuest")
        pendingFreshPhrase = PendingFreshPhrase(
            seed: accountIdentitySeed,
            accountIdBytes: result.accountId,
            accountIdB64: accountIdB64,
            session: result.session,
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
    /// Mirror of `apps/web/src/auth/AuthContext.tsx` `login`. Steps:
    ///
    ///   1. Turn the typed username into its login handle (``LoginName``,
    ///      ADR-0054 S5). No discovery lookup: the account is named by the
    ///      handle, and a name nobody holds fails as `invalid credentials`,
    ///      exactly like a wrong password.
    ///   2. Call ``AuthClient/login(handle:password:)``. The account id comes
    ///      back in the finish response.
    ///   3. Re-derive the device root key from the OPAQUE export
    ///      key. The derivation is deterministic for the same
    ///      password, so re-running it on every login produces the
    ///      same bytes the original signup wrote — the
    ///      ``DeviceKeyStore`` upsert is harmless.
    ///   4. Save the ``SessionStore`` marker for the account the finish
    ///      named, with the canonical username.
    ///   5. Land the session from the cached AccountIdentity seed, or — with
    ///      no seed on this device — park in ``AuthState/needsRecoveryPhrase(_:)``
    ///      (ADR-0017).
    ///   6. Re-bind the handle (best-effort, idempotent) so a device that
    ///      bound before a rename stays in step.
    public func login(
        username: String,
        password: Data
    ) async throws {
        guard !password.isEmpty else {
            throw AuthClientError.argument("password must be non-empty")
        }
        let name = try LoginName.make(
            username,
            emptyError: AuthClientError.argument("username must be non-empty")
        )

        let client = authClientFactory()
        let result = try await client.login(
            handle: name.handle,
            password: password
        )
        let accountIdBytes = result.accountId
        let accountIdB64 = base64Encode(accountIdBytes)
        // The reset grant of this password sign-in: in memory only.
        resetGrants.put(result.resetGrant, accountIdB64: accountIdB64)

        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: accountIdB64
        )
        try DeviceKeyStore.putDeviceKey(
            accountIdB64: accountIdB64,
            rootKey: deviceRootKey
        )
        // Phase 2a2c2 — refresh the cached export key under the
        // Keychain. Best-effort.
        do {
            try ExportKeyStore.putExportKey(
                accountIdB64: accountIdB64,
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
                accountIdB64: accountIdB64
            )
        } catch {
            NSLog("device-identity ensure (login) failed: \(error)")
        }
        // Idempotent upsert for a returning device; the first record of
        // this account on a fresh one.
        sessionStore.save(
            PersistedSession(accountIdB64: accountIdB64, username: name.canonical)
        )

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
            accountIdB64: accountIdB64,
            context: "login"
        )
        finishOrPromptForRecovery(
            accountIdentitySeed: accountIdentitySeed,
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            exportKey: result.exportKey,
            session: result.session,
            username: name.canonical
        )
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "login")
        // 1.20c — best-effort push subscribe.
        Task { await self.subscribePushBestEffort(context: "login", accountIdB64: accountIdB64) }
    }

    /// ADR-0054 S5 — re-`PUT` the login handle of `username` with
    /// `sessionToken`, once per `(session, username)` per process.
    ///
    /// Runs on every session landing (``state``) and after a sign-in or
    /// recovery by name (which can bind before a fresh device has its phrase
    /// and so before `.session`). Idempotent server-side for the handle the
    /// account holds; it binds an account whose session predates S5, or was
    /// established on a path that never names the handle, so a later sign-in
    /// by name works.
    ///
    ///   * A guest (empty username) has no handle: nothing is sent.
    ///   * A name the handle derivation refuses: nothing is sent, logged.
    ///   * Fire-and-forget: a failure never touches the session and shows
    ///     the user nothing. `409` and any other status is logged by status
    ///     only; `503 login-by-name-disabled` is not logged at all.
    func ensureLoginHandleBound(username: String, sessionToken: String, context: String) {
        guard !username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !sessionToken.isEmpty else { return }
        let name: LoginName
        do {
            name = try LoginName.make(
                username, emptyError: AuthClientError.argument("username must be non-empty"))
        } catch {
            NSLog("[auth] login handle re-bind (\(context)) skipped: the username derives no handle")
            return
        }
        ensureLoginHandleBound(name: name, sessionToken: sessionToken, context: context)
    }

    /// ``ensureLoginHandleBound(username:sessionToken:context:)`` for a name
    /// already turned into its handle.
    private func ensureLoginHandleBound(name: LoginName, sessionToken: String, context: String) {
        guard !sessionToken.isEmpty,
              loginHandleBinds.claim(sessionToken: sessionToken, canonical: name.canonical) else { return }
        let client = authClientFactory()
        let handle = name.handle
        Task {
            do {
                try await client.bindLoginHandle(handle: handle, sessionToken: sessionToken)
            } catch let error as AuthClientError {
                switch error {
                case .http(let status, _, _):
                    if AuthRateLimit.isLoginByNameDisabled(error) { return }
                    NSLog("[auth] login handle re-bind (\(context)) failed: status \(status)")
                case .network:
                    NSLog("[auth] login handle re-bind (\(context)) failed: network")
                case .argument:
                    NSLog("[auth] login handle re-bind (\(context)) failed: argument")
                }
            } catch {
                NSLog("[auth] login handle re-bind (\(context)) failed")
            }
        }
    }

    /// Sign in with a passkey.
    ///
    /// The ceremony is discoverable: it needs no account cached on this
    /// device and no typed name. The assertion grants access; what happens
    /// next is the shared access-only landing
    /// (``landAfterAccessRecovery(username:accountIdBytes:accountIdB64:session:context:pushContext:restoreKeys:)``),
    /// with one addition on a device without the keys: this passkey is
    /// asked for them before the restore prompt is shown.
    ///
    ///   * A device that holds the account's keys lands signed in.
    ///   * A device without them restores the keys when this passkey
    ///     carries them: its PRF output opens the blob stored for it and
    ///     the phrase inside matches the account's published identity
    ///     (fail-closed, ``provideRecoveryPhrase(mnemonicPhrase:)``).
    ///   * Otherwise the state parks in
    ///     ``AuthState/needsRecoveryPhrase(_:)`` with ``restoreNotice``
    ///     saying why.
    ///
    /// `usernameHint` names the account on a device that has none cached; it
    /// is kept only when discovery resolves it to the signed-in account (a
    /// passkey assertion carries only the account id, and a typed name must
    /// not be saved for an account it does not name). `forgotPassword`
    /// marks the sign-in as a forgot-password route, so the new-password
    /// step is offered once the device holds the keys.
    public func loginWithPasskey(
        usernameHint: String? = nil,
        forgotPassword: Bool = false
    ) async throws {
        let assertion = try await passkeyCeremony(nil)
        // The finish response carries URL-safe base64 without padding; the
        // marker and every store on this device use the standard form.
        guard let accountIdBytes = base64UrlDecodeNoPad(assertion.accountIdB64),
              accountIdBytes.count == accountIdLength else {
            throw AuthClientError.argument("Malformed account_id in passkey assertion.")
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        restoreNotice = nil

        let username: String
        if let cached = sessionStore.load(), cached.accountIdB64 == accountIdB64 {
            username = cached.username
        } else {
            username = await usernameNamingAccount(
                hint: usernameHint,
                accountIdBytes: accountIdBytes,
                sessionToken: assertion.sessionToken
            )
        }
        if forgotPassword { newPasswordOwed = true }

        await landAfterAccessRecovery(
            username: username,
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            session: assertion.session,
            context: "passkey login",
            pushContext: "passkey-login",
            restoreKeys: { [weak self] in
                guard let self else { return false }
                do {
                    let outcome = try await self.keysFromPasskey(
                        assertion, sessionToken: assertion.sessionToken)
                    if outcome == .restored { return true }
                    self.restoreNotice = outcome == .noKeys ? .passkeyNoKeys : .passkeyUnusable
                } catch {
                    // The vault or the directory could not be reached. The
                    // person is signed in; the restore prompt's passkey row
                    // retries.
                    NSLog("[auth] asking the passkey for the keys failed")
                }
                return false
            }
        )
    }

    /// The restore prompt's passkey row: only valid in
    /// ``AuthState/needsRecoveryPhrase(_:)``. Runs the passkey ceremony for
    /// the signed-in account and asks the passkey for the keys.
    /// ``PasskeyVault/KeysOutcome/restored`` lands the session; the other
    /// outcomes leave the prompt up and set ``restoreNotice``.
    func restoreKeysWithPasskey() async throws -> PasskeyVault.KeysOutcome {
        guard let pending = pendingRecovery else {
            throw RestoreNothingPending()
        }
        let assertion = try await passkeyCeremony(base64UrlEncodeNoPad(pending.accountIdBytes))
        guard base64UrlDecodeNoPad(assertion.accountIdB64) == pending.accountIdBytes else {
            throw PasskeyWrongAccount()
        }
        let outcome = try await keysFromPasskey(assertion, sessionToken: assertion.sessionToken)
        switch outcome {
        case .restored: break
        case .noKeys: restoreNotice = .passkeyNoKeys
        case .unusable: restoreNotice = .passkeyUnusable
        }
        return outcome
    }

    /// The session token of the sign-in parked in
    /// ``AuthState/needsRecoveryPhrase(_:)`` (the device is signed in, its
    /// keys are not here yet), else `nil`.
    public var pendingSessionToken: String? { pendingRecovery?.sessionToken }

    /// How many of the parked account's passkeys carry keys (`GET
    /// /vault/passkeys` with the parked session token), or `nil` when no
    /// sign-in is parked or the count could not be read. The restore prompt
    /// hides its passkey row when this is 0.
    public func passkeysWithKeysForPendingRestore() async -> Int? {
        guard let token = pendingRecovery?.sessionToken else { return nil }
        do {
            return try await authClientFactory().passkeyVaultList(sessionToken: token).count
        } catch {
            return nil
        }
    }

    /// Ask a passkey for the account's keys, while a recovery context is
    /// parked. The PRF output opens the blob stored for the credential into
    /// the 12-word phrase, and the phrase goes through
    /// ``provideRecoveryPhrase(mnemonicPhrase:)``, whose check against the
    /// published identity is fail-closed. `restored` means the session
    /// landed. The PRF output stays on this device.
    ///
    /// Throws only what is neither "no keys" nor "unusable": the vault fetch
    /// failing, or the published identity being unreachable.
    private func keysFromPasskey(
        _ assertion: PasskeyLoginResult,
        sessionToken: String
    ) async throws -> PasskeyVault.KeysOutcome {
        var blobStored = false
        var phrase: String?
        if let prfOutput = PasskeyVault.usablePrfOutput(assertion.prfOutput) {
            let sealed: Data?
            do {
                sealed = try await authClientFactory().passkeyVaultGet(
                    sessionToken: sessionToken, credentialId: assertion.credentialId)
            } catch AuthClientError.http(let status, _, _) where status == 503 {
                // The server keeps no keys for passkeys.
                sealed = nil
            }
            if let sealed {
                blobStored = true
                do {
                    phrase = try passkeyVaultOpen(prfOutput: prfOutput, sealed: sealed).phrase()
                } catch {
                    NSLog("[auth] the keys stored for this passkey did not open")
                }
            }
        }
        var matched = false
        if let phrase {
            do {
                try await provideRecoveryPhrase(mnemonicPhrase: phrase)
                matched = true
            } catch is PhraseMismatch {
                NSLog("[auth] the keys stored for this passkey are not this account's")
            }
        }
        return PasskeyVault.keysOutcome(
            prfOutput: assertion.prfOutput != nil,
            blobStored: blobStored,
            opened: phrase != nil,
            matched: matched
        )
    }

    /// `hint` in its canonical form when discovery resolves it to
    /// `accountIdBytes`, else the empty name. Runs with the session token of
    /// the sign-in that just succeeded (the lookup spends a token drawn with
    /// it).
    private func usernameNamingAccount(
        hint: String?,
        accountIdBytes: Data,
        sessionToken: String
    ) async -> String {
        let typed = (hint ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        guard !typed.isEmpty,
              let canonical = try? requireValidUsername(
                  typed, emptyError: AuthClientError.argument("empty username")) else {
            return ""
        }
        do {
            let found = try await restoreDirectory.lookupAccount(
                username: canonical, accountId: accountIdBytes, sessionToken: sessionToken)
            return found == accountIdBytes ? canonical : ""
        } catch {
            NSLog("[auth] could not check the typed name against the passkey's account")
            return ""
        }
    }

    // MARK: - Three words

    /// The account "Three words from your phrase" can sign in on this
    /// device, or `nil` when the route is not offered here. A LOCAL fact:
    /// an account is cached on this device with a username, and the device
    /// holds both its recovery phrase and its keys
    /// (``ThreeWords/offers(cachedUsername:phraseOnDevice:keysOnDevice:suspended:)``).
    /// No server is asked, so the answer leaks nothing about any account.
    public func threeWordsAccount() -> SessionAccount? {
        guard let context = threeWordsContext() else { return nil }
        return SessionAccount(accountIdB64: context.accountIdB64, username: context.username)
    }

    private func threeWordsContext() -> (accountIdB64: String, username: String, phrase: String)? {
        guard let cached = sessionStore.load() else { return nil }
        let phrase = DevicePhrase.read(accountIdB64: cached.accountIdB64)
        let keysOnDevice = Self.readCachedAccountIdentitySeed(
            accountIdB64: cached.accountIdB64, context: "three words") != nil
        guard let phrase,
              ThreeWords.offers(
                  cachedUsername: cached.username,
                  phraseOnDevice: true,
                  keysOnDevice: keysOnDevice,
                  suspended: ThreeWordsBudgetStore().isSuspended(accountIdB64: cached.accountIdB64)
              ) else {
            return nil
        }
        return (cached.accountIdB64, cached.username, phrase)
    }

    /// Three words, checked locally like a wallet does. `positions` are the
    /// three word numbers the form drew (`phraseCheckRandomPositions`) and
    /// `words[i]` answers `positions[i]`. The words are compared with the
    /// phrase this device holds; the server takes no part in the check and
    /// stores nothing about it.
    ///
    /// Returns `false` on wrong words (nothing is sent anywhere). On a match
    /// the device signs in with what it already holds: the recovery login
    /// (`/recovery/login/*`) run from the seed of the stored phrase, which
    /// yields a session without replacing the password. It then lands
    /// through the shared access-only landing with the new-password step
    /// owed, and returns `true`.
    ///
    /// The guess budget (five misses, then a 15-minute pause) is the form's
    /// (``ThreeWords``): a client-side gate in front of a credential this
    /// device already holds.
    ///
    /// Throws ``ThreeWordsError/unavailable`` when the route is not offered
    /// here, ``ThreeWordsError/rejected`` when the name answered for another
    /// account, and what the recovery login threw otherwise (a stale phrase
    /// is its `401`).
    public func recoverWithThreeWords(positions: Data, words: [String]) async throws -> Bool {
        guard let context = threeWordsContext() else {
            throw ThreeWordsError.unavailable
        }
        let mnemonic = try RecoveryMnemonic.fromPhrase(phrase: context.phrase)
        // Trims and lower-cases; a wrong or unknown word is `false`.
        guard try phraseCheckWords(mnemonic: mnemonic, positions: positions, words: words) else {
            return false
        }
        var seed = mnemonic.toSeed()
        defer { Self.zeroize(&seed) }

        // The words matched. Sign in with what this device already holds:
        // the recovery login takes the 64-byte seed of the phrase and
        // answers with a session; the password is left as it is (the
        // "Choose a new password" step follows and replaces it).
        let name = try LoginName.make(
            context.username, emptyError: ThreeWordsError.unavailable)
        let result: TotpRecoverResult
        do {
            result = try await authClientFactory().recoveryLogin(handle: name.handle, seed: seed)
        } catch {
            switch ThreeWords.stalePhraseVerdict(failure: ThreeWords.signInFailure(error)) {
            case .keep:
                throw error
            case .suspendThreeWords:
                // The phrase stored here may no longer be the account's.
                // Suspend the route and clear its budget; the phrase, the
                // keys and everything else stay.
                ThreeWordsBudgetStore().suspend(accountIdB64: context.accountIdB64)
                NSLog("[three-words] the sign-in from the stored phrase was refused; the route is suspended on this device")
                throw ThreeWordsError.suspended
            }
        }
        guard base64Encode(result.accountId) == context.accountIdB64 else {
            // The name answered for another account than the one cached
            // here. Adopt nothing.
            throw ThreeWordsError.rejected
        }
        restoreNotice = nil
        newPasswordOwed = true
        await landAfterAccessRecovery(
            username: name.canonical,
            accountIdBytes: result.accountId,
            accountIdB64: context.accountIdB64,
            session: result.session,
            context: "three words",
            pushContext: "three-words"
        )
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "three words")
        return true
    }

    /// The recovery phrase stored on this device for the signed-in account,
    /// or `nil` (Settings > Security > Recovery, "Show recovery phrase").
    /// Read from the device, never from a server. The caller gates the
    /// reveal behind the device's own authentication. Withheld (`nil`)
    /// while a recovery-credential replace is owed (``PhraseReveal``).
    public func devicePhrase() -> String? {
        guard case .session(let account) = state else { return nil }
        let phrase = DevicePhrase.read(accountIdB64: account.accountIdB64)
        guard PhraseReveal.decide(
            replaceOwed: markers.isPublishOwed(accountIdB64: account.accountIdB64),
            phraseStored: phrase != nil
        ) == .show else { return nil }
        return phrase
    }

    /// Whether a key reset's publish is owed for the signed-in account on
    /// this device (``RecoveryMarkers``).
    public var sessionPublishOwed: Bool {
        guard case .session(let account) = state else { return false }
        return markers.isPublishOwed(accountIdB64: account.accountIdB64)
    }

    /// Retry the owed publish of a key reset for the signed-in account with
    /// the attestation this device stored (the same publish a session
    /// landing retries in the background). A no-op when nothing is owed.
    /// Throws what the failing step threw (offline, `429`, `503`); the
    /// record then keeps what landed.
    public func retrySessionKeyResetPublish() async throws {
        guard case .session(let account) = state, let secrets = identitySecrets else {
            throw SessionNotReady()
        }
        try await runOwedKeyResetPublish(
            accountIdB64: account.accountIdB64,
            accountIdBytes: secrets.accountId,
            accountIdentitySeed: secrets.accountIdentitySeed,
            requireFresh: true
        )
    }

    // MARK: - Choose a new password

    /// Whether ``replacePassword(newPassword:)`` can run: a session with a
    /// username on a device that holds the account's keys. A guest (no
    /// username) has no login handle to address the reset by.
    public var canReplacePassword: Bool {
        guard case .session(let account) = state, let secrets = identitySecrets else { return false }
        return !account.username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && secrets.accountIdentitySeed.count == Self.accountIdentitySeedLength
    }

    /// Whether the "Choose a new password" step is owed: a forgot-password
    /// route succeeded and this device holds the keys. The app root renders
    /// the step while this is true.
    public var newPasswordOffered: Bool { newPasswordOwed && canReplacePassword }

    /// "Not now" on the new-password step.
    public func dismissNewPasswordOffer() {
        newPasswordOwed = false
    }

    /// Replace the account's password from a device that holds its keys,
    /// without typing the phrase. `/password/reset/*` is authorised by a
    /// recovery proof made from the 64-byte seed of the phrase; this device
    /// holds that seed, so the reset runs with only the new password typed.
    ///
    /// Deliberately NOT ``recover(mnemonicPhrase:newPassword:username:)``:
    /// that is the fresh-device path, which restores a backup and replaces
    /// local state. Here nothing on the device is re-keyed or wiped: the
    /// stored device key, the chats, the cached seed and the cached phrase
    /// stay; only the session token and the export-key cache are refreshed.
    public func replacePassword(newPassword: Data) async throws {
        guard case .session(let account) = state,
              let secrets = identitySecrets,
              canReplacePassword else {
            throw SessionNotReady()
        }
        guard !newPassword.isEmpty else {
            throw AuthClientError.argument("new password must be non-empty")
        }
        let name = try LoginName.make(account.username, emptyError: SessionNotReady())
        // The client uses the seed as the recovery input; hand it a copy so
        // the in-memory secret is never the buffer that is wiped.
        var seed = Data(secrets.accountIdentitySeed)
        defer { Self.zeroize(&seed) }
        let result = try await authClientFactory().recover(
            handle: name.handle, mnemonicSeed: seed, newPassword: newPassword)
        guard result.accountId == secrets.accountId else {
            // The name's handle answered for another account. The proof was
            // made from this account's seed, so this cannot verify; refuse
            // rather than adopt a session for an account that is not ours.
            throw AuthClientError.argument(
                "internal: the password reset answered for another account")
        }
        do {
            try ExportKeyStore.putExportKey(
                accountIdB64: account.accountIdB64, exportKey: result.exportKey)
        } catch {
            NSLog("export-key persist (replace password) failed: \(error)")
        }
        adoptIdentitySecrets(
            IdentitySecrets(
                accountId: secrets.accountId,
                exportKey: result.exportKey,
                accountIdentitySeed: secrets.accountIdentitySeed,
                session: result.session
            ),
            context: "replacePassword"
        )
        newPasswordOwed = false
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "replace password")
    }

    /// The 64-byte length of the AccountIdentity seed.
    static let accountIdentitySeedLength = 64

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
    /// the old and new account pubkeys are equal. The account is named by
    /// the login handle of `username` (ADR-0054 S5), or of the name cached on
    /// this device when the field is empty; the account id comes back in the
    /// finish response. No discovery lookup runs, so recover works on a
    /// FRESH device given the username + phrase.
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

        // ADR-0054 S5: the typed name, else the one cached on this device,
        // names the account by its login handle.
        let name = try signInName(
            typed: username,
            cached: sessionStore.load().map {
                CachedAccount(accountIdB64: $0.accountIdB64, username: $0.username)
            },
            emptyError: AuthClientError.argument(
                "Enter your username so Lakko can find your account."
            )
        )
        let resolvedUsername = name.canonical

        // Parse the mnemonic up front. `fromPhrase` collapses
        // unknown-word and bad-checksum into one error by design
        // (preserves BIP39's no-probing property).
        let mnemonic = try RecoveryMnemonic.fromPhrase(phrase: trimmedPhrase)
        let seed = mnemonic.toSeed()
        // The canonical form (12 lower-case words, single spaces): what the
        // device stores once the recovery proved the phrase is the account's.
        let canonicalPhrase = mnemonic.phrase()
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
            handle: name.handle,
            mnemonicSeed: seed,
            newPassword: newPassword
        )
        let accountIdBytes = result.accountId
        let accountIdB64 = base64Encode(accountIdBytes)
        tokenProvider.install(result.session, accountIdB64: accountIdB64)

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
        // The recovery proof succeeded with this phrase's seed, so the
        // phrase is the account's: this device holds it from now on.
        DevicePhrase.store(canonicalPhrase, accountIdB64: accountIdB64, context: "recover")
        // The password was replaced on this path: no new-password step.
        newPasswordOwed = false
        restoreNotice = nil

        // G-4 (ADR-0016) emit path — now DORMANT per ADR-0017. With a
        // phrase-seeded AccountIdentity, a routine recover does NOT
        // rotate the account key: the phrase is unchanged, so the new
        // account signature pubkey EQUALS the old one. We re-derive the
        // NEW canonical identity from the (unchanged) phrase seed,
        // compare it to the OLD pubkey captured above, and — when equal
        // (the only case on a routine recover) — SKIP the supersession
        // proof mint + upload entirely (emitting a proof for a key
        // nobody's pin changed would be spurious). The bundle is still
        // re-published WITHOUT a proof so any stale server-side proof
        // column clears (the KeyPackage is published separately, from the
        // long-lived identity, by `ChatRepository.republishKeyPackage`).
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
        // recover path where there was no cached row (the account id came
        // back in the reset finish).
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
        let (restoredSnapshot, restoredGroupIdsHex) = await restoreFromServerBackupBestEffort(
            accountId: accountIdBytes,
            accountIdB64: accountIdB64,
            sessionToken: result.sessionToken,
            phrase: canonicalPhrase,
            context: "recover"
        )
        // Written BEFORE the bootstrap below, because that is what reads the
        // set to rebuild `Group` handles. Additive: a group this device
        // already knew is not dropped by a backup that predates it.
        let knownGroups = KnownGroupsStore()
        for groupIdHex in restoredGroupIdsHex {
            knownGroups.add(accountIdB64: accountIdB64, groupIdHex: groupIdHex)
        }
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
        // ADR-0040 (G-35) — purge any stale pending-commit outbox for this
        // account on recover. Recover REPLACES the local MLS snapshot (with the
        // server-restored blob, or leaves the prior one), so any outbox entry
        // from a previous session references DISCARDED local epochs; the
        // restart sweep would mis-reconcile it against the freshly-restored
        // group. Drop the outbox so the new bootstrap sweeps a clean slate.
        CommitOutboxStore().deleteOutboxForAccount(accountIdB64: accountIdB64)

        // ADR-0017: also carry the phrase seed (the canonical identity
        // seed downstream callers rebuild from). Cleared on logout.
        adoptIdentitySecrets(
            IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: result.exportKey,
                accountIdentitySeed: accountIdentitySeed,
                session: result.session
            ),
            context: "recover"
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
            phrase: canonicalPhrase,
            context: "recover",
            wholeStorageSnapshot: restoredSnapshot
        )
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "recover")
        // 1.20c — best-effort push subscribe.
        Task { await self.subscribePushBestEffort(context: "recover", accountIdB64: accountIdB64) }
    }

    /// Drop the in-memory session. Keeps ``SessionStore`` and the
    /// Keychain-stored device key intact — re-login on the same
    /// device stays cheap.
    ///
    /// Mirror of the web's `logout()`. The cached username is
    /// surfaced in ``AuthState/anon(cachedUsername:)`` so the next
    /// Login form pre-fills it.
    public func logout() {
        // ADR-0042 (amended 2026-09-30) — FIRST: from here on no background
        // wake is handled for this account, and nothing it already received
        // stays on the lock screen.
        pushBinding.clear()
        removeDeliveredNotifications()
        let cached = sessionStore.load()
        // Drop the persisted session token so a relaunch after logout lands
        // logged out instead of silently restoring the session this call
        // just ended (``rehydrate(nowMs:)`` requires the token). The seed and
        // device key stay, which is what keeps the next login cheap.
        if let cached {
            persistence.deleteSessionToken(accountIdB64: cached.accountIdB64)
        }
        // ADR-0054 S3 — the push unregister spends a token; take it from
        // the pool BEFORE the session (and with it the pool) is cleared.
        let unregisterToken = AnonTokenPool.shared.takeIfAvailable(.pushRegister)
        tokenProvider.clear()
        resetGrants.clear()
        identitySecrets = nil
        pendingRecovery = nil
        pendingFreshPhrase = nil
        restoreNotice = nil
        newPasswordOwed = false
        freshPublishPending = false
        freshPublishNeedsSignIn = false
        keysResetThisSession = false
        sessionExpiredNotice = false
        state = .anon(cachedUsername: cached?.username)
        // 1.20c — fire-and-forget unsubscribe so the next page
        // reload doesn't silently keep accepting pushes against
        // a logged-out session.
        Task { await PushSubscriber.shared.unsubscribe(prepaidToken: unregisterToken) }
    }

    /// The session half of Forget device and Delete account: end the session
    /// on this device after ``AccountLocalDataWipe`` has removed the account's
    /// local data. It removes no store itself — calling it alone would leave
    /// every one behind, which is how Delete account used to leave the chats
    /// on disk — so its only caller is
    /// ``AppServices/wipeAccountLocalData(accountIdB64:)``.
    ///
    /// Distinct from ``logout()``, which keeps the account's data for its
    /// next sign-in: this drops the session record too, so the login screen
    /// comes up with no cached username, and after it the keys come back
    /// only from another device, a passkey that restores keys, or the
    /// recovery phrase.
    public func endSessionAfterLocalWipe() {
        // ADR-0042 (amended 2026-09-30) — FIRST, as in ``logout()``. The wipe
        // has already done both; repeated so the order holds on its own.
        pushBinding.clear()
        removeDeliveredNotifications()
        if let cached = sessionStore.load() {
            // The persisted session token (relaunch restore input), through
            // the injected persistence so a test observes it.
            persistence.deleteSessionToken(accountIdB64: cached.accountIdB64)
        }
        sessionStore.clear()
        // ADR-0054 S3 — the push unregister below spends a token; take it
        // before the session (and with it the pool) is cleared.
        let unregisterToken = AnonTokenPool.shared.takeIfAvailable(.pushRegister)
        tokenProvider.clear()
        resetGrants.clear()
        identitySecrets = nil
        // ADR-0017/0018 — drop any parked recovery context too (mirror of
        // logout + the Android forgetDevice), so an abandoned recover /
        // lost-everything mint leaves no in-memory seed behind.
        pendingRecovery = nil
        pendingFreshPhrase = nil
        restoreNotice = nil
        newPasswordOwed = false
        freshPublishPending = false
        freshPublishNeedsSignIn = false
        keysResetThisSession = false
        sessionExpiredNotice = false
        state = .anon(cachedUsername: nil)
        // 1.20c — full wipe of push state too. The contract is
        // "leave NOTHING per-account behind on this device", so
        // we both unregister server-side and clear the Keychain
        // pseudonym. Best-effort fire-and-forget — the local
        // clear inside `unsubscribe` runs regardless of the
        // server leg outcome.
        Task { await PushSubscriber.shared.unsubscribe(prepaidToken: unregisterToken) }
    }

    // MARK: - Push integration helpers

    /// 1.20c — fire ``PushSubscriber/subscribe(accountIdB64:stillSignedIn:)``
    /// on the MainActor with errors logged and swallowed. Called from
    /// signup / login / recover after the state flip so the
    /// user lands in a working session even when push fails.
    ///
    /// ADR-0042 (amended 2026-09-30) — the pseudonym is registered for
    /// `accountIdB64`, and bound to it only while that account is still the
    /// one signed in when the subscribe gets there.
    private func subscribePushBestEffort(context: String, accountIdB64: String) async {
        do {
            _ = try await PushSubscriber.shared.subscribe(
                accountIdB64: accountIdB64,
                stillSignedIn: { [weak self] in self?.signedInAccountIdB64 == accountIdB64 }
            )
        } catch {
            NSLog("push subscribe (\(context)) failed: \(error)")
        }
    }

    /// The account the current state is signed in as, including the states
    /// where ACCESS succeeded and identity is still being restored. `nil`
    /// while logged out.
    var signedInAccountIdB64: String? {
        switch state {
        case .session(let account), .needsRecoveryPhrase(let account), .saveFreshPhrase(let account):
            return account.accountIdB64
        case .loading, .anon:
            return nil
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
        session: SessionPair,
        username: String
    ) {
        guard let accountIdentitySeed else {
            // Fresh-device fail-safe (ADR-0017). Auth succeeded; the
            // identity restore is pending the phrase. Stash the context
            // and flip — `provideRecoveryPhrase` finishes from here. The
            // pair serves the restore prompt's bearer calls meanwhile.
            adoptSessionPair(session, accountIdB64: accountIdB64, context: "needs-recovery-phrase")
            pendingRecovery = PendingRecovery(
                accountIdBytes: accountIdBytes,
                accountIdB64: accountIdB64,
                exportKey: exportKey,
                session: session,
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
            session: session,
            username: username
        )
    }

    /// ADR-0017 — finish a warm session once the canonical
    /// AccountIdentity (phrase) seed is in hand: set the in-memory
    /// secrets (carrying the seed downstream callers rebuild the
    /// canonical identity from) and flip to `.session`, then check the
    /// published identity in the background
    /// (``checkPublishedIdentity(accountIdBytes:accountIdentitySeed:uploads:)``).
    /// Shared by the warm same-device login / passkey paths, the
    /// fresh-device `provideRecoveryPhrase` completion, the ACCESS-factor
    /// warm landing and the fresh-phrase confirmation.
    private func finishCanonicalSession(
        accountIdentitySeed: Data,
        accountIdBytes: Data,
        exportKey: Data,
        session: SessionPair,
        username: String
    ) {
        let accountIdB64 = base64Encode(accountIdBytes)
        // The keys are on this device now; nothing is left to explain.
        restoreNotice = nil
        pendingRecovery = nil
        // A fresh identity whose phrase was never confirmed: the phrase
        // screen comes before the session.
        if parkOwedFreshPhrase(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            username: username,
            seed: accountIdentitySeed,
            session: session
        ) {
            return
        }
        adoptIdentitySecrets(
            IdentitySecrets(
                accountId: accountIdBytes,
                exportKey: exportKey,
                accountIdentitySeed: accountIdentitySeed,
                session: session
            ),
            context: "finishCanonicalSession"
        )
        state = .session(
            SessionAccount(accountIdB64: accountIdB64, username: username)
        )
        // An explicit sign-in confirmed the account is the person's: an owed
        // publish runs without the 30-day bound.
        settleOwedKeyResetPublish(
            accountIdB64: accountIdB64,
            accountIdBytes: accountIdBytes,
            accountIdentitySeed: accountIdentitySeed,
            requireFresh: false
        )
        // ADR-0065 section 4.2 — observe the published identity BEFORE any
        // upload, then re-publish this device's bundle on an explicit
        // sign-in. Both run in this one task, in that order, so the
        // observation never sees this device's own upload.
        Task { [weak self] in
            await self?.checkPublishedIdentity(
                accountIdBytes: accountIdBytes,
                accountIdentitySeed: accountIdentitySeed,
                uploads: true
            )
        }
    }

    /// ADR-0065 section 4.2, on every landing that holds a cached seed:
    /// fetch the account's published identity, compare it with the one this
    /// device derives and with the key generation of this device's session
    /// pair, and act on ``KeyResetLanding``'s verdict.
    ///
    ///   * The same key: land; on an explicit sign-in re-publish the bundle
    ///     (what keeps an account reachable whose bundle was never
    ///     published, with the genesis `leaf_auth` and, when nothing is
    ///     published yet, the ADR-0036 attestation).
    ///   * Other keys at a higher generation: this device's keys are stale.
    ///     The signed-out screen and the automatic wipe
    ///     (``handleKeysSuperseded(accountIdB64:)``). Never an upload.
    ///   * Other keys at the same generation: the blocking screen with the
    ///     wipe on the tap. Never an upload.
    ///   * A fetch failure skips the upload and lands the session; the next
    ///     landing checks again.
    ///
    /// A logout or another sign-in while the fetch is out stops it: the
    /// identity is no longer this session's.
    func checkPublishedIdentity(
        accountIdBytes: Data,
        accountIdentitySeed: Data,
        uploads: Bool
    ) async {
        let identity: Identity
        do {
            identity = try Identity.fromSeed(seed: accountIdentitySeed)
        } catch {
            NSLog("published-identity check: identity derive failed: \(error)")
            return
        }
        let derivedPubkey = identity.signaturePublicKey()
        let accountIdB64 = base64Encode(accountIdBytes)
        // A reset this device made whose publish is still owed: the directory
        // is BEHIND this device, by design, until the owed upload lands
        // (section 2.3). Comparing against it now would read this device's
        // own new keys as a mismatch; the owed publish is what runs instead.
        guard !markers.isPublishOwed(accountIdB64: accountIdB64) else {
            NSLog("[auth] published-identity check skipped: this device's reset is still being published")
            return
        }

        let observed: KeyResetLanding.Observation
        do {
            if let bundle = try await restoreDirectory.fetchIdentityBundle(
                accountId: accountIdBytes, sessionToken: tokenProvider.current?.sessionToken) {
                observed = .present(
                    signaturePubkey: base64Decode(bundle.signaturePubkey),
                    keyGeneration: bundle.keyGeneration)
            } else {
                observed = .absent
            }
        } catch {
            NSLog("published-identity check failed: \(error)")
            observed = .unreachable
        }
        guard isLiveSession(accountIdBytes: accountIdBytes, accountIdentitySeed: accountIdentitySeed) else {
            return
        }
        let ownGeneration = tokenProvider.current?.keyGeneration ?? identitySecrets?.session.keyGeneration ?? 1
        let verdict = KeyResetLanding.decide(
            observed: observed,
            derivedSignaturePubkey: derivedPubkey,
            ownKeyGeneration: ownGeneration,
            uploads: uploads
        )
        switch verdict {
        case .staleKeys:
            NSLog("[auth] the directory publishes keys of a higher generation; this device's keys are stale")
            handleKeysSuperseded(accountIdB64: accountIdB64)
            return
        case .mismatch:
            NSLog("[auth] the directory publishes other keys at this generation; refusing to upload")
            showKeysMismatch(accountIdB64: accountIdB64)
            return
        case .land(let publish):
            guard publish != .skip, let token = tokenProvider.current?.sessionToken else { return }
            do {
                let leafAuth = try signLeafGenesis(
                    ed25519Seed: identity.ed25519Seed(),
                    usernameHash: transparencyAccountKey(accountId: accountIdBytes)
                )
                var genesisAttest: Data?
                if publish == .firstPublish {
                    genesisAttest = await Self.mintGenesisAttestation(
                        accountId: accountIdBytes,
                        identityPubkey: derivedPubkey,
                        sessionToken: token,
                        client: authClientFactory()
                    )
                }
                guard isLiveSession(accountIdBytes: accountIdBytes, accountIdentitySeed: accountIdentitySeed) else {
                    return
                }
                try await accountDirectory.publishIdentityBundle(
                    accountId: accountIdBytes,
                    signaturePubkey: derivedPubkey,
                    x25519Pubkey: identity.x25519PublicKey(),
                    leafAuth: leafAuth,
                    genesisAttest: genesisAttest,
                    keyResetAttest: nil,
                    sessionToken: token
                )
            } catch {
                if IdentityUploadRefusal.of(error) == .superseded {
                    handleKeysSuperseded(accountIdB64: accountIdB64)
                    return
                }
                NSLog("identity bundle refresh (sign-in) failed: \(error)")
            }
        }
    }

    /// `true` while the live session is still the one holding this account
    /// and identity seed.
    private func isLiveSession(accountIdBytes: Data, accountIdentitySeed: Data) -> Bool {
        identitySecrets?.accountId == accountIdBytes
            && identitySecrets?.accountIdentitySeed == accountIdentitySeed
    }

    // MARK: - ADR-0065 section 3.5: the keys were reset elsewhere

    /// The account's keys were reset on another device: the server refused
    /// this session's generation, or the directory publishes keys of a
    /// higher generation. Show the signed-out screen, stop every writer,
    /// and run the full forget-device wipe by itself — publishing nothing
    /// first and calling nothing that needs the superseded token. Once per
    /// account: a second signal while the wipe runs changes nothing.
    func handleKeysSuperseded(accountIdB64: String) {
        guard keysSignOut == nil else { return }
        guard signedInAccountIdB64 == accountIdB64 || sessionStore.load()?.accountIdB64 == accountIdB64 else {
            return
        }
        NSLog("[auth] the account keys were reset on another device; signing this device out")
        keysSignOutUsername = signedInUsername(accountIdB64: accountIdB64)
        keysSignOut = .removing
        // From here on no background wake is handled for this account and
        // nothing it received stays on the lock screen.
        pushBinding.clear()
        removeDeliveredNotifications()
        tokenProvider.clear()
        Task { [weak self] in
            guard let self else { return }
            await self.wipeAfterKeyReset(accountIdB64)
            if self.keysSignOut == .removing { self.keysSignOut = .removed }
        }
    }

    /// Rule 4 of ADR-0065 section 4.2: the directory publishes other keys
    /// at the same generation. The blocking screen; the wipe runs on the
    /// tap (``signOutAfterKeysMismatch()``). Nothing is uploaded.
    private func showKeysMismatch(accountIdB64: String) {
        guard keysSignOut == nil else { return }
        keysSignOutUsername = signedInUsername(accountIdB64: accountIdB64)
        keysSignOut = .mismatch
        pushBinding.clear()
        removeDeliveredNotifications()
    }

    /// The "Sign out" tap of the mismatch variant: the same wipe, then
    /// "Sign in".
    public func signOutAfterKeysMismatch() {
        guard keysSignOut == .mismatch, let accountIdB64 = signedInAccountIdB64 ?? sessionStore.load()?.accountIdB64 else {
            return
        }
        keysSignOut = .removing
        tokenProvider.clear()
        Task { [weak self] in
            guard let self else { return }
            await self.wipeAfterKeyReset(accountIdB64)
            if self.keysSignOut == .removing { self.keysSignOut = .removed }
        }
    }

    /// "Sign in" on the signed-out screen: the screen goes, the anonymous
    /// sign-in screen is behind it (the wipe ended the session).
    public func dismissKeysSignOut() {
        guard keysSignOut == .removed else { return }
        keysSignOut = nil
        keysSignOutUsername = ""
    }

    /// The username of `accountIdB64` as this device knows it: the live
    /// state's, else the session record's, else empty.
    private func signedInUsername(accountIdB64: String) -> String {
        switch state {
        case .session(let account), .needsRecoveryPhrase(let account), .saveFreshPhrase(let account):
            if account.accountIdB64 == accountIdB64 { return account.username }
        case .loading, .anon:
            break
        }
        if let cached = sessionStore.load(), cached.accountIdB64 == accountIdB64 { return cached.username }
        return ""
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
        // The canonical form (12 lower-case words, single spaces), which is
        // what the device stores once the check below has passed.
        let canonicalPhrase = mnemonic.phrase()
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

        // A device that signed in without a password (an authenticator
        // code, a passkey) parked with NO device root key (no export key
        // to derive one from). Bootstrap it from the VERIFIED identity
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
        // Strictly after the fail-closed check: the phrase is the account's.
        DevicePhrase.store(
            canonicalPhrase, accountIdB64: pending.accountIdB64, context: "provide-phrase")

        finishCanonicalSession(
            accountIdentitySeed: accountIdentitySeed,
            accountIdBytes: pending.accountIdBytes,
            exportKey: pending.exportKey,
            session: pending.currentSession(tokenProvider),
            username: pending.username
        )
    }

    /// Shared HARD-BLOCK verification (ADR-0017 + lakko-security
    /// finding #1): the supplied AccountIdentity seed MUST reproduce
    /// the account's PUBLISHED identity key. 404 (nil bundle) or a
    /// fetch error refuses (``IdentityVerificationUnavailable``); a
    /// mismatch refuses (``PhraseMismatch``). Never trust-on-first-use.
    /// Used by ``provideRecoveryPhrase(mnemonicPhrase:)``,
    /// ``completeFromPairing(phraseEntropy:session:)``
    /// and
    /// ``completeFromDeviceRestore(accountIdBytes:accountIdB64:username:phraseEntropy:session:forgotPassword:persistJoin:)``;
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
            bundle = try await restoreDirectory.fetchIdentityBundle(
                accountId: accountIdBytes,
                sessionToken: sessionToken
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
                "Try again, or restore with your recovery phrase.",
                kind: .missing
            )
        }
        guard let published = base64Decode(bundle.signaturePubkey),
              published == derivedPubkey else {
            throw PhraseMismatch()
        }
    }

    /// Finish a pending ``AuthState/needsRecoveryPhrase(_:)`` sign-in with
    /// what ANOTHER DEVICE handed over on the E2EE pairing channel (instead
    /// of typing the 12 words). Recovery payload v2: what arrives is the
    /// 16-byte entropy of the recovery phrase. The phrase and the 64-byte
    /// seed are rebuilt from it, the seed passes the same fail-closed gate
    /// as ``provideRecoveryPhrase(mnemonicPhrase:)``, and only then are the
    /// seed AND the phrase stored on this device. Prefers the freshly
    /// REDEEMED session token when the joiner's grant redeem succeeded.
    ///
    /// The derived seed is zeroized on every refusal; the caller owns the
    /// entropy buffer (``PairingOrchestrator/JoinRecoveryOutcome/wipeEntropy()``).
    ///
    /// - Throws: ``PhraseMismatch`` / ``IdentityVerificationUnavailable``
    ///   — same semantics as ``provideRecoveryPhrase(mnemonicPhrase:)``;
    ///   ``AuthClientError/argument(_:)`` when there is no pending
    ///   recovery; ``RecoveryPayloadError`` when the entropy is not 16
    ///   bytes.
    public func completeFromPairing(
        phraseEntropy: Data,
        session: SessionPair? = nil
    ) async throws {
        guard let pending = pendingRecovery else {
            throw AuthClientError.argument(
                "no pending identity recovery — sign in first"
            )
        }
        // Nothing is written before the check below.
        let transferred = try RecoveryPayload.phraseAndSeed(fromEntropy: phraseEntropy)
        var seed = transferred.seed
        do {
            try await verifySeedAgainstPublishedIdentity(
                accountIdBytes: pending.accountIdBytes,
                sessionToken: session?.sessionToken ?? pending.sessionToken,
                accountIdentitySeed: seed
            )
        } catch {
            Self.zeroize(&seed)
            throw error
        }
        // A device that signed in without a password parked with NO device
        // root key. Bootstrap it from the VERIFIED transferred seed (the
        // `completeFromDeviceRestore` recipe), strictly AFTER the
        // fail-closed verification above — a refusal must leave this
        // device untouched.
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
        // This device now holds the phrase like the one that confirmed it.
        DevicePhrase.store(
            transferred.phrase, accountIdB64: pending.accountIdB64, context: "device-pairing")
        finishCanonicalSession(
            accountIdentitySeed: seed,
            accountIdBytes: pending.accountIdBytes,
            exportKey: pending.exportKey,
            session: session ?? pending.currentSession(tokenProvider),
            username: pending.username
        )
    }

    /// The NO-PASSWORD restore: the fresh device ran the anonymous pairing
    /// join and holds the transferred phrase entropy (recovery payload v2)
    /// plus the REDEEMED session token (the only access on this device —
    /// there is no password, hence no OPAQUE export key).
    ///
    /// FAIL CLOSED FIRST — the phrase and its 64-byte seed are rebuilt from
    /// the entropy and the seed is verified against the published identity
    /// BEFORE any persistence; a mismatched / unverifiable seed leaves this
    /// device untouched.
    ///
    /// The device root key (the ``rehydrate(nowMs:)`` existence-gate) is
    /// derived from the identity seed via
    /// `deriveDeviceRootKeyFromIdentitySeed` — distinct HKDF domain
    /// from the exportKey-derived variant. The iOS per-account stores
    /// (`AccountIdentitySeedStore` / `DeviceIdentityStore` / …) are
    /// direct Keychain rows, NOT wrapped under the device root key, so
    /// a later password login overwriting it with the exportKey-derived
    /// value is harmless — same posture as Android.
    ///
    /// `IdentitySecrets.exportKey` is set EMPTY on this path — no
    /// consumer outside this repository reads it (the canonical identity +
    /// backup signing are phrase-seeded per ADR-0017), and a later password
    /// login replaces it.
    ///
    /// ADR-0056 security pass — after the seed verification and before
    /// ANY persistence, the typed `username` must resolve at discovery
    /// (the ordinary token-spending lookup, drawn with the redeemed
    /// session) to `accountIdBytes`: a squatted auth login handle can route
    /// the pairing request to another account, and discovery is a registry
    /// auth does not control. A mismatch or an unknown name refuses with
    /// ``TypedNameCheckRefusal/otherAccount``, a lookup that could not run
    /// with ``TypedNameCheckRefusal/unavailable``. On every refusal nothing
    /// is written and the session is never installed. `persistJoin` is the
    /// pairing's held writes (device seed, DeviceGroup snapshot, history,
    /// state bundle); it runs only once both checks passed, before the
    /// session lands.
    ///
    /// `forgotPassword`: the restore was reached through "Forgot password?",
    /// so the new-password step is offered once the session lands.
    ///
    /// The derived seed is zeroized on every refusal; the caller owns the
    /// entropy buffer (``PairingOrchestrator/JoinRecoveryOutcome/wipeEntropy()``).
    public func completeFromDeviceRestore(
        accountIdBytes: Data,
        accountIdB64: String,
        username: String,
        phraseEntropy: Data,
        session: SessionPair,
        forgotPassword: Bool = false,
        persistJoin: () async -> Void = {}
    ) async throws {
        let transferred = try RecoveryPayload.phraseAndSeed(fromEntropy: phraseEntropy)
        var seed = transferred.seed
        do {
            try await verifySeedAgainstPublishedIdentity(
                accountIdBytes: accountIdBytes,
                sessionToken: session.sessionToken,
                accountIdentitySeed: seed
            )
            try await TypedNameCheck.check(
                username: username,
                accountIdBytes: accountIdBytes
            ) { name in
                try await restoreDirectory.lookupAccount(
                    username: name,
                    accountId: accountIdBytes,
                    sessionToken: session.sessionToken
                )
            }
        } catch {
            Self.zeroize(&seed)
            throw error
        }
        // Both checks passed: the pairing's held state lands first, so the
        // DeviceGroup snapshot is on disk before the session bootstraps.
        await persistJoin()
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
        // This device now holds the phrase like the one that confirmed it.
        DevicePhrase.store(transferred.phrase, accountIdB64: accountIdB64, context: "device-restore")
        // Persist the account marker — there is no prior session row
        // on this fresh device.
        sessionStore.save(
            PersistedSession(
                accountIdB64: accountIdB64,
                username: username
            )
        )
        if forgotPassword { newPasswordOwed = true }
        finishCanonicalSession(
            accountIdentitySeed: seed,
            accountIdBytes: accountIdBytes,
            exportKey: Data(),
            session: session,
            username: username
        )
    }

    /// ADR-0018 factor (c) — recover ACCESS with an authenticator
    /// (TOTP) code. Mirror of the web `recoverWithTotp`.
    ///
    /// ACCESS only: the server verifies the code and issues the
    /// session triple; identity acquisition runs the ADR-0017 state
    /// machine below. NO password is involved, so there is NO OPAQUE
    /// export key on this path — `exportKey` is empty `Data()` (the
    /// ``completeFromDeviceRestore(accountIdBytes:accountIdB64:username:phraseEntropy:session:forgotPassword:persistJoin:)``
    /// convention).
    ///
    ///   * **Warm case** — a cached AccountIdentity (phrase) seed on
    ///     this device rebuilds the canonical identity directly. If
    ///     the device root key is missing (e.g. post-Keychain-reset),
    ///     it is bootstrapped from the cached (already-verified) seed
    ///     so the ``rehydrate(nowMs:)`` existence-gate passes on the next
    ///     launch.
    ///   * **Cold case** — park a ``PendingRecovery`` (empty
    ///     exportKey) and flip to `.needsRecoveryPhrase`; the
    ///     completion paths (`provideRecoveryPhrase` /
    ///     `completeFromPairing`) verify the supplied seed fail-closed
    ///     and bootstrap the device key from it.
    ///
    /// On a device that holds the keys the new-password step is offered.
    public func recoverWithTotp(username: String, code: String) async throws {
        // ADR-0054 S5: the typed name, else the one cached on this device,
        // names the account by its login handle. The account id comes back
        // in the response; no discovery lookup runs before the session.
        let name = try signInName(
            typed: username,
            cached: sessionStore.load().map {
                CachedAccount(accountIdB64: $0.accountIdB64, username: $0.username)
            },
            emptyError: AuthClientError.argument(
                "enter the username of the account to recover"
            )
        )

        let client = authClientFactory()
        let result = try await client.totpRecover(
            handle: name.handle,
            code: code
        )
        let accountIdBytes = result.accountId
        let accountIdB64 = base64Encode(accountIdBytes)
        let resolvedUsername = name.canonical

        restoreNotice = nil
        // A forgot-password route: the new-password step is owed once this
        // device holds the keys.
        newPasswordOwed = true
        // ADR-0018 ACCESS-only landing (warm cached-seed finish OR cold
        // fail-safe park).
        await landAfterAccessRecovery(
            username: resolvedUsername,
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            session: result.session,
            context: "totp recover",
            pushContext: "totp-recover"
        )
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "totp recover")
    }

    /// The landing every access-only route shares: the authenticator code,
    /// a passkey, and the sign-in that follows the local three-word check.
    /// The caller verified an access factor server-side and holds the
    /// session triple, but NO password (so there is no OPAQUE export key
    /// from this sign-in). "This device holds the keys" is decided here and
    /// nowhere else: a cached AccountIdentity seed is readable.
    ///
    ///   * **Warm case** — the cached seed rebuilds the canonical identity
    ///     directly. The device root key is bootstrapped from the cached
    ///     (already-verified) seed when absent so the
    ///     ``rehydrate(nowMs:)`` existence-gate passes on the next launch,
    ///     then ``finishCanonicalSession`` flips to `.session`. The export
    ///     key is whatever an earlier password sign-in cached on this
    ///     device, else empty.
    ///   * **Cold case** — park a ``PendingRecovery`` (empty exportKey) and
    ///     flip to `.needsRecoveryPhrase`, where the restore prompt offers
    ///     another device, a passkey or the phrase; the completion paths
    ///     verify the supplied seed fail-closed and bootstrap the device
    ///     key from it.
    ///
    /// `restoreKeys` is the passkey route's hook for the cold case: it runs
    /// after the recovery context is parked and before the restore prompt
    /// is shown, and returns `true` when it landed the session itself (the
    /// passkey carried the keys).
    private func landAfterAccessRecovery(
        username: String,
        accountIdBytes: Data,
        accountIdB64: String,
        session: SessionPair,
        context: String,
        pushContext: String,
        restoreKeys: (() async -> Bool)? = nil
    ) async {
        // Persist the session marker — important on a fresh device
        // where no row existed (the account id came back in the response).
        sessionStore.save(
            PersistedSession(
                accountIdB64: accountIdB64,
                username: username
            )
        )
        // The pair serves every bearer call of the restore prompt too.
        adoptSessionPair(session, accountIdB64: accountIdB64, context: context)

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
            let cachedExportKey = (try? ExportKeyStore.getExportKey(accountIdB64: accountIdB64)) ?? nil
            finishCanonicalSession(
                accountIdentitySeed: accountIdentitySeed,
                accountIdBytes: accountIdBytes,
                exportKey: cachedExportKey ?? Data(),
                session: session,
                username: username
            )
            Task { await self.subscribePushBestEffort(context: pushContext, accountIdB64: accountIdB64) }
            return
        }

        // Cold case — the keys are not on this device: access succeeded,
        // the restore is pending. exportKey EMPTY (no password); the
        // completion paths bootstrap the device key from the VERIFIED seed.
        pendingRecovery = PendingRecovery(
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            exportKey: Data(),
            session: session,
            username: username
        )
        if let restoreKeys, await restoreKeys() { return }
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

    /// "Reset account keys" (ADR-0065 section 2.3, UX 15.1): the
    /// lost-everything fallback for a person who is signed in but has NO
    /// recovery phrase, NO other device and NO passkey that restores keys.
    ///
    /// The account gets new keys and a new 12-word phrase in ONE server
    /// transaction (`POST /account/keys/reset/{start,finish}`): the key
    /// generation moves up, the recovery credential is replaced, every
    /// passkey is deleted, every other device is signed out, and this
    /// device gets a session pair at the new generation plus the
    /// auth-signed key-reset attestation that discovery and the DS require
    /// before they let the new identity replace the old one. Nothing can
    /// publish the old identity back afterwards.
    ///
    /// Reachable ONLY while a ``PendingRecovery`` is parked (signed in, from
    /// ``AuthState/needsRecoveryPhrase(_:)``) and ONLY via the explicit,
    /// typed-confirmation button. Order:
    ///
    ///   1. Generate the phrase + seed; derive the new identity.
    ///   2. The server step. A failure here changes nothing: `start` stores
    ///      nothing, `finish` is one transaction, the phrase is discarded.
    ///   3. PERSIST IMMEDIATELY, before any further network call: the new
    ///      pair (the old one is superseded with every other device's), the
    ///      seed and the phrase under the device key, the "phrase owed"
    ///      marker, and the owed-publish record with the attestation.
    ///   4. The publish: verify the attestation under the pinned key, the
    ///      directory upload with it, this device's KeyPackage under a
    ///      binding signed by the NEW key, the DeviceGroup re-founding and
    ///      its attested journal reset. Each step is recorded as it lands;
    ///      a failure parks ``freshPublishPending`` and
    ///      ``retryFreshPublish()`` repeats only what is left, with the SAME
    ///      attestation (a replay is harmless; `409 key-generation-not-newer`
    ///      means done).
    ///   5. Flip to ``AuthState/saveFreshPhrase(_:)``. The words are
    ///      withheld while the publish is owed.
    ///
    /// Does NOT land the session; ``confirmFreshPhraseSaved()`` does, after
    /// the person confirmed the phrase. Mirror of the web
    /// `mintFreshIdentity`.
    ///
    /// The server refuses a reset without a reset grant, which only a
    /// password sign-in mints (amended 2026-10-04, UX 15.1a). With a live
    /// grant held, `password` is not used. Without one, `password` runs the
    /// password sign-in first (``obtainResetGrant(password:username:pending:)``);
    /// with neither, ``KeyResetPasswordRequired`` is thrown before any
    /// network call. A wrong password throws ``KeyResetWrongPassword``; a
    /// `403 password-required` from the reset drops the grant and throws
    /// ``KeyResetPasswordRequired``. In each case nothing was reset.
    ///
    /// `username` is used only when the parked account has none on this
    /// device (a passkey sign-in that named no account): the sign-in is then
    /// addressed by the handle of the typed name. With no name to derive a
    /// handle from, ``KeyResetUsernameRequired`` is thrown before any call.
    public func mintFreshIdentity(password: Data? = nil, username: String? = nil) async throws -> String {
        guard var pending = pendingRecovery else {
            throw AuthClientError.argument("no pending identity recovery — sign in first")
        }

        // 0. The reset grant: the one held, else a password sign-in now.
        let grant: String
        if let held = resetGrants.live(accountIdB64: pending.accountIdB64, nowMs: grantNowMs()) {
            grant = held
        } else {
            guard let password, !password.isEmpty else { throw KeyResetPasswordRequired() }
            (grant, pending) = try await obtainResetGrant(
                password: password, username: username, pending: pending)
        }

        // 1. A fresh 12-word phrase + seed (the client-side generation
        //    signup uses) and the identity it seeds.
        let mnemonic = RecoveryMnemonic.generate()
        let phrase = mnemonic.phrase()
        let seed = mnemonic.toSeed()
        let newPubkey = try Identity.fromSeed(seed: seed).signaturePublicKey()

        // 2. The server step: the point of no return.
        let reset: KeyResetResult
        do {
            reset = try await keyResetOperation(
                pending.currentSession(tokenProvider).sessionToken, grant, seed, newPubkey)
        } catch {
            if KeyResetRefusal.isPasswordRequired(error) {
                // The grant expired on the way (or was spent): ask again.
                resetGrants.clear()
                throw KeyResetPasswordRequired()
            }
            throw error
        }
        // A successful reset spends the grant and every one before it.
        resetGrants.clear()

        // 3. Persist, before any further network call.
        adoptSessionPair(reset.session, accountIdB64: pending.accountIdB64, context: "key reset")
        bootstrapDeviceKeyFromVerifiedSeedIfAbsent(
            accountIdB64: pending.accountIdB64,
            accountIdentitySeed: seed,
            context: "mintFreshIdentity"
        )
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: pending.accountIdB64,
                seed: seed
            )
        } catch {
            NSLog("fresh-identity seed persist failed: \(error)")
        }
        // The device holds the new keys, so it holds the new phrase.
        DevicePhrase.store(phrase, accountIdB64: pending.accountIdB64, context: "fresh-identity")
        markers.markPhraseOwed(accountIdB64: pending.accountIdB64)
        let record = KeyResetPublishRecord(
            attestation: reset.attestation,
            keyGeneration: reset.session.keyGeneration,
            signedAtMs: 0,
            newSignaturePubkey: newPubkey
        )
        markers.markPublishOwed(record, accountIdB64: pending.accountIdB64)
        keysResetThisSession = true

        // The state flips before the publish so a failure parks the phrase
        // screen in its "could not finish publishing" form.
        pendingFreshPhrase = PendingFreshPhrase(
            seed: seed,
            accountIdBytes: pending.accountIdBytes,
            accountIdB64: pending.accountIdB64,
            session: reset.session,
            username: pending.username,
            phrase: phrase,
            publishOwed: true
        )
        freshPublishPending = true
        freshPublishNeedsSignIn = false
        pendingRecovery = nil
        restoreNotice = nil
        state = .saveFreshPhrase(
            SessionAccount(
                accountIdB64: pending.accountIdB64,
                username: pending.username
            )
        )

        // 4. The publish. The attestation was just minted: no freshness
        //    bound applies.
        do {
            try await runOwedKeyResetPublish(
                accountIdB64: pending.accountIdB64,
                accountIdBytes: pending.accountIdBytes,
                accountIdentitySeed: seed,
                requireFresh: false
            )
        } catch {
            NSLog("[auth] the publish after a key reset did not finish; the phrase screen offers a retry")
        }
        return phrase
    }

    /// Whether a live reset grant is held for the account parked on the
    /// restore prompt: the Reset account keys screen then shows no password
    /// field (UX 15.1a).
    public func holdsLiveResetGrantForPendingRestore() -> Bool {
        guard let pending = pendingRecovery else { return false }
        return resetGrants.holdsLive(accountIdB64: pending.accountIdB64, nowMs: grantNowMs())
    }

    /// The password sign-in the Reset account keys screen runs when no live
    /// grant is held: the same OPAQUE login the sign-in screen uses, by the
    /// account's login handle. It proves the password now and mints a grant.
    ///
    /// The sign-in returns a fresh session pair for the SAME account at the
    /// same key generation; it is installed in place of the parked one and
    /// nothing else happens: no session landing, so no landing rule, no
    /// handle re-bind, no push subscribe. A sign-in that names another
    /// account, or carries no grant, is refused and installs nothing.
    ///
    /// The account is named by its own username; `username` (typed on the
    /// warning screen) only when the device knows none. A typed name that
    /// signs in to a DIFFERENT account than the parked one is reported as a
    /// wrong password: it is not this account's password, and nothing of
    /// that sign-in is kept.
    private func obtainResetGrant(
        password: Data,
        username: String?,
        pending: PendingRecovery
    ) async throws -> (grant: String, pending: PendingRecovery) {
        let known = pending.username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let handle = ownLoginHandle(username: known.isEmpty ? (username ?? "") : known) else {
            throw KeyResetUsernameRequired()
        }
        let result: LoginResult
        do {
            result = try await resetLoginOperation(handle, password)
        } catch {
            if KeyResetRefusal.isWrongPassword(error) { throw KeyResetWrongPassword() }
            throw error
        }
        guard result.accountId == pending.accountIdBytes else {
            NSLog("[auth] WARN the password sign-in for a key reset named another account; refused")
            throw KeyResetWrongPassword()
        }
        guard let minted = result.resetGrant, minted.expiresAtMs > grantNowMs() else {
            NSLog("[auth] WARN the password sign-in carried no usable reset grant")
            throw KeyResetPasswordRequired()
        }
        // The restore prompt may have been left meanwhile (sign-out).
        guard pendingRecovery?.accountIdB64 == pending.accountIdB64 else {
            throw AuthClientError.argument("no pending identity recovery — sign in first")
        }
        adoptSessionPair(result.session, accountIdB64: pending.accountIdB64, context: "key reset sign-in")
        let renewed = PendingRecovery(
            accountIdBytes: pending.accountIdBytes,
            accountIdB64: pending.accountIdB64,
            exportKey: pending.exportKey,
            session: result.session,
            username: pending.username
        )
        pendingRecovery = renewed
        resetGrants.put(minted, accountIdB64: pending.accountIdB64)
        return (minted.grant, renewed)
    }

    /// Retry the owed publish of the keys ``mintFreshIdentity()`` reset.
    /// Never resets again. Throws what the failing step threw (offline,
    /// `429`, `503`), or ``KeyResetSignInRequired`` when the stored
    /// attestation is older than the 30-day bound.
    public func retryFreshPublish() async throws {
        guard let pending = pendingFreshPhrase else {
            throw AuthClientError.argument("no fresh identity to finish")
        }
        guard pending.publishOwed else { return }
        try await runOwedKeyResetPublish(
            accountIdB64: pending.accountIdB64,
            accountIdBytes: pending.accountIdBytes,
            accountIdentitySeed: pending.seed,
            requireFresh: true
        )
    }

    /// Finish the session once the person confirmed the new recovery
    /// phrase. Valid ONLY while a ``PendingFreshPhrase`` is parked, and
    /// refused (``FreshPublishPending``) while the new keys are not published
    /// yet: the phrase screen then offers "Try again" and does not show the
    /// words.
    public func confirmFreshPhraseSaved() throws {
        guard let pending = pendingFreshPhrase else {
            throw AuthClientError.argument("no fresh identity to confirm")
        }
        if pending.publishOwed {
            throw FreshPublishPending()
        }
        // The phrase was shown and confirmed on this screen.
        markers.clearPhraseOwed(accountIdB64: pending.accountIdB64)
        // A guest account has no username and so no password to replace; a
        // fresh identity reached through "Forgot password?" keeps the offer
        // (its recovery credential now belongs to the new phrase, so the
        // reset works).
        if pending.username.isEmpty { newPasswordOwed = false }
        pendingFreshPhrase = nil
        freshPublishPending = false
        freshPublishNeedsSignIn = false
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
            session: pending.currentSession(tokenProvider),
            username: pending.username
        )
    }

    /// A fresh identity whose phrase the person never confirmed (the app
    /// was closed on the phrase screen): park it again and show the phrase
    /// before the session. Returns `true` when it parked.
    ///
    /// A phrase that is owed but no longer readable on this device cannot
    /// be shown; the marker is dropped (and logged) rather than blocking
    /// the account forever.
    private func parkOwedFreshPhrase(
        accountIdBytes: Data,
        accountIdB64: String,
        username: String,
        seed: Data,
        session: SessionPair
    ) -> Bool {
        guard markers.isPhraseOwed(accountIdB64: accountIdB64) else { return false }
        guard let phrase = DevicePhrase.read(accountIdB64: accountIdB64) else {
            NSLog("[auth] a recovery phrase is owed but not readable on this device; it cannot be shown again")
            markers.clearPhraseOwed(accountIdB64: accountIdB64)
            return false
        }
        let publishOwed = markers.isPublishOwed(accountIdB64: accountIdB64)
        pendingFreshPhrase = PendingFreshPhrase(
            seed: seed,
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            session: session,
            username: username,
            phrase: phrase,
            publishOwed: publishOwed
        )
        freshPublishPending = publishOwed
        freshPublishNeedsSignIn = false
        identitySecrets = nil
        state = .saveFreshPhrase(SessionAccount(accountIdB64: accountIdB64, username: username))
        if publishOwed {
            // The background retry of the owed publish; "Try again" on the
            // screen repeats it. A relaunch is not a sign-in: the 30-day
            // bound applies.
            Task { [weak self] in
                do {
                    try await self?.retryFreshPublish()
                } catch {
                    NSLog("[auth] owed key-reset publish failed; the phrase screen offers a retry")
                }
            }
        }
        return true
    }

    /// A key reset whose publish did not finish leaves a record
    /// (``RecoveryMarkers``). When a session for that account lands, the
    /// device holds the new seed and the attestation, so the rest is tried
    /// again here, in the background. Success clears the record; a failure
    /// is logged and waits for the next session landing.
    ///
    /// Safe against a stale seed: the record is set only after the new seed
    /// was stored on this device, and every other way a seed reaches this
    /// device is checked against the published (new) identity first.
    private func settleOwedKeyResetPublish(
        accountIdB64: String,
        accountIdBytes: Data,
        accountIdentitySeed: Data,
        requireFresh: Bool
    ) {
        guard markers.isPublishOwed(accountIdB64: accountIdB64) else { return }
        Task { [weak self] in
            do {
                try await self?.runOwedKeyResetPublish(
                    accountIdB64: accountIdB64,
                    accountIdBytes: accountIdBytes,
                    accountIdentitySeed: accountIdentitySeed,
                    requireFresh: requireFresh
                )
            } catch {
                NSLog("[auth] owed key-reset publish failed; it is retried at the next session landing")
            }
        }
    }

    /// Run the steps a key reset still owes (``KeyResetPublish``), in
    /// order, recording each as it lands. The attestation is verified under
    /// the pinned attest key on EVERY run before anything is sent — with
    /// the 30-day bound when `requireFresh` (a relaunch, "Try again"),
    /// without it right after a sign-in. Clears the record and
    /// ``freshPublishPending`` once every step landed; on a failure the
    /// record keeps what landed and the error is thrown.
    private func runOwedKeyResetPublish(
        accountIdB64: String,
        accountIdBytes: Data,
        accountIdentitySeed: Data,
        requireFresh: Bool
    ) async throws {
        guard var record = markers.publishOwed(accountIdB64: accountIdB64) else {
            finishOwedPublish(accountIdB64: accountIdB64)
            return
        }
        let steps = KeyResetPublish.remaining(record)
        if steps.isEmpty {
            markers.clearPublishOwed(accountIdB64: accountIdB64)
            finishOwedPublish(accountIdB64: accountIdB64)
            return
        }
        let nowMs = SessionRestore.nowMs()
        let session = tokenProvider.current ?? pendingFreshPhrase?.session ?? identitySecrets?.session
        guard let session else { throw SessionNotReady() }
        let secrets = IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: identitySecrets?.exportKey ?? Data(),
            accountIdentitySeed: accountIdentitySeed,
            session: session
        )
        let identity = try Identity.fromSeed(seed: accountIdentitySeed)
        for step in steps {
            do {
                switch step {
                case .verify:
                    let claims = try await attestationVerifier.verify(
                        attestation: record.attestation,
                        accountId: accountIdBytes,
                        newSignaturePubkey: record.newSignaturePubkey,
                        nowMs: nowMs,
                        maxAgeMs: requireFresh ? KeyResetPublish.maxAttestationAgeMs : nil
                    )
                    guard claims.keyGeneration == record.keyGeneration,
                          record.newSignaturePubkey == identity.signaturePublicKey() else {
                        throw AuthClientError.argument("the stored attestation does not name this reset")
                    }
                    if record.signedAtMs != claims.signedAtMs {
                        record.signedAtMs = claims.signedAtMs
                        markers.markPublishOwed(record, accountIdB64: accountIdB64)
                    }
                case .discovery:
                    do {
                        try await publishFreshIdentityBundle(
                            accountId: accountIdBytes,
                            sessionToken: session.sessionToken,
                            seed: accountIdentitySeed,
                            keyResetAttest: record.attestation
                        )
                    } catch {
                        switch KeyResetPublish.discoveryOutcome(error) {
                        case .done:
                            NSLog("[auth] the directory already holds this reset")
                        case .superseded:
                            handleKeysSuperseded(accountIdB64: accountIdB64)
                            throw error
                        case .retry:
                            throw error
                        }
                    }
                case .keyPackage:
                    try await keyResetHooks.republishOwnKeyPackage(secrets)
                case .deviceGroup:
                    try await keyResetHooks.refoundDeviceGroup(accountIdBytes, accountIdB64, record.attestation)
                }
            } catch {
                if KeyResetVerifyFailure.isStale(error) {
                    freshPublishNeedsSignIn = true
                    throw KeyResetSignInRequired()
                }
                NSLog("[auth] key-reset publish step %@ failed: %@", step.rawValue, String(describing: error))
                throw KeyResetPublishFailed(step: step.rawValue, underlying: error)
            }
            record = KeyResetPublish.marking(record, done: step)
            if step != .verify {
                markers.markPublishOwed(record, accountIdB64: accountIdB64)
            }
        }
        markers.clearPublishOwed(accountIdB64: accountIdB64)
        finishOwedPublish(accountIdB64: accountIdB64)
    }

    /// Nothing is owed any more: the phrase screen may show the words.
    private func finishOwedPublish(accountIdB64: String) {
        if pendingFreshPhrase?.accountIdB64 == accountIdB64 {
            pendingFreshPhrase?.publishOwed = false
            freshPublishPending = false
        }
        freshPublishNeedsSignIn = false
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
        sessionToken: String,
        client: AuthClient = AuthClient()
    ) async -> Data? {
        do {
            return try await client.mintGenesisAttestation(
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

    /// Publish the fresh-identity bundle for `accountId` WITHOUT a
    /// supersession proof (ADR-0017 lost-everything). THROWS on failure — the
    /// caller publishes before any local mutation, so a throw leaves the
    /// device untouched and the user can retry.
    ///
    /// **No KeyPackage here, deliberately.** This used to also upload
    /// `identity.createKeyPackage()` minted on the throwaway `Identity` built
    /// just below. A KeyPackage's private init / leaf keys live in the
    /// provider of the `Identity` that minted it, and this one is dropped at
    /// the end of the call — so every Welcome sent to that KeyPackage failed
    /// on this device, and since discovery keeps one KeyPackage per account
    /// the user could not be added to any group at all. The KeyPackage is now
    /// published only from the long-lived identity
    /// (``ChatRepository/republishKeyPackage()``), once the session lands and
    /// the chat bootstrap has run.
    ///
    /// `keyResetAttest` is the ADR-0065 attestation on the upload that
    /// installs reset keys (the directory refuses an unattested key change).
    private func publishFreshIdentityBundle(
        accountId: Data,
        sessionToken: String,
        seed: Data,
        keyResetAttest: Data? = nil
    ) async throws {
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
            sessionToken: sessionToken,
            client: authClientFactory()
        )
        try await accountDirectory.publishIdentityBundle(
            accountId: accountId,
            signaturePubkey: identity.signaturePublicKey(),
            x25519Pubkey: identity.x25519PublicKey(),
            leafAuth: leafAuth,
            genesisAttest: genesisAttest,
            keyResetAttest: keyResetAttest,
            sessionToken: sessionToken
        )
    }


    /// Bootstrap the device root key from a VERIFIED AccountIdentity seed
    /// when the Keychain has none: every sign-in without a password (an
    /// authenticator code, a passkey, three words, another device) (the
    /// ``rehydrate(nowMs:)`` existence-gate would otherwise bounce the next
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
    /// failure is treated as "no key" (same as ``rehydrate(nowMs:)``) — the
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
            // rehydrate(nowMs:).
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
    ///     bundle WITHOUT a proof. The public keys are identical, so the
    ///     upsert keeps the directory current + a proof-less upload clears
    ///     any stale server-side proof column. (No KeyPackage: that is
    ///     ``ChatRepository/republishKeyPackage()``'s job.)
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
            // is ignored; but an account whose signup publish never landed (or
            // one created before signup published at all) can reach its FIRST
            // publish here — then this self-sig authorises the genesis log
            // entry. The key is unchanged, so the self-sig is valid.
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
            // Proof-less re-publish (account key unchanged). Clears any
            // stale server proof column. The KeyPackage is NOT refreshed
            // here: it must come from the long-lived identity, and
            // ``ChatRepository/republishKeyPackage()`` does that once the
            // session lands.
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
    ) async -> (snapshot: Data, knownGroupIdsHex: [String]) {
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
            return (Data(), [])
        case .noBackup:
            NSLog("backup restore (\(context)): no prior backup")
            return (Data(), [])
        case .wrongPhraseOrTampered:
            NSLog("backup restore (\(context)) failed: wrong phrase or tampered blob")
            return (Data(), [])
        case .transportFailed(let error):
            NSLog("backup restore (\(context)) failed: \(error)")
            return (Data(), [])
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
        // Written into the RESTORED account's list, named explicitly: the
        // session may not be installed yet when a restore runs.
        let existingContacts = AcceptedContactsStore.load(accountIdB64: accountIdB64)
        let restoredContacts = reconstructRestoredContacts(
            existing: existingContacts,
            restored: parsed.acceptedContacts,
            createdAtMs: parsed.createdAtMs
        )
        if !restoredContacts.isEmpty {
            AcceptedContactsStore.save(existingContacts + restoredContacts, accountIdB64: accountIdB64)
            // A live repository already bound to this account merges them
            // (its next save would otherwise overwrite the write above).
            NotificationCenter.default.post(
                name: .lakkoStateBundleApplied, object: restoredContacts,
                userInfo: [StateBundle.appliedAccountKey: accountIdB64])
            NSLog("backup restore (\(context)): merged \(restoredContacts.count) contact(s)")
        }
        // The index the snapshot cannot supply: `restoreStorage` lays down a
        // provider holding every group, but nothing can enumerate it, so
        // `ChatRepository.bootstrap` rebuilds handles from a separately
        // persisted set of ids. Without this the restored device held every
        // group and listed none.
        // ADR-0058 section 5.2 — the folded synced state (folders, pins,
        // mutes, archive, read cursors, drafts), merged into this device's
        // store before the DeviceGroup poll seeds from it. A running sync
        // runtime for the account folds it too.
        if !parsed.syncedState.isEmpty {
            SyncedStateStore.mergeIntoStored(bytes: parsed.syncedState, accountIdB64: accountIdB64)
            NotificationCenter.default.post(
                name: .lakkoSyncedStateRestored, object: nil,
                userInfo: ["accountIdB64": accountIdB64, "bytes": parsed.syncedState])
            NSLog("backup restore (\(context)): synced state restored")
        }
        let restoredGroupIdsHex = parsed.knownGroupIds.map(Self.hexString(fromBytes:))
        if !restoredGroupIdsHex.isEmpty {
            NSLog(
                "backup restore (\(context)): \(restoredGroupIdsHex.count) known group(s)")
        }
        return (parsed.wholeStorageSnapshot, restoredGroupIdsHex)
    }

    /// Lowercase hex, matching the form `KnownGroupsStore` holds.
    private static func hexString(fromBytes bytes: Data) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
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
        let contactRecords = AcceptedContactsStore.load(accountIdB64: accountIdB64).map { c in
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
                wholeStorageSnapshot: wholeStorageSnapshot,
                // ADR-0058 — whatever synced state this device already holds
                // (after a recover, the one just restored), so the baseline
                // does not overwrite the backup's folders with none.
                syncedState: SyncedStateStore.bytes(accountIdB64: accountIdB64)
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
    /// Signed in, keys not here. The user authenticated successfully (a
    /// password, a passkey, an authenticator code) but this device has NO
    /// cached AccountIdentity seed, so the canonical identity cannot be
    /// rebuilt. We MUST NOT derive it from the export key (the wrong
    /// identity) nor silently mint a fresh one (which would break every
    /// contact's pin). The UI renders the restore prompt
    /// (``RestoreIdentityPromptScreen``): another device, a passkey that
    /// carries keys, the 12-word phrase, or starting over.
    case needsRecoveryPhrase(SessionAccount)
    /// A fresh phrase is waiting to be confirmed: after a guest signup, or
    /// after the person chose to start over with a new identity (already
    /// published, without a supersession proof). The UI shows the new
    /// phrase before the session lands; ``confirmFreshPhraseSaved()``
    /// finishes it. While
    /// ``AuthRepository/freshPublishPending`` is set the words
    /// are withheld and the screen offers "Try again".
    case saveFreshPhrase(SessionAccount)
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
    /// The session pair (ADR-0065 section 3) of the most recent
    /// signup / login / recover / refresh. ``sessionToken`` is its access
    /// token, read by the repositories that attach a bearer; the token
    /// provider swaps in the current one on every request regardless.
    public let session: SessionPair

    public init(
        accountId: Data,
        exportKey: Data,
        accountIdentitySeed: Data,
        session: SessionPair
    ) {
        self.accountId = accountId
        self.exportKey = exportKey
        self.accountIdentitySeed = accountIdentitySeed
        self.session = session
    }

    /// The access token of ``session``.
    public var sessionToken: String { session.sessionToken }
    /// The access token's expiry as a unix-millisecond timestamp.
    public var sessionExpiresAtMs: Int64 { session.sessionExpiresAtMs }
}

/// Result handed back to the signup ViewModel so it can show the
/// freshly-generated 12-word recovery phrase.
public struct SignupOutcome: Equatable {
    /// The 12-word BIP39 recovery phrase. The UI MUST display this
    /// to the user and require the three-word confirmation before
    /// navigating away. The device keeps a copy (``DevicePhrase``):
    /// Settings > Security > Recovery shows it again after the device
    /// confirmed its owner.
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
    /// EMPTY `Data()` after an authenticator code or a passkey: no
    /// password was involved, so there is no export key and no device
    /// root key yet — the completion paths bootstrap one from the
    /// verified identity seed (`bootstrapDeviceKeyFromVerifiedSeedIfAbsent`).
    let exportKey: Data
    /// The pair the sign-in issued. The provider may have refreshed it
    /// since; ``currentSession(_:)`` reads the live one.
    let session: SessionPair
    let username: String

    var sessionToken: String { session.sessionToken }

    /// The provider's current pair for this account, else the one parked.
    func currentSession(_ provider: SessionTokenProvider) -> SessionPair {
        guard provider.account == accountIdB64, let live = provider.current else { return session }
        return live
    }
}

/// ADR-0017 lost-everything fallback — the completion context stashed by
/// ``AuthRepository/mintFreshIdentity()`` between minting the fresh identity
/// (already published) and the user acknowledging the new phrase. Holds no
/// password material; `seed` is the just-minted fresh AccountIdentity seed.
struct PendingFreshPhrase {
    let seed: Data
    let accountIdBytes: Data
    let accountIdB64: String
    /// The pair the reset (or the guest signup) issued.
    let session: SessionPair
    let username: String
    let phrase: String
    /// `true` while the new keys are not published yet (ADR-0065 section
    /// 2.3: the directory upload, this device's KeyPackage or the
    /// DeviceGroup journal is still owed). The phrase is not shown and the
    /// session does not land until it is `false`. Always `false` for a
    /// guest signup, which publishes at creation.
    var publishOwed: Bool = false

    var sessionToken: String { session.sessionToken }

    /// The provider's current pair for this account, else the one parked.
    func currentSession(_ provider: SessionTokenProvider) -> SessionPair {
        guard provider.account == accountIdB64, let live = provider.current else { return session }
        return live
    }
}

/// Why the restore prompt opened after a passkey sign-in: its first line.
public enum RestoreNotice: Equatable, Sendable {
    /// The passkey signed in and carries no keys (no PRF output, or nothing
    /// stored for it).
    case passkeyNoKeys
    /// Keys are stored for the passkey but did not open, or the phrase they
    /// gave is not the account's. Nothing was changed.
    case passkeyUnusable

    /// The catalog key of the line.
    public var catalogKey: String {
        switch self {
        case .passkeyNoKeys: return "restore_identity.from_passkey_no_keys"
        case .passkeyUnusable: return "restore_identity.from_passkey_unusable"
        }
    }
}

/// Why the local three-word route refused before or after its check.
public enum ThreeWordsError: Error, Equatable {
    /// The route is not offered on this device (no cached account with a
    /// username, no stored phrase, or no keys).
    case unavailable
    /// The words matched, but the sign-in answered for another account than
    /// the one cached here. Nothing was adopted.
    case rejected
    /// The words matched, but the server refused the stored phrase as the
    /// account's recovery credential and nothing is owed here (the phrase
    /// may have been replaced on another device). The three-word route was
    /// suspended on this device and its budget cleared; the phrase and
    /// everything else were kept.
    case suspended
}

/// The restore prompt's passkey row was used with no sign-in parked.
public struct RestoreNothingPending: Error {
    public init() {}
}

/// The passkey that answered belongs to another account than the one
/// signed in on this device.
public struct PasskeyWrongAccount: Error {
    public init() {}
}

/// The session is not ready for the operation (no session, a guest
/// account, or no keys on this device).
public struct SessionNotReady: Error {
    public init() {}
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
        Localization.text("restore_identity.error.phrase_mismatch")
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
    /// Why the check could not run; the screens pick their sentence by it.
    public enum Kind: Equatable, Sendable {
        /// The directory answered 404: no published identity.
        case missing
        /// The directory could not be reached.
        case unreachable
    }

    public let kind: Kind
    private let message: String
    public init(_ message: String, kind: Kind = .unreachable) {
        self.message = message
        self.kind = kind
    }
    /// A diagnostic; screens render ``kind`` from the catalog.
    public var errorDescription: String? { message }
}
