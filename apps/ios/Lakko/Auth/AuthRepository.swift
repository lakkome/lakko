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
            // ADR-0074 — every state that holds a session names this device
            // in it, and a session with keys records the account key at auth
            // until that has succeeded once. One hook for every landing: the
            // relaunch restore, passkey, pairing, the phrase completion.
            switch state {
            case .session(let account), .needsRecoveryPhrase(let account), .saveFreshPhrase(let account):
                sessionLanded(accountIdB64: account.accountIdB64)
            case .loading, .anon:
                break
            }
            // Whether the account now signed in is a guest account
            // (``guestAccount``) is a fact about the session, so it follows
            // every change of it.
            refreshGuestAccount()
            if case .anon = state {} else if guestAccountDeleted { guestAccountDeleted = false }
        }
    }

    /// ADR-0073 section 3.5 — this device's keys are not the account's keys
    /// any more: the keys were reset on another device (the automatic wipe
    /// runs), or the published identity differs at the same generation (the
    /// wipe runs on the tap). Replaces the whole app while set; `nil`
    /// otherwise. Cleared by "Sign in" on the screen.
    @Published public private(set) var keysSignOut: KeysSignOutState?

    /// Why ``keysSignOut`` is set: the keys were reset elsewhere (ADR-0073),
    /// or this device was removed from the account (ADR-0074 section 2.3).
    /// The screen's copy differs; the wipe is the same full one.
    @Published public private(set) var keysSignOutCause: DeviceSignOutCause = .keysReset

    /// The username the signed-out screen names (`@name`), captured when
    /// ``keysSignOut`` is set: the session record it would read is gone
    /// once the wipe ran. Empty for a guest account.
    public private(set) var keysSignOutUsername: String = ""

    /// The line the signed-out screen adds under its body, as a catalog key,
    /// or `nil`: a recall that ended because the tries ran out, or because
    /// the time passed while the form was open (UX 18.4). Cleared with the
    /// screen.
    public private(set) var keysSignOutNoticeKey: String?

    /// ADR-0074 section 1.1 / UX 18.4 — "Was it you?": the account's keys
    /// were reset on another device, this device deleted what the account
    /// kept on it and holds a recall record. Replaces the whole app while
    /// set, at launch too (it is read back from the record). `nil` otherwise.
    @Published public private(set) var recall: RecallScreenState?

    /// UX 18.4, "Done" — the account was taken back on this device in this
    /// session: one line at the top of the chat list. In memory only.
    @Published public private(set) var accountRecalledThisSession: Bool = false

    /// UX 15.3 — the keys were reset on THIS device in this session: the
    /// Devices and Passkeys screens show their one-session note. In memory
    /// only.
    @Published public private(set) var keysResetThisSession: Bool = false

    /// ADR-0073 section 3.3 — the refresh credential was refused as invalid:
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
    /// KeyPackage or the DeviceGroup journal is still owed; ADR-0073 section
    /// 2.3, UX 15.4). The phrase screen then shows an error and "Try again"
    /// instead of the words; ``retryFreshPublish()`` clears it.
    @Published public private(set) var freshPublishPending: Bool = false

    /// `true` when the owed publish may not run until the person signs in
    /// again: the stored attestation is older than the 30-day bound
    /// (ADR-0073 section 4.2, UX 15.4).
    @Published public private(set) var freshPublishNeedsSignIn: Bool = false

    /// ADR-0075 — whether the fresh identity parked on the phrase screen
    /// shows its words: `false` for an account that is not phrase-protected,
    /// whose only screen there is the "finishing" one while a publish is
    /// owed.
    @Published public private(set) var freshShowsWords: Bool = true

    /// What this device knows about the signed-in account as a GUEST account
    /// (`docs/ux/call-links.md` section 9, ``GuestAccountMarker``): made to
    /// join a call from a link, with no username and no password of the
    /// person's choosing. `nil` for an ordinary account and while signed out.
    ///
    /// Derived from the marker and the session: recomputed on every change of
    /// ``state`` and after every write of the marker.
    @Published public private(set) var guestAccount: GuestAccountState?

    /// The session is a guest account. A guest never lands in the app: it is
    /// in its call, or it is asked whether to keep the account.
    public var isGuest: Bool { guestAccount != nil }

    /// The username an unfinished ``claimGuestAccount(username:password:)``
    /// already took for this guest account, or `nil`. The claim is not a
    /// rename, so the form shows it and does not ask again.
    public var guestClaimedUsername: String? { guestAccount?.claimedUsername }

    /// A guest account was just deleted on this device
    /// (``discardGuestAccount()``). The call-link landing says so. Cleared by
    /// the next session landing.
    @Published public private(set) var guestAccountDeleted = false

    /// A guest account was just kept (``claimGuestAccount(username:password:)``):
    /// "Protect your account" comes before the app, exactly as after a
    /// sign-up (ADR-0075 D2). The recovery phrase is not shown. In memory
    /// only; cleared by ``dismissProtectStep()`` and with the session.
    @Published public private(set) var protectStepOwed = false

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

    /// ADR-0075 — what this device keeps about an account's protection: the
    /// phrase-saved flag the three-word surfaces are gated on, the last
    /// status read. Flags only.
    let protectionStore: AccountProtectionStore

    /// ADR-0075 — a session with keys landed for `accountIdB64`: the
    /// composition root reads the account's protection and its notices
    /// (``AccountProtectionCenter``). `nil` in a repository nobody wired, so
    /// no request leaves it.
    var onProtectedSessionLanded: (@MainActor (_ accountIdB64: String) -> Void)?

    /// A guest account chose a username on this device: the composition
    /// root fetches the username certificate and re-publishes the profile.
    var onUsernameClaimed: (@MainActor (_ account: SessionAccount) -> Void)?

    /// ADR-0076 — the account's username changed (renamed here, or a newer
    /// name arrived from the account's other devices): the composition root
    /// replaces the cached username certificate (`certificate`, or a fetch
    /// when `nil`) and re-publishes the profile, the Everyone record and
    /// terms, the introduction offer and the member cards.
    var onUsernameRenamed: (@MainActor (_ account: SessionAccount, _ certificate: Data?) -> Void)?

    /// Write the own-username register (the synced state). `false` when the
    /// state is not loaded yet. Wired by the composition root.
    var storeOwnUsername: (@MainActor (_ username: String) async -> Bool)?

    /// The recovery address this account's devices keep, or `nil`. Wired by
    /// the composition root.
    var heldRecoveryAddress: (@MainActor () -> String?)?

    /// ADR-0075 — this device changed the account's protection (it saved a
    /// phrase): the composition root re-reads the status.
    var onProtectionChanged: (@MainActor () -> Void)?

    /// ADR-0074 section 0.4 — this device's id for an account (its
    /// conversation leaf key), or `nil` when the device identity cannot be
    /// read or minted. Injectable for tests.
    typealias OwnDeviceId = @MainActor (_ accountIdB64: String) -> Data?

    private let ownDeviceId: OwnDeviceId

    /// The production device id: the public key of the per-account device
    /// identity seed (``DeviceIdentityStore``), minted on first use, which is
    /// the key of this device's conversation leaves and of its directory row.
    static let storedOwnDeviceId: OwnDeviceId = { accountIdB64 in
        do {
            let seed = try DeviceIdentityStore.ensureDeviceIdentitySeed(accountIdB64: accountIdB64)
            return try Identity.fromSeed(seed: seed).signaturePublicKey()
        } catch {
            NSLog("[auth] WARN this device's identity could not be read; the session stays unbound")
            return nil
        }
    }

    /// ADR-0074 section 0.1 — the per-account "account key recorded at
    /// auth" mark.
    private let identityKeyMarks: IdentityKeyRecordMarks

    /// ADR-0074 section 2.3 — the owed server half of Log out.
    private let logoutOwed: LogoutOwedStoring

    /// A bind is running for this account (one at a time).
    private var deviceBindInFlight: String?
    /// A genesis mint is running for this account.
    private var identityKeyRecordInFlight: String?

    /// Called once the session was re-issued naming this device, so what a
    /// device-bound session is needed for (the KeyPackage publish) can run
    /// again. Wired by the composition root.
    var onSessionDeviceBound: (@MainActor () -> Void)?

    /// ADR-0073 section 3.3 — the one token provider every REST client's
    /// request passes through. The repository installs the pair on every
    /// landing, clears it on sign-out, and answers its three callbacks.
    private let tokenProvider: SessionTokenProvider

    /// The server step of a key reset (`sessionToken`, the reset grant,
    /// `seed`, the new account key):
    /// ``AuthClient/resetAccountKeys(sessionToken:freshGrant:seed:newSignaturePubkey:)``
    /// in the app; injectable because the OPAQUE exchange cannot be
    /// answered by a scripted server.
    typealias KeyResetOperation = (
        _ sessionToken: String, _ freshGrant: String, _ seed: Data, _ newSignaturePubkey: Data
    ) async throws -> KeyResetResult

    private let keyResetOperation: KeyResetOperation

    /// The password login the Reset account keys screen runs to obtain a
    /// reset grant (`handle`, `password`): ``AuthClient/login(handle:password:)``
    /// in the app; injectable for the same reason as ``KeyResetOperation``.
    typealias ProofLoginOperation = (_ handle: Data, _ password: Data) async throws -> LoginResult

    private let proofLoginOperation: ProofLoginOperation

    /// The password step of keeping a guest account (`handle`, the 64-byte
    /// seed, the new password): ``AuthClient/recover(handle:mnemonicSeed:newPassword:)``
    /// in the app; injectable for the same reason as ``KeyResetOperation``.
    typealias FirstPasswordOperation = (
        _ handle: Data, _ seed: Data, _ newPassword: Data
    ) async throws -> RecoverResult

    private let firstPasswordOperation: FirstPasswordOperation

    /// The fresh-auth proof of the last password or passkey sign-in, signup
    /// or proof block (ADR-0073, amended 2026-10-06). In memory only: never
    /// persisted, never logged.
    private var freshAuth = FreshAuthHolder()

    /// How this session was landed (UX 17.6), in memory with the session.
    /// `nil` when no sign-in ran in this process (a relaunch restore).
    public private(set) var sessionLanding: SessionLanding?

    /// When the local three-word check last passed in front of a password
    /// change (``PasswordChangeGate``). In memory only.
    private var threeWordsPassedAtMs: Int64?

    /// When `It was not me` was answered on an account notice (UX 19.6). In
    /// memory only.
    private var noticeAnsweredAtMs: Int64?

    /// A resume of an owed rename is running.
    private var renameResumeInFlight = false

    /// Whether the account behind `sessionToken` has a passkey, or `nil`
    /// when it could not be read. Injectable for tests.
    typealias PasskeyPresence = (_ sessionToken: String) async -> Bool?

    private let passkeyPresence: PasskeyPresence

    /// The clock the reset grant's expiry is read against.
    private let grantNowMs: () -> Int64

    /// The attestation check against the pinned attest key.
    private let attestationVerifier: KeyResetAttestationVerifying

    /// The two publish steps of a reset that live outside this repository
    /// (ADR-0073 section 2.3 steps 4 and 5). The composition root wires the
    /// production ones (``AppServices``); a test injects recorders.
    struct KeyResetHooks {
        /// Re-publish this device's KeyPackage with a binding signed by the
        /// NEW account key. Throws when it did not land.
        var republishOwnKeyPackage: @MainActor (IdentitySecrets) async throws -> Void
        /// Re-found the DeviceGroup under the NEW account seed, which names
        /// a new journal (ADR-0078 section 6), and claim it. Throws when the
        /// claim did not land.
        var refoundDeviceGroup: @MainActor (
            _ accountIdBytes: Data, _ accountIdB64: String, _ accountSeed: Data
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
            refoundDeviceGroup: { accountIdBytes, accountIdB64, accountSeed in
                try await DeviceGroupDsAccess.refoundAfterKeyReset(
                    accountIdBytes: accountIdBytes,
                    accountIdB64: accountIdB64,
                    accountSeed: accountSeed,
                    ds: DsClient())
            }
        )
    }

    var keyResetHooks: KeyResetHooks

    /// The full forget-device wipe the signed-out screen runs by itself
    /// (ADR-0073 section 3.5): every local store of the account, then the
    /// session ends. The composition root replaces it with
    /// ``AppServices/wipeAccountLocalData(accountIdB64:)``, which also lets
    /// the runtimes go first; the default wipes the stores and ends the
    /// session here. Nothing is published first and no call needs the
    /// superseded token.
    var wipeAfterKeyReset: @MainActor (_ accountIdB64: String) async -> Void

    /// ADR-0074 section 1.1 — the STAGED wipe: the same deletion, except the
    /// recall record. Injectable like ``wipeAfterKeyReset``; the composition
    /// root points it at ``AppServices/stagedWipeAfterKeyReset(accountIdB64:)``.
    var stagedWipeAfterKeyReset: @MainActor (_ accountIdB64: String) async -> Void

    /// The recall record's store (the app-private Keychain in production).
    private let recallRecords: RecallRecordStoring
    /// The recall's server operations.
    private let recallOperations: RecallOperations
    /// A status read is running.
    private var recallStatusInFlight = false
    /// A take-back attempt is running.
    private var recallAttemptInFlight = false

    // MARK: ADR-0074 part 3 — a rotation of the account keys, the devices kept

    /// A kept device is fetching the account's new keys: its session was
    /// refused as of an old key generation and it has not yet adopted them
    /// (UX 18.3: "Updating keys..." where "Synchronizing..." would be; the
    /// chat list stays, sends wait). Never a full-screen state.
    @Published public private(set) var keysUpdating: Bool = false

    /// This device adopted keys another device changed: the one dismissible
    /// line at the top of the chat list (UX 18.3), kept across launches until
    /// dismissed.
    @Published public private(set) var keysRotatedNotice: Bool = false

    /// The pending `key-rotation` envelopes of a kept device.
    private let rotationPending: KeyRotationPendingStoring
    /// The account's own old seed(s), kept after a rotation.
    private let ownPredecessors: OwnPredecessorStoring
    private let rotatedNotice: KeysRotatedNoticeStore
    /// The rotating device's owed record (read here only for "is a publish
    /// owed").
    private let rotationStore: KeyRotationStore
    /// One adoption run at a time.
    private var rotationAdoptInFlight = false
    /// The retry of an adoption that got no verdict.
    private var rotationAdoptRetry: Task<Void, Never>?

    /// The session upgrade of a kept device (`refreshToken`, the account id,
    /// the NEW account identity's Ed25519 seed):
    /// ``AuthClient/upgradeSession(refreshToken:accountId:newAccountEd25519Seed:)``
    /// in the app; injectable so the flow runs under a scripted server.
    typealias SessionUpgradeOperation = (
        _ refreshToken: String, _ accountId: Data, _ newAccountEd25519Seed: Data
    ) async throws -> SessionPair

    private let sessionUpgradeOperation: SessionUpgradeOperation

    /// ADR-0074 section 3.3 — read this account's DeviceGroup journal to its
    /// head once (``DeviceGroupSyncRepository/waitForJournalHead(timeoutMs:)``);
    /// `false` when it could not be read. Wired by the composition root; the
    /// default reads nothing.
    var readDeviceGroupJournalToHead: @MainActor (_ accountIdB64: String) async -> Bool = { _ in false }

    /// The account's keys were rotated and this device holds the new ones
    /// (it rotated them, or it adopted them): what derives from the account
    /// seed outside this repository is re-keyed (the chat repository's
    /// account identity, the reach keys and the mailbox, this device's
    /// KeyPackage row). `predecessorSeed` is the OLD 64-byte seed. Wired by
    /// the composition root.
    var onAccountKeysRotated: (@MainActor (_ secrets: IdentitySecrets, _ predecessorSeed: Data) async -> Void)?

    /// How this repository builds the backup client of its restore, its
    /// first backup and its re-seal after a rotation. A seam: the production
    /// client carries default transports to storage and auth.
    static var makeBackupClient: () -> BackupClient = { BackupClient() }
    /// Where a phrase restore fetches the account backup from
    /// (``restoreBackupFromPhrase(accountId:accountIdB64:sessionToken:phrase:context:writes:)``).
    /// Injectable for tests.
    private let backupRestoreSource: BackupRestoreSource

    /// The Keychain cache of the recovery phrase: what the automatic backup
    /// seals under, and what a later attempt at an owed restore opens with.
    private let phraseStore: PhraseStore

    /// Where the backup bookkeeping lives (``BackupRestoreRetryStore``,
    /// ``RestoredKnownGroupsStore``). Injectable for tests.
    private let backupDefaults: UserDefaults

    /// Accounts whose owed backup restore is being attempted right now, so
    /// two landings close together make one attempt, not two.
    private var backupRestoreRetryInFlight: Set<String> = []

    /// The guest marker (plain `UserDefaults`).
    private let guestMarker: GuestAccountMarker

    /// Registers this device for push for `accountIdB64`
    /// (``PushSubscriber/subscribe(accountIdB64:stillSignedIn:)``).
    /// Injectable so a test can see which landings register and which do not:
    /// a guest landing must not.
    private let pushSubscribe: PushSubscribe

    /// The push registration as the repository calls it.
    typealias PushSubscribe = @MainActor (
        _ accountIdB64: String, _ stillSignedIn: @escaping @MainActor () -> Bool
    ) async throws -> Void

    /// The live push registration.
    static let livePushSubscribe: PushSubscribe = { accountIdB64, stillSignedIn in
        _ = try await PushSubscriber.shared.subscribe(
            accountIdB64: accountIdB64, stillSignedIn: stillSignedIn)
    }

    /// What ending a guest account needs from outside this repository: the
    /// two server deletes and the local wipe. The composition root sets the
    /// wipe (`LakkoApp`).
    var guestEnvironment = GuestAccountEnvironment()

    /// Unix milliseconds now. A guest account's age is measured with it.
    private let clock: () -> Int64

    /// Remove every local store of one account (``AccountLocalDataWipe``):
    /// what a launch runs for a guest account it will not restore. No runtime
    /// is bound to an account at launch, so the store wipe is the whole wipe
    /// there. Injectable for tests.
    private let wipeAccountStores: @MainActor (_ accountIdB64: String) -> Void

    /// The one expiry of a guest account in flight, so the offer appearing
    /// and `Create account` pressed at the same moment delete it once.
    private var guestExpiryInFlight: Task<Bool, Never>?

    /// Advanced whenever the account this device is bound to changes: every
    /// session landing (``adoptIdentitySecrets(_:context:)``), every parking
    /// in ``AuthState/needsRecoveryPhrase(_:)``, and every sign-out
    /// (``logout()``, ``endSessionAfterLocalWipe()``). A sign-in or restore
    /// captures it before its first `await` and checks it after each one, so
    /// a round trip that outlives a sign-out, or that another account's
    /// landing overtook, lands nothing. Work outside this repository that
    /// belongs to the session captures it too.
    let binding = BindingEpoch()

    /// The live `UNUserNotificationCenter` removal.
    nonisolated static func removeAllDeliveredNotifications() {
        UNUserNotificationCenter.current().removeAllDeliveredNotifications()
    }

    public convenience init(
        sessionStore: SessionStore = SessionStore(),
        authClientFactory: @escaping () -> AuthClient = { AuthClient() },
        persistence: SessionPersistence = KeychainSessionPersistence(),
        nowMs: @escaping () -> Int64 = SessionRestore.nowMs
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
        nowMs: @escaping () -> Int64,
        accountDirectory: AccountDirectory,
        loginHandleBinds: LoginHandleBindLedger = .shared,
        restoreDirectory: DeviceRestoreDirectory = DiscoveryDeviceRestoreDirectory(),
        pushBinding: PushAccountBindingStore = .live,
        removeDeliveredNotifications: @escaping () -> Void = AuthRepository.removeAllDeliveredNotifications,
        passkeyCeremony: @escaping PasskeyCeremony = AuthRepository.livePasskeyCeremony,
        markers: RecoveryMarkers = RecoveryMarkers(),
        protectionStore: AccountProtectionStore = AccountProtectionStore(),
        tokenProvider: SessionTokenProvider = .shared,
        keyResetOperation: KeyResetOperation? = nil,
        proofLoginOperation: ProofLoginOperation? = nil,
        firstPasswordOperation: FirstPasswordOperation? = nil,
        passkeyPresence: PasskeyPresence? = nil,
        grantNowMs: @escaping () -> Int64 = SessionRestore.nowMs,
        attestationVerifier: KeyResetAttestationVerifying = PinnedKeyResetAttestationVerifier(),
        keyResetHooks: KeyResetHooks = .production,
        wipeAfterKeyReset: (@MainActor (String) async -> Void)? = nil,
        stagedWipeAfterKeyReset: (@MainActor (String) async -> Void)? = nil,
        recallRecords: RecallRecordStoring = KeychainRecallRecordStore(),
        recallOperations: RecallOperations? = nil,
        ownDeviceId: OwnDeviceId? = nil,
        identityKeyMarks: IdentityKeyRecordMarks = IdentityKeyRecordMarks(),
        logoutOwed: LogoutOwedStoring = KeychainLogoutOwedStore(),
        rotationPending: KeyRotationPendingStoring = KeychainKeyRotationPendingStore(),
        ownPredecessors: OwnPredecessorStoring = KeychainOwnPredecessorStore(),
        rotatedNotice: KeysRotatedNoticeStore = KeysRotatedNoticeStore(),
        rotationStore: KeyRotationStore = KeyRotationStore(),
        sessionUpgradeOperation: SessionUpgradeOperation? = nil,
        backupRestoreSource: BackupRestoreSource = LiveBackupRestoreSource(),
        phraseStore: PhraseStore = PhraseStore(),
        backupDefaults: UserDefaults = .standard,
        markerDefaults: UserDefaults = .standard,
        pushSubscribe: PushSubscribe? = nil,
        wipeAccountStores: (@MainActor (_ accountIdB64: String) -> Void)? = nil
    ) {
        self.rotationPending = rotationPending
        self.ownPredecessors = ownPredecessors
        self.rotatedNotice = rotatedNotice
        self.rotationStore = rotationStore
        self.sessionUpgradeOperation = sessionUpgradeOperation ?? { refreshToken, accountId, seed in
            try await authClientFactory().upgradeSession(
                refreshToken: refreshToken, accountId: accountId, newAccountEd25519Seed: seed)
        }
        self.sessionStore = sessionStore
        self.authClientFactory = authClientFactory
        self.persistence = persistence
        self.accountDirectory = accountDirectory
        self.restoreDirectory = restoreDirectory
        self.loginHandleBinds = loginHandleBinds
        self.pushBinding = pushBinding
        self.removeDeliveredNotifications = removeDeliveredNotifications
        self.backupRestoreSource = backupRestoreSource
        self.phraseStore = phraseStore
        self.backupDefaults = backupDefaults
        self.guestMarker = GuestAccountMarker(defaults: markerDefaults)
        self.pushSubscribe = pushSubscribe ?? Self.livePushSubscribe
        self.clock = nowMs
        self.wipeAccountStores = wipeAccountStores ?? { accountIdB64 in
            AccountLocalDataWipe.wipe(accountIdB64: accountIdB64)
        }
        self.passkeyCeremony = passkeyCeremony
        self.markers = markers
        self.protectionStore = protectionStore
        self.ownDeviceId = ownDeviceId ?? AuthRepository.storedOwnDeviceId
        self.identityKeyMarks = identityKeyMarks
        self.logoutOwed = logoutOwed
        self.tokenProvider = tokenProvider
        self.keyResetOperation = keyResetOperation ?? { sessionToken, freshGrant, seed, newKey in
            try await authClientFactory().resetAccountKeys(
                sessionToken: sessionToken, freshGrant: freshGrant, seed: seed, newSignaturePubkey: newKey)
        }
        self.proofLoginOperation = proofLoginOperation ?? { handle, password in
            try await authClientFactory().login(handle: handle, password: password)
        }
        self.firstPasswordOperation = firstPasswordOperation ?? { handle, seed, newPassword in
            try await authClientFactory().recover(handle: handle, mnemonicSeed: seed, newPassword: newPassword)
        }
        self.passkeyPresence = passkeyPresence ?? { sessionToken in
            let client = WebauthnClient()
            client.setSessionToken(sessionToken)
            return (try? await client.listCredentials()).map { !$0.credentials.isEmpty }
        }
        self.grantNowMs = grantNowMs
        self.attestationVerifier = attestationVerifier
        self.keyResetHooks = keyResetHooks
        self.wipeAfterKeyReset = wipeAfterKeyReset ?? { _ in }
        // A caller that injected its own full wipe and no staged one gets
        // the same closure for both: it owns what "wipe" means.
        self.stagedWipeAfterKeyReset = stagedWipeAfterKeyReset ?? wipeAfterKeyReset ?? { _ in }
        self.recallRecords = recallRecords
        self.recallOperations = recallOperations ?? .live(authClientFactory)
        if wipeAfterKeyReset == nil {
            self.wipeAfterKeyReset = { [weak self] accountIdB64 in
                AccountLocalDataWipe.wipe(accountIdB64: accountIdB64)
                self?.endSessionAfterLocalWipe()
            }
            if stagedWipeAfterKeyReset == nil {
                self.stagedWipeAfterKeyReset = { [weak self] accountIdB64 in
                    AccountLocalDataWipe.wipe(accountIdB64: accountIdB64, keeping: [.recallRecord])
                    self?.endSessionAfterLocalWipe()
                }
            }
        }
        wireTokenProvider()
        rehydrate(nowMs: nowMs())
        restoreRecall(nowMs: nowMs())
        retryOwedLogout()
        restoreKeysRotatedNotice()
    }

    /// The provider's refresher and its three callbacks (ADR-0073 section
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
            // ADR-0074 section 3.3 — a superseded generation is a rotation
            // when this device holds (or finds) the account's new keys, and
            // only otherwise a reset.
            Task { @MainActor in self?.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil) }
        }
        tokenProvider.onRefreshInvalid = { [weak self] accountIdB64 in
            Task { @MainActor in self?.handleRefreshInvalid(accountIdB64: accountIdB64) }
        }
        tokenProvider.onDeviceRevoked = { [weak self] accountIdB64 in
            Task { @MainActor in self?.handleDeviceRevoked(accountIdB64: accountIdB64) }
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
        let decision = SessionRestore.decide(marker: marker, persistence: persistence, nowMs: nowMs)
        // A guest account (``GuestAccountMarker``) is never left as a cached,
        // signed-out account, and never outlives its day: nobody knows its
        // password, so a guest that cannot be restored, or is too old, leaves
        // the device here, before any session state lands.
        if let marker, let guest = guestMarker.state(accountIdB64: marker.accountIdB64) {
            var restorableToken: String?
            var restorablePair: SessionPair?
            if case .session(_, let secrets) = decision {
                restorableToken = secrets.sessionToken
                restorablePair = secrets.session
            }
            switch guestRestoreAction(guest: guest, restorable: restorableToken != nil, nowMs: nowMs) {
            case .none:
                break
            case .wipe:
                NSLog("[guest] a guest account that cannot be restored is removed from this device")
                wipeGuestAtLaunch(accountIdB64: marker.accountIdB64)
                return
            case .discard:
                NSLog("[guest] a guest account past its lifetime is deleted")
                // The state stays `.loading` until the servers were asked
                // and the device is wiped, so no screen of the session
                // renders in between. The server calls are read when the
                // task runs: the composition root has wired them by then.
                let accountIdB64 = marker.accountIdB64
                let sessionToken = restorableToken ?? ""
                // The persisted access token may have run out: with the pair
                // installed, the two deletes below are refreshed like any
                // other bearer call (ADR-0073 section 3.3).
                // ``wipeGuestAtLaunch(accountIdB64:)`` clears it again.
                if let restorablePair {
                    tokenProvider.install(restorablePair, accountIdB64: accountIdB64)
                }
                Task { [weak self] in
                    guard let self else { return }
                    let environment = self.guestEnvironment
                    let factory = self.authClientFactory
                    try? await GuestServerDelete.run(
                        guest: guest,
                        strict: false,
                        ports: GuestServerDelete.Ports(
                            deleteAtAuth: { try await environment.deleteAccountAtAuth(sessionToken, nil) },
                            deleteInDirectory: { try await environment.deleteAccountInDirectory(sessionToken) },
                            releaseHandle: {
                                if let release = environment.releaseLoginHandle {
                                    try await release(sessionToken, nil)
                                } else {
                                    _ = try await factory().removeLoginHandle(sessionToken: sessionToken)
                                }
                            }
                        ))
                    // An explicit sign-in that completed meanwhile owns the
                    // state; the guest's stores still go.
                    self.wipeGuestAtLaunch(accountIdB64: accountIdB64)
                }
                return
            }
        }
        switch decision {
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
            // ADR-0073 sections 3.3 and 4.2 — the silent restore refreshes
            // instead of reusing the persisted access token (a reset
            // elsewhere answers `key-generation-superseded` here, at
            // launch), then checks the published identity before any
            // upload. No upload on a silent restore.
            Task { [weak self] in
                guard let self else { return }
                switch await self.tokenProvider.refresh() {
                case .superseded, .revoked: return
                case .refreshed, .invalid, .noCredential, .failed: break
                }
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
            // A session landing: a phrase restore that is still owed gets
            // one attempt here.
            retryBackupRestoreIfMarked(
                accountId: secrets.accountId,
                sessionToken: secrets.sessionToken,
                context: "relaunch"
            )
        }
    }

    /// Remove a guest account the launch will not restore: every local store
    /// of it (the wipe Forget device uses, the cached-account record
    /// included), then land signed out with no cached username.
    private func wipeGuestAtLaunch(accountIdB64: String) {
        persistence.deleteSessionToken(accountIdB64: accountIdB64)
        if sessionStore.load()?.accountIdB64 == accountIdB64 { sessionStore.clear() }
        if tokenProvider.account == accountIdB64 { tokenProvider.clear() }
        wipeAccountStores(accountIdB64)
        if case .loading = state {
            binding.advance()
            identitySecrets = nil
            state = .anon(cachedUsername: nil)
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
            context: "refresh",
            rebinds: false
        )
    }

    /// The refresh credential was refused as invalid (ADR-0073 section 3.3):
    /// the device goes to the sign-in screen with the session-expired line.
    /// Nothing is wiped: the keys are still this account's.
    private func handleRefreshInvalid(accountIdB64: String) {
        guard signedInAccountIdB64 == accountIdB64 else { return }
        // A guest account whose session can no longer be refreshed (the
        // servers delete an unkept guest after a day) is never left as a
        // cached, signed-out account nobody knows the password of: it leaves
        // this device, as it would at the next launch.
        if guestMarker.state(accountIdB64: accountIdB64) != nil {
            NSLog("[guest] the guest session cannot be refreshed; the guest account is removed from this device")
            guestEndPorts(accountIdB64: accountIdB64, sessionToken: "").wipe()
            guestAccountDeleted = true
            return
        }
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
    ///
    /// `rebinds` is `false` for the calls that only replace the pair or the
    /// keys of the session that is ALREADY live (a token refresh, the device
    /// bind, a proof, a password or key change): the account bound to this
    /// device did not change, so work started under the binding goes on.
    private func adoptIdentitySecrets(_ secrets: IdentitySecrets, context: String, rebinds: Bool = true) {
        if rebinds { binding.advance() }
        // The account landed: its stores are written again.
        AccountStoreGate.open(base64Encode(secrets.accountId))
        identitySecrets = secrets
        adoptSessionPair(secrets.session, accountIdB64: base64Encode(secrets.accountId), context: context)
    }

    /// Install a session pair in the token provider and persist it, so every
    /// bearer request from now on carries it (refreshed ahead of expiry) and
    /// the next launch can restore the session. Also for the states that
    /// hold a pair before ``identitySecrets`` exists (a sign-in parked
    /// without keys, the fresh phrase). Best-effort persist, as above.
    private func adoptSessionPair(_ pair: SessionPair, accountIdB64: String, context: String) {
        // A session exists again on this device: a logout still owed from
        // before must not be sent now (its floor would end this session).
        logoutOwed.clear()
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
        let writes = SignInKeyWrites()
        let client = authClientFactory()
        let deviceId = ownDeviceId
        let result = try await client.signup(
            password: password, enrolRecovery: true,
            deviceIdFor: { accountId in
                // ADR-0074 section 0.4 — signup knows the account id before
                // the finish, so the first session already names this device.
                LandingDevice.asserted(
                    accountKnownBeforeFinish: true,
                    ownDeviceId: await deviceId(base64Encode(accountId)))
            },
            onCommitting: onCommitting)
        // A fresh unstructured task does not inherit the caller's cancellation.
        return try await Task { @MainActor in
            try await self.completeSignup(result: result, username: canonical, writes: writes)
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
    ///
    /// `writes` is the route's ``SignInKeyWrites``, taken when ``signup(username:password:onCommitting:)``
    /// began (a test driving this half directly takes it here).
    func completeSignup(
        result: SignupResult,
        username: String,
        writes: SignInKeyWrites = SignInKeyWrites()
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
        // The signup finish is a fresh-auth proof (the person just chose the
        // password): the onboarding's passkey offer asks for nothing more.
        freshAuth.put(result.freshGrant, accountIdB64: base64Encode(result.accountId))
        sessionLanding = .other
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
        try writes.required(accountIdB64) {
            try DeviceKeyStore.putDeviceKey(
                accountIdB64: accountIdB64,
                rootKey: deviceRootKey
            )
        }
        // Phase 2a2c2 — cache the OPAQUE export key under the
        // Keychain so a future sign-in-with-passkey on the SAME
        // device can reconstruct the full IdentitySecrets
        // without going through the password round-trip. Best-
        // effort: a persistence failure here just means the
        // user falls back to password login on this device.
        do {
            try writes.ifNotWiped(accountIdB64) {
                try ExportKeyStore.putExportKey(
                    accountIdB64: accountIdB64,
                    exportKey: result.exportKey
                )
            }
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
            try writes.ifNotWiped(accountIdB64) {
                _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                    accountIdB64: accountIdB64
                )
            }
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
            try writes.ifNotWiped(accountIdB64) {
                try AccountIdentitySeedStore.putAccountIdentitySeed(
                    accountIdB64: accountIdB64,
                    seed: accountIdentitySeed
                )
            }
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
        DevicePhrase.store(
            phrase, accountIdB64: accountIdB64, context: "signup", since: writes.mark(accountIdB64))

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
            await self.bootstrapBackupBestEffort(
                accountId: result.accountId,
                accountIdB64: accountIdB64,
                accountIdentitySeed: accountIdentitySeed,
                sessionToken: result.sessionToken,
                phrase: phrase,
                context: "signup",
                writes: writes
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
    /// call without a login (`docs/ux/call-links.md` section 9).
    ///
    /// Reuses the audited ``signup(username:password:onCommitting:)``
    /// machinery VERBATIM minus the username claim: the SAME PoW-gated OPAQUE
    /// registration (``AuthClient/signup(password:enrolRecovery:)``) — under a
    /// RANDOM 32-byte password the user NEVER sees — plus the device-root-key
    /// / export-key / device-identity / account-identity-seed derivation, and
    /// the first-ever identity-bundle publish (with the genesis `leaf_auth`,
    /// G-30 #2). SECURITY: guest creation MUST route through this same
    /// PoW-gated signup — there is no bypass.
    ///
    /// The session then lands at once
    /// (``completeGuestSignup(result:onAccountReady:)``): no recovery-phrase
    /// screen stands between the person and the call they were invited to
    /// (2026-10-02; the first version parked the phrase and showed it first).
    /// Whether this becomes an account is asked after the call
    /// (``claimGuestAccount(username:password:)`` /
    /// ``discardGuestAccount()``); the servers delete an account that never
    /// got a username a day after it was made.
    ///
    /// - Parameter onAccountReady: called with the new account's id right
    ///   before its session lands, so what the first screen of the session
    ///   reads (the guest's own profile name) is in place when it renders.
    public func signupGuest(
        onCommitting: (@MainActor () -> Void)? = nil,
        onAccountReady: (@MainActor (_ accountIdB64: String) -> Void)? = nil
    ) async throws {
        // A 32-byte random password the user never sees — the guest HAS a real,
        // random password, so the normal signup derivations apply. `SystemRandom-
        // NumberGenerator` is the OS CSPRNG on Apple platforms.
        var rng = SystemRandomNumberGenerator()
        let randomBytes = (0..<32).map { _ in UInt8.random(in: UInt8.min...UInt8.max, using: &rng) }
        let randomPassword = Data(Data(randomBytes).base64EncodedString().utf8)
        let writes = SignInKeyWrites()

        // The SAME PoW-gated OPAQUE registration `signup` uses — no bypass
        // (ADR-0054 S4: the one heavy proof, cancellable until `onCommitting`).
        let client = authClientFactory()
        let result = try await client.signup(
            password: randomPassword, enrolRecovery: true, onCommitting: onCommitting)
        // A fresh unstructured task does not inherit the caller's cancellation:
        // an account the finish created is completed regardless.
        try await Task { @MainActor in
            try await self.completeGuestSignup(
                result: result, onAccountReady: onAccountReady, writes: writes)
        }.value
    }

    /// Everything the guest signup does once the OPAQUE registration
    /// committed. Mirror of the post-registration half of the web
    /// `signupGuest` (`apps/web/src/auth/AuthContext.tsx`); split out so it
    /// can be driven by a test without an OPAQUE server.
    ///
    /// Order, and why:
    ///
    ///   1. The device material, exactly as ``completeSignup(result:username:)``
    ///      persists it. No username claim: the account stays username-less.
    ///   2. The first-ever identity-bundle publish. Fail-loud: a publish
    ///      failure rejects the guest signup rather than landing an identity
    ///      nobody can verify.
    ///   3. The recovery phrase, kept on this device under the GUEST's own
    ///      key in the phrase cache (``guestPhraseKey(accountIdB64:)``) and
    ///      NOT the account's: the automatic backup reads the phrase by the
    ///      account id, finds nothing, and so keeps nothing on a server for
    ///      an account the person has not decided to keep. Fail-loud: an
    ///      account whose phrase is neither shown nor kept could never be
    ///      kept afterwards.
    ///   4. The guest marker, and the session record with an EMPTY username.
    ///   5. The session lands, with the real export key, exactly as the
    ///      post-phrase path used to land it — and WITHOUT registering for
    ///      push: a person who was sent a call link is not asked whether this
    ///      app may send notifications.
    ///
    /// `writes` is the route's ``SignInKeyWrites``, taken when ``signupGuest(onCommitting:onAccountReady:)``
    /// began (a test driving this half directly takes it here).
    func completeGuestSignup(
        result: SignupResult,
        onAccountReady: (@MainActor (_ accountIdB64: String) -> Void)? = nil,
        writes: SignInKeyWrites = SignInKeyWrites()
    ) async throws {
        let accountIdB64 = base64Encode(result.accountId)

        guard let phrase = result.recoveryPhrase else {
            throw AuthClientError.argument(
                "internal: guest signup completed without a recovery phrase"
            )
        }
        let accountIdentitySeed = try RecoveryMnemonic
            .fromPhrase(phrase: phrase)
            .toSeed()

        tokenProvider.install(result.session, accountIdB64: accountIdB64)
        freshAuth.put(result.freshGrant, accountIdB64: accountIdB64)
        sessionLanding = .other

        // Device root key from the OPAQUE export key + persist the key /
        // export key / device-identity / account-identity seed, exactly as
        // `signup` does. Best-effort mirrors (a persistence failure just
        // degrades a future warm resume on this device).
        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: accountIdB64
        )
        try writes.required(accountIdB64) {
            try DeviceKeyStore.putDeviceKey(
                accountIdB64: accountIdB64,
                rootKey: deviceRootKey
            )
        }
        do {
            try writes.ifNotWiped(accountIdB64) {
                try ExportKeyStore.putExportKey(
                    accountIdB64: accountIdB64,
                    exportKey: result.exportKey
                )
            }
        } catch {
            NSLog("export-key persist (signupGuest) failed: \(error)")
        }
        do {
            try writes.ifNotWiped(accountIdB64) {
                _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                    accountIdB64: accountIdB64
                )
            }
        } catch {
            NSLog("device-identity ensure (signupGuest) failed: \(error)")
        }
        do {
            try writes.ifNotWiped(accountIdB64) {
                try AccountIdentitySeedStore.putAccountIdentitySeed(
                    accountIdB64: accountIdB64,
                    seed: accountIdentitySeed
                )
            }
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

        // The phrase, under the guest's own key (step 3 above). ADR-0075 D2:
        // it is not shown, here or when the account is kept.
        var phraseBytes = Data(phrase.utf8)
        defer { phraseBytes.resetBytes(in: 0..<phraseBytes.count) }
        try writes.required(accountIdB64) {
            try phraseStore.putPhrase(
                accountIdB64: guestPhraseKey(accountIdB64: accountIdB64),
                phraseBytes: phraseBytes
            )
        }
        guestMarker.mark(accountIdB64: accountIdB64, nowMs: clock())
        // Username EMPTY (unclaimed); a guest has no password to replace.
        newPasswordOwed = false
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: ""))

        onAccountReady?(accountIdB64)

        // No push registration and no backup here; neither is registered by
        // `finishCanonicalSession`.
        finishCanonicalSession(
            accountIdentitySeed: accountIdentitySeed,
            accountIdBytes: result.accountId,
            exportKey: result.exportKey,
            session: result.session,
            username: ""
        )
    }

    // MARK: - Guest account: keep it, or delete it

    /// Recompute ``guestAccount`` from the marker and the session.
    private func refreshGuestAccount() {
        let next: GuestAccountState?
        if case .session(let account) = state {
            next = guestMarker.state(accountIdB64: account.accountIdB64)
        } else {
            next = nil
        }
        if guestAccount != next { guestAccount = next }
    }

    /// Keep a guest account: give it `username` and its first `password`
    /// (``GuestAccountClaim`` holds the order). The one claim of a guest
    /// account (`docs/ux/call-links.md` section 9 for when it is offered,
    /// ADR-0076 for what it does): reachable only from the question asked
    /// once no call is running.
    ///
    /// Resolving leaves the account an ordinary one, with a username and a
    /// password, and "Protect your account" owed (``protectStepOwed``),
    /// exactly as after a sign-up (ADR-0075 D2). The recovery phrase is not
    /// shown and no three-word check runs.
    ///
    /// A retry after the username was taken and a later step failed goes on
    /// with the SAME username; `username` is then ignored.
    ///
    /// - Throws: ``GuestAccountExpired`` when the account was past its
    ///   lifetime (it has been deleted and the session is over);
    ///   ``GuestPhraseUnavailable`` when this device no longer holds
    ///   the guest's phrase (such an account cannot be kept);
    ///   ``UsernameTaken`` / ``UsernameBindLimited`` / ``UsernameRuleViolation``
    ///   from the claim; whatever the password step throws.
    public func claimGuestAccount(username: String, password: Data) async throws {
        guard case .session(let account) = state,
              let secrets = identitySecrets,
              guestAccount != nil else {
            throw NoGuestAccount()
        }
        guard !password.isEmpty else {
            throw AuthClientError.argument("password must be non-empty")
        }
        let writes = SignInKeyWrites()
        // An account past its lifetime is not kept: the servers are about to
        // delete it, or already have.
        if await expireGuestIfDue() { throw GuestAccountExpired() }
        let accountIdB64 = account.accountIdB64
        try await GuestAccountClaim.run(
            username: username,
            password: password,
            ports: GuestAccountClaim.Ports(
                readPhrase: { [phraseStore] in
                    // The guest's own row, else the account's row: an earlier
                    // attempt that got as far as its last step moved the
                    // phrase there before it was interrupted.
                    for key in [guestPhraseKey(accountIdB64: accountIdB64), accountIdB64] {
                        do {
                            guard var bytes = try phraseStore.getPhrase(accountIdB64: key) else {
                                continue
                            }
                            defer { bytes.resetBytes(in: 0..<bytes.count) }
                            if let phrase = String(data: bytes, encoding: .utf8) { return phrase }
                        } catch {
                            NSLog("[guest] reading the recovery phrase failed: \(error)")
                        }
                    }
                    return nil
                },
                claimedUsername: { [weak self] in self?.guestAccount?.claimedUsername },
                claimUsername: { [weak self] name in
                    guard let self else { throw NoGuestAccount() }
                    // The login handle at auth first, then the directory,
                    // with the handle released if the directory refuses: the
                    // `signup` order, through the same helper. The session's
                    // anonymous-token pool is already bound to this account.
                    let sessionToken = self.liveSessionToken(for: accountIdB64) ?? secrets.sessionToken
                    let canonical = try await claimSignupUsername(
                        accountId: secrets.accountId,
                        username: name,
                        sessionToken: sessionToken,
                        directory: self.accountDirectory,
                        binder: self.authClientFactory()
                    )
                    // The claim just bound the handle with this session.
                    _ = self.loginHandleBinds.claim(
                        sessionToken: sessionToken, canonical: canonical)
                    return canonical
                },
                noteUsernameClaimed: { [weak self] name in
                    guard let self else { return }
                    self.guestMarker.noteUsernameClaimed(accountIdB64: accountIdB64, username: name)
                    self.sessionStore.save(
                        PersistedSession(accountIdB64: accountIdB64, username: name))
                    self.refreshGuestAccount()
                },
                setPassword: { [weak self] _, password, name in
                    guard let self else { throw NoGuestAccount() }
                    // The guest account was made under a random password
                    // nobody saw. Its first password is set from the keys
                    // this device holds (ADR-0076): the reset is proved by
                    // the seed, nothing is typed, and nothing on the device
                    // is re-keyed or replaced.
                    try await self.setFirstPassword(
                        password, username: name, accountIdB64: accountIdB64, writes: writes)
                },
                finish: { [weak self] phrase in
                    self?.finishGuestClaim(phrase: phrase, accountIdB64: accountIdB64, writes: writes)
                }
            )
        )
    }

    /// The access token of the live session of `accountIdB64`, or `nil`.
    private func liveSessionToken(for accountIdB64: String) -> String? {
        guard signedInAccountIdB64 == accountIdB64 else { return nil }
        return identitySecrets?.sessionToken
    }

    /// The password step of ``claimGuestAccount(username:password:)``: the
    /// account's FIRST password, set through `/password/reset/*` with the
    /// recovery proof made from the seed this device holds. The session and
    /// the export-key cache follow the answer; the device key, the cached
    /// seed and the phrase stay.
    private func setFirstPassword(
        _ newPassword: Data, username: String, accountIdB64: String, writes: SignInKeyWrites
    ) async throws {
        guard signedInAccountIdB64 == accountIdB64, let secrets = identitySecrets,
              secrets.accountIdentitySeed.count == Self.accountIdentitySeedLength else {
            throw NoGuestAccount()
        }
        let name = try LoginName.make(username, emptyError: NoGuestAccount())
        var seed = Data(secrets.accountIdentitySeed)
        defer { Self.zeroize(&seed) }
        let bound = binding.capture()
        let result = try await firstPasswordOperation(name.handle, seed, newPassword)
        // Signed out, or deleted, while the reset was out: the password is
        // set on the server and nothing lands here.
        try bound.require()
        guard result.accountId == secrets.accountId else {
            throw AuthClientError.argument(
                "internal: the password reset answered for another account")
        }
        do {
            try writes.ifNotWiped(accountIdB64) {
                try ExportKeyStore.putExportKey(accountIdB64: accountIdB64, exportKey: result.exportKey)
            }
        } catch {
            NSLog("export-key persist (guest claim) failed: \(error)")
        }
        adoptIdentitySecrets(
            IdentitySecrets(
                accountId: secrets.accountId,
                exportKey: result.exportKey,
                accountIdentitySeed: secrets.accountIdentitySeed,
                session: result.session
            ),
            context: "guest claim",
            rebinds: false
        )
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "guest claim")
    }

    /// The last step of ``claimGuestAccount(username:password:)``: only now
    /// does the account stop being a guest.
    ///
    ///   1. The phrase moves from the guest's row to the account's own, so
    ///      this device seals the account's backup from here on. It is not
    ///      shown (ADR-0075 D2).
    ///   2. "Protect your account" becomes owed BEFORE the guest marker goes,
    ///      so no render sees an ordinary session without that step in front
    ///      of it.
    ///   3. The session takes the username; the account's other devices and
    ///      the profile learn it (ADR-0076).
    ///   4. The first backup (no MLS state) and the push registration: from
    ///      here on it is an ordinary account.
    private func finishGuestClaim(phrase: String, accountIdB64: String, writes: SignInKeyWrites) {
        guard signedInAccountIdB64 == accountIdB64, let secrets = identitySecrets,
              let username = guestAccount?.claimedUsername ?? sessionStore.load()?.username,
              !username.isEmpty else {
            return
        }
        DevicePhrase.store(
            phrase, accountIdB64: accountIdB64, context: "guest-claim",
            since: writes.mark(accountIdB64), store: phraseStore)
        protectStepOwed = true
        guestMarker.clear(accountIdB64: accountIdB64)
        do {
            try phraseStore.deletePhrase(accountIdB64: guestPhraseKey(accountIdB64: accountIdB64))
        } catch {
            // The account wipe removes the row too.
            NSLog("[guest] removing the guest recovery phrase row failed: \(error)")
        }
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: username))
        let named = SessionAccount(accountIdB64: accountIdB64, username: username)
        // The landing hook reads the marker: the account is no longer a guest.
        state = .session(named)
        onUsernameClaimed?(named)
        Task { [weak self] in
            guard let self else { return }
            // ADR-0076 — the account's other devices learn the name.
            await self.writeOwnUsernameRegister(username, accountIdB64: accountIdB64)
            await self.bootstrapBackupBestEffort(
                accountId: secrets.accountId,
                accountIdB64: accountIdB64,
                accountIdentitySeed: secrets.accountIdentitySeed,
                sessionToken: self.liveSessionToken(for: accountIdB64) ?? secrets.sessionToken,
                phrase: phrase,
                context: "guest-claim",
                writes: writes
            )
        }
        Task { await self.subscribePushBestEffort(context: "guest-claim", accountIdB64: accountIdB64) }
    }

    /// "Protect your account" was shown after a guest account was kept.
    public func dismissProtectStep() {
        protectStepOwed = false
    }

    /// Delete a guest account at once (``GuestAccountDiscard`` holds the
    /// order): on the servers, and every trace of it on this device. Lands
    /// signed out. A required server delete that fails throws and leaves
    /// everything in place; the order frees a username an unfinished
    /// `Create account` took (``guestDeletePlan(_:)``).
    public func discardGuestAccount() async throws {
        guard case .session(let account) = state,
              let secrets = identitySecrets,
              let guest = guestAccount else {
            throw NoGuestAccount()
        }
        try await GuestAccountDiscard.run(
            guest: guest,
            ports: guestEndPorts(accountIdB64: account.accountIdB64, sessionToken: secrets.sessionToken))
        guestAccountDeleted = true
    }

    /// If the live session is a guest account past its lifetime
    /// (``guestExpired(_:nowMs:)``): delete it on the servers best effort,
    /// wipe the device, land signed out, and answer `true`. Otherwise `false`
    /// and nothing happens.
    ///
    /// Called when the guest's one question appears and at the start of
    /// ``claimGuestAccount(username:password:)``. Never while a call runs:
    /// the question is not shown then.
    @discardableResult
    public func expireGuestIfDue() async -> Bool {
        if let running = guestExpiryInFlight { return await running.value }
        guard case .session(let account) = state,
              let secrets = identitySecrets,
              let guest = guestAccount,
              guestExpired(guest, nowMs: clock()) else {
            return false
        }
        let ports = guestEndPorts(
            accountIdB64: account.accountIdB64, sessionToken: secrets.sessionToken)
        let task = Task { @MainActor [weak self] in
            NSLog("[guest] the guest account is past its lifetime and is deleted")
            await GuestAccountExpiry.run(guest: guest, ports: ports)
            self?.guestAccountDeleted = true
            return true
        }
        guestExpiryInFlight = task
        defer { guestExpiryInFlight = nil }
        return await task.value
    }

    /// The server deletes and the local wipe that end a guest account.
    ///
    /// The auth service deletes an account with no login handle and no
    /// passkey on its session alone, and releases the handle of an account
    /// younger than a day the same way, so nothing here depends on a
    /// fresh-auth grant. One that is live is sent along for a server that
    /// refuses unexpectedly.
    private func guestEndPorts(accountIdB64: String, sessionToken: String) -> GuestAccountDiscard.Ports {
        let environment = guestEnvironment
        return GuestAccountDiscard.Ports(
            servers: GuestServerDelete.Ports(
                deleteAtAuth: { [weak self] in
                    try await environment.deleteAccountAtAuth(
                        self?.liveSessionToken(for: accountIdB64) ?? sessionToken,
                        self?.liveFreshGrant())
                },
                deleteInDirectory: { [weak self] in
                    try await environment.deleteAccountInDirectory(
                        self?.liveSessionToken(for: accountIdB64) ?? sessionToken)
                },
                releaseHandle: { [weak self] in
                    let token = self?.liveSessionToken(for: accountIdB64) ?? sessionToken
                    let grant = self?.liveFreshGrant()
                    if let release = environment.releaseLoginHandle {
                        try await release(token, grant)
                    } else if let self {
                        _ = try await self.authClientFactory().removeLoginHandle(
                            sessionToken: token, freshGrant: grant)
                    }
                }
            ),
            wipe: { [weak self] in
                if let wipe = environment.wipeLocalData {
                    wipe(accountIdB64)
                } else {
                    self?.wipeAccountStores(accountIdB64)
                    self?.endSessionAfterLocalWipe()
                }
            }
        )
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
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        let result = try await client.login(
            handle: name.handle,
            password: password
        )
        // A sign-out, or another account's landing, while the round trip was
        // out: this sign-in was overtaken and lands nothing.
        try bound.require()
        let accountIdBytes = result.accountId
        let accountIdB64 = base64Encode(accountIdBytes)
        // The fresh-auth proof of this password sign-in: in memory only.
        freshAuth.put(result.freshGrant, accountIdB64: accountIdB64)
        sessionLanding = .password

        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: accountIdB64
        )
        try writes.required(accountIdB64) {
            try DeviceKeyStore.putDeviceKey(
                accountIdB64: accountIdB64,
                rootKey: deviceRootKey
            )
        }
        // Phase 2a2c2 — refresh the cached export key under the
        // Keychain. Best-effort.
        do {
            try writes.ifNotWiped(accountIdB64) {
                try ExportKeyStore.putExportKey(
                    accountIdB64: accountIdB64,
                    exportKey: result.exportKey
                )
            }
        } catch {
            NSLog("export-key persist (login) failed: \(error)")
        }
        // Phase 2a3b0 (ADR-0007) — ensure DeviceIdentity seed.
        // Idempotent: a returning user with a seed already on
        // file gets the cached one; first-login generates
        // fresh bytes.
        do {
            try writes.ifNotWiped(accountIdB64) {
                _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                    accountIdB64: accountIdB64
                )
            }
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
                // ADR-0076 section 2 — never a blind bind: a device with a
                // stale name would revert a rename made elsewhere. Ask
                // first; bind only when the account has no handle at all.
                switch HandleSync.action(
                    try await client.checkLoginHandle(handle: handle, sessionToken: sessionToken)) {
                case .nothing:
                    await self.handleConfirmed(canonical: name.canonical, sessionToken: sessionToken)
                case .bind:
                    try await client.bindLoginHandle(handle: handle, sessionToken: sessionToken)
                    await self.handleConfirmed(canonical: name.canonical, sessionToken: sessionToken)
                case .waitForRegister:
                    NSLog("[auth] the account is bound to another name (\(context)); waiting for it from the other devices")
                }
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
    /// (``landAfterAccessRecovery(username:accountIdBytes:accountIdB64:session:context:pushContext:writes:restoreKeys:)``),
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
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        let assertion = try await passkeyCeremony(nil)
        // A sign-out, or another account's landing, while the ceremony was
        // out: this sign-in was overtaken and lands nothing.
        try bound.require()
        // The finish response carries URL-safe base64 without padding; the
        // marker and every store on this device use the standard form.
        guard let accountIdBytes = base64UrlDecodeNoPad(assertion.accountIdB64),
              accountIdBytes.count == accountIdLength else {
            throw AuthClientError.argument("Malformed account_id in passkey assertion.")
        }
        let accountIdB64 = base64Encode(accountIdBytes)
        restoreNotice = nil
        // A passkey sign-in is a fresh-auth proof: in memory only.
        freshAuth.put(assertion.freshGrant, accountIdB64: accountIdB64)
        sessionLanding = .passkey

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
            writes: writes,
            restoreKeys: { [weak self] in
                guard let self else { return false }
                do {
                    let outcome = try await self.keysFromPasskey(
                        assertion, sessionToken: assertion.sessionToken, writes: writes)
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
        let writes = SignInKeyWrites()
        let assertion = try await passkeyCeremony(base64UrlEncodeNoPad(pending.accountIdBytes))
        guard base64UrlDecodeNoPad(assertion.accountIdB64) == pending.accountIdBytes else {
            throw PasskeyWrongAccount()
        }
        // The ceremony is a passkey sign-in for this account: a proof.
        freshAuth.put(assertion.freshGrant, accountIdB64: pending.accountIdB64)
        let outcome = try await keysFromPasskey(
            assertion, sessionToken: assertion.sessionToken, writes: writes)
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
    /// ADR-0075 — the protection of the account whose keys are being
    /// restored (signed in, no keys here): `GET /recovery-email` with the
    /// parked session. Stored, so the phrase-saved flag is known before any
    /// route of the prompt runs (a reset shows its new words only to a
    /// phrase-protected account). `nil` when it could not be read.
    public func protectionForPendingRestore() async -> RecoveryEmailStatus? {
        guard let pending = pendingRecovery else { return nil }
        guard let status = try? await authClientFactory().recoveryEmailStatus(
            sessionToken: pending.sessionToken) else { return nil }
        guard pendingRecovery?.accountIdB64 == pending.accountIdB64 else { return nil }
        protectionStore.setStatus(status, accountIdB64: pending.accountIdB64)
        return status
    }

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
        sessionToken: String,
        writes: SignInKeyWrites
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
                // Opened from the passkey's vault: never typed.
                try await provideRecoveryPhrase(
                    mnemonicPhrase: phrase, writes: writes, phraseHeldByPerson: false)
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
                  suspended: ThreeWordsBudgetStore().isSuspended(accountIdB64: cached.accountIdB64),
                  phraseSaved: protectionStore.phraseSaved(accountIdB64: cached.accountIdB64)
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
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        do {
            result = try await authClientFactory().recoveryLogin(handle: name.handle, seed: seed)
            // As in ``login(username:password:)``: overtaken, lands nothing.
            try bound.require()
        } catch let stale as StaleBindingError {
            throw stale
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
        // The words matched the phrase this device holds: the new-password
        // step that follows may run (``PasswordChangeGate``).
        threeWordsPassedAtMs = grantNowMs()
        sessionLanding = .other
        await landAfterAccessRecovery(
            username: name.canonical,
            accountIdBytes: result.accountId,
            accountIdB64: context.accountIdB64,
            session: result.session,
            context: "three words",
            pushContext: "three-words",
            writes: writes
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
        // ADR-0075: the words are shown only once the person saved them
        // (`Save a recovery phrase`); until then the device holds them unseen.
        guard protectionStore.phraseSaved(accountIdB64: account.accountIdB64) else { return nil }
        let phrase = DevicePhrase.read(accountIdB64: account.accountIdB64)
        guard PhraseReveal.decide(
            replaceOwed: markers.isPublishOwed(accountIdB64: account.accountIdB64),
            phraseStored: phrase != nil
        ) == .show else { return nil }
        return phrase
    }

    /// ADR-0075 section 2 — the recovery phrase this device holds for the
    /// signed-in account, for sealing the email vault at an enrolment. Not
    /// for display: an account whose phrase was never saved keeps its words
    /// unseen (``devicePhrase()`` is the gated read). `nil` while a key
    /// reset's publish is owed, when the stored words are not yet the
    /// account's published ones.
    func heldPhraseForEmailVault() -> String? {
        guard case .session(let account) = state,
              !markers.isPublishOwed(accountIdB64: account.accountIdB64) else { return nil }
        return DevicePhrase.read(accountIdB64: account.accountIdB64)
    }

    /// ADR-0075 — whether the signed-in account's recovery phrase was saved,
    /// as this device knows it. The new words of a rotation, a reset or a
    /// recall, and every link to them, are shown only then.
    public var sessionPhraseProtected: Bool {
        guard case .session(let account) = state else { return false }
        return protectionStore.phraseSaved(accountIdB64: account.accountIdB64)
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
        // Not on the bare session: a live fresh-auth proof, or the local
        // three-word check, comes first (``PasswordChangeGate``).
        guard passwordChangeAllowed else { throw FreshAuthRequired() }
        guard !newPassword.isEmpty else {
            throw AuthClientError.argument("new password must be non-empty")
        }
        let name = try LoginName.make(account.username, emptyError: SessionNotReady())
        // The client uses the seed as the recovery input; hand it a copy so
        // the in-memory secret is never the buffer that is wiped.
        var seed = Data(secrets.accountIdentitySeed)
        defer { Self.zeroize(&seed) }
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        let result = try await authClientFactory().recover(
            handle: name.handle, mnemonicSeed: seed, newPassword: newPassword)
        // Signed out, or the device forgot the account, while the reset was
        // out: the password is changed on the server, and nothing lands here.
        try bound.require()
        guard result.accountId == secrets.accountId else {
            // The name's handle answered for another account. The proof was
            // made from this account's seed, so this cannot verify; refuse
            // rather than adopt a session for an account that is not ours.
            throw AuthClientError.argument(
                "internal: the password reset answered for another account")
        }
        do {
            try writes.ifNotWiped(account.accountIdB64) {
                try ExportKeyStore.putExportKey(
                    accountIdB64: account.accountIdB64, exportKey: result.exportKey)
            }
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
            context: "replacePassword",
            rebinds: false
        )
        newPasswordOwed = false
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "replace password")
    }

    // MARK: - ADR-0076: the own-username register

    /// The name whose handle auth confirmed for the account, and the
    /// session it was confirmed in. In memory only: a new session asks again.
    private var confirmedHandle: (canonical: String, sessionToken: String)?

    /// The register as last read (`.none` until the synced state loaded).
    private var lastOwnUsernameRegister: String??

    /// The own-username register was read (the synced state loaded) or
    /// changed (this device or a sibling wrote it): seed it once from the
    /// local marker, finish this device's own owed write, or adopt a name
    /// written elsewhere (``OwnUsernameRule``).
    public func ownUsernameRegisterChanged(_ register: String?) async {
        guard case .session(let account) = state else { return }
        let accountIdB64 = account.accountIdB64
        lastOwnUsernameRegister = .some(register)
        // A rename this device has not finished decides the name, not the
        // register (which still holds the name being replaced).
        guard protectionStore.renameOwed(accountIdB64: accountIdB64) == nil else { return }
        let pending = protectionStore.usernameRegisterOwed(accountIdB64: accountIdB64)
        let confirmed = confirmedHandle.map {
            $0.canonical == account.username && $0.sessionToken == identitySecrets?.sessionToken
        } ?? false
        switch OwnUsernameRule.action(
            local: account.username, register: register, pending: pending, handleConfirmed: confirmed) {
        case .nothing:
            if pending != nil { protectionStore.setUsernameRegisterOwed(nil, accountIdB64: accountIdB64) }
        case .seed(let name), .writePending(let name):
            if await storeOwnUsername?(name) == true {
                protectionStore.setUsernameRegisterOwed(nil, accountIdB64: accountIdB64)
            }
        case .adopt(let name):
            adoptUsernameFromRegister(name, accountIdB64: accountIdB64)
        }
    }

    /// Auth confirmed in this session that the account is bound to
    /// `canonical`'s handle (the check answered `same`, or this device made
    /// the first bind). Only now may an empty register be seeded from the
    /// local marker; a register already read as empty is looked at again.
    private func handleConfirmed(canonical: String, sessionToken: String) async {
        confirmedHandle = (canonical, sessionToken)
        if case .some(let register) = lastOwnUsernameRegister {
            await ownUsernameRegisterChanged(register)
        }
    }

    /// A newer name arrived from the account's other devices: the local
    /// marker and the live session take it, the handle state of this
    /// session is known (the account is bound to this name's handle, so no
    /// sync runs for it), and what derives from the name is re-published.
    private func adoptUsernameFromRegister(_ name: String, accountIdB64: String) {
        guard case .session(let account) = state, account.accountIdB64 == accountIdB64,
              let secrets = identitySecrets,
              let canonical = try? requireValidUsername(name, emptyError: SessionNotReady()) else { return }
        NSLog("[auth] the account's username changed on another device; adopting it")
        _ = loginHandleBinds.claim(sessionToken: secrets.sessionToken, canonical: canonical)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: canonical))
        let named = SessionAccount(accountIdB64: accountIdB64, username: canonical)
        state = .session(named)
        onUsernameRenamed?(named, nil)
    }

    /// Record a name this device just wrote locally as owed to the
    /// register, and try the write.
    private func writeOwnUsernameRegister(_ name: String, accountIdB64: String) async {
        protectionStore.setUsernameRegisterOwed(name, accountIdB64: accountIdB64)
        if await storeOwnUsername?(name) == true {
            protectionStore.setUsernameRegisterOwed(nil, accountIdB64: accountIdB64)
        }
    }

    // MARK: - ADR-0076: changing the username

    /// When the next username change is possible by this device's own stamp
    /// (unix ms), or `nil`. The server's `429` is the authority.
    public var renameTooSoonUntilMs: Int64? {
        guard case .session(let account) = state else { return nil }
        return UsernameRename.tooSoonUntilMs(
            lastChangeAtMs: protectionStore.lastRenameAtMs(accountIdB64: account.accountIdB64),
            nowMs: grantNowMs())
    }

    /// The `recovery_email` part of a handle change towards `handle`: the
    /// address this device holds and, while no phrase is saved, the email
    /// vault sealed (off the main thread) under THAT handle with a fresh
    /// salt. `nil` when the account has no recovery email or this device
    /// does not hold the address: the change then drops the recovery email.
    private func recoveryEmailCarry(
        to handle: Data, accountIdB64: String
    ) async throws -> LoginHandleRecoveryEmail? {
        let status = protectionStore.status(accountIdB64: accountIdB64)
        let address = heldRecoveryAddress?()
        switch UsernameRename.emailCarry(
            recoveryEmailEnabled: status?.enabled == true, addressHeld: address != nil,
            phraseSaved: protectionStore.phraseSaved(accountIdB64: accountIdB64)) {
        case .none:
            return nil
        case .addressOnly:
            return address.map { LoginHandleRecoveryEmail(address: $0) }
        case .addressAndVault:
            guard let address, let phrase = heldPhraseForEmailVault() else { throw SessionNotReady() }
            var rng = SystemRandomNumberGenerator()
            let salt = Data((0..<16).map { _ in UInt8.random(in: UInt8.min...UInt8.max, using: &rng) })
            let sealed = try await EmailVaultWork.seal(
                phrase: phrase, handle: handle, address: address, vaultSalt: salt)
            return LoginHandleRecoveryEmail(address: address, vaultSalt: salt, sealedEntropy: sealed)
        }
    }

    /// Change the signed-in account's username (ADR-0076 section 4). One
    /// resumable operation; the owed record is written before the first
    /// request.
    ///
    ///   a. the rule, the new handle;
    ///   b. `PUT /auth/login-handle` with the grant, the previous handle
    ///      and, when this device holds the address, the recovery email
    ///      re-keyed (the vault re-sealed under the NEW handle);
    ///   c. discovery's `username/replace`; on a refusal the old handle is
    ///      bound back with the same re-key and the cause is thrown;
    ///   d. the certificate, the local marker, the session, the register;
    ///   e. the re-publish (``onUsernameRenamed``).
    ///
    /// Throws ``FreshAuthRequired`` (no live proof; nothing was sent) or
    /// ``UsernameRenameFailed``. Returns the canonical new name.
    public func renameUsername(to typed: String) async throws -> String {
        guard case .session(let account) = state, let secrets = identitySecrets,
              !account.username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw SessionNotReady()
        }
        let accountIdB64 = account.accountIdB64
        let new: LoginName
        let old: LoginName
        do {
            new = try LoginName.make(typed, emptyError: UsernameRenameFailed(.rule))
            old = try LoginName.make(account.username, emptyError: SessionNotReady())
        } catch is UsernameRuleViolation {
            throw UsernameRenameFailed(.rule)
        }
        guard new.canonical != old.canonical else { throw UsernameRenameFailed(.sameName) }
        if let until = renameTooSoonUntilMs { throw UsernameRenameFailed(.tooSoon(untilMs: until)) }
        guard let grant = liveFreshGrant() else { throw FreshAuthRequired() }

        // b. The handle. The record first: a crash from here is resumed.
        let carry = try await recoveryEmailCarry(to: new.handle, accountIdB64: accountIdB64)
        var record = UsernameRename.Record(
            oldName: old.canonical, newName: new.canonical, phase: .binding, startedAtMs: grantNowMs())
        protectionStore.setRenameOwed(record, accountIdB64: accountIdB64)
        let client = authClientFactory()
        do {
            try await client.changeLoginHandle(
                handle: new.handle, previousHandle: old.handle, freshGrant: grant,
                recoveryEmail: carry, sessionToken: secrets.sessionToken)
        } catch {
            if case .network = error as? AuthClientError {
                // Unknown whether it landed: the next landing asks.
                throw UsernameRenameFailed(.offline)
            }
            protectionStore.setRenameOwed(nil, accountIdB64: accountIdB64)
            if FreshAuthRefusal.isRequired(error) {
                dropFreshAuth()
                throw FreshAuthRequired()
            }
            if let err = error as? AuthClientError, let refusal = AuthRateLimit.refusal(in: err) {
                throw UsernameRenameFailed(.bindLimited(retryAfterSecs: refusal.retryAfterSecs))
            }
            if case .http(409, _, let body) = error as? AuthClientError, body.contains(handleTakenCode) {
                throw UsernameRenameFailed(.taken)
            }
            throw UsernameRenameFailed(.failed)
        }
        record.phase = .bound
        protectionStore.setRenameOwed(record, accountIdB64: accountIdB64)

        // c. Discovery.
        return try await replaceAtDiscovery(record: record, old: old, new: new, grant: grant)
    }

    /// Steps c to e for a rename whose handle is already the new one.
    private func replaceAtDiscovery(
        record: UsernameRename.Record, old: LoginName, new: LoginName, grant: String?
    ) async throws -> String {
        guard case .session(let account) = state, let secrets = identitySecrets else { throw SessionNotReady() }
        let accountIdB64 = account.accountIdB64
        var record = record
        var replaced: ReplacedUsername?
        var failure: Error?
        do {
            replaced = try await accountDirectory.replaceUsername(
                accountId: secrets.accountId, username: new.canonical, sessionToken: secrets.sessionToken)
        } catch {
            failure = error
        }
        let outcome = UsernameRename.ReplaceOutcome.of(failure)
        switch UsernameRename.afterReplace(outcome) {
        case .keepOwed:
            throw UsernameRenameFailed(.offline)
        case .rollBack:
            // The old handle back, the recovery email re-keyed back.
            guard let grant, UsernameRename.canRollBack(holdsGrant: true) else {
                // No proof in hand (a landing): the record stays; the
                // rename screen finishes the rollback after the next proof.
                throw UsernameRenameFailed(UsernameRename.failure(outcome, nowMs: grantNowMs()))
            }
            do {
                try await authClientFactory().changeLoginHandle(
                    handle: old.handle, previousHandle: new.handle, freshGrant: grant,
                    recoveryEmail: try await recoveryEmailCarry(to: old.handle, accountIdB64: accountIdB64),
                    sessionToken: secrets.sessionToken)
                protectionStore.setRenameOwed(nil, accountIdB64: accountIdB64)
            } catch {
                NSLog("[auth] WARN the old username's handle could not be bound back; retried at the next landing")
            }
            throw UsernameRenameFailed(UsernameRename.failure(outcome, nowMs: grantNowMs()))
        case .finalize:
            break
        }
        record.phase = .replaced
        protectionStore.setRenameOwed(record, accountIdB64: accountIdB64)
        await finalizeRename(record: record, certificate: replaced?.certificate)
        return new.canonical
    }

    /// Steps d and e: the marker, the session, the register, the re-publish.
    private func finalizeRename(record: UsernameRename.Record, certificate: Data?) async {
        guard case .session(let account) = state, let secrets = identitySecrets else { return }
        let accountIdB64 = account.accountIdB64
        // The handle of the new name is the bound one: no sync runs for it.
        _ = loginHandleBinds.claim(sessionToken: secrets.sessionToken, canonical: record.newName)
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: record.newName))
        protectionStore.setLastRenameAtMs(grantNowMs(), accountIdB64: accountIdB64)
        // The register is owed BEFORE the record goes: until it is written
        // the register still holds the old name and must not be adopted.
        protectionStore.setUsernameRegisterOwed(record.newName, accountIdB64: accountIdB64)
        protectionStore.setRenameOwed(nil, accountIdB64: accountIdB64)
        let named = SessionAccount(accountIdB64: accountIdB64, username: record.newName)
        state = .session(named)
        await writeOwnUsernameRegister(record.newName, accountIdB64: accountIdB64)
        onUsernameRenamed?(named, certificate)
        // The recovery email was re-keyed, or dropped: read it again.
        onProtectionChanged?()
    }

    /// At a landing: finish (or roll back) a rename this device started.
    func resumeOwedRename() async {
        guard case .session(let account) = state, let secrets = identitySecrets,
              var record = protectionStore.renameOwed(accountIdB64: account.accountIdB64) else { return }
        let accountIdB64 = account.accountIdB64
        guard let old = try? LoginName.make(record.oldName, emptyError: SessionNotReady()),
              let new = try? LoginName.make(record.newName, emptyError: SessionNotReady()) else {
            protectionStore.setRenameOwed(nil, accountIdB64: accountIdB64)
            return
        }
        var step = UsernameRename.resume(record)
        if step == .checkHandle {
            guard let state = try? await authClientFactory().checkLoginHandle(
                handle: new.handle, sessionToken: secrets.sessionToken) else { return }
            step = UsernameRename.afterBindingCheck(newHandle: state)
            if step == .nothing {
                // The bind never landed: nothing changed.
                protectionStore.setRenameOwed(nil, accountIdB64: accountIdB64)
                return
            }
            record.phase = .bound
            protectionStore.setRenameOwed(record, accountIdB64: accountIdB64)
        }
        switch step {
        case .replace:
            _ = try? await replaceAtDiscovery(record: record, old: old, new: new, grant: liveFreshGrant())
        case .finalize:
            await finalizeRename(record: record, certificate: nil)
        case .nothing, .checkHandle:
            break
        }
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
    ///
    /// `phraseHeldByPerson` is `true` only when the person typed the phrase
    /// (the restore screen): they have shown they hold it, so a phrase owed
    /// to them (ADR-0077 decision 3, rule 6) is no longer owed. A phrase the
    /// device opened itself (the recovery-email vault) leaves the marker.
    public func recover(
        mnemonicPhrase: String,
        newPassword: Data,
        username: String? = nil,
        phraseHeldByPerson: Bool = false
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
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        let result = try await client.recover(
            handle: name.handle,
            mnemonicSeed: seed,
            newPassword: newPassword
        )
        // A sign-out, or another account's landing, while the reset was out:
        // the password is changed on the server, and nothing lands here.
        try bound.require()
        let accountIdBytes = result.accountId
        let accountIdB64 = base64Encode(accountIdBytes)
        tokenProvider.install(result.session, accountIdB64: accountIdB64)

        let deviceRootKey = deriveDeviceRootKey(
            exportKey: result.exportKey,
            accountIdB64: accountIdB64
        )
        try writes.required(accountIdB64) {
            try DeviceKeyStore.putDeviceKey(
                accountIdB64: accountIdB64,
                rootKey: deviceRootKey
            )
        }
        // Phase 2a2c2 — refresh the cached export key under the
        // freshly-rotated device key. Any prior cached value was
        // bound to the OLD password; the upsert here overwrites
        // it cleanly. Best-effort.
        do {
            try writes.ifNotWiped(accountIdB64) {
                try ExportKeyStore.putExportKey(
                    accountIdB64: accountIdB64,
                    exportKey: result.exportKey
                )
            }
        } catch {
            NSLog("export-key persist (recover) failed: \(error)")
        }
        // ADR-0017: cache the AccountIdentity (phrase) seed under the
        // freshly-rotated device. This is what makes the warm
        // same-device path (and a later passkey resume) rebuild the
        // SAME canonical identity without re-entering the phrase.
        do {
            try writes.ifNotWiped(accountIdB64) {
                try AccountIdentitySeedStore.putAccountIdentitySeed(
                    accountIdB64: accountIdB64,
                    seed: accountIdentitySeed
                )
            }
        } catch {
            NSLog("account-identity-seed persist (recover) failed: \(error)")
        }
        // The recovery proof succeeded with this phrase's seed, so the
        // phrase is the account's: this device holds it from now on.
        DevicePhrase.store(
            canonicalPhrase, accountIdB64: accountIdB64, context: "recover",
            since: writes.mark(accountIdB64))
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
        try bound.require()
        // Phase 2a3b0 (ADR-0007) — DeviceIdentity seed is
        // decoupled from the OPAQUE password, so recovery does
        // NOT rotate it. `ensureDeviceIdentitySeed` returns
        // the existing seed when present and only generates
        // fresh bytes if the Keychain row is missing.
        do {
            try writes.ifNotWiped(accountIdB64) {
                _ = try DeviceIdentityStore.ensureDeviceIdentitySeed(
                    accountIdB64: accountIdB64
                )
            }
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

        // Restore-then-bootstrap on recover.
        //
        // The user has just gone through recovery, which lands on a device
        // that may or may not have been used by them before. If a backup
        // sits on the server, it is fetched, opened and applied before a
        // fresh baseline is posted, so the baseline carries the merged
        // contacts, synced state and conversation list instead of
        // replacing them with this device's (possibly empty) own.
        //
        // The backup carries NO MLS state. After a recover this device's MLS
        // state is whatever it already held locally for the account
        // (possibly nothing); it is admitted to the chats it does not hold by
        // their other members (ADR-0050 section 2.5a). The local snapshot and
        // the ADR-0040 pending-commit outbox are therefore left exactly as
        // they are: the outbox still describes the local epochs.
        //
        // **Ordering matters:** the stores are written BEFORE the `state`
        // flip below. The session shell wakes on the flip and binds the
        // contacts and synced-state repositories, which read them.
        let backupOutcome = await restoreBackupFromPhrase(
            accountId: accountIdBytes,
            accountIdB64: accountIdB64,
            sessionToken: result.sessionToken,
            phrase: canonicalPhrase,
            context: "recover",
            writes: writes
        )
        // The fetch suspended; the session must not land over a sign-out or
        // another account that landed meanwhile.
        try bound.require()
        // Typed and accepted by the recovery proof: the person holds the
        // phrase, so it is not owed (and is not shown back to them).
        if phraseHeldByPerson {
            markers.clearPhraseOwed(accountIdB64: accountIdB64)
        }

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

        // Post-recover backup bootstrap: cache the phrase on this device and
        // post a baseline /backup envelope carrying what was just merged
        // (contacts, synced state, and the conversation list: this device's
        // own groups plus the ones the restored backup named).
        //
        // G-10 (known-gaps): AWAITED, not a detached `Task`, so the rewrite
        // is part of the recover round-trip. `bootstrapBackupBestEffort` is
        // non-throwing (it logs + continues on any internal failure), so
        // awaiting it cannot fail the recover itself.
        //
        // When the fetch above FAILED the restore is still owed and the
        // baseline is not posted: it would replace the blob the next
        // landing is going to fetch.
        await bootstrapBackupBestEffort(
            accountId: accountIdBytes,
            accountIdB64: accountIdB64,
            accountIdentitySeed: accountIdentitySeed,
            sessionToken: result.sessionToken,
            phrase: canonicalPhrase,
            context: "recover",
            writes: writes,
            uploadBaseline: backupOutcome != .fetchFailed
                && !BackupRestoreRetryStore.isPending(
                    accountIdB64: accountIdB64, defaults: backupDefaults)
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
        let leaving = signedInAccountIdB64 ?? cached?.accountIdB64
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
        // ADR-0074 section 2.1 — the session is signed out at the server
        // too, best effort: the pair is read before the provider forgets it,
        // and the local sign-out below never waits for the call.
        let endingPair = tokenProvider.current
        tokenProvider.clear()
        freshAuth.clear()
        sessionLanding = nil
        threeWordsPassedAtMs = nil
        noticeAnsweredAtMs = nil
        identitySecrets = nil
        pendingRecovery = nil
        pendingFreshPhrase = nil
        restoreNotice = nil
        newPasswordOwed = false
        freshPublishPending = false
        freshPublishNeedsSignIn = false
        keysResetThisSession = false
        accountRecalledThisSession = false
        keysUpdating = false
        keysRotatedNotice = false
        rotationAdoptRetry?.cancel()
        rotationAdoptRetry = nil
        sessionExpiredNotice = false
        protectStepOwed = false
        // A sign-in or restore still in flight must not land after this, and
        // a restore parked on the stale-backup question is over.
        binding.advance()
        resolveStaleBackupOverride(false)
        state = .anon(cachedUsername: cached?.username)
        if let endingPair { logoutAtServer(endingPair) }
        // 1.20c — fire-and-forget unsubscribe so the next page
        // reload doesn't silently keep accepting pushes against
        // a logged-out session. ADR-0078: the account's push-gw rows are
        // named by the subscription ids in its route rows, which a logout
        // keeps, so they are read inside.
        Task { await PushSubscriber.shared.unsubscribe(accountIdB64: leaving, prepaidToken: unregisterToken) }
        // Last, after the runtimes let go above: wait for this account's
        // store writes in flight and refuse the later ones until it lands
        // here again. Nothing is deleted.
        if let leaving { AccountStoreGate.seal(leaving) }
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
        // ADR-0074 section 2.1 — Forget this device signs the session out at
        // the server as Log out does. After a reset elsewhere or a removal
        // the provider is already empty and nothing is sent.
        let endingPair = tokenProvider.current
        tokenProvider.clear()
        freshAuth.clear()
        sessionLanding = nil
        threeWordsPassedAtMs = nil
        noticeAnsweredAtMs = nil
        // As in ``logout()``.
        binding.advance()
        resolveStaleBackupOverride(false)
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
        accountRecalledThisSession = false
        keysUpdating = false
        keysRotatedNotice = false
        rotationAdoptRetry?.cancel()
        rotationAdoptRetry = nil
        sessionExpiredNotice = false
        protectStepOwed = false
        state = .anon(cachedUsername: nil)
        if let endingPair { logoutAtServer(endingPair) }
        // 1.20c — full wipe of push state too. The contract is
        // "leave NOTHING per-account behind on this device", so the
        // account's push-gw rows are removed (ADR-0078: by the subscription
        // ids the wipe handed over before it cleared the route rows) and the
        // device's tokens forgotten. Best-effort fire-and-forget — the local
        // clear inside `unsubscribe` runs regardless of the server leg
        // outcome.
        Task { await PushSubscriber.shared.unsubscribe(accountIdB64: nil, prepaidToken: unregisterToken) }
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
            try await pushSubscribe(
                accountIdB64,
                { [weak self] in self?.signedInAccountIdB64 == accountIdB64 }
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
            binding.advance()
            // ACCESS is granted: the restore that follows writes this
            // account's stores before the session lands.
            AccountStoreGate.open(accountIdB64)
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
            completeParkFromOwedPhrase(accountIdB64: accountIdB64)
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
        username: String,
        retryMarkedBackupRestore: Bool = true
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
        // A session landing: a backup restore that is still owed gets one
        // attempt here. The route that has just made its own attempt passes
        // `false`: the next one belongs to the NEXT landing.
        if retryMarkedBackupRestore {
            retryBackupRestoreIfMarked(
                accountId: accountIdBytes,
                sessionToken: session.sessionToken,
                context: "sign-in"
            )
        }
        // ADR-0073 section 4.2 — observe the published identity BEFORE any
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

    /// ADR-0073 section 4.2, on every landing that holds a cached seed:
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
        // The same holds for a rotation this device made (ADR-0074 section
        // 3.2): its directory upload is owed until it lands.
        guard !keyResetPublishOwed(accountIdB64: accountIdB64) else {
            NSLog("[auth] published-identity check skipped: this device's new keys are still being published")
            return
        }

        let observed: KeyResetLanding.Observation
        var publishedSignaturePubkey: Data?
        do {
            if let bundle = try await restoreDirectory.fetchIdentityBundle(
                accountId: accountIdBytes, sessionToken: tokenProvider.current?.sessionToken) {
                publishedSignaturePubkey = base64Decode(bundle.signaturePubkey)
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
            // ADR-0074 section 3.3 — "the published bundle carries the
            // proof's new key" is how a kept device sees the commit: a
            // pending envelope for that key is adopted; with none, the
            // journal is read once, and only then is this a reset.
            keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: publishedSignaturePubkey)
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

    // MARK: - ADR-0073 section 3.5: the keys were reset elsewhere

    /// The account's keys were reset on another device: the server refused
    /// this session's generation, or the directory publishes keys of a
    /// higher generation. Show the signed-out screen, stop every writer,
    /// and run the full forget-device wipe by itself — publishing nothing
    /// first and calling nothing that needs the superseded token. Once per
    /// account: a second signal while the wipe runs changes nothing.
    ///
    /// ADR-0074 section 1.1 changes WHICH wipe (``SignOutWipe``): a device
    /// that holds the account's keys runs the STAGED wipe, keeps a recall
    /// record and shows "Was it you?" (``recall``); a device that never held
    /// them keeps nothing and shows the signed-out screen as before.
    func handleKeysSuperseded(accountIdB64: String) {
        guard keysSignOut == nil, recall == nil else { return }
        guard signedInAccountIdB64 == accountIdB64 || sessionStore.load()?.accountIdB64 == accountIdB64 else {
            return
        }
        let username = signedInUsername(accountIdB64: accountIdB64)
        if let record = recallRecordForSupersededDevice(accountIdB64: accountIdB64, username: username) {
            NSLog("[auth] the account keys were reset on another device; deleting this device's data, keeping the recall record")
            recall = RecallScreenState(record: record)
            pushBinding.clear()
            removeDeliveredNotifications()
            tokenProvider.clear()
            Task { [weak self] in
                guard let self else { return }
                await self.stagedWipeAfterKeyReset(accountIdB64)
                await self.refreshRecallStatus()
            }
            return
        }
        NSLog("[auth] the account keys were reset on another device; signing this device out")
        keysSignOutUsername = username
        keysSignOutCause = .keysReset
        keysSignOutNoticeKey = nil
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

    // MARK: - ADR-0074 section 2.3: this device was removed

    /// This device was removed from the account on another device: auth
    /// answered `401 device-revoked`, or this device's own DeviceGroup turned
    /// inactive (it applied its own removal). Show "This device was removed",
    /// stop every writer, and wipe EVERYTHING by itself: no button, nothing
    /// published first, no call that needs the revoked token, and no recall
    /// record (a removal changes no keys, so there is nothing to take back).
    /// Once per account; a signal while a sign-out is already running changes
    /// nothing.
    ///
    /// Never called on a failed request alone: discovery's `403
    /// device-revoked` and a DS `401` are not proof.
    func handleDeviceRevoked(accountIdB64: String) {
        guard keysSignOut == nil, recall == nil else { return }
        guard signedInAccountIdB64 == accountIdB64 || sessionStore.load()?.accountIdB64 == accountIdB64 else {
            return
        }
        NSLog("[auth] this device was removed from the account; signing out and deleting its data")
        keysSignOutUsername = signedInUsername(accountIdB64: accountIdB64)
        keysSignOutCause = .deviceRemoved
        keysSignOutNoticeKey = nil
        // ADR-0074 section 2.3 — a removal changes no keys: no recall record
        // is written, and one left from before goes with the full wipe.
        recallRecords.clear(accountIdB64: accountIdB64)
        keysSignOut = .removing
        pushBinding.clear()
        removeDeliveredNotifications()
        // A logout owed from before is moot, and nothing is sent with the
        // revoked pair.
        logoutOwed.clear()
        tokenProvider.clear()
        Task { [weak self] in
            guard let self else { return }
            await self.wipeAfterKeyReset(accountIdB64)
            if self.keysSignOut == .removing { self.keysSignOut = .removed }
        }
    }

    /// The account's own DeviceGroup reports this device out of it (the
    /// sync runtime read `isActive == false`): proof of removal by a device
    /// of the account (ADR-0074 section 2.3).
    public func deviceGroupReportedRemoval() {
        guard let accountIdB64 = signedInAccountIdB64 else { return }
        handleDeviceRevoked(accountIdB64: accountIdB64)
    }

    // MARK: - ADR-0074 sections 0.1, 0.4: the session names a device; the account key is recorded

    /// A state holding a session for `accountIdB64` was entered: name this
    /// device in the session if it is not named yet, then record the account
    /// key at auth if that never succeeded for the key this device holds.
    private func sessionLanded(accountIdB64: String) {
        // ADR-0074 part 3 — a kept device's pending rotations age out, and
        // the "keys were changed" line survives a relaunch until dismissed.
        pruneKeyRotationPending(accountIdB64: accountIdB64)
        keysRotatedNotice = rotatedNotice.isOwed(accountIdB64: accountIdB64)
        // A guest account (`docs/ux/call-links.md` section 9) has no name, no
        // recovery email and no notices: what protects an account is read
        // once it is kept (the landing ``finishGuestClaim(phrase:accountIdB64:)``
        // makes). The device bind and the identity-key record below still
        // run: the call needs a session that names this device.
        let guest = guestMarker.state(accountIdB64: accountIdB64) != nil
        if case .session = state, !guest { onProtectedSessionLanded?(accountIdB64) }
        // ADR-0076 — a rename this device started and did not finish.
        if case .session = state, !guest,
           protectionStore.renameOwed(accountIdB64: accountIdB64) != nil, !renameResumeInFlight {
            renameResumeInFlight = true
            Task { [weak self] in
                await self?.resumeOwedRename()
                self?.renameResumeInFlight = false
            }
        }
        Task { [weak self] in
            guard let self else { return }
            await self.bindSessionDeviceIfNeeded(accountIdB64: accountIdB64, context: "landing")
            guard let secrets = self.identitySecrets, base64Encode(secrets.accountId) == accountIdB64 else {
                return
            }
            await self.recordIdentityKeyIfNeeded(
                accountIdBytes: secrets.accountId, accountIdentitySeed: secrets.accountIdentitySeed)
        }
    }

    /// Name this device in the installed session (``SessionDeviceBinding``):
    /// `POST /auth/session/bind-device` for a pair that names none, after
    /// which the unbound pair is dropped. Returns whether the session names
    /// this device when the call returns.
    @discardableResult
    func bindSessionDeviceIfNeeded(accountIdB64: String, context: String) async -> Bool {
        guard tokenProvider.account == accountIdB64, let pair = tokenProvider.current else { return false }
        switch SessionDeviceBinding.decide(pair: pair, ownDeviceId: ownDeviceId(accountIdB64)) {
        case .bound:
            return true
        case .noDeviceIdentity:
            return false
        case .boundToAnother:
            NSLog("[auth] WARN the session names another device id; a fresh sign-in is needed to publish")
            return false
        case .bind(let deviceId):
            guard deviceBindInFlight != accountIdB64, pair.refreshUsable(nowMs: grantNowMs()) else {
                return false
            }
            deviceBindInFlight = accountIdB64
            defer { deviceBindInFlight = nil }
            do {
                let bound = try await authClientFactory().bindDevice(
                    sessionToken: pair.sessionToken, deviceId: deviceId, refreshToken: pair.refreshToken)
                // The session may have ended or changed while the call ran.
                guard tokenProvider.account == accountIdB64 else { return false }
                if let secrets = identitySecrets, base64Encode(secrets.accountId) == accountIdB64 {
                    adoptIdentitySecrets(
                        IdentitySecrets(
                            accountId: secrets.accountId,
                            exportKey: secrets.exportKey,
                            accountIdentitySeed: secrets.accountIdentitySeed,
                            session: bound),
                        context: "bind-device (\(context))",
                        rebinds: false)
                } else {
                    adoptSessionPair(bound, accountIdB64: accountIdB64, context: "bind-device (\(context))")
                }
                onSessionDeviceBound?()
                return true
            } catch {
                switch SessionDeviceBinding.refusal(of: error) {
                case .deviceRevoked:
                    handleDeviceRevoked(accountIdB64: accountIdB64)
                case .superseded:
                    handleKeysSuperseded(accountIdB64: accountIdB64)
                case .refreshInvalid:
                    handleRefreshInvalid(accountIdB64: accountIdB64)
                case .alreadyBound:
                    NSLog("[auth] WARN bind-device: the session already names another device")
                case .badDeviceId, .other:
                    NSLog("[auth] bind-device did not land (%@); the next landing tries again",
                          String(describing: type(of: error)))
                }
                return false
            }
        }
    }

    /// Record the account key at auth (`POST /auth/genesis/attest`) unless
    /// the mint already succeeded for this key (``IdentityKeyRecord``).
    func recordIdentityKeyIfNeeded(accountIdBytes: Data, accountIdentitySeed: Data) async {
        let accountIdB64 = base64Encode(accountIdBytes)
        guard identityKeyRecordInFlight != accountIdB64,
              let pubkey = try? Identity.fromSeed(seed: accountIdentitySeed).signaturePublicKey(),
              IdentityKeyRecord.needsMint(
                recordedPubkeyB64: identityKeyMarks.recorded(accountIdB64: accountIdB64), currentPubkey: pubkey),
              tokenProvider.account == accountIdB64,
              let token = tokenProvider.current?.sessionToken else {
            return
        }
        identityKeyRecordInFlight = accountIdB64
        defer { identityKeyRecordInFlight = nil }
        do {
            _ = try await authClientFactory().mintGenesisAttestation(
                sessionToken: token, identityPubkey: pubkey)
            identityKeyMarks.markRecorded(pubkey: pubkey, accountIdB64: accountIdB64)
        } catch {
            switch IdentityKeyRecord.refusal(of: error) {
            case .identityKeyMismatch:
                NSLog("[auth] WARN auth holds another account key for this account; not recorded")
            case .unusableKey:
                NSLog("[auth] WARN auth refused this account key as unusable")
            case .retryLater:
                NSLog("[auth] the account key was not recorded at auth; the next landing tries again")
            }
        }
    }

    /// Whether auth has this account's key on record as far as this device
    /// knows, and ask for it again when not (a removal answered
    /// `account-key-required`).
    public func recordIdentityKeyNow() {
        guard let secrets = identitySecrets else { return }
        Task { [weak self] in
            await self?.recordIdentityKeyIfNeeded(
                accountIdBytes: secrets.accountId, accountIdentitySeed: secrets.accountIdentitySeed)
        }
    }

    /// Whether a key-reset publish is owed for the signed-in account: a
    /// device removal must not run in that window (ADR-0074 "T1 as built"
    /// point 15).
    public func keyResetPublishOwed(accountIdB64: String) -> Bool {
        markers.isPublishOwed(accountIdB64: accountIdB64)
            || rotationStore.publishOwed(accountIdB64: accountIdB64)
    }

    /// This device's id for `accountIdB64` (its conversation leaf key).
    public func deviceId(accountIdB64: String) -> Data? {
        ownDeviceId(accountIdB64)
    }

    // MARK: - ADR-0074 section 2.1: Log out at the server

    /// `POST /auth/session/logout` for the pair a Log out just dropped, best
    /// effort. An unreachable server leaves one owed record for the next
    /// launch (``retryOwedLogout()``); the local sign-out never waits.
    private func logoutAtServer(_ pair: SessionPair) {
        guard !pair.sessionToken.isEmpty else { return }
        logoutOwed.store(pair)
        let factory = authClientFactory
        let owed = logoutOwed
        Task {
            var failure: Error?
            do {
                try await factory().logoutSession(sessionToken: pair.sessionToken)
            } catch {
                failure = error
            }
            // A sign-in since then cleared the record; this attempt must not
            // put a stale one back, so it only ever removes.
            if !LogoutOwed.keepsRecord(after: LogoutOwed.attempt(of: failure), isLaunchRetry: false) {
                if owed.load()?.sessionToken == pair.sessionToken { owed.clear() }
            }
        }
    }

    /// The one retry of an owed logout, at launch: with the stored access
    /// token while it lives, else after one refresh. The record is dropped
    /// whatever the retry came to. Nothing is shown.
    private func retryOwedLogout() {
        guard let pair = logoutOwed.load() else { return }
        // A session was restored for this launch: it is not the one that
        // logged out (a logout drops the persisted pair), but the same
        // device id, so the owed logout would end it. Drop the record.
        if tokenProvider.current != nil {
            logoutOwed.clear()
            return
        }
        let factory = authClientFactory
        let owed = logoutOwed
        let plan = LogoutOwed.retryPlan(pair: pair, nowMs: grantNowMs())
        Task {
            // Read again right before sending: a sign-in meanwhile cleared it.
            guard owed.load()?.sessionToken == pair.sessionToken else { return }
            switch plan {
            case .logout(let token):
                try? await factory().logoutSession(sessionToken: token)
            case .refreshThenLogout(let refreshToken):
                if let fresh = try? await factory().refreshSession(refreshToken: refreshToken) {
                    try? await factory().logoutSession(sessionToken: fresh.sessionToken)
                }
            case .drop:
                break
            }
            if owed.load()?.sessionToken == pair.sessionToken { owed.clear() }
        }
    }

    // MARK: - ADR-0074 section 1: "Was it you?" and taking the account back

    /// The recall record a superseded device keeps, or `nil` when it keeps
    /// none (``SignOutWipe``): the one already stored for the account (a
    /// wipe that was interrupted), else a new one built from the account
    /// identity seed this device holds, written to the store BEFORE the
    /// wipe starts. A device without the seed, or whose record could not be
    /// written, gets `nil` and runs the full wipe.
    private func recallRecordForSupersededDevice(accountIdB64: String, username: String) -> RecallRecord? {
        let existing = recallRecords.load().flatMap { $0.accountIdB64 == accountIdB64 ? $0 : nil }
        let oldSeed = heldAccountSigningSeed(accountIdB64: accountIdB64)
        switch SignOutWipe.plan(
            cause: .keysReset, holdsAccountSeed: oldSeed != nil, recallRecordExists: existing != nil) {
        case .full:
            return nil
        case .staged:
            if let existing { return existing }
            guard let oldSeed else { return nil }
            let record = RecallRecord(
                accountIdB64: accountIdB64, oldSeed: oldSeed, username: username, learnedAtMs: grantNowMs(),
                phraseSaved: protectionStore.phraseSaved(accountIdB64: accountIdB64))
            do {
                try recallRecords.put(record)
                return record
            } catch {
                NSLog("[auth] WARN the recall record could not be stored; this device keeps nothing")
                return nil
            }
        }
    }

    /// The 32-byte Ed25519 seed of the account identity this device holds
    /// for `accountIdB64`, or `nil` when it holds no account keys.
    private func heldAccountSigningSeed(accountIdB64: String) -> Data? {
        var phraseSeed: Data?
        if let secrets = identitySecrets, base64Encode(secrets.accountId) == accountIdB64 {
            phraseSeed = secrets.accountIdentitySeed
        } else if let pending = pendingFreshPhrase, pending.accountIdB64 == accountIdB64 {
            phraseSeed = pending.seed
        } else {
            phraseSeed = persistence.accountIdentitySeed(accountIdB64: accountIdB64)
        }
        guard let phraseSeed, !phraseSeed.isEmpty,
              let seed = try? Identity.fromSeed(seed: phraseSeed).ed25519Seed(),
              seed.count == RecallRecord.seedLength else { return nil }
        return seed
    }

    /// At launch: a stored recall record puts "Was it you?" back on screen
    /// (the screen is driven by the record, not by memory).
    ///
    ///   * A session for the same account was restored with OTHER keys than
    ///     the record's: the recall landed and the app closed before the
    ///     record was destroyed. Destroy it.
    ///   * A session for the same account with the SAME keys: the staged
    ///     wipe was interrupted. The refresh at launch answers
    ///     `key-generation-superseded` again and the wipe runs again,
    ///     keeping this record.
    ///   * No session: show the screen, or end it when the 30 days passed.
    private func restoreRecall(nowMs: Int64) {
        guard let record = recallRecords.load() else { return }
        if let held = heldAccountSigningSeed(accountIdB64: record.accountIdB64),
           signedInAccountIdB64 == record.accountIdB64 || pendingFreshPhrase?.accountIdB64 == record.accountIdB64 {
            if held != record.oldSeed {
                recallRecords.clear(accountIdB64: record.accountIdB64)
            }
            return
        }
        guard case .anon = state else { return }
        recall = RecallScreenState(record: record)
        if record.locallyExpired(nowMs: nowMs) {
            endRecall(.expired)
            return
        }
        Task { [weak self] in await self?.refreshRecallStatus() }
    }

    /// Apply a transition of the recall screen.
    private func applyRecall(_ transition: RecallMachine.Transition) {
        switch transition {
        case .show(let next):
            if recall != nil { recall = next }
        case .end(let end):
            endRecall(end)
        }
    }

    /// The recall is over without the account being taken back: destroy the
    /// record, finish the wipe, and show the signed-out screen (UX 15.2),
    /// with its extra line when there is one.
    private func endRecall(_ end: RecallEnd) {
        guard let current = recall else { return }
        NSLog("[auth] the recall ended (%@); finishing the wipe", String(describing: end))
        recallRecords.clear(accountIdB64: current.accountIdB64)
        recall = nil
        keysSignOutUsername = current.username
        keysSignOutCause = .keysReset
        keysSignOutNoticeKey = end.noticeKey
        keysSignOut = .removing
        let accountIdB64 = current.accountIdB64
        Task { [weak self] in
            guard let self else { return }
            await self.wipeAfterKeyReset(accountIdB64)
            if self.keysSignOut == .removing { self.keysSignOut = .removed }
        }
    }

    /// Ask the server whether the account can still be taken back from this
    /// device (`POST /account/recall/status`). Called after the staged wipe,
    /// at launch, and by the screen while it shows the offline line. A
    /// refusal destroys the record and finishes the wipe; no answer leaves
    /// the question on screen with **No** disabled.
    public func refreshRecallStatus() async {
        guard let current = recall, !recallStatusInFlight, !recallAttemptInFlight else { return }
        guard let record = recallRecords.load(), record.accountIdB64 == current.accountIdB64 else {
            // The record is gone: nothing can be proven any more.
            endRecall(.closed)
            return
        }
        if RecallMachine.expired(current, nowMs: grantNowMs()) {
            endRecall(.expired)
            return
        }
        recallStatusInFlight = true
        let answer = await readRecallStatus(record)
        recallStatusInFlight = false
        guard let latest = recall, latest.accountIdB64 == record.accountIdB64 else { return }
        applyRecall(RecallMachine.afterStatus(latest, answer, nowMs: grantNowMs()))
    }

    private func readRecallStatus(_ record: RecallRecord) async -> RecallAnswers.Status {
        do {
            return .open(try await recallOperations.status(record))
        } catch {
            return RecallAnswers.status(of: error)
        }
    }

    /// **Yes, it was me**: the confirmation step, inside the screen.
    public func recallTapYes() {
        guard let current = recall else { return }
        recall = RecallMachine.tapYes(current)
    }

    /// **No, take the account back**: the form.
    public func recallTapNo() {
        guard let current = recall else { return }
        recall = RecallMachine.tapNo(current)
    }

    /// `Go back` / `Back`.
    public func recallBack() {
        guard let current = recall else { return }
        recall = RecallMachine.back(current)
    }

    /// `Delete and sign in`: local only, nothing is sent. The record is
    /// destroyed and the rest of the wipe runs.
    public func recallConfirmYes() {
        guard let current = recall else { return }
        applyRecall(RecallMachine.confirmYes(current))
    }

    /// Take the account back with the password it had before the reset
    /// (ADR-0074 section 1.2). The identity afterwards is NEW: a fresh
    /// phrase and seed are generated here exactly as a reset generates them,
    /// and once `POST /account/recall/finish` answered the flow IS the key
    /// reset's (``landKeyReset(reset:seed:phrase:newPubkey:accountIdBytes:accountIdB64:username:context:)``):
    /// the owed-publish record, the attested directory upload, this device's
    /// KeyPackage, the DeviceGroup re-founding, the genesis mint, and the
    /// new words shown last, withheld while the publish is owed.
    ///
    /// Every failure leaves the form on screen in the matching state, or
    /// ends the recall when the server says it is over. The finish carries
    /// no device id: this device's identity was deleted by the staged wipe,
    /// and the session is bound right after the landing, as after any
    /// sign-in that learns the account from the answer.
    public func recallTakeBack(oldPassword: Data, newPassword: Data) async {
        await recallTakeBack(proof: .password(oldPassword), newPassword: newPassword)
    }

    /// The same, with either proof (UX 18.4, 19.7): the password from before
    /// the reset, or the recovery phrase from before it (`Use my recovery
    /// phrase instead`; only when the server said the anchor accepts it).
    /// Words that are no phrase at all read as a wrong proof without a
    /// request.
    public func recallTakeBack(proof: RecallProof, newPassword: Data) async {
        var oldPassword = Data()
        var oldPhraseSeed: Data?
        switch proof {
        case .password(let password):
            oldPassword = password
        case .phrase(let phrase):
            guard let seed = try? RecoveryMnemonic.fromPhrase(
                phrase: phrase.trimmingCharacters(in: .whitespacesAndNewlines)).toSeed() else {
                if let current = recall, current.step == .form {
                    var next = current
                    next.form = .wrongPassword(triesLeft: nil)
                    recall = next
                }
                return
            }
            oldPhraseSeed = seed
        }
        guard let current = recall, !recallAttemptInFlight,
              case .open(let window) = current.check,
              let working = RecallMachine.submit(current) else { return }
        guard let record = recallRecords.load(), record.accountIdB64 == current.accountIdB64,
              let accountIdBytes = record.accountId else {
            endRecall(.closedInForm)
            return
        }
        recallAttemptInFlight = true
        defer { recallAttemptInFlight = false }
        recall = working
        let writes = SignInKeyWrites()

        let mnemonic = RecoveryMnemonic.generate()
        let phrase = mnemonic.phrase()
        let seed = mnemonic.toSeed()
        // ADR-0077 decision 3, rule 1: the new phrase is in the account's
        // Keychain row before `recall/finish`, which makes it the account's
        // only way back. The staged wipe sealed the account's stores before
        // this route began, which the route's mark does not count. A row
        // that cannot be written ends the attempt with nothing sent; the
        // form says so as it does for any other failure.
        let kept: FreshPhraseKeep
        do {
            kept = try FreshPhraseKeep.keep(
                phrase: phrase, accountIdB64: record.accountIdB64,
                since: writes.mark(record.accountIdB64), store: phraseStore)
        } catch {
            NSLog("[auth] recall refused: the new recovery phrase could not be stored on this device")
            if let latest = recall {
                applyRecall(RecallMachine.afterRefusal(latest, .failed))
            }
            return
        }
        let landed: RecallLanded
        let newPubkey: Data
        do {
            newPubkey = try Identity.fromSeed(seed: seed).signaturePublicKey()
            landed = try await recallOperations.takeBack(
                RecallAttempt(
                    record: record,
                    anchorSeq: window.anchorSeq,
                    oldPassword: oldPassword,
                    oldPhraseSeed: oldPhraseSeed,
                    newPassword: newPassword,
                    newSeed: seed,
                    newSignaturePubkey: newPubkey,
                    deviceId: nil))
        } catch {
            // The account was not taken back: the row goes back to what it
            // held (nothing, after the staged wipe).
            kept.undo()
            guard let latest = recall else { return }
            switch RecallAnswers.refusal(of: error) {
            case .wrongPassword:
                // How many tries are left is the server's to say.
                let status = await readRecallStatus(record)
                guard let after = recall else { return }
                applyRecall(RecallMachine.afterWrongPassword(after, status: status, nowMs: grantNowMs()))
            case let refusal:
                NSLog("[auth] the recall did not land (%@)", String(describing: refusal))
                applyRecall(RecallMachine.afterRefusal(latest, refusal))
            }
            return
        }

        // The account was removed from this device (Forget device, the end
        // of the recall) while the take-back was out: the server took the
        // account back, and nothing of it is written here again.
        guard !writes.isWiped(record.accountIdB64) else {
            NSLog("[auth] the account was removed from this device while it was taken back; nothing is kept here")
            recall = nil
            return
        }
        // The server took the account back. The record has done its work:
        // destroy it before anything else, then the key reset's client order.
        recallRecords.clear(accountIdB64: record.accountIdB64)
        recall = nil
        // The account lands on this device again: the staged wipe sealed its
        // stores, and what follows writes them.
        AccountStoreGate.open(record.accountIdB64)
        freshAuth.put(landed.freshGrant, accountIdB64: record.accountIdB64)
        // The staged wipe removed the phrase-saved flag; the record kept it.
        // Taking the account back with the phrase is itself only possible
        // for a phrase-protected account.
        protectionStore.setPhraseSaved(
            oldPhraseSeed != nil || record.phraseSaved == true, accountIdB64: record.accountIdB64)
        accountRecalledThisSession = true
        // The staged wipe removed the session record; a relaunch on the
        // phrase screen restores from it.
        sessionStore.save(PersistedSession(accountIdB64: record.accountIdB64, username: record.username))
        await landKeyReset(
            reset: landed.reset,
            seed: seed,
            phrase: phrase,
            newPubkey: newPubkey,
            accountIdBytes: accountIdBytes,
            accountIdB64: record.accountIdB64,
            username: record.username,
            context: "recall",
            writes: writes
        )
    }

    // MARK: - ADR-0075 section 8: taking the account back with the phrase, no old device

    /// `Get back in > Recovery phrase` (UX 19.7): the words typed are not
    /// the account's phrase NOW. Ask the server whether they were its phrase
    /// before a reset in the last month: `POST /account/recall/status` by
    /// login handle, the statement signed by the key derived from the typed
    /// words. `nil` for every other answer (an unknown name, words that were
    /// never this account's, a passed window, an account that had an email
    /// vault, no connection): the form then says what it always says.
    public func phraseRecallOffer(username: String, mnemonicPhrase: String) async -> PhraseRecallOffer? {
        guard let name = try? LoginName.make(username, emptyError: SessionNotReady()),
              let mnemonic = try? RecoveryMnemonic.fromPhrase(
                phrase: mnemonicPhrase.trimmingCharacters(in: .whitespacesAndNewlines)),
              let oldKeySeed = try? Identity.fromSeed(seed: mnemonic.toSeed()).ed25519Seed(),
              let answer = try? await recallOperations.statusByHandle(name.handle, oldKeySeed),
              PhraseRecallOffer.offered(answer.window, nowMs: grantNowMs()) else {
            return nil
        }
        return PhraseRecallOffer(accountId: answer.accountId, username: name.canonical, window: answer.window)
    }

    /// `Take the account back` on the 19.7 prompt: the recall of 18.4 with
    /// the phrase as the proof, from a device that holds nothing else of the
    /// account. The identity afterwards is NEW, exactly as after
    /// ``recallTakeBack(proof:newPassword:)``: the reset's client order, the
    /// new words shown last.
    ///
    /// Throws ``RecallWrongPassword`` (the words did not open the anchor) or
    /// what the routes answered; nothing changed then.
    public func takeBackWithPhrase(
        offer: PhraseRecallOffer, mnemonicPhrase: String, newPassword: Data
    ) async throws {
        guard !recallAttemptInFlight else { return }
        let old = try RecoveryMnemonic.fromPhrase(
            phrase: mnemonicPhrase.trimmingCharacters(in: .whitespacesAndNewlines))
        var oldSeed = old.toSeed()
        defer { Self.zeroize(&oldSeed) }
        let accountIdB64 = base64Encode(offer.accountId)
        let record = RecallRecord(
            accountIdB64: accountIdB64,
            oldSeed: try Identity.fromSeed(seed: oldSeed).ed25519Seed(),
            username: offer.username,
            learnedAtMs: grantNowMs())
        recallAttemptInFlight = true
        defer { recallAttemptInFlight = false }
        let writes = SignInKeyWrites()

        let mnemonic = RecoveryMnemonic.generate()
        let phrase = mnemonic.phrase()
        let seed = mnemonic.toSeed()
        let newPubkey = try Identity.fromSeed(seed: seed).signaturePublicKey()
        // ADR-0077 decision 3, rule 1: the new phrase is in the account's
        // Keychain row before `recall/finish`; a row that cannot be written
        // throws ``FreshPhraseKeep/NotStored`` with nothing sent, and a
        // take-back that does not land puts the row back.
        let kept: FreshPhraseKeep
        do {
            kept = try FreshPhraseKeep.keep(
                phrase: phrase, accountIdB64: accountIdB64,
                since: writes.mark(accountIdB64), store: phraseStore)
        } catch {
            NSLog("[auth] recall by phrase refused: the new recovery phrase could not be stored on this device")
            throw FreshPhraseKeep.NotStored()
        }
        let landed: RecallLanded
        do {
            landed = try await recallOperations.takeBack(
                RecallAttempt(
                    record: record,
                    anchorSeq: offer.window.anchorSeq,
                    oldPassword: Data(),
                    oldPhraseSeed: oldSeed,
                    newPassword: newPassword,
                    newSeed: seed,
                    newSignaturePubkey: newPubkey,
                    deviceId: nil))
        } catch {
            kept.undo()
            throw error
        }
        // Removed from this device while the take-back was out: nothing of
        // it is written here again.
        if writes.isWiped(accountIdB64) { throw AccountStoreGate.Sealed() }
        freshAuth.put(landed.freshGrant, accountIdB64: accountIdB64)
        // The account lands on this device (again): its stores are written.
        AccountStoreGate.open(accountIdB64)
        accountRecalledThisSession = true
        sessionLanding = .other
        sessionStore.save(PersistedSession(accountIdB64: accountIdB64, username: offer.username))
        // The new identity has a recovery phrase the person is about to be
        // shown: the account stays phrase-protected.
        protectionStore.setPhraseSaved(true, accountIdB64: accountIdB64)
        await landKeyReset(
            reset: landed.reset,
            seed: seed,
            phrase: phrase,
            newPubkey: newPubkey,
            accountIdBytes: offer.accountId,
            accountIdB64: accountIdB64,
            username: offer.username,
            context: "recall by phrase",
            writes: writes
        )
    }

    /// The person dismissed the "You took your account back" line.
    public func dismissRecalledNotice() {
        accountRecalledThisSession = false
    }

    /// Rule 4 of ADR-0073 section 4.2: the directory publishes other keys
    /// at the same generation. The blocking screen; the wipe runs on the
    /// tap (``signOutAfterKeysMismatch()``). Nothing is uploaded.
    private func showKeysMismatch(accountIdB64: String) {
        guard keysSignOut == nil, recall == nil else { return }
        keysSignOutUsername = signedInUsername(accountIdB64: accountIdB64)
        keysSignOutCause = .keysReset
        keysSignOutNoticeKey = nil
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
        keysSignOutNoticeKey = nil
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
    ///
    /// `phraseHeldByPerson` is `true` only when the person typed the phrase
    /// (the restore prompt): they have shown they hold it, so a phrase owed
    /// to them (ADR-0077 decision 3, rule 6) is not shown back. A phrase the
    /// device brought itself (the row kept for an owed phrase, a passkey's
    /// vault) leaves the marker as it is.
    public func provideRecoveryPhrase(mnemonicPhrase: String, phraseHeldByPerson: Bool = false) async throws {
        try await provideRecoveryPhrase(
            mnemonicPhrase: mnemonicPhrase, writes: SignInKeyWrites(),
            phraseHeldByPerson: phraseHeldByPerson)
    }

    /// ``provideRecoveryPhrase(mnemonicPhrase:phraseHeldByPerson:)`` for a
    /// route that began earlier (a passkey sign-in asking its passkey for the
    /// keys): `writes` is that route's ``SignInKeyWrites``.
    private func provideRecoveryPhrase(
        mnemonicPhrase: String, writes: SignInKeyWrites, phraseHeldByPerson: Bool = false
    ) async throws {
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
        let bound = binding.capture()
        do {
            try await verifySeedAgainstPublishedIdentity(
                accountIdBytes: pending.accountIdBytes,
                sessionToken: pending.sessionToken,
                accountIdentitySeed: accountIdentitySeed
            )
            // Signed out (or forgot the device) while the bundle was being
            // fetched: persist nothing for the account that was parked.
            try bound.require()
            // Nor for an account wiped on this device since the route began.
            if writes.isWiped(pending.accountIdB64) { throw AccountStoreGate.Sealed() }
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
            context: "provideRecoveryPhrase",
            writes: writes
        )

        // Cache the phrase seed under the device so future warm logins
        // skip the prompt. Best-effort.
        do {
            try writes.ifNotWiped(pending.accountIdB64) {
                try AccountIdentitySeedStore.putAccountIdentitySeed(
                    accountIdB64: pending.accountIdB64,
                    seed: accountIdentitySeed
                )
            }
        } catch {
            NSLog(
                "account-identity-seed persist (provideRecoveryPhrase) " +
                "failed: \(error)"
            )
        }
        // Strictly after the fail-closed check: the phrase is the account's.
        DevicePhrase.store(
            canonicalPhrase, accountIdB64: pending.accountIdB64, context: "provide-phrase",
            since: writes.mark(pending.accountIdB64))

        // The phrase restored the identity, so the account backup is fetched
        // and applied here exactly as the restore screen does it (canonical
        // phrase, the bytes every envelope is sealed under). This is the
        // completion of EVERY route that brings the phrase to a parked
        // sign-in: the typed phrase, a passkey that carries the keys, and
        // the recovery-email code (ADR-0075), each of which hands the phrase
        // here. None of this can fail the sign-in.
        let backupOutcome = await restoreBackupFromPhrase(
            accountId: pending.accountIdBytes,
            accountIdB64: pending.accountIdB64,
            sessionToken: pending.currentSession(tokenProvider).sessionToken,
            phrase: canonicalPhrase,
            context: "provideRecoveryPhrase",
            writes: writes
        )
        // The fetch suspended. If the parked recovery went away meanwhile
        // (a log out), there is no session to land. The epoch and not the
        // account id: signing out and parking the same account again is a
        // new binding, with its own session token.
        guard bound.isCurrent else {
            Self.zeroize(&accountIdentitySeed)
            throw StaleBindingError()
        }

        // Typed and accepted: the person holds the phrase, so it is not owed
        // and the landing below does not show it back to them. Before the
        // landing, which reads the marker.
        if phraseHeldByPerson {
            markers.clearPhraseOwed(accountIdB64: pending.accountIdB64)
        }
        let landed = pending.currentSession(tokenProvider)
        finishCanonicalSession(
            accountIdentitySeed: accountIdentitySeed,
            accountIdBytes: pending.accountIdBytes,
            exportKey: pending.exportKey,
            session: landed,
            username: pending.username,
            retryMarkedBackupRestore: false
        )
        // The route ends with the keys on this device: a baseline backup
        // carrying what was just merged (no MLS state). Not when the fetch
        // failed: the restore is still owed and the baseline would replace
        // the blob the next landing is going to fetch.
        uploadBaselineBackupAfterRestore(
            accountId: pending.accountIdBytes,
            accountIdB64: pending.accountIdB64,
            accountIdentitySeed: accountIdentitySeed,
            sessionToken: landed.sessionToken,
            phrase: canonicalPhrase,
            restoreOutcome: backupOutcome,
            context: "provideRecoveryPhrase",
            writes: writes
        )
    }

    /// The baseline backup of a route that just restored the keys on this
    /// device, in the background: never delays or fails the landing. Stands
    /// aside while the restore is still owed.
    private func uploadBaselineBackupAfterRestore(
        accountId: Data,
        accountIdB64: String,
        accountIdentitySeed: Data,
        sessionToken: String,
        phrase: String,
        restoreOutcome: PhraseRestoreBackupOutcome?,
        context: String,
        writes: SignInKeyWrites
    ) {
        let bound = binding.capture()
        Task { [weak self] in
            guard let self, bound.isCurrent else { return }
            await self.bootstrapBackupBestEffort(
                accountId: accountId,
                accountIdB64: accountIdB64,
                accountIdentitySeed: accountIdentitySeed,
                sessionToken: sessionToken,
                phrase: phrase,
                context: context,
                writes: writes,
                uploadBaseline: restoreOutcome != .fetchFailed
                    && !BackupRestoreRetryStore.isPending(
                        accountIdB64: accountIdB64, defaults: self.backupDefaults)
            )
        }
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
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        do {
            try await verifySeedAgainstPublishedIdentity(
                accountIdBytes: pending.accountIdBytes,
                sessionToken: session?.sessionToken ?? pending.sessionToken,
                accountIdentitySeed: seed
            )
            // Signed out while the bundle was being fetched: no session
            // lands and nothing is persisted.
            try bound.require()
            // Nor for an account wiped on this device since the route began.
            if writes.isWiped(pending.accountIdB64) { throw AccountStoreGate.Sealed() }
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
            context: "completeFromPairing",
            writes: writes
        )
        do {
            try writes.ifNotWiped(pending.accountIdB64) {
                try AccountIdentitySeedStore.putAccountIdentitySeed(
                    accountIdB64: pending.accountIdB64,
                    seed: seed
                )
            }
        } catch {
            NSLog(
                "account-identity-seed persist (completeFromPairing) " +
                "failed: \(error)"
            )
        }
        // This device now holds the phrase like the one that confirmed it.
        DevicePhrase.store(
            transferred.phrase, accountIdB64: pending.accountIdB64, context: "device-pairing",
            since: writes.mark(pending.accountIdB64))
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
        // Another sign-in that lands while the checks below are out owns the
        // device: this restore then writes nothing and lands nothing.
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        // The pairing's held state (`persistJoin`) is written before the
        // session lands.
        AccountStoreGate.open(base64Encode(accountIdBytes))
        do {
            try await verifySeedAgainstPublishedIdentity(
                accountIdBytes: accountIdBytes,
                sessionToken: session.sessionToken,
                accountIdentitySeed: seed
            )
            try bound.require()
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
            try bound.require()
        } catch {
            Self.zeroize(&seed)
            throw error
        }
        // Both checks passed: the pairing's held state lands first, so the
        // DeviceGroup snapshot is on disk before the session bootstraps.
        await persistJoin()
        guard bound.isCurrent else {
            Self.zeroize(&seed)
            throw StaleBindingError()
        }
        // The account was wiped on this device while the checks were out:
        // none of its keys go back.
        if writes.isWiped(accountIdB64) {
            Self.zeroize(&seed)
            throw AccountStoreGate.Sealed()
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
            try writes.ifNotWiped(accountIdB64) {
                try DeviceKeyStore.putDeviceKey(
                    accountIdB64: accountIdB64,
                    rootKey: deviceRootKey
                )
            }
        } catch {
            NSLog(
                "device-key persist (completeFromDeviceRestore) " +
                "failed: \(error)"
            )
        }
        Self.zeroize(&deviceRootKey)
        do {
            try writes.ifNotWiped(accountIdB64) {
                try AccountIdentitySeedStore.putAccountIdentitySeed(
                    accountIdB64: accountIdB64,
                    seed: seed
                )
            }
        } catch {
            NSLog(
                "account-identity-seed persist (completeFromDeviceRestore) " +
                "failed: \(error)"
            )
        }
        // This device now holds the phrase like the one that confirmed it.
        DevicePhrase.store(
            transferred.phrase, accountIdB64: accountIdB64, context: "device-restore",
            since: writes.mark(accountIdB64))
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
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        let result = try await client.totpRecover(
            handle: name.handle,
            code: code
        )
        // As in ``login(username:password:)``: overtaken, lands nothing.
        try bound.require()
        let accountIdBytes = result.accountId
        let accountIdB64 = base64Encode(accountIdBytes)
        let resolvedUsername = name.canonical

        restoreNotice = nil
        // An authenticator code signs in; it is not a fresh-auth proof.
        freshAuth.put(nil, accountIdB64: accountIdB64)
        sessionLanding = .authenticator
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
            pushContext: "totp-recover",
            writes: writes
        )
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "totp recover")
    }

    /// ADR-0075 section 7, password mode (UX 19.3 step 4, "Phrase saved"):
    /// the mailed code (and the authenticator code, when the account has
    /// one) was accepted and the account has a saved phrase, so the code
    /// changes the password and nothing else. `ticket` is the one
    /// `recover/verify` answered and `recover/open` put in password mode.
    ///
    /// The new password is registered under the ticket
    /// (`POST /recovery-email/recover/password/{start,finish}`), which
    /// answers a session and the account id, and NO fresh-auth grant. The
    /// landing is the access-only one: a device that holds the keys lands
    /// signed in, any other shows **Restore your keys**.
    public func recoverPasswordByEmail(username: String, ticket: String, newPassword: Data) async throws {
        let name = try LoginName.make(
            username, emptyError: AuthClientError.argument("enter the username of the account to recover"))
        let bound = binding.capture()
        let writes = SignInKeyWrites()
        let result = try await authClientFactory().recoveryEmailRecoverPassword(
            ticket: ticket, newPassword: newPassword)
        // A sign-out, or another account's landing, while the round trip was
        // out: the password is changed on the server, and nothing lands here.
        try bound.require()
        let accountIdBytes = result.accountId
        let accountIdB64 = base64Encode(accountIdBytes)

        restoreNotice = nil
        // A mailed code is not a fresh-auth proof.
        freshAuth.put(nil, accountIdB64: accountIdB64)
        sessionLanding = .other
        // The password was just chosen: no new-password step.
        newPasswordOwed = false
        if Self.readCachedAccountIdentitySeed(accountIdB64: accountIdB64, context: "email recover") != nil {
            do {
                try writes.ifNotWiped(accountIdB64) {
                    try ExportKeyStore.putExportKey(accountIdB64: accountIdB64, exportKey: result.exportKey)
                }
            } catch {
                NSLog("export-key persist (email recover) failed: \(error)")
            }
        }
        await landAfterAccessRecovery(
            username: name.canonical,
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            session: result.session,
            context: "email recover",
            pushContext: "email-recover",
            writes: writes
        )
        ensureLoginHandleBound(name: name, sessionToken: result.sessionToken, context: "email recover")
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
        writes: SignInKeyWrites,
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
                context: context,
                writes: writes
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
        // A new binding: ACCESS is granted, and the restore that follows
        // writes this account's stores before the session lands.
        binding.advance()
        AccountStoreGate.open(accountIdB64)
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
        completeParkFromOwedPhrase(accountIdB64: accountIdB64)
    }

    /// A sign-in parked in the restore prompt because this device holds no
    /// identity seed for the account, while the phrase of a fresh identity
    /// the person was never shown is still owed (``RecoveryMarkers``) and
    /// held on this device (the seed did not persist, or the phrase screen
    /// never came up). Asking for that phrase would ask for something nobody
    /// has seen, so the park is completed from the held phrase, through
    /// ``provideRecoveryPhrase(mnemonicPhrase:)`` and its check against the
    /// published identity. The landing then shows the owed phrase first
    /// (``parkOwedFreshPhrase(accountIdBytes:accountIdB64:username:seed:session:)``).
    ///
    /// Any failure leaves the account parked in the prompt as before.
    private func completeParkFromOwedPhrase(accountIdB64: String) {
        guard markers.isPhraseOwed(accountIdB64: accountIdB64),
              let phrase = DevicePhrase.read(accountIdB64: accountIdB64, store: phraseStore) else {
            return
        }
        let bound = binding.capture()
        Task { @MainActor [weak self] in
            guard let self, bound.isCurrent else { return }
            do {
                // Completed from the kept row: the person has not seen it,
                // so it stays owed and is shown at the landing.
                try await self.provideRecoveryPhrase(mnemonicPhrase: phrase, phraseHeldByPerson: false)
            } catch {
                if error is StaleBindingError { return }
                NSLog("[auth] the kept recovery phrase did not complete the restore: \(error)")
            }
        }
    }

    /// The just-minted fresh recovery phrase, for the
    /// ``AuthState/saveFreshPhrase(_:)`` screen to display. `nil` outside
    /// that flow. Kept off the ``AuthState`` so it is not threaded through
    /// the router (mirror of the web `freshPhrase()` accessor).
    public func freshPhrase() -> String? {
        // ADR-0075: an account that is not phrase-protected is shown none.
        guard let pending = pendingFreshPhrase, pending.showsWords else { return nil }
        return pending.phrase
    }

    /// "Reset account keys" (ADR-0073 section 2.3, UX 15.1): the
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
    ///   1. Generate the phrase + seed; derive the new identity; write the
    ///      phrase to this account's Keychain row (``FreshPhraseKeep``). A
    ///      row that cannot be written throws ``FreshPhraseKeep/NotStored``
    ///      and nothing is sent (ADR-0077 decision 3).
    ///   2. The server step. A failure here changes nothing: `start` stores
    ///      nothing, `finish` is one transaction, the phrase is discarded and
    ///      the row is put back as step 1 found it.
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
    /// password sign-in first (``obtainProofForPendingRestore(password:username:pending:)``);
    /// with neither, ``FreshAuthRequired`` is thrown before any
    /// network call. A wrong password throws ``FreshAuthWrongPassword``; a
    /// `403 fresh-auth-required` from the reset drops the grant and throws
    /// ``FreshAuthRequired``. In each case nothing was reset.
    ///
    /// `username` is used only when the parked account has none on this
    /// device (a passkey sign-in that named no account): the sign-in is then
    /// addressed by the handle of the typed name. With no name to derive a
    /// handle from, ``FreshAuthUsernameRequired`` is thrown before any call.
    public func mintFreshIdentity(password: Data? = nil, username: String? = nil) async throws -> String {
        guard var pending = pendingRecovery else {
            throw AuthClientError.argument("no pending identity recovery — sign in first")
        }
        let writes = SignInKeyWrites()

        // 0. The fresh-auth proof: the one held (a sign-in or a passkey
        //    gesture in the last ten minutes), else a password sign-in now.
        let grant: String
        if let held = freshAuth.live(accountIdB64: pending.accountIdB64, nowMs: grantNowMs()) {
            grant = held
        } else {
            guard let password, !password.isEmpty else { throw FreshAuthRequired() }
            do {
                try await proveWithPassword(password: password, username: username)
            } catch is FreshAuthNoAccount {
                // The restore prompt was left meanwhile (sign-out).
                throw AuthClientError.argument("no pending identity recovery — sign in first")
            }
            guard let renewed = pendingRecovery, renewed.accountIdB64 == pending.accountIdB64,
                  let minted = freshAuth.live(accountIdB64: pending.accountIdB64, nowMs: grantNowMs()) else {
                throw AuthClientError.argument("no pending identity recovery — sign in first")
            }
            pending = renewed
            grant = minted
        }

        // 1. A fresh 12-word phrase + seed (the client-side generation
        //    signup uses) and the identity it seeds.
        let mnemonic = RecoveryMnemonic.generate()
        let phrase = mnemonic.phrase()
        let seed = mnemonic.toSeed()
        let newPubkey = try Identity.fromSeed(seed: seed).signaturePublicKey()

        // ADR-0077 decision 3, rule 1: the phrase goes into this account's
        // own Keychain row BEFORE the server step. From that step on it is
        // the account's only way back, and it must not exist in this call
        // alone: the app can be killed right after the answer. A row that
        // cannot be written refuses the reset here, where nothing
        // irreversible has happened; a failed server step puts the row back.
        // No wait sits between this write and the server call, so there is
        // no binding check in between to fail.
        let kept: FreshPhraseKeep
        do {
            kept = try FreshPhraseKeep.keep(
                phrase: phrase, accountIdB64: pending.accountIdB64,
                since: writes.mark(pending.accountIdB64), store: phraseStore)
        } catch {
            NSLog("[auth] key reset refused: the new recovery phrase could not be stored on this device")
            throw FreshPhraseKeep.NotStored()
        }

        // 2. The server step: the point of no return.
        // ADR-0074 "T1 as built" point 10: the pair a reset returns keeps the
        // device the bearer names, so the client-side note moves with it.
        let bearer = pending.currentSession(tokenProvider)
        // What the answer lands into is decided by what the device is bound
        // to once it is back (``FreshIdentityLanding``): the reset cannot be
        // taken back, but a sign-out, a wipe or another account's landing
        // while it was out changes what may be written here.
        let bound = binding.capture()
        let reset: KeyResetResult
        do {
            let answered = try await keyResetOperation(bearer.sessionToken, grant, seed, newPubkey)
            reset = KeyResetResult(
                attestation: answered.attestation,
                session: answered.session.naming(deviceIdB64: bearer.deviceIdB64))
        } catch {
            // Nothing was reset: the row goes back to what it held.
            kept.undo()
            if FreshAuthRefusal.isRequired(error) {
                // The grant expired on the way (or was spent): ask again.
                freshAuth.clear()
                throw FreshAuthRequired()
            }
            throw error
        }
        // A successful reset spends the grant and every one before it.
        freshAuth.clear()

        // 3 to 5. Persist, park the phrase screen, publish.
        let landed = await landKeyReset(
            reset: reset,
            seed: seed,
            phrase: phrase,
            newPubkey: newPubkey,
            accountIdBytes: pending.accountIdBytes,
            accountIdB64: pending.accountIdB64,
            username: pending.username,
            context: "key reset",
            writes: writes,
            bound: bound
        )
        // Not this device's session any more (another account signed in, or
        // the account was removed from this device): no screen of this flow
        // is shown and the caller reads no success.
        guard landed else { throw StaleBindingError() }
        return phrase
    }

    /// Steps 3 to 5 of a key reset's client order (ADR-0073 section 2.3),
    /// after the server step answered: persist the new pair, the seed and
    /// the phrase with the owed-publish record BEFORE any further network
    /// call, park the phrase screen, then run the publish. The ONE code path
    /// for it: ``mintFreshIdentity(password:username:)`` comes here after
    /// `POST /account/keys/reset/finish`, and a recall (ADR-0074 section
    /// 1.2: "from the reset's client order onward the recall IS a reset")
    /// after `POST /account/recall/finish`.
    @discardableResult
    private func landKeyReset(
        reset: KeyResetResult,
        seed: Data,
        phrase: String,
        newPubkey: Data,
        accountIdBytes: Data,
        accountIdB64: String,
        username: String,
        context: String,
        writes: SignInKeyWrites,
        bound: BindingEpoch.Captured? = nil
    ) async -> Bool {
        // The account was forgotten on this device (Forget device, Delete
        // account) while the server step was out: its stores were wiped and
        // nothing of it is written back. Measured from the route's start
        // (`writes`): a recall runs for an account whose stores a staged
        // wipe removed BEFORE it began, and writes them. The hold covers the
        // store writes of step 3 only, never a wait.
        guard let gate = AccountStoreGate.enter(unlessWipedSince: writes.mark(accountIdB64)) else {
            NSLog("[auth] account keys reset after the account was removed from this device; nothing is kept here")
            return false
        }
        // Where the answer lands: here, unless another account signed in
        // while the server step was out. A caller with no binding of its own
        // (a recall, which runs signed out) lands here.
        let landing = bound.map {
            FreshIdentityLanding.decide(
                mintedFor: accountIdB64,
                bindingIsCurrent: $0.isCurrent,
                signedIn: signedInAccountIdB64)
        } ?? .showPhrase(rebinds: false)
        // 3. Persist, before any further network call. The pair goes to the
        // token provider only when this account is the one the device is
        // bound to: it serves every bearer call of whoever is signed in.
        if landing != .keepForNextSignIn {
            adoptSessionPair(reset.session, accountIdB64: accountIdB64, context: context)
        }
        bootstrapDeviceKeyFromVerifiedSeedIfAbsent(
            accountIdB64: accountIdB64,
            accountIdentitySeed: seed,
            context: context,
            writes: writes
        )
        do {
            try AccountIdentitySeedStore.putAccountIdentitySeed(
                accountIdB64: accountIdB64,
                seed: seed
            )
        } catch {
            NSLog("fresh-identity seed persist failed: \(error)")
        }
        // The device holds the new keys, so it holds the new phrase.
        DevicePhrase.store(
            phrase, accountIdB64: accountIdB64, context: "fresh-identity",
            since: writes.mark(accountIdB64))
        // ADR-0075 — the new words are shown only to a phrase-protected
        // account. Any other account gets its new keys with no words on any
        // screen: the device keeps them unseen, as after sign-up.
        let showsWords = NewWordsRule.shown(
            phraseProtected: protectionStore.phraseSaved(accountIdB64: accountIdB64))
        if showsWords { markers.markPhraseOwed(accountIdB64: accountIdB64) }
        let record = KeyResetPublishRecord(
            attestation: reset.attestation,
            keyGeneration: reset.session.keyGeneration,
            signedAtMs: 0,
            newSignaturePubkey: newPubkey
        )
        markers.markPublishOwed(record, accountIdB64: accountIdB64)
        // ADR-0074 section 3.5: a reset makes every predecessor, proven or
        // not, a superseded key (web `deleteKeyRotationState` on the same
        // step). ADR-0078 section 6: the DeviceGroup id is derived from the
        // oldest predecessor while one is held, so a kept one would name the
        // OLD DeviceGroup instead of the one the new seed founds.
        ownPredecessors.clear(accountIdB64: accountIdB64)
        gate.leave()

        switch landing {
        case .showPhrase(let rebinds):
            // After a sign-out the phrase screen binds the device to this
            // account again: a sign-in still out from the signed-out state
            // was overtaken by it.
            if rebinds { binding.advance() }
        case .keepForNextSignIn:
            // Another account signed in while the reset was out. Its session
            // and its screens are not this flow's: nothing of the state or
            // of the token provider is touched. The new seed, the phrase and
            // the owed records are on this device (above), so this account's
            // next sign-in here lands on the new keys, shows an owed phrase
            // first and finishes the publish
            // (``parkOwedFreshPhrase(accountIdBytes:accountIdB64:username:seed:session:)``,
            // ``settleOwedKeyResetPublish(accountIdB64:accountIdBytes:accountIdentitySeed:requireFresh:)``).
            NSLog("[auth] account keys reset after another account signed in; the rest is owed at that account's next sign-in")
            return false
        }

        keysResetThisSession = true
        // ADR-0074 section 0.1 — auth recorded the new key inside the reset;
        // the mint for it is idempotent and moves this device's mark.
        Task { [weak self] in
            await self?.recordIdentityKeyIfNeeded(
                accountIdBytes: accountIdBytes, accountIdentitySeed: seed)
        }

        // The state flips before the publish so a failure parks the screen
        // in its "could not finish publishing" form. For an account that is
        // not phrase-protected that form names no words, and once the
        // publish lands the session does too, with no phrase screen
        // (``finishOwedPublish(accountIdB64:)``).
        freshShowsWords = showsWords
        pendingFreshPhrase = PendingFreshPhrase(
            seed: seed,
            accountIdBytes: accountIdBytes,
            accountIdB64: accountIdB64,
            session: reset.session,
            username: username,
            phrase: phrase,
            publishOwed: true,
            showsWords: showsWords
        )
        freshPublishPending = true
        freshPublishNeedsSignIn = false
        pendingRecovery = nil
        restoreNotice = nil
        state = .saveFreshPhrase(
            SessionAccount(
                accountIdB64: accountIdB64,
                username: username
            )
        )

        // 4. The publish. The attestation was just minted: no freshness
        //    bound applies.
        do {
            try await runOwedKeyResetPublish(
                accountIdB64: accountIdB64,
                accountIdBytes: accountIdBytes,
                accountIdentitySeed: seed,
                requireFresh: false
            )
        } catch {
            NSLog("[auth] the publish after a key reset did not finish; the phrase screen offers a retry")
        }
        return true
    }

    /// Whether a live fresh-auth proof is held for the account parked on the
    /// restore prompt: the Reset account keys screen then shows no proof
    /// block (UX 17.2).
    public func holdsLiveFreshAuthForPendingRestore() -> Bool {
        guard let pending = pendingRecovery else { return false }
        return freshAuth.holdsLive(accountIdB64: pending.accountIdB64, nowMs: grantNowMs())
    }

    // MARK: - Fresh-auth proof (ADR-0073, amended 2026-10-06; UX 17)

    /// The account a proof is obtained for: the one parked on the restore
    /// prompt, else the one signed in.
    private struct ProofAccount {
        let accountIdBytes: Data
        let accountIdB64: String
        let username: String
    }

    private func proofAccount() -> ProofAccount? {
        if let pending = pendingRecovery {
            return ProofAccount(
                accountIdBytes: pending.accountIdBytes,
                accountIdB64: pending.accountIdB64,
                username: pending.username)
        }
        if case .session(let account) = state, let secrets = identitySecrets {
            return ProofAccount(
                accountIdBytes: secrets.accountId,
                accountIdB64: account.accountIdB64,
                username: account.username)
        }
        return nil
    }

    /// Whether a live fresh-auth proof is held for the current account (the
    /// one signed in, or parked on the restore prompt). A surface draws no
    /// proof block while this is true.
    public func holdsLiveFreshAuth() -> Bool {
        guard let account = proofAccount() else { return false }
        return freshAuth.holdsLive(accountIdB64: account.accountIdB64, nowMs: grantNowMs())
    }

    /// The grant to send on a gated route for the current account, or `nil`
    /// when no live proof is held (the surface then shows its block). Never
    /// logged or stored by a caller.
    public func liveFreshGrant() -> String? {
        guard let account = proofAccount() else { return nil }
        return freshAuth.live(accountIdB64: account.accountIdB64, nowMs: grantNowMs())
    }

    /// A gated route answered `403 fresh-auth-required`: the proof ran out
    /// on the way. Drop it so the surface shows its block.
    public func dropFreshAuth() {
        freshAuth.clear()
    }

    /// Whether the device knows the current account's username (the password
    /// proof names the account by its login handle; a passkey sign-in that
    /// named no account leaves none).
    public var proofUsernameKnown: Bool {
        guard let account = proofAccount() else { return false }
        return !account.username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    /// Whether the current account has at least one passkey (`GET
    /// /webauthn/credentials` with the current token), or `nil` when it
    /// could not be read. A proof block offers `Use a passkey instead` only
    /// on `true`.
    public func accountHasPasskey() async -> Bool? {
        guard let account = proofAccount() else { return nil }
        _ = account
        let token = pendingRecovery?.currentSession(tokenProvider).sessionToken
            ?? identitySecrets?.sessionToken
        guard let token else { return nil }
        return await passkeyPresence(token)
    }

    /// Prove who the person is with the account password: the same OPAQUE
    /// login the sign-in screen uses, by the account's login handle. It
    /// mints a fresh-auth grant, kept in memory.
    ///
    /// The sign-in returns a fresh session pair for the SAME account at the
    /// same key generation; it is installed in place of the current one and
    /// nothing else happens: no session landing, so no landing rule, no
    /// wipe, no handle re-bind, no push subscribe. A sign-in that names
    /// another account, or carries no grant, is refused and installs
    /// nothing.
    ///
    /// The account is named by its own username; `username` (typed into the
    /// block) only when the device knows none. A typed name that signs in to
    /// a DIFFERENT account is reported as a wrong password: it is not this
    /// account's password, and nothing of that sign-in is kept.
    ///
    /// Throws ``FreshAuthWrongPassword``, ``FreshAuthUsernameRequired``,
    /// ``FreshAuthRequired`` (the sign-in carried no usable grant),
    /// ``FreshAuthNoAccount``, or what the login threw (offline, `429`).
    public func proveWithPassword(password: Data, username: String? = nil) async throws {
        guard let account = proofAccount() else { throw FreshAuthNoAccount() }
        guard !password.isEmpty else { throw FreshAuthRequired() }
        let known = account.username.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let handle = ownLoginHandle(username: known.isEmpty ? (username ?? "") : known) else {
            throw FreshAuthUsernameRequired()
        }
        let result: LoginResult
        do {
            result = try await proofLoginOperation(handle, password)
        } catch {
            if FreshAuthRefusal.isWrongPassword(error) { throw FreshAuthWrongPassword() }
            throw error
        }
        guard result.accountId == account.accountIdBytes else {
            NSLog("[auth] WARN the password sign-in for a proof named another account; refused")
            throw FreshAuthWrongPassword()
        }
        guard let minted = result.freshGrant, minted.expiresAtMs > grantNowMs() else {
            NSLog("[auth] WARN the password sign-in carried no usable fresh-auth grant")
            throw FreshAuthRequired()
        }
        try installProofSession(result.session, for: account, context: "proof sign-in")
        freshAuth.put(minted, accountIdB64: account.accountIdB64)
    }

    /// Prove who the person is with a passkey: the passkey sign-in ceremony
    /// again, limited to this account's passkeys. Same installation rule as
    /// ``proveWithPassword(password:username:)``. A passkey of another
    /// account throws ``FreshAuthPasskeyOtherAccount`` and installs nothing;
    /// a dismissed sheet throws the assertion's `.cancelled`
    /// (``AuthErrorCopy/isPasskeyCancelled(_:)``).
    public func proveWithPasskey() async throws {
        guard let account = proofAccount() else { throw FreshAuthNoAccount() }
        let assertion = try await passkeyCeremony(base64UrlEncodeNoPad(account.accountIdBytes))
        guard base64UrlDecodeNoPad(assertion.accountIdB64) == account.accountIdBytes else {
            NSLog("[auth] WARN the passkey for a proof belongs to another account; refused")
            throw FreshAuthPasskeyOtherAccount()
        }
        guard let minted = assertion.freshGrant, minted.expiresAtMs > grantNowMs() else {
            NSLog("[auth] WARN the passkey sign-in carried no usable fresh-auth grant")
            throw FreshAuthRequired()
        }
        try installProofSession(assertion.session, for: account, context: "proof passkey")
        freshAuth.put(minted, accountIdB64: account.accountIdB64)
    }

    /// Install the pair a proof sign-in returned for the SAME account, in
    /// place of the current one. Not a landing: ``state`` is not touched.
    private func installProofSession(
        _ session: SessionPair, for account: ProofAccount, context: String
    ) throws {
        if let pending = pendingRecovery {
            // The restore prompt may have changed meanwhile.
            guard pending.accountIdB64 == account.accountIdB64 else { throw FreshAuthNoAccount() }
            adoptSessionPair(session, accountIdB64: account.accountIdB64, context: context)
            pendingRecovery = PendingRecovery(
                accountIdBytes: pending.accountIdBytes,
                accountIdB64: pending.accountIdB64,
                exportKey: pending.exportKey,
                session: session,
                username: pending.username
            )
            sessionLanded(accountIdB64: account.accountIdB64)
            return
        }
        guard let secrets = identitySecrets, secrets.accountId == account.accountIdBytes else {
            // Signed out (or the account changed) while the proof ran.
            throw FreshAuthNoAccount()
        }
        adoptIdentitySecrets(
            IdentitySecrets(
                accountId: secrets.accountId,
                exportKey: secrets.exportKey,
                accountIdentitySeed: secrets.accountIdentitySeed,
                session: session
            ),
            context: context,
            rebinds: false
        )
        // ADR-0074 — the proof sign-in issued a pair that names no device;
        // name this one again.
        sessionLanded(accountIdB64: account.accountIdB64)
    }

    // MARK: - Password change gate

    /// Whether a password change may run now (``PasswordChangeGate``): a
    /// live fresh-auth proof, or the local three-word check passed within
    /// the last ten minutes.
    public var passwordChangeAllowed: Bool {
        return PasswordChangeGate.allowed(
            holdsLiveProof: holdsLiveFreshAuth(),
            threeWordsPassedAtMs: threeWordsPassedAtMs,
            noticeAnsweredAtMs: noticeAnsweredAtMs,
            nowMs: grantNowMs())
    }

    /// UX 19.6 — the person answered `It was not me` to a notice that the
    /// password was changed or the account was opened by email. The password
    /// in force may be one they never chose, and an account without a saved
    /// phrase has no three words to check, so the new-password form of that
    /// one screen opens on what this device holds (the keys). Ten minutes,
    /// in memory.
    public func openPasswordChangeForNotice() {
        guard case .session = state else { return }
        noticeAnsweredAtMs = grantNowMs()
    }

    /// Whether the signed-in device can run the local three-word check: it
    /// holds the account's recovery phrase, and the account is
    /// phrase-protected (ADR-0075: an account whose phrase was never saved
    /// has no words the person could know).
    public var threeWordsCheckAvailable: Bool {
        guard case .session(let account) = state else { return false }
        return protectionStore.phraseSaved(accountIdB64: account.accountIdB64)
            && DevicePhrase.read(accountIdB64: account.accountIdB64) != nil
    }

    /// The local three-word check of the recovery phrase, for the signed-in
    /// account, in front of a password change. Compared with the phrase this
    /// device holds; nothing is sent anywhere. `true` opens
    /// ``passwordChangeAllowed`` for ten minutes. The guess budget is the
    /// form's (``ThreeWords``).
    public func checkThreeWordsForPasswordChange(positions: Data, words: [String]) throws -> Bool {
        guard case .session(let account) = state,
              let phrase = DevicePhrase.read(accountIdB64: account.accountIdB64) else {
            throw ThreeWordsError.unavailable
        }
        let mnemonic = try RecoveryMnemonic.fromPhrase(phrase: phrase)
        guard try phraseCheckWords(mnemonic: mnemonic, positions: positions, words: words) else {
            return false
        }
        threeWordsPassedAtMs = grantNowMs()
        return true
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
        landPendingFreshIdentity(pending)
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
                    // ADR-0074 section 2.2 — a KeyPackage publish needs a
                    // session naming this device; a reset run from a session
                    // that had not been bound yet binds it first.
                    await bindSessionDeviceIfNeeded(accountIdB64: accountIdB64, context: "key reset publish")
                    try await keyResetHooks.republishOwnKeyPackage(secrets)
                case .deviceGroup:
                    try await keyResetHooks.refoundDeviceGroup(
                        accountIdBytes, accountIdB64, accountIdentitySeed)
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
        // ADR-0075 — the new keys are published: a device that holds the
        // recovery address can put the email vault back now.
        onProtectionChanged?()
        // Not phrase-protected: there are no words to confirm, so the
        // session lands now.
        if let pending = pendingFreshPhrase, pending.accountIdB64 == accountIdB64, !pending.showsWords {
            landPendingFreshIdentity(pending)
        }
    }

    /// Land the session of a parked fresh identity (its words were
    /// confirmed, or it shows none).
    private func landPendingFreshIdentity(_ pending: PendingFreshPhrase) {
        // A guest account has no username and so no password to replace; a
        // fresh identity reached through "Forgot password?" keeps the offer
        // (its recovery credential now belongs to the new phrase, so the
        // reset works).
        if pending.username.isEmpty { newPasswordOwed = false }
        pendingFreshPhrase = nil
        freshShowsWords = true
        freshPublishPending = false
        freshPublishNeedsSignIn = false
        sessionStore.save(
            PersistedSession(
                accountIdB64: pending.accountIdB64,
                username: pending.username
            )
        )
        let landed = pending.currentSession(tokenProvider)
        finishCanonicalSession(
            accountIdentitySeed: pending.seed,
            accountIdBytes: pending.accountIdBytes,
            exportKey: Data(),
            session: landed,
            username: pending.username
        )
        // A key reset (and a recall, which is one) ends with the new keys on
        // this device: the backup on the server is sealed under the phrase
        // that was replaced, so a baseline under the new one goes up (no MLS
        // state).
        if let phrase = DevicePhrase.read(accountIdB64: pending.accountIdB64, store: phraseStore) {
            uploadBaselineBackupAfterRestore(
                accountId: pending.accountIdBytes,
                accountIdB64: pending.accountIdB64,
                accountIdentitySeed: pending.seed,
                sessionToken: landed.sessionToken,
                phrase: phrase,
                restoreOutcome: nil,
                context: "key reset",
                // Measured from this landing: the phrase was just read back
                // from this device, and a wipe from here on refuses it.
                writes: SignInKeyWrites()
            )
        }
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
    /// `keyResetAttest` is the ADR-0073 attestation on the upload that
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
    ///
    /// `writes` is the calling route's ``SignInKeyWrites``: no key is written
    /// for an account wiped since the route began.
    private func bootstrapDeviceKeyFromVerifiedSeedIfAbsent(
        accountIdB64: String,
        accountIdentitySeed: Data,
        context: String,
        writes: SignInKeyWrites
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
            try writes.ifNotWiped(accountIdB64) {
                try DeviceKeyStore.putDeviceKey(
                    accountIdB64: accountIdB64,
                    rootKey: deviceRootKey
                )
            }
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

    // MARK: - Backup restore + bootstrap helpers

    /// The backup step of EVERY route that restores the identity from the
    /// recovery phrase: the restore screen (``recover(mnemonicPhrase:newPassword:username:)``)
    /// and the phrase prompt after an ordinary sign-in on a device with no
    /// cached seed (``provideRecoveryPhrase(mnemonicPhrase:)``). iOS has no
    /// three-word vault route; the another-device route is not one of these
    /// (its state arrives in the pairing bundle).
    ///
    /// Before anything is fetched, and so before the session lands, the
    /// phrase is cached on this device and the restore is recorded as OWED
    /// (``BackupRestoreRetryStore``). It stays owed until it reaches a
    /// definitive answer, so an app killed mid-restore, or a fetch that
    /// failed, is tried again at the next session landing.
    ///
    /// Fetch, gate, open, apply:
    ///
    ///   * ADR-0037 section 6 (G-11): the served blob runs behind the
    ///     rollback gate. A tampered / rolled-back head refuses outright and
    ///     raises ``backupRollbackAlarm``; a stale head asks the user through
    ///     ``pendingStaleBackupOverride`` (fail-closed on decline). Both are
    ///     presented at the app root, so every route shows them.
    ///   * A restored payload is merged through ``PhraseRestoreBackup/apply(_:accountIdB64:context:defaults:)``:
    ///     contacts, the ADR-0058 synced state, and the list of the account's
    ///     conversations. Never MLS state: the backup carries none, and this
    ///     device's MLS state stays whatever it already held locally.
    ///   * No backup (404) is not an error.
    ///   * A fetch failure does not fail the sign-in. It is logged and the
    ///     restore stays owed.
    ///
    /// `phrase` is the CANONICAL phrase (`RecoveryMnemonic.phrase()`): the
    /// envelope is sealed under those bytes. `writes` is the calling route's
    /// ``SignInKeyWrites``: the phrase is not cached for an account wiped
    /// since the route began.
    @discardableResult
    func restoreBackupFromPhrase(
        accountId: Data,
        accountIdB64: String,
        sessionToken: String,
        phrase: String,
        context: String,
        writes: SignInKeyWrites
    ) async -> PhraseRestoreBackupOutcome {
        // The phrase first: it is what a later attempt opens the blob with,
        // and what this device seals its own backups under.
        cacheBackupPhraseBestEffort(
            accountIdB64: accountIdB64, phrase: phrase, context: context, writes: writes)
        BackupRestoreRetryStore.mark(accountIdB64: accountIdB64, defaults: backupDefaults)
        var phraseBytes = Data(phrase.utf8)
        defer { phraseBytes.resetBytes(in: 0..<phraseBytes.count) }
        return await restoreBackup(
            accountId: accountId,
            accountIdB64: accountIdB64,
            sessionToken: sessionToken,
            phraseBytes: phraseBytes,
            context: context
        )
    }

    /// One attempt. The restore is owed on entry (the caller marked it) and
    /// stops being owed here on every DEFINITIVE answer: applied, no backup,
    /// refused by the head gate, a stale head the person declined, a blob
    /// that does not open under the phrase or does not parse. A transport
    /// failure is not an answer: the mark stays.
    private func restoreBackup(
        accountId: Data,
        accountIdB64: String,
        sessionToken: String,
        phraseBytes: Data,
        context: String
    ) async -> PhraseRestoreBackupOutcome {
        // ADR-0037 section 6: arm the rollback gate — verify the auth-signed
        // backup head against the pinned attest key before trusting the
        // served envelope. A stale head suspends on the confirmation dialog;
        // declining aborts the restore (fail-closed — never a silent
        // downgrade).
        let gate = RestoreGateContext(
            accountId: accountId,
            accountIdB64: accountIdB64,
            confirmStaleOverride: { [weak self] reason in
                guard let self else { return false }
                return await self.awaitStaleBackupOverride(reason: reason)
            }
        )
        let bound = binding.capture()
        // A restore runs for an account whose ACCESS was just granted, on
        // some routes before its session lands; what it restores is written
        // to that account's stores.
        AccountStoreGate.open(accountIdB64)
        let outcome = await backupRestoreSource.restore(
            sessionToken: sessionToken, phraseBytes: phraseBytes, gate: gate)
        // Signed out, or another account landed, while the fetch was out:
        // the answer is not this session's. Nothing is applied, no alarm is
        // raised on whoever is signed in now, and the restore stays owed for
        // this account's next landing.
        guard bound.isCurrent else {
            NSLog("backup restore (\(context)): the session ended meanwhile, still owed")
            return .fetchFailed
        }

        let result: PhraseRestoreBackupOutcome
        switch outcome {
        case .rollbackBlocked(let hard, let reason):
            if hard {
                // Refuse + alarm. The served state is NOT loaded (invariant #4).
                NSLog(
                    "backup restore (\(context)) BLOCKED a rolled-back / tampered "
                    + "backup: \(reason)"
                )
                backupRollbackAlarm = BackupRollbackAlarm(reason: reason)
            } else {
                NSLog("backup restore (\(context)): user declined a stale backup: \(reason)")
            }
            result = .refused
        case .noBackup:
            NSLog("backup restore (\(context)): no prior backup")
            result = .noBackup
        case .wrongPhraseOrTampered:
            NSLog("backup restore (\(context)) failed: wrong phrase or tampered blob")
            result = .refused
        case .transportFailed(let error):
            NSLog("backup restore (\(context)) fetch failed, still owed: \(error)")
            return .fetchFailed
        case .restored(let parsed):
            result = .applied(PhraseRestoreBackup.apply(
                parsed, accountIdB64: accountIdB64, context: context, defaults: backupDefaults))
        }
        // A definitive answer: nothing is owed any more.
        BackupRestoreRetryStore.clear(accountIdB64: accountIdB64, defaults: backupDefaults)
        return result
    }

    /// At a session landing: if a phrase restore is still owed
    /// (``BackupRestoreRetryStore``), make one attempt in the background.
    /// Never delays or fails the landing.
    private func retryBackupRestoreIfMarked(
        accountId: Data,
        sessionToken: String,
        context: String
    ) {
        let accountIdB64 = base64Encode(accountId)
        guard BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: backupDefaults) else {
            return
        }
        Task { [weak self] in
            await self?.runMarkedBackupRestoreRetry(
                accountId: accountId,
                accountIdB64: accountIdB64,
                sessionToken: sessionToken,
                context: context
            )
        }
    }

    /// One attempt at an owed restore; `nil` when none is owed or one is
    /// already running for the account. The mark is NOT removed before the
    /// attempt: it goes only on a definitive answer
    /// (``restoreBackup(accountId:accountIdB64:sessionToken:phraseBytes:context:)``),
    /// or when this device holds no cached phrase, since nothing here could
    /// then ever open the blob. A fetch that fails again leaves the restore
    /// owed for the next landing.
    @discardableResult
    func runMarkedBackupRestoreRetry(
        accountId: Data,
        accountIdB64: String,
        sessionToken: String,
        context: String
    ) async -> PhraseRestoreBackupOutcome? {
        guard BackupRestoreRetryStore.isPending(accountIdB64: accountIdB64, defaults: backupDefaults),
              !backupRestoreRetryInFlight.contains(accountIdB64)
        else { return nil }
        backupRestoreRetryInFlight.insert(accountIdB64)
        defer { backupRestoreRetryInFlight.remove(accountIdB64) }

        let cached: Data?
        do {
            cached = try phraseStore.getPhrase(accountIdB64: accountIdB64)
        } catch {
            // A Keychain fault is not an answer about the backup. The same
            // fault stops this device sealing a backup of its own, so
            // leaving the restore owed holds back nothing that could go out.
            NSLog("backup restore retry (\(context)): phrase cache read failed: \(error)")
            return nil
        }
        guard var phraseBytes = cached else {
            NSLog("backup restore retry (\(context)): no cached phrase on this device, nothing owed")
            BackupRestoreRetryStore.clear(accountIdB64: accountIdB64, defaults: backupDefaults)
            return nil
        }
        defer { phraseBytes.resetBytes(in: 0..<phraseBytes.count) }
        return await restoreBackup(
            accountId: accountId,
            accountIdB64: accountIdB64,
            sessionToken: sessionToken,
            phraseBytes: phraseBytes,
            context: "retry after \(context)"
        )
    }

    /// Cache the recovery phrase in the Keychain. Best-effort: without it
    /// this device cannot seal a backup of its own, and nothing else.
    private func cacheBackupPhraseBestEffort(
        accountIdB64: String,
        phrase: String,
        context: String,
        writes: SignInKeyWrites
    ) {
        // The one writer of the account's phrase row (canonical form; a
        // failure is logged by context, never with the words). Not for an
        // account wiped since the route began.
        DevicePhrase.store(
            phrase, accountIdB64: accountIdB64, context: context,
            since: writes.mark(accountIdB64), store: phraseStore)
    }

    /// Cache the recovery phrase under the Keychain + post a
    /// baseline backup envelope to /backup. Called from
    /// ``signup`` and ``recover`` right after the state flip.
    /// Best-effort end-to-end; every step logs + continues on
    /// failure so a transient transport hiccup does not block
    /// the user from landing in a working session.
    ///
    /// The baseline carries the accepted contacts, the synced state and the
    /// conversation list (this device's own groups plus the ones a restored
    /// backup named, ``RestoredKnownGroupsStore``). It carries no MLS state.
    /// `uploadBaseline == false` caches the phrase and posts nothing.
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
    ///
    /// `writes` is the calling route's ``SignInKeyWrites``: this runs after
    /// the landing, often in a task of its own, and an account wiped since
    /// the route began gets neither its phrase back nor a baseline.
    private func bootstrapBackupBestEffort(
        accountId: Data,
        accountIdB64: String,
        accountIdentitySeed: Data,
        sessionToken: String,
        phrase: String,
        context: String,
        writes: SignInKeyWrites,
        uploadBaseline: Bool = true
    ) async {
        guard !writes.isWiped(accountIdB64) else {
            NSLog("backup bootstrap (\(context)) skipped: the account was removed from this device")
            return
        }
        // Cache the phrase first — even if the network leg fails, the
        // automatic backup re-tries from the cached value.
        cacheBackupPhraseBestEffort(
            accountIdB64: accountIdB64, phrase: phrase, context: context, writes: writes)
        guard uploadBaseline else {
            NSLog("backup baseline upload (\(context)) skipped: the backup restore is still owed")
            return
        }

        let phraseBytes = Data(phrase.utf8)
        let backupClient = Self.makeBackupClient()
        backupClient.setSessionToken(sessionToken)

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
                // The chat layer has not bootstrapped yet, so the groups this
                // device holds are read from their persisted index; with them
                // go the conversations the restored backup named, so this
                // baseline never erases the list.
                knownGroupIdsHex: RestoredKnownGroupsStore.uploadGroupIds(
                    accountIdB64: accountIdB64,
                    own: Array(KnownGroupsStore().load(accountIdB64: accountIdB64)),
                    defaults: backupDefaults
                ),
                // ADR-0058 — whatever synced state this device already holds
                // (after a recover, the one just merged), so the baseline
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
    /// The session pair (ADR-0073 section 3) of the most recent
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

/// Result of a sign-up.
public struct SignupOutcome: Equatable {
    /// The 12-word BIP39 recovery phrase the device generated and keeps
    /// (``DevicePhrase``). ADR-0075 D2: it is NOT shown at sign-up; the
    /// person may save a phrase later (`Save a recovery phrase`, which
    /// gives the account new keys and new words).
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
    /// `true` while the new keys are not published yet (ADR-0073 section
    /// 2.3: the directory upload, this device's KeyPackage or the
    /// DeviceGroup journal is still owed). The phrase is not shown and the
    /// session does not land until it is `false`. Always `false` for a
    /// guest signup, which publishes at creation.
    var publishOwed: Bool = false
    /// ADR-0075 — `false` for an account that is not phrase-protected: its
    /// new words are shown on no screen, and the session lands as soon as
    /// the publish did.
    var showsWords: Bool = true

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

/// Where ``AuthRepository/mintFreshIdentity()`` goes once the fresh identity
/// is published, which cannot be taken back.
enum FreshIdentityLanding: Equatable {
    /// Show the new phrase now. `rebinds` when the binding the mint started
    /// under has ended (a sign-out, or the same account parked again): the
    /// phrase screen then binds the device to the account anew.
    case showPhrase(rebinds: Bool)
    /// Another account is signed in: its session is left exactly as it is,
    /// and the phrase is kept for the minting account's next sign-in.
    case keepForNextSignIn

    /// `signedIn` is ``AuthRepository/signedInAccountIdB64`` after the
    /// publish returned.
    static func decide(
        mintedFor: String, bindingIsCurrent: Bool, signedIn: String?
    ) -> FreshIdentityLanding {
        if bindingIsCurrent { return .showPhrase(rebinds: false) }
        if let signedIn, signedIn != mintedFor { return .keepForNextSignIn }
        return .showPhrase(rebinds: true)
    }
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

// MARK: - ADR-0074 part 3: a rotation of the account keys, the devices kept

extension AuthRepository {

    /// The account key this device holds for the signed-in account.
    private func heldAccountPublicKey(accountIdB64: String) -> Data? {
        guard let secrets = identitySecrets, base64Encode(secrets.accountId) == accountIdB64 else {
            return nil
        }
        return try? Identity.fromSeed(seed: secrets.accountIdentitySeed).signaturePublicKey()
    }

    /// The account's own old seed(s) kept after a rotation (the predecessor
    /// slot), newest last.
    public func ownPredecessorSeeds(accountIdB64: String) -> [Data] {
        ownPredecessors.load(accountIdB64: accountIdB64)
    }

    private func restoreKeysRotatedNotice() {
        guard let accountIdB64 = signedInAccountIdB64 else { return }
        keysRotatedNotice = rotatedNotice.isOwed(accountIdB64: accountIdB64)
    }

    /// The person dismissed "Your account keys were changed on another
    /// device".
    public func dismissKeysRotatedNotice() {
        if let accountIdB64 = signedInAccountIdB64 {
            rotatedNotice.clear(accountIdB64: accountIdB64)
        }
        keysRotatedNotice = false
    }

    // MARK: A kept device: the envelope

    /// A dispatched `key-rotation` envelope (ADR-0074 section 3.3). It is
    /// meaningful ONLY in the account's own DeviceGroup; from anywhere else
    /// it is dropped. Here it is verified against the account key this
    /// device holds and stored as PENDING, keyed on the new key. Nothing
    /// else changes: the device adopts only when the server side shows that
    /// key as the account's.
    ///
    /// The plaintext carries the account's next seed: it is never logged.
    public func receiveKeyRotationEnvelope(_ plaintext: Data, arrival: KeptDeviceRule.Arrival) {
        guard let accountIdB64 = signedInAccountIdB64,
              let heldKey = heldAccountPublicKey(accountIdB64: accountIdB64) else {
            NSLog("[auth] a key-rotation envelope arrived with no account keys on this device; dropped")
            return
        }
        let nowMs = UInt64(max(0, grantNowMs()))
        let verified: PendingKeyRotation? = arrival == .ownDeviceGroup
            ? (try? verifyKeyRotationEnvelope(
                plaintext: plaintext, heldAccountPubkey: heldKey, nowUnixSecs: Int64(nowMs / 1000)))
                .map { PendingKeyRotation($0) }
            : nil
        let held = KeptDeviceRule.pruned(
            rotationPending.load(accountIdB64: accountIdB64), nowMs: nowMs,
            expired: keyRotationPendingExpired(preparedAtMs:nowMs:))
        switch KeptDeviceRule.receive(
            arrival: arrival, verified: verified, held: held, nowMs: nowMs,
            expired: keyRotationPendingExpired(preparedAtMs:nowMs:)) {
        case .store(let pending):
            do {
                try rotationPending.save(held + [pending], accountIdB64: accountIdB64)
                NSLog("[auth] a key rotation of this account is pending on this device")
            } catch {
                NSLog("[auth] WARN a pending key rotation could not be stored")
            }
        case .alreadyHeld:
            break
        case .drop(let reason):
            NSLog("[auth] a key-rotation envelope was dropped (%@)", String(describing: reason))
        }
    }

    /// Drop the pending envelopes that are too old to keep (14 days). Called
    /// at every session landing.
    func pruneKeyRotationPending(accountIdB64: String) {
        let stored = rotationPending.load(accountIdB64: accountIdB64)
        guard !stored.isEmpty else { return }
        let kept = KeptDeviceRule.pruned(
            stored, nowMs: UInt64(max(0, grantNowMs())),
            expired: keyRotationPendingExpired(preparedAtMs:nowMs:))
        if kept.count != stored.count {
            try? rotationPending.save(kept, accountIdB64: accountIdB64)
        }
    }

    // MARK: A kept device: superseded

    /// This device learned its keys or its session are of an old key
    /// generation (a refresh answered `key-generation-superseded`, or the
    /// directory publishes other keys at a higher generation).
    ///
    /// A device that holds the account's keys first finds out whether this
    /// was a ROTATION: it tries the pending envelopes it holds; with none it
    /// reads its DeviceGroup journal to the head once; and only when nothing
    /// can be adopted does it treat the change as a reset (the staged wipe
    /// and "Was it you?", ``handleKeysSuperseded(accountIdB64:)``). While it
    /// is finding out, the app stays as it was with "Updating keys...".
    ///
    /// A device without keys has nothing to adopt with and signs out at
    /// once, as before.
    func keysSuperseded(accountIdB64: String, publishedSignaturePubkey: Data?) {
        guard keysSignOut == nil, recall == nil else { return }
        guard heldAccountPublicKey(accountIdB64: accountIdB64) != nil, case .session = state else {
            handleKeysSuperseded(accountIdB64: accountIdB64)
            return
        }
        guard !rotationAdoptInFlight else { return }
        rotationAdoptInFlight = true
        keysUpdating = true
        rotationAdoptRetry?.cancel()
        rotationAdoptRetry = nil
        Task { [weak self] in
            guard let self else { return }
            let outcome = await self.runKeyRotationAdoption(
                accountIdB64: accountIdB64, publishedSignaturePubkey: publishedSignaturePubkey)
            self.rotationAdoptInFlight = false
            switch outcome {
            case .adopted:
                self.keysUpdating = false
            case .treatAsReset:
                self.keysUpdating = false
                self.handleKeysSuperseded(accountIdB64: accountIdB64)
            case .wipeRevoked:
                // This device was the one removed: the full wipe.
                self.keysUpdating = false
                self.handleDeviceRevoked(accountIdB64: accountIdB64)
            case .signInAgain:
                self.keysUpdating = false
                self.handleRefreshInvalid(accountIdB64: accountIdB64)
            case .retryLater:
                // No verdict: nothing is wiped, the line stays, and the
                // upgrade is tried again.
                self.scheduleKeyRotationAdoptionRetry(accountIdB64: accountIdB64)
            }
        }
    }

    /// How long an adoption that got no verdict waits before it is tried
    /// again. Chosen, not measured.
    static let keyRotationAdoptRetryNanos: UInt64 = 30_000_000_000

    private func scheduleKeyRotationAdoptionRetry(accountIdB64: String) {
        rotationAdoptRetry?.cancel()
        rotationAdoptRetry = Task { [weak self] in
            try? await Task.sleep(nanoseconds: Self.keyRotationAdoptRetryNanos)
            guard !Task.isCancelled, let self, self.keysUpdating,
                  self.signedInAccountIdB64 == accountIdB64 else { return }
            self.keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
        }
    }

    /// Retry a pending adoption now (the app came to the foreground, the
    /// network returned).
    public func retryKeysUpdating() {
        guard keysUpdating, !rotationAdoptInFlight, let accountIdB64 = signedInAccountIdB64 else { return }
        keysSuperseded(accountIdB64: accountIdB64, publishedSignaturePubkey: nil)
    }

    /// One run of ``KeptDeviceRule``: prune, read the journal once when
    /// nothing is pending, then try the session upgrade with each pending
    /// envelope until the server accepts one.
    func runKeyRotationAdoption(
        accountIdB64: String, publishedSignaturePubkey: Data?
    ) async -> KeptDeviceRule.Outcome {
        guard let secrets = identitySecrets, base64Encode(secrets.accountId) == accountIdB64 else {
            return .treatAsReset
        }
        let refreshToken = tokenProvider.current?.refreshToken ?? secrets.session.refreshToken
        let deviceIdB64 = tokenProvider.current?.deviceIdB64 ?? secrets.session.deviceIdB64
        var journalRead = false
        while true {
            pruneKeyRotationPending(accountIdB64: accountIdB64)
            let held = rotationPending.load(accountIdB64: accountIdB64)
            switch KeptDeviceRule.onSuperseded(
                held: held, publishedSignaturePubkey: publishedSignaturePubkey, journalRead: journalRead) {
            case .readJournalFirst:
                NSLog("[auth] no pending key rotation on this device; reading the device journal once")
                _ = await readDeviceGroupJournalToHead(accountIdB64)
                journalRead = true
                continue
            case .treatAsReset:
                return .treatAsReset
            case .tryAdopting(let candidates):
                var answers: [SessionUpgradeAnswer] = []
                for candidate in candidates {
                    guard let mnemonic = try? RecoveryMnemonic.fromEntropy(entropy: candidate.newEntropy),
                          let account = try? Identity.fromSeed(seed: mnemonic.toSeed()),
                          account.signaturePublicKey() == candidate.newSignaturePubkey else {
                        answers.append(.notAccepted)
                        continue
                    }
                    var failure: Error?
                    var upgraded: SessionPair?
                    do {
                        upgraded = try await sessionUpgradeOperation(
                            refreshToken, secrets.accountId, account.ed25519Seed())
                    } catch {
                        failure = error
                    }
                    let answer = SessionUpgradeAnswer.of(failure)
                    answers.append(answer)
                    if let upgraded, KeptDeviceRule.afterUpgrade(answer) == .adopt {
                        do {
                            // The upgraded pair names the same device.
                            try await installRotatedKeys(
                                accountIdB64: accountIdB64,
                                phrase: mnemonic.phrase(),
                                seed: mnemonic.toSeed(),
                                session: upgraded.naming(deviceIdB64: deviceIdB64),
                                context: "key rotation adopted")
                        } catch {
                            NSLog("[auth] ERR the adopted keys could not be stored on this device")
                            return .retryLater
                        }
                        rotationPending.clear(accountIdB64: accountIdB64)
                        rotatedNotice.markOwed(accountIdB64: accountIdB64)
                        keysRotatedNotice = true
                        NSLog("[auth] this device adopted the account's new keys; every local store was kept")
                        return .adopted
                    }
                    if KeptDeviceRule.afterUpgrade(answer) != .tryNext { break }
                }
                return KeptDeviceRule.outcome(of: answers)
            }
        }
    }

    // MARK: Both sides: the new keys replace the old ones, everything is kept

    /// The new seed, phrase and session pair replace the old ones on this
    /// device (ADR-0074 section 3.2 step 6, section 3.3 "adopting"). ALL
    /// local data is kept: chats, contacts, history, the device identity and
    /// its binding. The OLD seed goes to the predecessor slot FIRST: without
    /// it this device's stored binding would be re-minted under the new key
    /// at the next launch, and its conversation leaves would stop being its
    /// own (fact 14). A failure there throws and nothing is changed.
    func installRotatedKeys(
        accountIdB64: String,
        phrase: String,
        seed: Data,
        session: SessionPair,
        context: String
    ) async throws {
        guard let old = identitySecrets, base64Encode(old.accountId) == accountIdB64 else {
            throw SessionNotReady()
        }
        let predecessorSeed = old.accountIdentitySeed
        guard predecessorSeed != seed else {
            // Already adopted (a retry): only the pair moves.
            adoptSessionPair(session, accountIdB64: accountIdB64, context: context)
            return
        }
        try ownPredecessors.add(predecessorSeed, accountIdB64: accountIdB64)
        // ADR-0074 section 3.5a rule 4 — the old mailbox is read for 30 days
        // from NOW (the adoption), then left. Recorded once per key.
        if let oldKey = try? Identity.fromSeed(seed: predecessorSeed).signaturePublicKey() {
            PredecessorMailbox.AdoptionStore().recordIfAbsent(
                accountIdB64: accountIdB64, keyB64: base64Encode(oldKey),
                nowMs: Int64(Date().timeIntervalSince1970 * 1000))
        }
        // The session is this account's (checked above, after the wait):
        // refused only for an account wiped and not landed again.
        do {
            let hold = try AccountStoreGate.enterUnlessWipedOrEnd(accountIdB64)
            defer { hold.leave() }
            try AccountIdentitySeedStore.putAccountIdentitySeed(accountIdB64: accountIdB64, seed: seed)
            // The device holds the new keys, so it holds the new phrase.
            DevicePhrase.store(phrase, accountIdB64: accountIdB64, context: context, since: nil)
        }
        let secrets = IdentitySecrets(
            accountId: old.accountId,
            exportKey: old.exportKey,
            accountIdentitySeed: seed,
            session: session
        )
        adoptIdentitySecrets(secrets, context: context, rebinds: false)
        // ADR-0074 section 0.1 — auth recorded the new key inside the
        // rotation; the mint for it is idempotent and moves this device's
        // mark.
        Task { [weak self] in
            await self?.recordIdentityKeyIfNeeded(
                accountIdBytes: secrets.accountId, accountIdentitySeed: seed)
        }
        await onAccountKeysRotated?(secrets, predecessorSeed)
    }

    /// The rotating device's step 6: adopt what `rotate/finish` answered.
    /// The pair names the device the caller's bearer names.
    public func adoptOwnKeyRotation(material: KeyRotationMaterial, session: SessionPair) async throws {
        guard let accountIdB64 = signedInAccountIdB64 else { throw SessionNotReady() }
        let deviceIdB64 = tokenProvider.current?.deviceIdB64 ?? identitySecrets?.session.deviceIdB64
        try await installRotatedKeys(
            accountIdB64: accountIdB64,
            phrase: material.phrase,
            seed: material.seed,
            session: session.naming(deviceIdB64: deviceIdB64),
            context: "key rotation")
        // This device's own copy of the envelope has done its work.
        rotationPending.clear(accountIdB64: accountIdB64)
    }

    /// Keep the rotating device's own prepared rotation as a pending
    /// envelope until the commit answered: a commit whose answer is lost is
    /// then adopted like a kept device's, instead of being read as a reset.
    public func storeOwnPendingKeyRotation(_ pending: PendingKeyRotation, accountIdB64: String) throws {
        let held = rotationPending.load(accountIdB64: accountIdB64)
            .filter { $0.newSignaturePubkey != pending.newSignaturePubkey }
        try rotationPending.save(held + [pending], accountIdB64: accountIdB64)
    }

    /// The proven-rotation upload of the rotating device (ADR-0074 section
    /// 3.5, "Discovery"): the new bundle with the key-reset attestation (when
    /// there is one) and, when `withProof`, the supersession proof, on the
    /// new session. No genesis attestation is minted here: auth moved its
    /// record of the account key inside the rotation.
    public func publishRotatedIdentity(
        supersessionProof: Data, attestation: Data?, withProof: Bool
    ) async throws {
        guard let secrets = identitySecrets, let token = tokenProvider.current?.sessionToken else {
            throw SessionNotReady()
        }
        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        try await accountDirectory.publishRotatedIdentityBundle(
            accountId: secrets.accountId,
            signaturePubkey: identity.signaturePublicKey(),
            x25519Pubkey: identity.x25519PublicKey(),
            supersessionProof: withProof ? supersessionProof : nil,
            keyResetAttest: attestation,
            sessionToken: token
        )
    }

    /// The backup, re-sealed under the phrase this device holds now and
    /// uploaded, awaited (ADR-0074 section 3.5, "Backup").
    ///
    /// Nothing here is best-effort: it returns only when the storage tier
    /// accepted the new envelope, and throws on anything else (no session,
    /// no phrase, a phrase that is not the rotated one, the phrase cache,
    /// the seal, the upload), so the rotation keeps the step owed and
    /// retries it. The one soft step is the ADR-0037 head mint inside
    /// ``BackupClient/uploadNow``: a failed mint still writes the legacy
    /// unframed envelope, and that counts as uploaded.
    ///
    /// The envelope carries no MLS state (the account backup never does).
    /// `knownGroupIdsHex` comes from a chat layer the caller has already
    /// found ready (``KeyRotationBackup``).
    public func resealBackupAfterKeyRotation(
        knownGroupIdsHex: [String],
        syncedState: Data?
    ) async throws {
        guard let secrets = identitySecrets, let token = tokenProvider.current?.sessionToken else {
            throw SessionNotReady()
        }
        let accountIdB64 = base64Encode(secrets.accountId)
        guard let phrase = DevicePhrase.read(accountIdB64: accountIdB64) else {
            throw AuthClientError.argument("the new phrase is not readable on this device")
        }
        // An envelope sealed under the OLD words would be exactly the backup
        // this step exists to replace.
        let phraseSeed = try RecoveryMnemonic.fromPhrase(phrase: phrase).toSeed()
        guard phraseSeed == secrets.accountIdentitySeed else {
            throw AuthClientError.argument("the phrase on this device is not the rotated one")
        }
        var phraseBytes = Data(phrase.utf8)
        defer { phraseBytes.resetBytes(in: 0..<phraseBytes.count) }

        let backupClient = Self.makeBackupClient()
        backupClient.setSessionToken(token)
        // The scheduled backup seals with the cached phrase: left on the old
        // words it would overwrite this envelope with one the new words
        // cannot open, so a failed cache write keeps the step owed too.
        try backupClient.cachePhrase(accountIdB64: accountIdB64, phraseBytes: phraseBytes)

        let identity = try Identity.fromSeed(seed: secrets.accountIdentitySeed)
        let contacts = AcceptedContactsStore.load(accountIdB64: accountIdB64).map { c in
            BackupContactRecord(
                ed25519Pubkey: c.request.fromPubkey,
                x25519Pubkey: Data(count: 32),
                displayName: c.displayName ?? "",
                accountId: c.request.fromAccountId
            )
        }
        try await backupClient.uploadNow(
            accountId: secrets.accountId,
            accountIdB64: accountIdB64,
            ed25519Seed: identity.ed25519Seed(),
            phraseBytes: phraseBytes,
            contacts: contacts,
            knownGroupIdsHex: knownGroupIdsHex,
            syncedState: syncedState
        )
        // The scheduled backup has nothing older to send.
        AutoBackupStore.save(
            accountIdB64: accountIdB64,
            state: AutoBackup.markedUploaded(
                AutoBackupStore.load(accountIdB64: accountIdB64),
                nowMs: Int64(Date().timeIntervalSince1970 * 1000))
        )
    }
}
