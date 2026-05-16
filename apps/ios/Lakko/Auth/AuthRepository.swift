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

        // Stash the export key + session token for ContactsClient /
        // ChatRepository / DiscoveryClient. The bytes survive until
        // logout / forgetDevice clears the publisher; signup is a
        // one-shot ceremony so we don't expect any surprises here.
        identitySecrets = IdentitySecrets(
            accountId: result.accountId,
            exportKey: result.exportKey,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs
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
        // Phase 2a1c — post-signup backup bootstrap. Caches the
        // phrase in the Keychain + posts a baseline /backup
        // envelope. Best-effort end-to-end; errors are logged
        // and swallowed so a transient transport hiccup does
        // not block signup from completing.
        Task {
            await Self.bootstrapBackupBestEffort(
                accountId: result.accountId,
                accountIdB64: accountIdB64,
                exportKey: result.exportKey,
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
        sessionStore.save(cached) // idempotent upsert — schema-versioned

        identitySecrets = IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: result.exportKey,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs
        )
        state = .session(
            SessionAccount(
                accountIdB64: cached.accountIdB64,
                username: cached.username
            )
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

        identitySecrets = IdentitySecrets(
            accountId: cachedAccountBytes,
            exportKey: exportKey,
            sessionToken: assertion.sessionToken,
            sessionExpiresAtMs: assertion.sessionExpiresAtMs
        )
        state = .session(
            SessionAccount(
                accountIdB64: cached.accountIdB64,
                username: cached.username
            )
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
        // Phase 2a2c2 — refresh the cached export key under the
        // freshly-rotated device key. Any prior cached value was
        // bound to the OLD password; the upsert here overwrites
        // it cleanly. Best-effort.
        do {
            try ExportKeyStore.putExportKey(
                accountIdB64: cached.accountIdB64,
                exportKey: result.exportKey
            )
        } catch {
            NSLog("export-key persist (recover) failed: \(error)")
        }
        sessionStore.save(cached) // idempotent upsert

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
                accountIdB64: cached.accountIdB64,
                blob: restoredSnapshot
            )
        }

        identitySecrets = IdentitySecrets(
            accountId: accountIdBytes,
            exportKey: result.exportKey,
            sessionToken: result.sessionToken,
            sessionExpiresAtMs: result.sessionExpiresAtMs
        )
        state = .session(
            SessionAccount(
                accountIdB64: cached.accountIdB64,
                username: cached.username
            )
        )

        // Phase 2a1c — post-recover backup bootstrap: re-cache
        // the phrase under the freshly-rotated device root key
        // and post a baseline /backup envelope that re-publishes
        // the restored snapshot (so the next device-recover
        // sees the same state we just rebuilt). Best-effort,
        // fire-and-forget — the state flip above already drove
        // the UI; bootstrap is housekeeping.
        Task {
            await Self.bootstrapBackupBestEffort(
                accountId: accountIdBytes,
                accountIdB64: cached.accountIdB64,
                exportKey: result.exportKey,
                sessionToken: result.sessionToken,
                phrase: trimmedPhrase,
                context: "recover",
                wholeStorageSnapshot: restoredSnapshot
            )
        }
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
            "backup restore (\(context)): restored \(parsed.contacts.count) contact(s) "
            + "from snapshot at \(parsed.createdAtMs), "
            + "MLS snapshot \(parsed.wholeStorageSnapshot.count) bytes"
        )
        // TODO(2a1d-followup): once the iOS persistent contact
        // store lands, apply `parsed.contacts` here via a
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
    /// `ed25519Seed` is derived from `exportKey` via the same
    /// `Identity.fromSeed` flow `ChatRepository` already uses
    /// — re-derive here rather than threading the seed through
    /// the auth-repository public surface so this helper stays
    /// self-contained.
    private static func bootstrapBackupBestEffort(
        accountId: Data,
        accountIdB64: String,
        exportKey: Data,
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

        // Derive the ed25519 signing seed deterministically from
        // the OPAQUE export key — same `Identity.fromSeed` path
        // every other identity-bearing operation uses.
        let identity: Identity
        do {
            identity = try Identity.fromSeed(seed: exportKey)
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
        sessionToken: String,
        sessionExpiresAtMs: Int64
    ) {
        self.accountId = accountId
        self.exportKey = exportKey
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
