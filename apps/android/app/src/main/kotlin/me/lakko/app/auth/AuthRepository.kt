// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import me.lakko.app.backup.DevicePhrase
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.push.PushSubscriber
import me.lakko.app.webauthn.PasskeyKeysChoiceResult
import me.lakko.app.webauthn.PasskeyKeysOutcome
import me.lakko.app.webauthn.PasskeyLoginResult
import me.lakko.app.webauthn.PasskeyPhraseNotThisAccount
import me.lakko.app.webauthn.PasskeyVaultUnusable
import me.lakko.app.webauthn.WebauthnClient
import me.lakko.app.webauthn.WebauthnHttpError
import me.lakko.app.webauthn.WebauthnUserCancelledError
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.RecoveryMnemonic
import uniffi.lakko_ffi_uniffi.passkeyVaultOpen
import uniffi.lakko_ffi_uniffi.passkeyVaultSeal
import uniffi.lakko_ffi_uniffi.phraseCheckWords
import uniffi.lakko_ffi_uniffi.signLeafGenesis
import uniffi.lakko_ffi_uniffi.transparencyAccountKey
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicReference

/**
 * Single source of truth for "is the user authenticated on this
 * device?". Mirror of `apps/web/src/auth/AuthContext.tsx`.
 *
 * Owns:
 *
 *   * an [AuthClient] (Ktor HTTP),
 *   * a [SessionStore] (SharedPreferences-backed `(accountId,
 *     username)` marker),
 *   * implicit access to [DeviceKeyStore] (Keystore-wrapped
 *     device root key on disk).
 *
 * Exposes a [StateFlow] of [AuthState] for the UI to
 * `collectAsStateWithLifecycle` against. Three states:
 *
 *   * [AuthState.Loading] — initial value before the constructor
 *     finishes rehydrating from disk. Practically invisible to
 *     the user because rehydration is synchronous SharedPreferences
 *     + a file `exists()` check; we keep the state for symmetry
 *     with `apps/web` where the IndexedDB lookup is genuinely
 *     async.
 *   * [AuthState.Anon] — no on-device session. Optionally carries
 *     the cached username so the Login form can pre-fill it
 *     ("welcome back, @alice — re-enter password").
 *   * [AuthState.Session] — fully restored or freshly authenticated.
 *
 * ## Recovery model
 *
 * Keys come back on a new device from another device, from a passkey that
 * carries them, or from the whole 12-word phrase; a device that still holds
 * the keys can sign in again with a passkey, three words of the phrase
 * (checked locally) or an authenticator code, and is then offered a new
 * password. Every device that holds the keys also holds the phrase
 * ([DevicePhrase]); every landing path that gives the device keys writes it.
 *
 * ## Discovery
 *
 * Like the web `AuthContext`, signup claims the `@username` (the login handle at
 * auth first, then discovery: [claimUsernameAtSignup]), login and recovery name
 * the account by its login handle with no lookup before a session
 * ([signInByName], ADR-0054 S5), and every landing that
 * holds the phrase seed publishes the identity bundle
 * ([identityPublicationFor]). KeyPackage upload and MLS state live in
 * `ChatRepository`, not here.
 */
public class AuthRepository(
    private val applicationContext: Context,
    private val sessionStore: SessionStore = SessionStore(applicationContext),
    private val deviceKeyStore: DeviceKeyStore = DeviceKeyStore,
    /** Lazy because constructing a Ktor [AuthClient] eagerly costs
     *  ~50 ms (OkHttp engine + thread pool). The login / signup
     *  screens only need it on submit, not on first paint. */
    private val authClientFactory: () -> AuthClient = { AuthClient() },
    /** G-4 (ADR-0016) — factory for the [DiscoveryClient] used to
     *  re-publish the identity bundle (+ supersession proof) on
     *  recover. Lazy for the same reason as [authClientFactory].
     *  Overridable so tests can supply a stub. */
    private val discoveryClientFactory: () -> DiscoveryClient = { DiscoveryClient() },
    /** ADR-0054 S5 - username to login handle (the native core). */
    private val loginHandleSource: LoginHandleSource = UniffiLoginHandleSource,
    /** ADR-0074 - this device's id for an account (its conversation leaf
     *  signature key); the native core derives it from the device seed. */
    private val deviceIds: DeviceIdSource = OnDeviceIdSource(applicationContext),
) {
    /** 1.20d — dedicated scope for fire-and-forget push
     *  subscribe / unsubscribe coroutines. `SupervisorJob` so
     *  one failed leg doesn't tear down the others; bound to
     *  `Dispatchers.IO` because both legs are network-heavy. The
     *  scope's lifetime is the process — `AuthRepository` is a
     *  process-singleton in `LakkoApp.kt`. */
    private val pushScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** `@username` register (signup), over short-lived discovery clients.
     *  Nothing here looks a name up: sign-in and recovery go by login handle. */
    private val accountDirectory: AccountDirectory =
        DiscoveryAccountDirectory(discoveryClientFactory)

    /** ADR-0054 S5 - `PUT` / `DELETE /auth/login-handle`. */
    private val loginHandleBinder: LoginHandleBinder =
        AuthClientLoginHandleBinder(authClientFactory)

    /** ADR-0054 S5 - re-bind the login handle once per (session, username)
     *  per process, whichever path landed the session. */
    private val loginHandleRebinder: LoginHandleSessionRebinder =
        LoginHandleSessionRebinder(
            source = loginHandleSource,
            binder = loginHandleBinder,
            launch = { block -> pushScope.launch { block() } },
            log = { message -> Log.w("lakko-auth", message) },
            // ADR-0076 - a confirmed handle may let this device seed the
            // own-username register: the rule is evaluated again.
            onConfirmed = {
                pushScope.launch {
                    try {
                        onOwnUsernameState()
                    } catch (cause: Throwable) {
                        if (cause is kotlinx.coroutines.CancellationException) throw cause
                        Log.w("lakko-auth", "own username not applied: ${cause.javaClass.simpleName}")
                    }
                }
            },
        )

    private val mutableState: MutableStateFlow<AuthState> =
        MutableStateFlow(AuthState.Loading)

    public val state: StateFlow<AuthState> = mutableState.asStateFlow()

    /**
     * ADR-0073 section 3.5 / 4.2 — non-null once this device learned that its
     * keys are not the account's keys any more: a `401
     * key-generation-superseded` from the refresh or a bearer route (the
     * account was reset elsewhere), or a landing that found the directory
     * serving other keys. The app root shows the signed-out screen instead
     * of the app and runs the wipe ([wipeAfterKeyReset]); nothing dismisses
     * it but its one button, which goes to the sign-in screen.
     */
    private val mutableKeyResetSignOut: MutableStateFlow<KeyResetSignOut?> =
        MutableStateFlow(null)

    public val keyResetSignOut: StateFlow<KeyResetSignOut?> = mutableKeyResetSignOut.asStateFlow()

    private val recallStore: RecallRecordStore by lazy { RecallRecordStore.onDevice(applicationContext) }

    /**
     * Non-null while this device holds a recall record: the account's keys
     * were reset elsewhere, the account's data is already deleted here, and
     * the app root shows "Was it you?" (UX 18.4) instead of the app, in
     * every auth state and after a restart (it is loaded from the record at
     * launch, [rehydrate]).
     */
    private val mutableRecall: MutableStateFlow<RecallUi?> = MutableStateFlow(null)

    public val recall: StateFlow<RecallUi?> = mutableRecall.asStateFlow()

    /** `true` for the rest of the session that took the account back: one
     *  line at the top of the chat list (`devices.recall.done`). Memory only. */
    private val mutableRecallDone: MutableStateFlow<Boolean> = MutableStateFlow(false)

    public val recallDone: StateFlow<Boolean> = mutableRecallDone.asStateFlow()

    public fun dismissRecallDone() {
        mutableRecallDone.value = false
    }

    /**
     * ADR-0073 section 3.3 — the one token provider every request of this
     * process goes through ([me.lakko.app.transport.SessionBearerEngine]
     * reads it per request). Installed with the pair of every landing;
     * refreshes ahead of expiry; reports the refresh route's refusals here.
     */
    private val sessionTokenProvider: SessionTokenProvider = SessionTokenProvider(
        refresher = object : SessionTokenProvider.SessionRefresher {
            override suspend fun refresh(refreshToken: String): SessionPair =
                authClientFactory().use { it.sessionRefresh(refreshToken) }
        },
        log = { message -> Log.w("lakko-auth", message) },
    )

    /**
     * In-memory identity secrets — the OPAQUE export key + the
     * raw account id bytes for the active session.
     *
     * Mirror of `apps/web/src/auth/AuthContext.tsx::identitySecrets`.
     * Populated whenever the user has **just authenticated**
     * (signup, login, recover) — those flows have the export key
     * in scope and can stash it for the duration of the current
     * process lifetime. **Cleared on logout / forgetDevice**.
     *
     * **Also populated on a cold-start restore** when the persisted
     * session token is unexpired and the AccountIdentity (phrase)
     * seed is readable ([decideSessionRestore]). A restore that
     * cannot populate it lands [AuthState.Anon] instead, so
     * `state == Session` always implies non-null secrets. (Before
     * this, a restore returned `Session` with `null` secrets and
     * locked the user out until they logged out and back in.)
     * Consumers still null-check, because the secrets are cleared
     * a moment before the state flips on logout.
     *
     * The shape mirrors the web `IdentitySecrets` so the
     * cross-platform mental model stays uniform: same lifecycle,
     * same restore-from-persisted-token, same wipe-on-logout.
     */
    private val mutableIdentitySecrets: MutableStateFlow<IdentitySecrets?> =
        MutableStateFlow(null)

    public val identitySecrets: StateFlow<IdentitySecrets?> =
        mutableIdentitySecrets.asStateFlow()

    /**
     * ADR-0037 §6 (G-11) — non-null while a restore is BLOCKED on the user
     * deciding whether to accept a STALE backup head (a possible rollback
     * replay). The restore coroutine (in the recover flow) suspends on
     * [awaitStaleOverride]; the UI observes this StateFlow, shows a destructive
     * confirm dialog, and resolves it via [resolveStaleOverride]. NEVER
     * auto-accepted (invariant #4).
     */
    private val mutableStaleOverridePrompt: MutableStateFlow<StaleOverridePrompt?> =
        MutableStateFlow(null)

    public val staleOverridePrompt: StateFlow<StaleOverridePrompt?> =
        mutableStaleOverridePrompt.asStateFlow()

    /** The in-flight stale-override decision channel — one at a time (a restore
     *  is a serial flow). */
    private val staleOverrideDeferred: AtomicReference<CompletableDeferred<Boolean>?> =
        AtomicReference(null)

    /**
     * ADR-0037 §6 (G-11) — non-null when a restore was HARD-refused by the
     * rollback gate (an unambiguous tamper / corruption of the served backup
     * head). Surfaced as a dismissible security alarm banner. `null` in the
     * normal case; the restore proceeds with no backup applied.
     */
    private val mutableRollbackAlert: MutableStateFlow<BackupRollbackAlert?> =
        MutableStateFlow(null)

    public val rollbackAlert: StateFlow<BackupRollbackAlert?> =
        mutableRollbackAlert.asStateFlow()

    /**
     * ADR-0017 fresh-device fail-safe: when a password / passkey
     * auth succeeds but no AccountIdentity (phrase) seed is cached on
     * this device, we stash everything needed to finish the session
     * here and flip the state to [AuthState.NeedsRecoveryPhrase]. The
     * UI then calls [provideRecoveryPhrase], which reads this back.
     * Single-slot — only one auth flow is ever pending at a time.
     */
    private val pendingRecovery: AtomicReference<PendingRecovery?> =
        AtomicReference(null)

    /** The fresh-auth proof of the newest password or passkey sign-in (or
     *  sign-up), in memory only ([FreshAuthHolder]; UX section 17): dropped
     *  on sign-out, on the account wipe, on expiry, on an account change and
     *  after a key reset. */
    private val freshAuth: FreshAuthHolder = FreshAuthHolder()

    /** How the session on this device was landed (UX 17.6), in memory with
     *  the session. */
    @Volatile
    private var sessionLandingKind: SessionLanding = SessionLanding.Other

    /** When the local three-word check last passed for an account, this
     *  process only: the other key to the password change (brief item 7). */
    private val wordsCheckedAtMs: java.util.concurrent.ConcurrentHashMap<String, Long> =
        java.util.concurrent.ConcurrentHashMap()

    /** ADR-0017 lost-everything fallback — the minted fresh phrase +
     *  completion context, parked between [mintFreshIdentity] and
     *  [confirmFreshPhraseSaved]. */
    private val pendingFreshPhrase: AtomicReference<PendingFreshPhrase?> =
        AtomicReference(null)

    /** The debts a landing can leave: a phrase still to be shown, the
     *  publish of a reset's new keys ([RecoveryOwedStore]). */
    private val owedStore: RecoveryOwedStore by lazy { RecoveryOwedStore.onDevice(applicationContext) }

    // ADR-0074 - declared ahead of `init`, which starts the restore that reads them.
    private val genesisRecorded: GenesisRecordedStore by lazy { GenesisRecordedStore.onDevice(applicationContext) }

    private val logoutOwedStore: LogoutOwedStore by lazy { LogoutOwedStore.onDevice(applicationContext) }

    /** One bind / mint at a time: two landings must not race each other's pair. */
    private val sessionDutyMutex: Mutex = Mutex()

    /**
     * Bumped each time a session that was already installed got bound to
     * this device in the background (a restored session, or a landing whose
     * bind could not be made at once). The KeyPackage publish of an unbound
     * session is refused (`403 device-mismatch`), so the app root republishes
     * when this moves.
     */
    private val mutableDeviceBoundRevision: MutableStateFlow<Int> = MutableStateFlow(0)

    public val deviceBoundRevision: StateFlow<Int> = mutableDeviceBoundRevision.asStateFlow()

    /** ADR-0073: the attestation and step markers of a reset whose publish is
     *  still owed. */
    /** ADR-0074 T5 item 25 - a `recall/finish` that was sent and not
     *  answered, with the phrase it would make the account's. Memory only:
     *  a process death loses it, and the person then opens another attempt. */
    private class PendingRecallFinish(
        val accountIdB64: String,
        val request: RecallFinishRequest,
        val firstSentAtMs: Long,
        val seed: ByteArray,
        val phrase: String,
        val newSignaturePubkey: ByteArray,
    )

    private val pendingRecallFinish = java.util.concurrent.atomic.AtomicReference<PendingRecallFinish?>(null)

    private val keyResetOwedStore: KeyResetOwedStore by lazy { KeyResetOwedStore.onDevice(applicationContext) }

    /** When the person last signed in on this device, per account, this
     *  process: what lets a reset older than 30 days be published after all
     *  ([attestationAgeAt]). */
    private val signedInAtMs: java.util.concurrent.ConcurrentHashMap<String, Long> =
        java.util.concurrent.ConcurrentHashMap()

    /** The local three-word guess budget, per account. */
    private val threeWordsBudgetStore: ThreeWordsBudgetStore by lazy {
        ThreeWordsBudgetStore.onDevice(applicationContext)
    }

    /**
     * The recovery phrase a fresh identity generated and whose screen was
     * left, until the person confirms they saved it; `null` otherwise. (A
     * sign-up shows no phrase: ADR-0075 D2.) While non-null the app root shows the phrase step instead
     * of the app. The fact that it is owed survives a restart
     * ([RecoveryOwedStore]); the phrase itself is read back from the phrase
     * store when the next session for the account lands.
     */
    private val mutableSignupPhrase: MutableStateFlow<String?> = MutableStateFlow(null)

    public val signupPhrase: StateFlow<String?> = mutableSignupPhrase.asStateFlow()

    // ---- ADR-0075: how the account is protected --------------------------

    private val protectionStore: AccountProtectionStore by lazy { AccountProtectionStore.onDevice(applicationContext) }

    private val noticeStore: AccountNoticeStore by lazy { AccountNoticeStore.onDevice(applicationContext) }

    /** `GET /recovery-email` of the signed-in account as last read; `null`
     *  until a landing (or Settings) read it. Never the address. */
    private val mutableRecoveryEmail: MutableStateFlow<RecoveryEmailStatus?> = MutableStateFlow(null)

    public val recoveryEmail: StateFlow<RecoveryEmailStatus?> = mutableRecoveryEmail.asStateFlow()

    /** `true` while the "Protect your account" step of sign-up (UX 19.1a) is
     *  not answered: the app root shows it instead of the app. Survives a
     *  restart ([AccountProtectionStore]). */
    private val mutableProtectStepOwed: MutableStateFlow<Boolean> = MutableStateFlow(false)

    public val protectStepOwed: StateFlow<Boolean> = mutableProtectStepOwed.asStateFlow()

    /** What happened to the account elsewhere and was not shown here yet
     *  (UX 19.2 / 19.5 / 19.6), oldest first. Memory only: the cursor is
     *  stored once a notice is fetched, so each is shown once. */
    private val mutableAccountNotices: MutableStateFlow<List<AccountNotice>> = MutableStateFlow(emptyList())

    public val accountNotices: StateFlow<List<AccountNotice>> = mutableAccountNotices.asStateFlow()

    /** The synced state's recovery-email register (ADR-0075, amended
     *  2026-10-13); set by the app root, which owns the sync runtime. */
    @Volatile
    public var recoveryAddressPort: RecoveryAddressPort? = null

    /** The address this device holds for the signed-in account, or `null`.
     *  Shown in Settings; never logged. */
    private val mutableRecoveryAddress: MutableStateFlow<String?> = MutableStateFlow(null)

    public val recoveryAddress: StateFlow<String?> = mutableRecoveryAddress.asStateFlow()

    /** The synced state's own-username register (ADR-0076). */
    @Volatile
    public var ownUsernamePort: OwnUsernamePort? = null

    /** Told that the account's username changed on this device's marker
     *  (a rename here, or one adopted from another device): the app root
     *  updates the stored profile and re-publishes. [renamedElsewhere] is
     *  `true` for an adopted name (the certificate must be fetched again). */
    @Volatile
    public var onUsernameChanged: ((username: String, renamedElsewhere: Boolean) -> Unit)? = null

    /** A name this device wrote while the synced state was closed (sign-up,
     *  a guest claim): account id to name. */
    private val pendingUsernameWrite: AtomicReference<Pair<String, String>?> = AtomicReference(null)

    private val renameStore: UsernameRenameStore by lazy { UsernameRenameStore.onDevice(applicationContext) }

    /** `true` while this device's own rename is between its steps. */
    @Volatile
    private var renaming: Boolean = false

    /** A register write made before the synced state was open (a recovery
     *  on a fresh device): account id to address (`null` = removed). */
    private val pendingAddressWrite: AtomicReference<Pair<String, String?>?> = AtomicReference(null)

    /** Accounts with a re-seal in flight, and those already tried since the
     *  last landing or register change (one attempt, never a loop). */
    private val resealInFlight: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()
    private val resealTried: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** Until when "It was not me" lets this device choose a new password
     *  without another proof (UX 19.6: the device holds the keys). */
    private val noticePasswordUnlockUntilMs: MutableMap<String, Long> = java.util.concurrent.ConcurrentHashMap()

    /** Bumped whenever the publish debt of the signed-in account may have
     *  changed, so a screen that shows it reads it again. */
    private val mutablePublishOwedRevision: MutableStateFlow<Int> = MutableStateFlow(0)

    public val publishOwedRevision: StateFlow<Int> = mutablePublishOwedRevision.asStateFlow()

    /** Why the restore prompt is up after a passkey sign-in, or `null`. */
    private val mutableRestoreNotice: MutableStateFlow<RestoreNotice?> = MutableStateFlow(null)

    public val restoreNotice: StateFlow<RestoreNotice?> = mutableRestoreNotice.asStateFlow()

    /**
     * A forgot-password route succeeded (three words, an authenticator code,
     * a passkey, another device from the recovery list), so "Choose a new
     * password" is owed once the device holds the keys. In memory only: a
     * restart drops the offer, and Settings > Security keeps the same form.
     * The app root shows the step while this is `true` AND
     * [canReplacePassword].
     */
    private val mutableNewPasswordOwed: MutableStateFlow<Boolean> = MutableStateFlow(false)

    public val newPasswordOwed: StateFlow<Boolean> = mutableNewPasswordOwed.asStateFlow()

    /**
     * ADR-0073 section 2.3 — non-null while the account keys were reset on
     * this device but the new identity is not yet published to the directory
     * and the device list (a step failed, or the attestation is too old to
     * use without a sign-in). The phrase screen then shows the reason and
     * "Try again" instead of the words; [retryFreshPublish] clears it.
     */
    private val mutablePublishOwedReason: MutableStateFlow<PublishOwedReason?> = MutableStateFlow(null)

    public val freshPublishOwed: StateFlow<PublishOwedReason?> = mutablePublishOwedReason.asStateFlow()

    /**
     * UX 15.3 — `true` for the rest of the session in which THIS device reset
     * the account keys: Settings > Devices shows a dismissible note ("the
     * other devices were signed out and removed") and Settings > Passkeys
     * says the passkeys were deleted. In memory only.
     */
    private val mutableResetThisSession: MutableStateFlow<Boolean> = MutableStateFlow(false)

    public val resetThisSession: StateFlow<Boolean> = mutableResetThisSession.asStateFlow()

    /** The Devices note of UX 15.3 was dismissed. */
    public fun dismissResetNote() {
        mutableResetThisSession.value = false
    }

    /** Cold-start restore scope — see [init]. */
    private val restoreScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    init {
        // Rehydrate OFF the main thread, staying in [AuthState.Loading] until
        // the decision is made. The reads are small but not cheap: the device
        // key is an Android Keystore decrypt and the session token, the
        // AccountIdentity seed and the export key each live in their own
        // `EncryptedSharedPreferences`, whose first open per process builds a
        // Tink keyset behind a Keystore master key. Measured on the emulator
        // this constructor cost ~650 ms of `MainActivity.onCreate` — before
        // `setContent`, so before the first frame could even be composed.
        //
        // The "Session without identity secrets" half state stays
        // unreachable: [rehydrate] still writes the secrets flow BEFORE the
        // state flow, in one coroutine, and nothing observes `Session` until
        // the state flips. `AppRoot` renders its loading screen meanwhile.
        // ADR-0073 - the transport reads this provider on every bearer request.
        SessionTokens.provider = sessionTokenProvider
        restoreScope.launch {
            rehydrate()
            // ADR-0074 - a logout made offline is told to the server now.
            retryOwedLogouts()
        }
        // ADR-0054 S5 - every landed session whose username this device knows
        // binds its login handle (restore, passkey, pairing, another-device
        // confirmation, ...), not only signup and sign-in by name; otherwise
        // an account that never signed in by name could not do so after a
        // logout. The rebinder dedupes per (session, username).
        pushScope.launch {
            combine(state, identitySecrets, ::loginHandleBindTarget).collect { target ->
                if (target != null) loginHandleRebinder.onSessionAvailable(target)
            }
        }
        // ADR-0054 S3 - the anonymous-token pool draws its batches WITH this
        // session (the tokens it hands out carry nothing about the account).
        // Keyed on the account, so a switch never reuses another account's
        // tokens. S5: each account's pool persists under noBackupFilesDir
        // between launches and across a logout (account data, 2026-10-01);
        // the forget-device wipe removes that account's only.
        me.lakko.app.anon.AnonTokens.installStore(
            me.lakko.app.anon.FileAnonTokenStore.under(applicationContext.noBackupFilesDir),
        )
        me.lakko.app.anon.AnonTokens.installSession {
            identitySecrets.value?.let {
                me.lakko.app.anon.AnonSession(
                    accountKey = base64Encode(it.accountId),
                    bearer = it.sessionToken,
                )
            }
        }
    }

    /**
     * Inspect persisted state and land the initial [AuthState] — silently
     * restoring the full session when everything it needs is on disk.
     *
     * The rule and its trade-off live on [decideSessionRestore]; in short:
     *
     *   * No `SessionStore` record → fresh device, [AuthState.Anon].
     *   * Record present but the device key, an unexpired session token or
     *     the AccountIdentity (phrase) seed is missing / undecryptable →
     *     [AuthState.Anon] with `cachedUsername` so the Login form pre-fills.
     *   * All present → [AuthState.Session] WITH [identitySecrets] populated,
     *     so the chat bootstrap (and its KeyPackage republish) runs exactly as
     *     after a password login.
     *
     * Before this, a restore returned `Session` with no secrets, which locked
     * the user out after every process restart while claiming they were
     * signed in.
     */
    private fun rehydrate() {
        // ADR-0074 - a device that was mid-"Was it you?" comes back to it.
        // The staged wipe left no session, so the decision below lands Anon
        // underneath; the app root draws the recall screen over it.
        restoreRecallAtLaunch()
        val decision = decideSessionRestore(
            source = productionRestoreSource(),
            nowMs = System.currentTimeMillis(),
            log = { message, cause -> Log.w("lakko-auth", message, cause) },
        )
        when (decision) {
            SessionRestoreDecision.NoAccount -> {
                mutableState.value = AuthState.Anon()
            }
            is SessionRestoreDecision.SignedOut -> {
                mutableState.value = AuthState.Anon(cachedUsername = decision.cachedUsername)
            }
            is SessionRestoreDecision.Restored -> {
                // ADR-0073 section 3.3 - the restore refreshes the access token
                // instead of reusing the persisted one: the provider is
                // installed with the persisted pair and asked for a fresh
                // token before the landing's first request. Offline, the app
                // opens on local data and every server call fails as it did.
                // A refresh refused as superseded lands the signed-out screen
                // through the provider's refusal hook, below.
                val accountIdB64 = decision.account.accountIdB64
                installSessionPair(accountIdB64, decision.secrets.session)
                // Secrets first, then state: an observer that reacts to the
                // Session flip must already find the secrets it needs.
                publishIdentitySecrets(decision.secrets)
                mutableState.value = AuthState.Session(account = decision.account)
                // ADR-0054 UX section 3 - a push registration that failed
                // (a used-up allowance, no network) is retried silently at the
                // next launch, which is this restore.
                subscribePushBestEffort("restore")
                // ADR-0073 section 4.2 - the same read-before-write check a
                // password login runs (does the directory still serve OUR
                // keys, at OUR generation?), re-publishing only when the
                // directory is missing the bundle; stale keys land the
                // signed-out screen and never an upload. Preceded by the
                // refresh, so a reset the server already knows about is
                // learned from the refresh refusal first. Not while a reset's
                // publish is owed on THIS device: the directory then still
                // serves the old key, and the owed retry is what publishes.
                refreshThenCheckPublishedIdentityInBackground(
                    accountIdB64 = accountIdB64,
                    accountIdBytes = decision.secrets.accountId,
                    accountIdentitySeed = decision.secrets.accountIdentitySeed,
                )
            }
        }
    }

    /** The on-device stores the cold-start decision reads. */
    private fun productionRestoreSource(): SessionRestoreSource = object : SessionRestoreSource {
        override fun loadMarker(): PersistedSession? = sessionStore.load()

        override fun hasDeviceKey(accountIdB64: String): Boolean {
            // Only the existence signal matters; wipe the bytes immediately.
            val keyBytes = deviceKeyStore.getDeviceKey(applicationContext, accountIdB64)
                ?: return false
            keyBytes.fill(0)
            return true
        }

        override fun loadSessionPair(accountIdB64: String): SessionPair? =
            SessionTokenStore.getSessionPair(applicationContext, accountIdB64)

        override fun loadAccountIdentitySeed(accountIdB64: String): ByteArray? =
            AccountIdentitySeedStore.getAccountIdentitySeed(applicationContext, accountIdB64)

        override fun loadExportKey(accountIdB64: String): ByteArray? =
            ExportKeyStore.getExportKey(applicationContext, accountIdB64)

        override fun discardSessionToken(accountIdB64: String) {
            SessionTokenStore.deleteSessionToken(applicationContext, accountIdB64)
        }
    }

    /**
     * Install freshly-authenticated [secrets] as the live session AND persist
     * the bearer token so the next cold start can restore it
     * ([decideSessionRestore]). Every landing path that populates
     * [identitySecrets] goes through here, so no path can land a session that
     * a relaunch would silently lose.
     *
     * The persist is best-effort in the fail-closed direction: if it fails the
     * live session is unaffected and the next launch simply lands signed out.
     */
    private fun installIdentitySecrets(accountIdB64: String, secrets: IdentitySecrets) {
        persistSessionPairBestEffort(accountIdB64, secrets.session)
        installSessionPair(accountIdB64, secrets.session)
        publishIdentitySecrets(secrets)
    }

    /** Persist [pair] where the session token always was; a failed write
     *  only means the next launch lands signed out (fail closed). */
    private fun persistSessionPairBestEffort(accountIdB64: String, pair: SessionPair) {
        try {
            SessionTokenStore.putSessionPair(applicationContext, accountIdB64, pair)
        } catch (cause: Throwable) {
            Log.w(
                "lakko-auth",
                "session-pair persist failed; the next launch will require sign-in",
                cause,
            )
        }
    }

    /**
     * ADR-0073 section 3.3 — hand [pair] to the token provider as the session
     * of [accountIdB64]: every refreshed pair is persisted under the same
     * account, and a refresh refused as `key-generation-superseded` lands the
     * signed-out screen ([onKeysSuperseded]); `refresh-invalid` lands the
     * sign-in screen with nothing wiped ([onRefreshInvalid]).
     */
    private fun installSessionPair(accountIdB64: String, pair: SessionPair) {
        // ADR-0074 - a logout of an EARLIER session of this account that the
        // server was not told about must not be replayed now: its floor
        // would refuse the session that is landing.
        logoutOwedStore.clear(accountIdB64)
        sessionTokenProvider.install(
            pair = pair,
            persist = { refreshed ->
                persistSessionPairBestEffort(accountIdB64, refreshed)
                // The live secrets carry the pair too (the anonymous-token
                // session, the per-screen closures read it).
                mutableIdentitySecrets.value?.let { live ->
                    if (base64Encode(live.accountId) == accountIdB64) {
                        mutableIdentitySecrets.value = live.copy(session = refreshed)
                    }
                }
            },
            onRefusal = { refusal ->
                when (refusal) {
                    SessionRefusal.KeysReset -> onKeysSuperseded(accountIdB64)
                    SessionRefusal.DeviceRevoked -> onDeviceRevoked(accountIdB64)
                    SessionRefusal.RefreshInvalid, SessionRefusal.Unauthorized -> onRefreshInvalid(accountIdB64)
                }
            },
        )
    }

    /**
     * A pair for a session that has not landed yet (a device without the
     * account's keys, parked in [AuthState.NeedsRecoveryPhrase]): the provider
     * serves and refreshes it in memory; nothing is persisted until the
     * session lands.
     */
    private fun installPendingSessionPair(accountIdB64: String, pair: SessionPair) {
        logoutOwedStore.clear(accountIdB64)
        sessionTokenProvider.install(
            pair = pair,
            persist = { refreshed ->
                pendingRecovery.get()?.let { pending ->
                    if (pending.accountIdB64 == accountIdB64) {
                        pendingRecovery.compareAndSet(pending, pending.copy(session = refreshed))
                    }
                }
            },
            onRefusal = { refusal ->
                when (refusal) {
                    SessionRefusal.KeysReset -> onKeysSuperseded(accountIdB64)
                    SessionRefusal.DeviceRevoked -> onDeviceRevoked(accountIdB64)
                    SessionRefusal.RefreshInvalid, SessionRefusal.Unauthorized -> onRefreshInvalid(accountIdB64)
                }
            },
        )
    }

    /** The person signed in on this device just now (a password, a passkey,
     *  an authenticator code, three words, the recovery phrase): what confirms
     *  a weeks-old reset may still be published ([attestationAgeAt]). */
    private fun markSignedInNow(accountIdB64: String) {
        signedInAtMs[accountIdB64] = System.currentTimeMillis()
    }

    /**
     * The one place a live session's secrets are published. Binds the
     * per-account contact list ([me.lakko.app.contacts.AcceptedContactsStore])
     * to the landing account FIRST, so an observer of [identitySecrets] never
     * reads another account's contacts; [clearIdentitySecrets] unbinds it.
     */
    private fun publishIdentitySecrets(secrets: IdentitySecrets) {
        me.lakko.app.contacts.AcceptedContactsStore.bind(secrets.accountId)
        val accountIdB64 = base64Encode(secrets.accountId)
        // Before the secrets (and so before the state flip): a phrase that is
        // still owed must be up by the time the session renders.
        restoreOwedPhrase(accountIdB64)
        restoreKeysRotatedNotice(accountIdB64)
        mutableIdentitySecrets.value = secrets
        settleOwedPublish(accountIdB64, secrets.accountId, secrets.accountIdentitySeed)
        runSessionDuties(secrets)
        // ADR-0075 - how the account is protected, and what happened to it
        // elsewhere, read at every landing.
        mutableProtectStepOwed.value = protectionStore.protectStepOwed(accountIdB64)
        mutableRecoveryEmail.value = null
        resealTried.remove(accountIdB64)
        pushScope.launch { refreshAccountProtection(accountIdB64) }
        // ADR-0074 section 3.3 - a pending key rotation whose key the
        // directory already serves is adopted at the landing.
        pushScope.launch {
            try {
                checkPendingRotations()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                Log.w("lakko-auth", "pending key rotation not checked: ${cause.javaClass.simpleName}")
            }
        }
    }

    // -----------------------------------------------------------------
    // ADR-0074: the session names this device; auth records the account key
    // -----------------------------------------------------------------

    /** What a landing sends as `device_id_b64` when it expects to land
     *  [candidateAccountIdB64]; `null` (send nothing, bind afterwards) when
     *  the account is not known before the request. */
    private fun landingDeviceId(candidateAccountIdB64: String?): ByteArray? =
        when (val landing = landingDeviceFor(candidateAccountIdB64) { deviceIds.deviceIdFor(it) }) {
            is LandingDevice.Named -> landing.deviceId
            LandingDevice.BindAfter -> null
        }

    /** The account a sign-in by [typedUsername] is expected to land: the
     *  one this device holds a session record for under that name. */
    private fun candidateAccount(typedUsername: String): String? =
        candidateAccountForName(typedUsername, sessionStore.load())

    /**
     * The pair to install for a session that just landed as [accountIdB64]:
     * [pair] itself when it already names this device, else the pair
     * `bind-device` returns (the unbound one is dropped). A bind that cannot
     * be made now (offline, a limit) leaves the session unbound and
     * [runSessionDuties] tries again; nothing here fails a sign-in for that.
     *
     * A session that names ANOTHER device id cannot be used by this device
     * (it cannot publish its KeyPackage and cannot be re-bound): it is ended
     * at the server, the stale session record that caused the wrong guess
     * is dropped, and the sign-in is refused so the next one lands cleanly.
     */
    private suspend fun boundSession(accountIdB64: String, pair: SessionPair): SessionPair {
        val deviceId = deviceIds.deviceIdFor(accountIdB64) ?: return pair
        return when (deviceBindingActionFor(pair.accessToken, deviceId)) {
            DeviceBindingAction.Bound, DeviceBindingAction.Unknown -> pair
            DeviceBindingAction.Bind -> when (val outcome = bindDevice(pair, deviceId)) {
                is BindDeviceOutcome.Bound -> outcome.pair
                BindDeviceOutcome.AlreadyBoundElsewhere -> refuseForeignLanding(accountIdB64, pair)
                BindDeviceOutcome.Revoked, BindDeviceOutcome.Superseded -> pair
                is BindDeviceOutcome.Retry -> {
                    Log.w("lakko-auth", "the session is not bound to this device yet: ${outcome.cause.javaClass.simpleName}")
                    pair
                }
            }
            DeviceBindingAction.Foreign -> refuseForeignLanding(accountIdB64, pair)
        }
    }

    private suspend fun bindDevice(pair: SessionPair, deviceId: ByteArray): BindDeviceOutcome {
        var bound: SessionPair? = null
        val error = try {
            bound = authClientFactory().use { it.bindDevice(pair.accessToken, deviceId, pair.refreshToken) }
            null
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            cause
        }
        return bindDeviceOutcome(bound, error)
    }

    private suspend fun refuseForeignLanding(accountIdB64: String, pair: SessionPair): Nothing {
        Log.w("lakko-auth", "the landed session names another device id; ending it and asking for a new sign-in")
        try {
            authClientFactory().use { it.sessionLogout(pair.accessToken) }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            // Best effort: the credentials are dropped here either way.
        }
        // The record that named another account under this name is what
        // made the landing send the wrong id; without it the next sign-in
        // binds after its answer.
        if (sessionStore.load()?.accountIdB64 != accountIdB64) sessionStore.clear()
        throw AuthClientArgError(
            "the session names another device id",
            me.lakko.app.l10n.UiText("common.error.session_ended"),
        )
    }

    private suspend fun LoginResult.bound(): LoginResult =
        copy(session = boundSession(base64Encode(accountId), session))

    private suspend fun RecoverResult.bound(): RecoverResult =
        copy(session = boundSession(base64Encode(accountId), session))

    private suspend fun TotpRecoverResult.bound(): TotpRecoverResult =
        copy(session = boundSession(base64Encode(accountId), session))

    /**
     * The duties of a session that is live with the account's keys, run in
     * the background after every landing and every restore:
     *
     *   1. bind the session to this device when it is not (a session
     *      restored from before the bind, or a bind that failed at the
     *      landing);
     *   2. have auth record the account's identity key
     *      (`POST /auth/genesis/attest`), until that succeeded once for the
     *      key this device holds - again after a key reset, for the new key.
     *
     * Best effort: every failure is retried at the next landing.
     */
    private fun runSessionDuties(secrets: IdentitySecrets) {
        val accountIdB64 = base64Encode(secrets.accountId)
        val seed = secrets.accountIdentitySeed.copyOf()
        pushScope.launch {
            try {
                sessionDutyMutex.withLock {
                    bindInstalledSession(accountIdB64)
                    recordIdentityKey(accountIdB64, seed)
                }
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "session duties failed: ${cause.javaClass.simpleName}")
            } finally {
                seed.fill(0)
            }
        }
    }

    /** Whether the live secrets are [accountIdB64]'s. (Not the auth state:
     *  a landing publishes its secrets a moment before the state flips.) */
    private fun secretsAreFor(accountIdB64: String): Boolean =
        mutableIdentitySecrets.value?.let { base64Encode(it.accountId) == accountIdB64 } == true

    private suspend fun bindInstalledSession(accountIdB64: String) {
        if (!secretsAreFor(accountIdB64)) return
        val installed = sessionTokenProvider.peek() ?: return
        val deviceId = deviceIds.deviceIdFor(accountIdB64) ?: return
        when (deviceBindingActionFor(installed.accessToken, deviceId)) {
            DeviceBindingAction.Bound, DeviceBindingAction.Unknown -> return
            DeviceBindingAction.Foreign -> {
                Log.w("lakko-auth", "the installed session names another device id: signing out (nothing wiped)")
                logout()
                return
            }
            DeviceBindingAction.Bind -> Unit
        }
        // A fresh access token first, then the pair it belongs to: the bind
        // wants the bearer and the refresh credential of ONE session.
        sessionTokenProvider.current() ?: return
        val pair = sessionTokenProvider.peek() ?: return
        when (val outcome = bindDevice(pair, deviceId)) {
            is BindDeviceOutcome.Bound -> {
                if (!secretsAreFor(accountIdB64)) return
                sessionTokenProvider.adopt(outcome.pair)
                mutableDeviceBoundRevision.value += 1
            }
            BindDeviceOutcome.AlreadyBoundElsewhere -> {
                Log.w("lakko-auth", "the session is bound to another device id: signing out (nothing wiped)")
                logout()
            }
            BindDeviceOutcome.Revoked -> onDeviceRevoked(accountIdB64)
            BindDeviceOutcome.Superseded -> onKeysSuperseded(accountIdB64)
            is BindDeviceOutcome.Retry ->
                Log.w("lakko-auth", "bind-device not made: ${outcome.cause.javaClass.simpleName}")
        }
    }

    private suspend fun recordIdentityKey(accountIdB64: String, accountIdentitySeed: ByteArray) {
        if (!secretsAreFor(accountIdB64)) return
        val pubkey = Identity.fromSeed(accountIdentitySeed).use { it.signaturePublicKey() }
        val pubkeyB64 = base64Encode(pubkey)
        if (!genesisMintNeeded(genesisRecorded.recordedPubkeyB64(accountIdB64), pubkeyB64)) return
        val token = sessionTokenProvider.current() ?: return
        val error = try {
            authClientFactory().use { it.genesisAttest(token, pubkey) }
            null
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            cause
        }
        when (genesisMintOutcome(error)) {
            GenesisMintOutcome.Recorded -> genesisRecorded.markRecorded(accountIdB64, pubkeyB64)
            GenesisMintOutcome.Mismatch ->
                Log.w("lakko-auth", "auth holds another identity key for this account; not recorded")
            GenesisMintOutcome.Retry ->
                Log.w("lakko-auth", "identity key not recorded at auth yet: ${error?.javaClass?.simpleName}")
        }
    }

    /**
     * Whether auth is known to hold this account's identity key (the mint
     * succeeded once for the key this device holds). Device removal needs
     * it: without a recorded key the server answers `account-key-required`.
     */
    public fun identityKeyRecorded(): Boolean {
        val secrets = mutableIdentitySecrets.value ?: return false
        val accountIdB64 = base64Encode(secrets.accountId)
        val recorded = genesisRecorded.recordedPubkeyB64(accountIdB64) ?: return false
        return try {
            Identity.fromSeed(secrets.accountIdentitySeed).use { base64Encode(it.signaturePublicKey()) } == recorded
        } catch (_: Throwable) {
            false
        }
    }

    /** Run the identity-key mint now (the Devices screen, before a removal
     *  on an account whose key is not recorded yet). */
    public suspend fun ensureIdentityKeyRecorded(): Boolean {
        val secrets = mutableIdentitySecrets.value ?: return false
        val accountIdB64 = base64Encode(secrets.accountId)
        val seed = secrets.accountIdentitySeed.copyOf()
        try {
            sessionDutyMutex.withLock { recordIdentityKey(accountIdB64, seed) }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            // Read back below.
        } finally {
            seed.fill(0)
        }
        return identityKeyRecorded()
    }

    // -----------------------------------------------------------------
    // ADR-0074 section 2: logout tells the server
    // -----------------------------------------------------------------

    /**
     * `POST /auth/session/logout` for the session that just ended on this
     * device, best effort: the pair is put aside durably first (when
     * [keepForRetry]), the call is made in the background, and a call that
     * found no connection is made once more at the next launch
     * ([retryOwedLogouts]). The local sign-out never waits for it.
     */
    private fun tellServerLoggedOut(accountIdB64: String, pair: SessionPair, keepForRetry: Boolean) {
        val owed = LogoutOwed(
            accessToken = pair.accessToken,
            accessExpiresAtMs = pair.accessExpiresAtMs,
            refreshToken = pair.refreshToken,
            refreshExpiresAtMs = pair.refreshExpiresAtMs,
        )
        if (keepForRetry) logoutOwedStore.save(accountIdB64, owed)
        pushScope.launch { attemptServerLogout(accountIdB64, owed, isRetry = !keepForRetry) }
    }

    private suspend fun attemptServerLogout(accountIdB64: String, owed: LogoutOwed, isRetry: Boolean) {
        try {
            authClientFactory().use { client ->
                when (logoutRetryStep(owed, System.currentTimeMillis())) {
                    LogoutRetryStep.Drop -> Unit
                    LogoutRetryStep.Logout -> client.sessionLogout(owed.accessToken)
                    LogoutRetryStep.RefreshThenLogout ->
                        client.sessionLogout(client.sessionRefresh(owed.refreshToken).accessToken)
                }
            }
            logoutOwedStore.clear(accountIdB64)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "server logout not confirmed: ${cause.javaClass.simpleName}")
            if (!keepLogoutOwed(cause, isRetry)) logoutOwedStore.clear(accountIdB64)
        }
    }

    /** The one retry of a logout made offline, at launch. Never for an
     *  account that is signed in again: the floor a late logout writes
     *  would refuse the session that is live now. */
    private suspend fun retryOwedLogouts() {
        for (accountIdB64 in logoutOwedStore.accounts()) {
            val owed = logoutOwedStore.load(accountIdB64)
            if (owed == null || signedInAccountIdB64() == accountIdB64 ||
                pendingRecovery.get()?.accountIdB64 == accountIdB64
            ) {
                logoutOwedStore.clear(accountIdB64)
                continue
            }
            attemptServerLogout(accountIdB64, owed, isRetry = true)
        }
    }

    /**
     * A phrase generated on this device and never confirmed as saved is
     * shown again at the next session for that account, read back from the
     * phrase store. Not while the publish of a reset is outstanding: a
     * phrase for an identity nobody can find is not a recovery phrase yet
     * ([settleOwedPublish] calls back here once the publish lands). A phrase
     * that is owed but no longer readable cannot be shown and is logged
     * rather than blocking the account.
     */
    private fun restoreOwedPhrase(accountIdB64: String) {
        if (mutableSignupPhrase.value != null) return
        if (!owedStore.phraseShowable(accountIdB64)) return
        // ADR-0075 - new words are shown only to a phrase-protected account.
        if (!showsNewWords(protectionStore.phraseSaved(accountIdB64))) {
            owedStore.clearPhraseOwed(accountIdB64)
            return
        }
        val phrase = DevicePhrase.read(applicationContext, accountIdB64)
        if (phrase == null) {
            Log.w(
                "lakko-auth",
                "a recovery phrase is owed but not readable on this device; it cannot be shown again",
            )
            return
        }
        mutableSignupPhrase.value = phrase
    }

    /** The person has saved the phrase [signupPhrase] showed: drop it from
     *  memory and clear the debt. */
    public fun acknowledgeSignupPhrase() {
        signedInAccountIdB64()?.let { owedStore.clearPhraseOwed(it) }
        mutableSignupPhrase.value = null
    }

    /**
     * ADR-0073 section 2.3 — a reset whose publish did not finish leaves the
     * attestation and the `publish-owed` marker. When a session for that
     * account lands, the device holds the new seed, so the remaining steps
     * are tried again here, in the background, with the SAME attestation.
     * Success clears the marker (and shows the phrase if it was never
     * confirmed); a failure is recorded on [freshPublishOwed] and waits for
     * the next landing or "Try again".
     */
    private fun settleOwedPublish(
        accountIdB64: String,
        accountIdBytes: ByteArray,
        accountIdentitySeed: ByteArray,
    ) {
        if (!owedStore.isPublishOwed(accountIdB64)) return
        val seed = accountIdentitySeed.copyOf()
        val accountId = accountIdBytes.copyOf()
        pushScope.launch {
            try {
                runOwedPublish(accountIdB64, accountId, seed)
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                Log.w(
                    "lakko-auth",
                    "owed publish of the reset keys failed; retried at the next session landing: " +
                        cause.javaClass.simpleName,
                )
            } finally {
                seed.fill(0)
            }
        }
    }

    /**
     * The retry of a reset's publish for [accountIdB64] (ADR-0073 section
     * 2.3 steps 3 and 5): the directory upload with the attestation, then
     * the DeviceGroup re-root, each skipped when its marker says it landed.
     * On success the debt is cleared and a phrase that was never confirmed
     * comes up. Returns normally when nothing is owed any more; throws what
     * the failing step threw (the debt then stays, with its reason on
     * [freshPublishOwed]).
     */
    private suspend fun runOwedPublish(
        accountIdB64: String,
        accountIdBytes: ByteArray,
        accountIdentitySeed: ByteArray,
    ) {
        val owed = keyResetOwedStore.load(accountIdB64)
        if (owed == null) {
            // The marker without the attestation: nothing can be retried, and
            // the old key cannot be published back either. Only a sign-out
            // and a new reset leave this state; log it.
            Log.w("lakko-auth", "publish owed but no reset attestation is stored; the debt cannot be settled here")
            mutablePublishOwedReason.value = PublishOwedReason.Refused(IllegalStateException("no attestation"))
            return
        }
        when (attestationAgeAt(owed.signedAtMs, System.currentTimeMillis(), signedInAtMs[accountIdB64])) {
            AttestationAge.Fresh, AttestationAge.StaleConfirmed -> Unit
            AttestationAge.StaleNeedsSignIn -> {
                mutablePublishOwedReason.value = PublishOwedReason.SignInRequired
                return
            }
        }
        val attestation = owed.attestation
        val verified = verifyResetAttestationLocally(
            attestation = attestation,
            accountIdBytes = accountIdBytes,
            newSignaturePubkey = owed.newSignaturePubkey,
        )
        when (verified) {
            is ResetAttestationVerdict.Unusable -> {
                mutablePublishOwedReason.value = verified.reason
                return
            }
            is ResetAttestationVerdict.Usable -> {
                // ADR-0073 R.4 - the marker of this device's own reset: the
                // continuity key of the new mailbox is registered from it,
                // with its expiry, by the reach sync that follows the reset.
                me.lakko.app.reach.ContinuityMarkerStore.write(
                    applicationContext,
                    accountIdB64,
                    me.lakko.app.reach.ContinuityMarker(owed.keyGeneration, verified.signedAtMs),
                )
                // The 30-day bound counts from the mint time the attestation
                // carries, not from when this device received it.
                if (verified.signedAtMs != owed.signedAtMs) {
                    keyResetOwedStore.save(
                        accountIdB64,
                        KeyResetOwed(
                            owed.attestationB64, owed.keyGeneration, verified.signedAtMs,
                            owed.newSignaturePubkeyB64, owed.discoveryPublished, owed.deviceGroupReset,
                        ),
                    )
                }
            }
        }
        var step = nextKeyResetStep(owed)
        while (step != KeyResetStep.Done) {
            val outcome = when (step) {
                KeyResetStep.DiscoveryPublish -> uploadResetBundle(
                    accountIdBytes = accountIdBytes,
                    accountIdentitySeed = accountIdentitySeed,
                    attestation = attestation,
                )
                KeyResetStep.DeviceGroupReset -> reRootDeviceGroup(
                    accountIdBytes = accountIdBytes,
                    accountIdB64 = accountIdB64,
                    accountIdentitySeed = accountIdentitySeed,
                    attestationB64 = owed.attestationB64,
                    keyGeneration = owed.keyGeneration,
                )
                KeyResetStep.Done -> KeyResetStepOutcome.Landed
            }
            when (outcome) {
                KeyResetStepOutcome.Landed, KeyResetStepOutcome.AlreadyLanded -> {
                    keyResetOwedStore.markStep(accountIdB64, step)
                }
                is KeyResetStepOutcome.Retry -> {
                    mutablePublishOwedReason.value = PublishOwedReason.Retryable(outcome.cause, outcome.retryAfterSecs)
                    throw outcome.cause
                }
                is KeyResetStepOutcome.Refused -> {
                    mutablePublishOwedReason.value = PublishOwedReason.Refused(outcome.cause)
                    throw outcome.cause
                }
            }
            step = nextKeyResetStep(keyResetOwedStore.load(accountIdB64) ?: return)
        }
        keyResetOwedStore.clear(accountIdB64)
        owedStore.clearPublishOwed(accountIdB64)
        mutablePublishOwedReason.value = null
        mutablePublishOwedRevision.value = mutablePublishOwedRevision.value + 1
        if (signedInAccountIdB64() == accountIdB64) restoreOwedPhrase(accountIdB64)
    }

    /**
     * Whether the publish of a reset is owed for the signed-in account.
     * While it is, "Show recovery phrase" withholds the words
     * ([phraseRevealGate]): nobody can find the new keys yet.
     */
    public fun publishOwed(): Boolean {
        val accountIdB64 = signedInAccountIdB64() ?: return false
        // ADR-0074 part 3: and while a rotation this device ran has not
        // published the new keys yet (the same rule, UX 18.1).
        return owedStore.isPublishOwed(accountIdB64) || rotationPublishOwed(accountIdB64)
    }

    /**
     * "Try again" in Settings while a reset's publish is owed: the same
     * steps a session landing runs in the background, awaited. Never resets
     * the keys again. Throws what the failing step threw (offline, `429`,
     * `503`); returns normally when nothing is owed any more.
     */
    public suspend fun retryOwedPublish() {
        val accountIdB64 = signedInAccountIdB64() ?: throw AuthClientArgError("not signed in")
        if (!owedStore.isPublishOwed(accountIdB64)) return
        val secrets = mutableIdentitySecrets.value ?: throw AuthClientArgError("not signed in")
        val seed = secrets.accountIdentitySeed.copyOf()
        try {
            runOwedPublish(accountIdB64, secrets.accountId, seed)
        } finally {
            seed.fill(0)
        }
    }

    /** Synchronously drop the persisted bearer token for [accountIdB64]. */
    private fun deletePersistedSessionTokenBestEffort(accountIdB64: String, context: String) {
        try {
            SessionTokenStore.deleteSessionToken(applicationContext, accountIdB64)
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "session-token delete ($context) failed", cause)
        }
    }

    /**
     * Drive the full OPAQUE-PAKE signup flow.
     *
     * Steps:
     *   1. Validate the username (non-empty after trim).
     *   2. Call [AuthClient.signup] with `enrolRecovery = true`.
     *   2a. Claim the username ([claimUsernameAtSignup]): bind the login
     *      handle at auth, then register at discovery; a taken name fails
     *      signup with the form's message.
     *   3. Save the `(accountId, normalisedUsername)` marker to [SessionStore].
     *   4. Derive the device root key via [deriveDeviceRootKey].
     *   5. Wrap + persist via [DeviceKeyStore.putDeviceKey].
     *   6. Zeroize the OPAQUE export key + the derived root key.
     *   6a. Publish the identity bundle (genesis, fail loud).
     *   7. Flip state to [AuthState.Session].
     *   8. Record that the "Protect your account" step is owed (ADR-0075).
     *      The 12-word phrase is generated and stored on the device as
     *      before, and is NOT shown or returned (D2).
     *
     * The order matters. We persist the marker BEFORE deriving
     * the device key so a crash between (3) and (5) leaves the
     * user with a recoverable state on next launch (the rehydrate
     * gate detects "marker but no device key" and asks for the
     * password again). The reverse ordering would leave a working
     * device key in Keystore with no record of who it belongs to.
     *
     * @throws AuthClientArgError on a blank username
     * @throws AuthHttpError on a non-2xx server response
     * @throws AuthNetworkError on a transport failure
     */
    public suspend fun signup(
        username: String,
        password: ByteArray,
        onProofSolved: () -> Unit = {},
    ): SignupOutcome {
        val trimmed = username.trim()
        if (trimmed.isEmpty()) {
            throw AuthClientArgError("username must be non-empty")
        }
        return authClientFactory().use { client ->
            val result = client.signup(
                password,
                enrolRecovery = true,
                onProofSolved = onProofSolved,
                // ADR-0074 - the session names this device from its first
                // credential (the device seed for the new account is minted
                // here; the ensure further down finds it).
                deviceIdFor = { accountId -> landingDeviceId(base64Encode(accountId)) },
            )
            try {
                val accountIdB64 = base64Encode(result.accountId)
                // UX section 17 - the sign-up is a fresh-auth proof (so the
                // passkey offered during onboarding asks for nothing more).
                freshAuth.put(
                    accountIdB64,
                    result.freshGrant,
                    result.freshGrantExpiresAtMs,
                    System.currentTimeMillis(),
                )
                sessionLandingKind = SessionLanding.Password
                // Claim the username FIRST, before anything is persisted on
                // this device (web order): the login handle at auth, then the
                // name at discovery (ADR-0054 S5 claim order). Without this binding
                // nobody can find the account by name — no contact request,
                // no add-to-group, no sign-in on another device. A taken name
                // fails the signup with the form's message; the OPAQUE account
                // then exists server-side with no username, which web accepts
                // too (the user retries with another name).
                val normalisedUsername = claimUsernameAtSignup(
                    directory = accountDirectory,
                    binder = loginHandleBinder,
                    handles = loginHandleSource,
                    sessionToken = result.sessionToken,
                    accountId = result.accountId,
                    username = trimmed,
                )
                // The claim above bound the handle for this session; the
                // session landing below need not repeat it.
                loginHandleRebinder.markBound(result.sessionToken, normalisedUsername)
                // ADR-0076 - the name goes into the own-username register
                // once the synced state is open.
                pendingUsernameWrite.set(accountIdB64 to normalisedUsername)
                sessionStore.save(
                    PersistedSession(
                        accountIdB64 = accountIdB64,
                        username = normalisedUsername,
                    ),
                )
                val deviceRootKey =
                    deriveDeviceRootKey(result.exportKey, accountIdB64)
                try {
                    deviceKeyStore.putDeviceKey(
                        context = applicationContext,
                        accountIdB64 = accountIdB64,
                        rootKey = deviceRootKey,
                    )
                } finally {
                    deviceRootKey.fill(0)
                }
                // Phase 2a2c2 — cache the OPAQUE export key under
                // EncryptedSharedPreferences so a future sign-
                // in-with-passkey on the SAME device can
                // reconstruct the full IdentitySecrets without
                // going through the password round-trip. Best-
                // effort.
                try {
                    ExportKeyStore.putExportKey(
                        applicationContext,
                        accountIdB64,
                        result.exportKey,
                    )
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "export-key persist (signup) failed", cause)
                }
                // Phase 2a3b0 (ADR-0007) — ensure a random
                // per-device DeviceIdentity seed exists on this
                // device. Distinct from the deterministic
                // AccountIdentity above; used as the MLS leaf
                // in the user's DeviceGroup (2a3a) once the
                // add-device flow lands. Best-effort.
                try {
                    DeviceIdentityStore.ensureDeviceIdentitySeed(
                        applicationContext,
                        accountIdB64,
                    )
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "device-identity ensure (signup) failed", cause)
                }
                // ADR-0017: the canonical AccountIdentity is now
                // derived from the stable BIP39 recovery-phrase seed,
                // NOT the password export key — so a future recover /
                // password change preserves the identity. We need the
                // phrase here, so fail loud BEFORE deriving the seed
                // if it is absent (signup always enrols recovery; a
                // missing phrase is a code-path bypass we must not
                // navigate the user past).
                val phrase = result.recoveryPhrase ?: throw IllegalStateException(
                    "internal: signup completed without a recovery phrase",
                )
                // ADR-0017: derive the 64-byte AccountIdentity (phrase)
                // seed and cache it under EncryptedSharedPreferences —
                // same wrapping as the export-key cache — so a same-
                // device login / passkey-resume can rebuild the
                // canonical identity without re-entering the phrase.
                // Best-effort cache: a persistence failure just means a
                // future fresh-resume on this device falls into the
                // "needs recovery phrase" fail-safe.
                val accountIdentitySeed = RecoveryMnemonic.fromPhrase(phrase).use {
                    it.toSeed()
                }
                try {
                    AccountIdentitySeedStore.putAccountIdentitySeed(
                        applicationContext,
                        accountIdB64,
                        accountIdentitySeed,
                    )
                } catch (cause: Throwable) {
                    Log.w(
                        "lakko-auth",
                        "account-identity-seed persist (signup) failed",
                        cause,
                    )
                }
                // The device holds the keys, so it holds the phrase.
                DevicePhrase.store(applicationContext, accountIdB64, phrase, DevicePhrase.LandingPath.Signup)
                // Publish the identity bundle from the canonical (phrase-seeded)
                // AccountIdentity — the FIRST publish of this key, so it carries
                // the ADR-0019 genesis self-signature (+ the ADR-0036 attestation
                // when mintable). Fail loud, BEFORE the session lands: an account
                // whose bundle is missing cannot be verified by any peer, nor by
                // its own next device (`provideRecoveryPhrase` refuses on a 404).
                // Web does the same. The KeyPackage is NOT published here —
                // `ChatRepository.republishKeyPackage` does that from the
                // long-lived Identity on the chat bootstrap this landing triggers.
                // The provider serves the signup's pair from here on (the
                // session lands right after; the install below repeats it).
                installSessionPair(accountIdB64, result.session)
                try {
                    publishIdentityFor(
                        landing = IdentityLanding.SIGNUP,
                        accountIdB64 = accountIdB64,
                        accountId = result.accountId,
                        accountIdentitySeed = accountIdentitySeed,
                    )
                } catch (cause: Throwable) {
                    // The seed never reaches the live session on this path.
                    accountIdentitySeed.fill(0)
                    throw cause
                }
                // ADR-0075 D2 - the phrase is NOT shown at sign-up. It is
                // stored on this device as before; what the person owes is
                // the "Protect your account" step (UX 19.1a), up before the
                // session lands and recorded so a restart shows it again.
                protectionStore.setPhraseSaved(accountIdB64, false)
                protectionStore.setProtectStepOwed(accountIdB64, true)
                // Stash the export key + the phrase seed for
                // ContactsClient / future ChatClient + the like. The
                // bytes survive until logout / forgetDevice clears the
                // flow; signup is a one-shot ceremony so we don't
                // expect any surprises here.
                installIdentitySecrets(
                    accountIdB64,
                    IdentitySecrets(
                        accountId = result.accountId,
                        exportKey = result.exportKey.copyOf(),
                        accountIdentitySeed = accountIdentitySeed,
                        session = result.session,
                    ),
                )
                markSignedInNow(accountIdB64)
                mutableState.value = AuthState.Session(
                    account = SessionAccount(
                        accountIdB64 = accountIdB64,
                        username = normalisedUsername,
                    ),
                )
                // 1.20d — best-effort push subscribe. Errors are
                // logged inside the subscriber + never thrown
                // past the auth flow.
                subscribePushBestEffort("signup")
                // Phase 2a1c — post-signup backup bootstrap: posts a
                // baseline /backup envelope. Best-effort; failures log
                // and continue.
                bootstrapBackupBestEffort(
                    accountId = result.accountId,
                    accountIdB64 = accountIdB64,
                    exportKey = result.exportKey,
                    sessionToken = result.sessionToken,
                    phrase = phrase,
                    context = "signup",
                )
                SignupOutcome(accountIdB64 = accountIdB64)
            } finally {
                // The original OPAQUE export key buffer drops out
                // of scope here. We already cloned it into
                // `mutableIdentitySecrets` above; the wipe here
                // affects the AuthClient's buffer only.
                result.exportKey.fill(0)
            }
        }
    }

    /**
     * ADR-0032 guest join — create a FULL account with NO username and no
     * user-chosen password, so an anonymous `/c/<slug>` visitor can join a
     * call without a login. Mirror of `apps/web/src/auth/AuthContext.tsx::signupGuest`.
     *
     * Reuses the [signup] machinery VERBATIM: the SAME PoW-gated OPAQUE
     * registration ([AuthClient.signup] with a RANDOM 32-byte password the
     * user never sees) + device-root-key derivation + the AccountIdentity-seed
     * cache + the first-ever identity-bundle publish (with the genesis
     * `leafAuth`) — but SKIPS the username claim, yielding a username-less
     * account. The recovery phrase is stored on the device and NOT shown
     * (ADR-0075 D2); the session lands at once through the shared completion
     * ([confirmFreshPhraseSaved]).
     *
     * SECURITY: guest creation routes through the SAME PoW-gated signup — no
     * bypass. The password is 32 random bytes; recovery is phrase-only,
     * matching the ACCESS-only convention the recovery cascade already uses.
     * No username is claimed; the account is username-less until a later
     * "claim your username" onboarding step.
     *
     * @return the new account (no phrase is handed to the screen).
     */
    public suspend fun signupGuest(onProofSolved: () -> Unit = {}): SignupOutcome {
        // 32 random bytes the user NEVER sees — recovery is phrase-only.
        val randomPassword = ByteArray(32).also { SecureRandom().nextBytes(it) }
        return authClientFactory().use { client ->
            // Same PoW-gated OPAQUE registration signup uses — no bypass.
            val result = client.signup(
                randomPassword,
                enrolRecovery = true,
                onProofSolved = onProofSolved,
                deviceIdFor = { accountId -> landingDeviceId(base64Encode(accountId)) },
            )
            try {
                val accountIdB64 = base64Encode(result.accountId)
                freshAuth.put(
                    accountIdB64,
                    result.freshGrant,
                    result.freshGrantExpiresAtMs,
                    System.currentTimeMillis(),
                )
                sessionLandingKind = SessionLanding.Other
                // ADR-0032: NO username claim — the account stays username-less
                // until a later onboarding step.
                val phrase = result.recoveryPhrase ?: throw IllegalStateException(
                    "internal: guest signup completed without a recovery phrase",
                )
                // ADR-0017: the canonical AccountIdentity (phrase) seed.
                val accountIdentitySeed = RecoveryMnemonic.fromPhrase(phrase).use {
                    it.toSeed()
                }
                // Device root key from the OPAQUE export key (the guest HAS a
                // real, random password, so this is the normal signup
                // derivation) + persist key / export key / device-identity /
                // account-identity seed, exactly as signup does.
                val deviceRootKey = deriveDeviceRootKey(result.exportKey, accountIdB64)
                try {
                    deviceKeyStore.putDeviceKey(
                        context = applicationContext,
                        accountIdB64 = accountIdB64,
                        rootKey = deviceRootKey,
                    )
                } finally {
                    deviceRootKey.fill(0)
                }
                try {
                    ExportKeyStore.putExportKey(applicationContext, accountIdB64, result.exportKey)
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "export-key persist (signupGuest) failed", cause)
                }
                try {
                    DeviceIdentityStore.ensureDeviceIdentitySeed(applicationContext, accountIdB64)
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "device-identity ensure (signupGuest) failed", cause)
                }
                try {
                    AccountIdentitySeedStore.putAccountIdentitySeed(
                        applicationContext,
                        accountIdB64,
                        accountIdentitySeed,
                    )
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "account-identity-seed persist (signupGuest) failed", cause)
                }
                DevicePhrase.store(applicationContext, accountIdB64, phrase, DevicePhrase.LandingPath.SignupGuest)
                // Persist the account marker — username EMPTY (unclaimed).
                sessionStore.save(PersistedSession(accountIdB64 = accountIdB64, username = ""))
                // G-30 #2 (ADR-0019): publish the FIRST-EVER bundle WITH the
                // genesis leaf self-signature so the transparency-log append
                // accepts the new binding — identical to signup. Fail-loud: a
                // mint / upload failure rejects the guest signup rather than
                // parking an unverifiable identity.
                installSessionPair(accountIdB64, result.session)
                publishIdentityFor(
                    landing = IdentityLanding.SIGNUP,
                    accountIdB64 = accountIdB64,
                    accountId = result.accountId,
                    accountIdentitySeed = accountIdentitySeed,
                )
                // ADR-0075 D2 - the phrase is not shown at sign-up, a guest's
                // included: it stays stored on this device and the person
                // may save one later in Settings (the no-protection banner
                // says so). The session is persisted and landed at once
                // through the shared completion ([confirmFreshPhraseSaved]),
                // which owns the seed from here on.
                persistSessionPairBestEffort(accountIdB64, result.session)
                installSessionPair(accountIdB64, result.session)
                protectionStore.setPhraseSaved(accountIdB64, false)
                pendingFreshPhrase.set(
                    PendingFreshPhrase(
                        seed = accountIdentitySeed,
                        accountIdBytes = result.accountId,
                        accountIdB64 = accountIdB64,
                        session = result.session,
                        username = "",
                        phrase = phrase,
                        // A guest signup published its genesis bundle above;
                        // nothing is owed.
                        publishOwed = false,
                    ),
                )
                mutablePublishOwedReason.value = null
                confirmFreshPhraseSaved()
                SignupOutcome(accountIdB64 = accountIdB64)
            } finally {
                result.exportKey.fill(0)
            }
        }
    }

    /**
     * Drive the full OPAQUE-PAKE login flow.
     *
     * Mirror of `apps/web/src/auth/AuthContext.tsx::login`. Steps:
     *   1. Derive the login handle of the typed username (ADR-0054 S5).
     *      No discovery lookup runs: the account id comes back from
     *      `/login/finish`. An unregistered username fails as a wrong
     *      password (the server takes its ordinary path on a stand-in).
     *   2. Call [AuthClient.login] with `(handle, password)`; afterwards
     *      re-bind the handle in the background (idempotent).
     *   3. Re-derive the device root key from the OPAQUE export
     *      key. The derivation is deterministic for the same
     *      password, so re-running it on every login produces the
     *      same bytes the original signup wrote — the
     *      [DeviceKeyStore.putDeviceKey] upsert is harmless.
     *   4. Re-save the SessionStore marker (idempotent — same
     *      `accountId`, same `username`).
     *   5. Flip state to [AuthState.Session].
     *
     * `session_key` from the OPAQUE finish is dropped on the floor
     * in 1.15d5. When the auth server gains token-bound sessions
     * (post-1.18 sprint on the wider service mesh) we'll feed it
     * into a [SessionStore]-adjacent token cache.
     *
     * @throws AuthClientArgError on empty username / empty password
     * @throws AuthHttpError 401 on wrong password / unknown account
     * @throws AuthNetworkError on transport failure
     */
    public suspend fun login(
        username: String,
        password: ByteArray,
    ) {
        val trimmed = username.trim()
        if (trimmed.isEmpty()) {
            throw AuthClientArgError("username must be non-empty")
        }
        if (password.isEmpty()) {
            throw AuthClientArgError("password must be non-empty")
        }
        // ADR-0054 S5 - the request names the account by the login handle of
        // the typed name; the account id is learned from the finish. The
        // ADR-0017 identity acquisition below is unchanged, so a device with
        // no cached phrase seed still lands in NeedsRecoveryPhrase.
        authClientFactory().use { client ->
            val (named, result) = signInByName(
                cached = null,
                typed = trimmed,
                source = loginHandleSource,
                // ADR-0074 - name this device at the landing when the device
                // already knows the account behind the typed name; else bind
                // right after the answer.
                call = { client.login(it.handle, password, landingDeviceId(candidateAccount(trimmed))).bound() },
                sessionTokenOf = { it.sessionToken },
                rebind = ::rebindLoginHandleInBackground,
            )
            val accountIdBytes = result.accountId
            val accountIdB64 = base64Encode(accountIdBytes)
            val resolvedUsername = named.username
            // UX section 17 - a password sign-in is a fresh-auth proof;
            // kept in memory only.
            freshAuth.put(
                accountIdB64,
                result.freshGrant,
                result.freshGrantExpiresAtMs,
                System.currentTimeMillis(),
            )
            sessionLandingKind = SessionLanding.Password
            try {
                // Re-derive + upsert the device key. Deterministic
                // for the same password — the on-disk envelope is
                // overwritten with byte-equivalent content; harmless
                // unless the account was previously recovered via
                // mnemonic with a new password, in which case the
                // old envelope decrypts to the previous device key
                // and the new envelope replaces it cleanly.
                val deviceRootKey = deriveDeviceRootKey(result.exportKey, accountIdB64)
                try {
                    deviceKeyStore.putDeviceKey(
                        context = applicationContext,
                        accountIdB64 = accountIdB64,
                        rootKey = deviceRootKey,
                    )
                } finally {
                    deviceRootKey.fill(0)
                }
                // Phase 2a2c2 — refresh the cached export key
                // under EncryptedSharedPreferences. Best-effort.
                try {
                    ExportKeyStore.putExportKey(
                        applicationContext,
                        accountIdB64,
                        result.exportKey,
                    )
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "export-key persist (login) failed", cause)
                }
                // Phase 2a3b0 (ADR-0007) — ensure DeviceIdentity
                // seed. Idempotent: a returning user with a
                // seed already on file gets the cached one;
                // first-login generates fresh bytes.
                try {
                    DeviceIdentityStore.ensureDeviceIdentitySeed(
                        applicationContext,
                        accountIdB64,
                    )
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "device-identity ensure (login) failed", cause)
                }
                // Upsert the marker with the account the finish named, so a
                // cross-device first sign-in leaves the device knowing whose
                // account it holds.
                sessionStore.save(
                    PersistedSession(accountIdB64 = accountIdB64, username = resolvedUsername),
                )

                // ADR-0017: the canonical AccountIdentity is seeded
                // from the BIP39 phrase seed, NOT the export key. Read
                // the cached seed (warm same-device path). If present,
                // finish the session with it. If ABSENT (fresh device,
                // or a device that only ever held the export key), we
                // MUST NOT (a) derive the identity from the export key
                // — that is the wrong identity now — nor (b) silently
                // mint a fresh one — that breaks every contact's pin.
                // Instead we flip to the NeedsRecoveryPhrase fail-safe
                // state and let the user supply their phrase.
                val accountIdentitySeed: ByteArray? = try {
                    AccountIdentitySeedStore.getAccountIdentitySeed(
                        applicationContext,
                        accountIdB64,
                    )
                } catch (cause: Throwable) {
                    // Decrypt / store failure — treat as "no cached
                    // seed" and fall into the fail-safe rather than the
                    // wrong-identity branch.
                    Log.w(
                        "lakko-auth",
                        "account-identity-seed read failed (login)",
                        cause,
                    )
                    null
                }

                if (accountIdentitySeed == null) {
                    // Fresh-device fail-safe (ADR-0017). Auth succeeded;
                    // identity restore is pending the phrase. Stash the
                    // context + flip state — provideRecoveryPhrase
                    // finishes from here.
                    pendingRecovery.set(
                        PendingRecovery(
                            accountIdBytes = accountIdBytes,
                            accountIdB64 = accountIdB64,
                            exportKey = result.exportKey.copyOf(),
                            session = result.session,
                            username = resolvedUsername,
                        ),
                    )
                    installPendingSessionPair(accountIdB64, result.session)
                    markSignedInNow(accountIdB64)
                    mutableState.value = AuthState.NeedsRecoveryPhrase(
                        account = SessionAccount(
                            accountIdB64 = accountIdB64,
                            username = resolvedUsername,
                        ),
                    )
                    // 1.20d — best-effort push subscribe (the auth
                    // factor succeeded, so the device is enrolled).
                    subscribePushBestEffort("login")
                    return@use
                }

                // Stash the export key + the phrase seed + session
                // token for ContactsClient / DiscoveryClient.
                installIdentitySecrets(
                    accountIdB64,
                    IdentitySecrets(
                        accountId = accountIdBytes,
                        exportKey = result.exportKey.copyOf(),
                        accountIdentitySeed = accountIdentitySeed,
                        session = result.session,
                    ),
                )
                markSignedInNow(accountIdB64)
                mutableState.value = AuthState.Session(
                    account = SessionAccount(
                        accountIdB64 = accountIdB64,
                        username = resolvedUsername,
                    ),
                )
                // ADR-0073 section 4.2 - read the directory before any write:
                // stale keys land the signed-out screen; our key is
                // re-published. Before this, a password login never
                // published the bundle, so an account whose bundle was missing
                // stayed unverifiable however often its owner signed in.
                refreshPublishedIdentityInBackground(
                    landing = IdentityLanding.SIGN_IN,
                    accountIdB64 = accountIdB64,
                    accountIdBytes = accountIdBytes,
                    accountIdentitySeed = accountIdentitySeed,
                )
                // 1.20d — best-effort push subscribe.
                subscribePushBestEffort("login")
            } finally {
                // Best-effort wipe of the original buffers — the
                // identity-secrets clone above survives in
                // `mutableIdentitySecrets` until logout.
                result.exportKey.fill(0)
                result.sessionKey.fill(0)
            }
        }
    }

    /**
     * Drive the full recover-from-mnemonic flow.
     *
     * Mirror of `apps/web/src/auth/AuthContext.tsx::recover`.
     * Drives `lakko-svc-auth`'s `/password/reset/{start,finish}`
     * endpoint pair via [AuthClient.recover]:
     *
     *   1. Validate caller-side preconditions (cached account
     *      present, mnemonic + new password non-empty, new
     *      password ≥ 8 chars; mnemonic checksum is validated
     *      inside `RecoveryMnemonic.fromPhrase` and surfaces as a
     *      generic "invalid recovery phrase" error).
     *   2. Parse the typed phrase via [RecoveryMnemonic.fromPhrase]
     *      to validate the BIP39 checksum and derive the 64-byte
     *      seed. The OPAQUE recovery flow consumes the seed as
     *      its "secret"; a wrong word or a checksum failure
     *      surfaces here before the network call.
     *   3. Call [AuthClient.recover] with `(accountId, seed,
     *      newPasswordBytes)` — no proof of work, parallel OPAQUE
     *      recovery-login + new-password-registration round.
     *   4. Re-derive the device root key from the **new** export
     *      key. The previous key (if any) is no longer reachable
     *      from the new password; the upsert overwrites the on-
     *      disk envelope cleanly.
     *   5. Idempotent re-save of the [SessionStore] marker
     *      (`accountId` unchanged, `username` unchanged).
     *   6. Flip state to [AuthState.Session].
     *
     * A wrong mnemonic surfaces as an [AuthHttpError] from
     * `AuthClient.recover` — by design the failure is
     * indistinguishable from a transport-mangled response (OPAQUE
     * AKE refuses to agree on a session key, the server returns
     * a generic error code). We let it bubble unchanged so the
     * UI form can render "invalid recovery phrase" without
     * leaking further.
     *
     * @throws AuthClientArgError on empty / too-short inputs or
     *         no cached account on this device
     * @throws me.lakko.app.auth.FfiException via [RecoveryMnemonic.fromPhrase]
     *         when the BIP39 checksum fails
     * @throws AuthHttpError on a non-2xx server response or a
     *         rejected mnemonic
     * @throws AuthNetworkError on transport failure
     */
    public suspend fun recover(
        mnemonicPhrase: String,
        newPassword: ByteArray,
        username: String? = null,
    ) {
        if (mnemonicPhrase.isBlank()) {
            throw AuthClientArgError("recovery phrase must be non-empty")
        }
        if (newPassword.isEmpty()) {
            throw AuthClientArgError("new password must be non-empty")
        }
        // ADR-0054 S5 - the recover names the account by the login handle
        // of the typed name, or of the name cached on this device when the
        // field is blank; the account id is learned from the reset finish.
        // No discovery lookup runs before the session. (The old export key
        // the G-4 path used to capture here needed the account id before the
        // call; under ADR-0017 it was only ever wiped, so it is gone.)
        val cached = sessionStore.load()?.let {
            CachedAccount(accountIdB64 = it.accountIdB64, username = it.username)
        }
        val named = resolveNamedSignIn(cached, username, loginHandleSource)

        // Parse the mnemonic up front. `fromPhrase` collapses
        // unknown-word and bad-checksum into one FfiException by
        // design (preserves BIP39's no-probing property). Burn
        // the seed buffer at the end — it's the only thing
        // standing between an attacker and the OPAQUE export key
        // we'll derive next.
        // The canonical form (12 lower-case words, single spaces) is what the
        // device stores once the recover has landed.
        val (canonicalPhrase, seed) = RecoveryMnemonic.fromPhrase(mnemonicPhrase).use { mnemonic ->
            mnemonic.phrase() to mnemonic.toSeed()
        }
        // ADR-0017: the canonical AccountIdentity is derived from this
        // BIP39 phrase seed. `seed` itself is zeroed in the `finally`
        // (it doubles as the OPAQUE recovery input), so keep a private
        // copy for the identity cache + in-memory secrets, which must
        // outlive this call.
        val accountIdentitySeed = seed.copyOf()
        // Tracks whether `accountIdentitySeed` was handed into the live
        // IdentitySecrets (which owns its lifetime thereafter). When it
        // was NOT (an early throw), the `finally` wipes our copy.
        var seedHandedOff = false
        try {
            authClientFactory().use { client ->
                val result = signInByName(
                    named = named,
                    call = {
                        client.recover(it.handle, seed, newPassword, landingDeviceId(candidateAccount(named.username)))
                            .bound()
                    },
                    sessionTokenOf = { it.sessionToken },
                    rebind = ::rebindLoginHandleInBackground,
                )
                val accountIdBytes = result.accountId
                val accountIdB64 = base64Encode(accountIdBytes)
                val resolvedUsername = named.username
                try {
                    val deviceRootKey =
                        deriveDeviceRootKey(result.exportKey, accountIdB64)
                    try {
                        deviceKeyStore.putDeviceKey(
                            context = applicationContext,
                            accountIdB64 = accountIdB64,
                            rootKey = deviceRootKey,
                        )
                    } finally {
                        deviceRootKey.fill(0)
                    }
                    // Phase 2a2c2 — refresh the cached export key
                    // under the freshly-rotated device key. Any
                    // prior cached value was bound to the OLD
                    // password; the upsert here overwrites it
                    // cleanly. Best-effort.
                    try {
                        ExportKeyStore.putExportKey(
                            applicationContext,
                            accountIdB64,
                            result.exportKey,
                        )
                    } catch (cause: Throwable) {
                        Log.w("lakko-auth", "export-key persist (recover) failed", cause)
                    }
                    // ADR-0017: cache the AccountIdentity (phrase) seed
                    // under the freshly-rotated device key. This is
                    // what makes the warm same-device path (and a later
                    // passkey resume) rebuild the SAME canonical
                    // identity without re-entering the phrase.
                    // Best-effort.
                    try {
                        AccountIdentitySeedStore.putAccountIdentitySeed(
                            applicationContext,
                            accountIdB64,
                            accountIdentitySeed,
                        )
                    } catch (cause: Throwable) {
                        Log.w(
                            "lakko-auth",
                            "account-identity-seed persist (recover) failed",
                            cause,
                        )
                    }
                    // The device holds the keys, so it holds the phrase. The
                    // recover proved the phrase is the account's (the
                    // recovery credential accepted it).
                    DevicePhrase.store(
                        applicationContext,
                        accountIdB64,
                        canonicalPhrase,
                        DevicePhrase.LandingPath.Recover,
                    )
                    // Phase 2a3b0 (ADR-0007) — DeviceIdentity
                    // seed is decoupled from the OPAQUE
                    // password, so recovery does NOT rotate it.
                    // `ensureDeviceIdentitySeed` returns the
                    // existing seed when present and only
                    // generates fresh bytes if the row is
                    // missing.
                    try {
                        DeviceIdentityStore.ensureDeviceIdentitySeed(
                            applicationContext,
                            accountIdB64,
                        )
                    } catch (cause: Throwable) {
                        Log.w("lakko-auth", "device-identity ensure (recover) failed", cause)
                    }
                    // Persist the account marker — important on the
                    // fresh-device recover path where there was no
                    // cached row (idempotent upsert otherwise).
                    sessionStore.save(
                        PersistedSession(
                            accountIdB64 = accountIdB64,
                            username = resolvedUsername,
                        ),
                    )
                    // Phase 2a1d / 2a3d7 — restore-then-bootstrap
                    // on recover.
                    //
                    // The user has just gone through recovery,
                    // which lands on a device that may or may
                    // not have been used by them before. If a
                    // prior backup envelope sits on the server,
                    // we fetch + decrypt + parse it before
                    // posting a fresh baseline.
                    //
                    // **Ordering matters:** the restore +
                    // snapshot-write to [IdentitySnapshotStore]
                    // MUST happen BEFORE the
                    // `mutableIdentitySecrets.value = ...` flip
                    // below. The compose-side
                    // `LaunchedEffect(identitySecrets)` in
                    // `MainActivity` wakes on the flip and
                    // immediately drives
                    // `chatRepository.bootstrap(secrets)`, which
                    // reads `snapshotStore.getSnapshot(...)` for
                    // the MLS state. If we restored AFTER the
                    // flip, that read would race against the
                    // write and the recovered device would land
                    // with no MLS state. Doing it before is the
                    // only way to make the rehydrate path see
                    // the restored snapshot on the very first
                    // ChatRepository.bootstrap call.
                    //
                    // On Android the application of restored
                    // contacts into [ContactsRepository] is not
                    // wired yet — the contacts list is in-
                    // memory only and there's no persistent
                    // target to upsert into. The helper logs the
                    // parsed-contact count for now and the merge
                    // layer lands alongside the Android contact-
                    // persistence sub-sprint.
                    // ADR-0050 — a recovered device holds the identity and is
                    // admitted to the account's chats by its other devices or its
                    // contacts; the chat list says "Synchronizing..." until the
                    // first chat arrives.
                    me.lakko.app.devices.StateBundle.markRestored(applicationContext, accountIdB64)
                    val restoredBackup = restoreFromServerBackupBestEffort(
                        sessionToken = result.sessionToken,
                        accountId = accountIdBytes,
                        accountIdB64 = accountIdB64,
                        phrase = mnemonicPhrase,
                        context = "recover",
                    )
                    val restoredSnapshot = restoredBackup.snapshot
                    // Written BEFORE the bootstrap below, because that is what
                    // reads the set to rebuild `Group` handles. Additive: a
                    // group this device already knew is not dropped by a
                    // backup that predates it.
                    val knownGroups = me.lakko.app.chat.KnownGroupsStore(applicationContext)
                    for (groupIdHex in restoredBackup.knownGroupIdsHex) {
                        knownGroups.add(accountIdB64, groupIdHex)
                    }
                    if (restoredSnapshot.isNotEmpty()) {
                        // Drop the restored MLS blob into the
                        // local snapshot store. ChatRepository's
                        // rehydrate path picks it up
                        // automatically — no wiring change
                        // needed beyond this single putSnapshot
                        // call.
                        try {
                            me.lakko.app.chat.IdentitySnapshotStore.putSnapshot(
                                applicationContext,
                                accountIdB64,
                                restoredSnapshot,
                            )
                        } catch (cause: Throwable) {
                            Log.w(
                                "lakko-auth",
                                "post-restore IdentitySnapshotStore.putSnapshot failed",
                                cause,
                            )
                        }
                    }

                    // ADR-0040 (G-35) — purge any durable pending-commit
                    // outbox for this account BEFORE the fresh
                    // `chatRepository.bootstrap` sweep runs, so a prior
                    // session's un-acked commits (staged against a now-replaced
                    // snapshot) are never re-published against the restored
                    // state. Best-effort; runs whether or not a server backup
                    // was restored.
                    try {
                        me.lakko.app.chat.CommitOutboxStore.deleteForAccount(
                            applicationContext,
                            accountIdB64,
                        )
                    } catch (cause: Throwable) {
                        Log.w(
                            "lakko-auth",
                            "recover: CommitOutboxStore.deleteForAccount failed",
                            cause,
                        )
                    }

                    // Stash the NEW export key + the phrase seed.
                    // ADR-0017: the phrase seed is UNCHANGED across a
                    // recover, so the canonical AccountIdentity rebuilt
                    // from it is byte-for-byte the same as before —
                    // which is the whole point: contacts never re-verify.
                    installIdentitySecrets(
                        accountIdB64,
                        IdentitySecrets(
                            accountId = accountIdBytes,
                            exportKey = result.exportKey.copyOf(),
                            accountIdentitySeed = accountIdentitySeed,
                            session = result.session,
                        ),
                    )
                    markSignedInNow(accountIdB64)
                    // The live IdentitySecrets now owns the seed buffer
                    // — do NOT wipe it in the `finally`.
                    seedHandedOff = true
                    mutableState.value = AuthState.Session(
                        account = SessionAccount(
                            accountIdB64 = accountIdB64,
                            username = resolvedUsername,
                        ),
                    )
                    // Clear any pending fresh-device fail-safe —
                    // recover just restored the identity directly.
                    pendingRecovery.set(null)
                    mutableRestoreNotice.value = null
                    // A new password was just chosen; nothing is owed.
                    mutableNewPasswordOwed.value = false

                    // G-4 (ADR-0016) emit path — now DORMANT per
                    // ADR-0017. With a phrase-seeded AccountIdentity, a
                    // routine recover does NOT rotate the account key:
                    // the phrase is unchanged, so the new account
                    // signature pubkey EQUALS the old one. We compare
                    // them explicitly and, when equal (the only case on
                    // a routine recover), SKIP the supersession proof
                    // mint + upload entirely and re-publish the bundle
                    // proof-less. "old == new account pubkey => skip" is
                    // the load-bearing ADR-0017 emit invariant.
                    republishBundleAfterRecover(
                        accountIdB64 = accountIdB64,
                        accountId = accountIdBytes,
                        accountIdentitySeed = accountIdentitySeed,
                    )

                    // Phase 2a1c — post-recover backup
                    // bootstrap: post a
                    // baseline /backup envelope that re-publishes
                    // the restored snapshot (so the next
                    // device-recover sees the same state we just
                    // rebuilt). Best-effort.
                    bootstrapBackupBestEffort(
                        accountId = accountIdBytes,
                        accountIdB64 = accountIdB64,
                        exportKey = result.exportKey,
                        sessionToken = result.sessionToken,
                        phrase = mnemonicPhrase,
                        context = "recover",
                        wholeStorageSnapshot = restoredSnapshot,
                        // ADR-0058 — re-publish the restored folded state too,
                        // or this baseline would drop it from the backup.
                        syncedState = restoredBackup.syncedState,
                    )
                    // 1.20d — best-effort push subscribe.
                    subscribePushBestEffort("recover")
                } finally {
                    result.exportKey.fill(0)
                }
            }
        } finally {
            // BIP39 seed is the highest-value secret we touch in
            // this flow — wipe before GC even thinks about it.
            seed.fill(0)
            // ADR-0017: wipe the private phrase-seed copy ONLY when it
            // was not handed into the live IdentitySecrets (an early
            // throw). On success the IdentitySecrets owns it and
            // clearIdentitySecrets() wipes it on logout / forgetDevice.
            if (!seedHandedOff) {
                accountIdentitySeed.fill(0)
            }
        }
    }

    /**
     * ADR-0018 factor (c) — recover ACCESS with a TOTP authenticator
     * code. Mirror of `apps/web/src/auth/AuthContext.tsx::recoverWithTotp`.
     *
     * ACCESS only: the server verifies the 6-digit code against the
     * previously-enrolled secret and issues the session triple. There
     * is NO password, hence NO OPAQUE export key — `exportKey` is the
     * EMPTY array on every path here (the [completeFromDeviceRestore]
     * convention). Identity acquisition runs the ADR-0017 state
     * machine:
     *
     *   * **Warm same-device case** — a cached AccountIdentity
     *     (phrase) seed exists: finish the session directly. ALSO
     *     ensure the [DeviceKeyStore] rehydration existence-gate
     *     passes — with no password there is no export key to derive
     *     a device root key from, so when none exists we bootstrap it
     *     from the identity seed (best-effort; on Android nothing is
     *     wrapped under the device key, it is purely the gate).
     *   * **Cold fresh-device case** — no cached seed: park a
     *     [PendingRecovery] (exportKey EMPTY) and flip to
     *     [AuthState.NeedsRecoveryPhrase]; [provideRecoveryPhrase] /
     *     [completeFromPairing] complete the identity restore (and
     *     bootstrap the device key from the VERIFIED seed).
     *
     * @throws AuthClientArgError on a blank username, a name failing the
     *         username rule, or a malformed code. An unknown name is not
     *         told apart: it fails as a wrong code (ADR-0054 S5).
     * @throws AuthHttpError 401 on a wrong/expired/replayed code
     *         (enumeration-safe), 429 under lockout, 503 when TOTP is
     *         disabled server-side.
     * @throws AuthNetworkError on transport failure.
     */
    public suspend fun recoverWithTotp(username: String, code: String) {
        val trimmed = username.trim()
        if (trimmed.isEmpty()) {
            throw AuthClientArgError(
                "enter the username of the account to recover",
                me.lakko.app.l10n.UiText("recovery.username_required_to_recover"),
            )
        }
        // ADR-0054 S5 - the code goes with the login handle of the name; the
        // account id comes back in the response. No lookup before a session.
        val (named, result) = authClientFactory().use { client ->
            signInByName(
                cached = null,
                typed = trimmed,
                source = loginHandleSource,
                call = { client.totpRecover(it.handle, code, landingDeviceId(candidateAccount(trimmed))).bound() },
                sessionTokenOf = { it.sessionToken },
                rebind = ::rebindLoginHandleInBackground,
            )
        }
        val accountIdBytes = result.accountId
        val accountIdB64 = base64Encode(accountIdBytes)
        val resolvedUsername = named.username

        mutableRestoreNotice.value = null
        // An authenticator code signs in and proves nothing more (UX 17.1):
        // a proof held for another account goes, none is minted.
        freshAuth.onSignInWithoutProof(accountIdB64)
        sessionLandingKind = SessionLanding.Authenticator
        // A forgot-password route: the new-password step is owed once this
        // device holds the keys.
        mutableNewPasswordOwed.value = true
        // Shared ADR-0018 ACCESS-only landing (warm cached-seed finish
        // OR cold fail-safe park).
        landAfterAccessRecovery(
            username = resolvedUsername,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            session = result.session,
            pushContext = "totp-recover",
        )
    }

    // -----------------------------------------------------------------
    // Three words from the phrase, checked on this device
    // -----------------------------------------------------------------

    /**
     * The account "Three words from your phrase" can sign in on this device,
     * or `null` when the route is not offered here. A LOCAL fact
     * ([offersThreeWords]): an account is cached on this device with a
     * username, the device holds both its recovery phrase and its keys, and
     * the route is not suspended.
     * No server is asked, so the answer leaks nothing about any account.
     * Opens Keystore-backed stores: call it off the main thread.
     */
    public fun threeWordsAccount(): ThreeWordsAccount? =
        threeWordsContext()?.let { ThreeWordsAccount(it.accountIdB64, it.username) }

    private class ThreeWordsContext(val accountIdB64: String, val username: String, val phrase: String)

    private fun threeWordsContext(): ThreeWordsContext? {
        val cached = sessionStore.load() ?: return null
        val phrase = DevicePhrase.read(applicationContext, cached.accountIdB64)
        val keysOnDevice = holdsKeys(cached.accountIdB64)
        val offered = offersThreeWords(
            cachedUsername = cached.username,
            phraseOnDevice = phrase != null,
            keysOnDevice = keysOnDevice,
            suspended = threeWordsBudgetStore.isSuspended(cached.accountIdB64),
            // ADR-0075 - only a phrase-protected account has words the
            // person was ever shown.
            phraseSaved = protectionStore.phraseSaved(cached.accountIdB64),
        )
        if (phrase == null || !offered) return null
        return ThreeWordsContext(cached.accountIdB64, cached.username, phrase)
    }

    /**
     * Whether this device "holds the keys" of [accountIdB64]: a stored
     * device key exists for the account AND a cached 64-byte AccountIdentity
     * seed is readable. Decided here for every route that asks.
     */
    private fun holdsKeys(accountIdB64: String): Boolean = try {
        val deviceKey = deviceKeyStore.getDeviceKey(applicationContext, accountIdB64)
        if (deviceKey == null) {
            false
        } else {
            deviceKey.fill(0)
            val seed = AccountIdentitySeedStore.getAccountIdentitySeed(applicationContext, accountIdB64)
            val present = seed != null && seed.size == ACCOUNT_IDENTITY_SEED_LENGTH
            seed?.fill(0)
            present
        }
    } catch (cause: Throwable) {
        Log.w("lakko-auth", "reading the keys stored on this device failed", cause)
        false
    }

    /** Whether the three-word route is suspended on this device for the
     *  signed-in account (Settings > Security > Recovery). */
    public fun threeWordsSuspended(): Boolean {
        val accountIdB64 = signedInAccountIdB64() ?: return false
        return threeWordsBudgetStore.isSuspended(accountIdB64)
    }

    /** The three-word budget of [accountIdB64] as stored on this device. */
    public fun threeWordsBudget(accountIdB64: String): ThreeWordsBudget =
        threeWordsBudgetStore.load(accountIdB64)

    /** Record one miss at [nowMs] and return the budget after it. */
    public fun recordThreeWordsMiss(accountIdB64: String, nowMs: Long): ThreeWordsBudget {
        val next = afterMiss(threeWordsBudgetStore.load(accountIdB64), nowMs)
        threeWordsBudgetStore.save(accountIdB64, next)
        return next
    }

    /**
     * Three words, checked locally like a wallet does. [positions] are the
     * three word numbers the form drew (`phraseCheckRandomPositions`) and
     * `words[i]` answers `positions[i]`. The words are compared with the
     * phrase this device holds; no server takes part in the check and none
     * stores anything about it.
     *
     * Returns `false` on wrong words (nothing is sent anywhere; the form
     * counts the miss). On a match the device signs in with what it already
     * holds: the recovery login run from the seed of the stored phrase, which
     * yields a session without replacing the password. It then lands through
     * the shared sign-in-only landing with the new-password step owed, clears
     * the budget and returns `true`.
     *
     * @throws AuthClientArgError when the route is not offered here, or the
     *   name answered for another account than the cached one.
     * @throws AuthHttpError `401` when the stored phrase is no longer the
     *   account's recovery credential, `429`, `503`.
     * @throws AuthNetworkError on a transport failure.
     */
    public suspend fun recoverWithThreeWords(positions: ByteArray, words: List<String>): Boolean {
        val found = threeWordsContext() ?: throw AuthClientArgError(
            "three words are not available on this device",
            me.lakko.app.l10n.UiText("recovery.words.error.unavailable"),
        )
        // Trims and lower-cases; a wrong or unknown word is `false`.
        val seed = RecoveryMnemonic.fromPhrase(found.phrase).use { mnemonic ->
            if (!phraseCheckWords(mnemonic, positions, words)) return false
            mnemonic.toSeed()
        }
        try {
            // The words matched. Sign in with what this device already holds;
            // the password is left as it is ("Choose a new password" follows).
            val (named, result) = authClientFactory().use { client ->
                signInByName(
                    cached = null,
                    typed = found.username,
                    source = loginHandleSource,
                    call = { named ->
                        try {
                            // This device holds the account (its phrase): the
                            // id is known before the request.
                            client.recoveryLogin(named.handle, seed, landingDeviceId(found.accountIdB64)).bound()
                        } catch (cause: Throwable) {
                            if (cause is kotlinx.coroutines.CancellationException) throw cause
                            throw afterRefusedRecoveryLogin(found.accountIdB64, cause)
                        }
                    },
                    sessionTokenOf = { it.sessionToken },
                    rebind = ::rebindLoginHandleInBackground,
                )
            }
            val accountIdB64 = base64Encode(result.accountId)
            if (accountIdB64 != found.accountIdB64) {
                // The name answered for another account than the one cached
                // here. Adopt nothing.
                throw AuthClientArgError(
                    "the recovery sign-in answered for another account",
                    me.lakko.app.l10n.UiText("recovery.words.error.rejected"),
                )
            }
            mutableRestoreNotice.value = null
            mutableNewPasswordOwed.value = true
            // The words were checked on this device a moment ago: that is
            // what unlocks "Choose a new password" (brief item 7).
            wordsCheckedAtMs[accountIdB64] = System.currentTimeMillis()
            freshAuth.onSignInWithoutProof(accountIdB64)
            sessionLandingKind = SessionLanding.Other
            landAfterAccessRecovery(
                username = named.username,
                accountIdBytes = result.accountId,
                accountIdB64 = accountIdB64,
                session = result.session,
                pushContext = "three-words",
            )
            threeWordsBudgetStore.clear(accountIdB64)
            return true
        } finally {
            seed.fill(0)
        }
    }

    /**
     * The recovery login made with the seed of the stored phrase failed with
     * [cause]. Applies [storedPhraseVerdict] and returns what to throw:
     *
     *   - refused as invalid credentials and nothing owed: the three-word
     *     route is suspended for the account on this device (a persisted
     *     marker) and its budget cleared, so the row is no longer offered.
     *     NOTHING is deleted: a refusal is not proof that the phrase is
     *     stale. The phrase store, the identity seed, the device key and
     *     the MLS state are untouched; Settings keeps showing the phrase.
     *   - anything else (offline, `429`, `503`, ...): nothing changes and
     *     [cause] goes on unchanged.
     */
    private fun afterRefusedRecoveryLogin(accountIdB64: String, cause: Throwable): Throwable =
        when (storedPhraseVerdict(recoveryLoginFailureKind(cause))) {
            StoredPhraseVerdict.Keep -> cause
            StoredPhraseVerdict.SuspendThreeWords -> {
                Log.w("lakko-auth", "the recovery sign-in with the stored phrase was refused; three words suspended")
                threeWordsBudgetStore.suspend(accountIdB64)
                ThreeWordsSuspendedError(cause)
            }
        }


    // -----------------------------------------------------------------
    // ADR-0075: the recovery email, the account's protection, its notices
    // -----------------------------------------------------------------

    private fun deviceLanguageTag(): String? =
        me.lakko.app.l10n.Localization.strings.value.language.takeIf { it.isNotBlank() }

    /** Whether this server can mail a recovery code (anonymous). `false`
     *  when it cannot be asked. */
    public suspend fun mailAvailable(): Boolean = try {
        authClientFactory().use { it.recoveryEmailAvailable() }
    } catch (cause: Throwable) {
        if (cause is kotlinx.coroutines.CancellationException) throw cause
        false
    }

    /** Read the account's mode with [sessionToken] and keep the flag; a
     *  failure leaves the flag as last learned. */
    private suspend fun learnPhraseSaved(accountIdB64: String, sessionToken: String): RecoveryEmailStatus? = try {
        authClientFactory().use { it.recoveryEmailStatus(sessionToken) }.also {
            protectionStore.setPhraseSaved(accountIdB64, it.phraseSaved)
        }
    } catch (cause: Throwable) {
        if (cause is kotlinx.coroutines.CancellationException) throw cause
        Log.w("lakko-auth", "account mode not read: ${cause.javaClass.simpleName}")
        null
    }

    /**
     * "Restore your keys" (UX section 8): `GET /recovery-email` for the
     * account that is signed in without keys, read with the parked session.
     * `null` when nothing is parked or the server could not be asked.
     */
    public suspend fun restoreKeysStatus(): RecoveryEmailStatus? {
        val pending = pendingRecovery.get() ?: return null
        return learnPhraseSaved(pending.accountIdB64, pending.sessionToken)
    }

    /** Whether new words are shown on this device after its account's keys
     *  changed (ADR-0075: only to a phrase-protected account). */
    public fun showsNewWordsHere(): Boolean = showsNewWords(phraseProtected())

    /** Whether the account on this device is phrase-protected, as last
     *  learned (a local flag: the three-word surfaces read it). */
    public fun phraseProtected(): Boolean {
        val accountIdB64 = signedInAccountIdB64()
            ?: pendingFreshPhrase.get()?.accountIdB64
            ?: sessionStore.load()?.accountIdB64
            ?: return false
        return protectionStore.phraseSaved(accountIdB64)
    }

    /** Read `GET /recovery-email` for the signed-in account and the notices
     *  waiting for this device. Best effort; every failure is retried at
     *  the next landing or when Settings opens. */
    private suspend fun refreshAccountProtection(accountIdB64: String) {
        try {
            resumeOwedRename(accountIdB64)
            refreshRecoveryEmail()
            fetchAccountNotices(accountIdB64)
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w("lakko-auth", "account protection not read: ${cause.javaClass.simpleName}")
        }
    }

    /**
     * `GET /recovery-email` for the signed-in account; updates [recoveryEmail]
     * and the local phrase-saved flag. `null` when there is no session or
     * the server could not be asked (the last answer stays).
     */
    public suspend fun refreshRecoveryEmail(): RecoveryEmailStatus? {
        // The live secrets, not the auth state: a landing publishes its
        // secrets a moment before the state flips.
        val accountIdB64 = mutableIdentitySecrets.value?.let { base64Encode(it.accountId) } ?: return null
        val token = sessionTokenProvider.current() ?: return null
        val status = try {
            authClientFactory().use { it.recoveryEmailStatus(token) }
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w("lakko-auth", "recovery email status not read: ${cause.javaClass.simpleName}")
            return null
        }
        if (!secretsAreFor(accountIdB64)) return null
        protectionStore.setPhraseSaved(accountIdB64, status.phraseSaved)
        mutableRecoveryEmail.value = status
        // The automatic re-seal is evaluated after every read.
        pushScope.launch { resealIfNeeded() }
        return status
    }

    private suspend fun fetchAccountNotices(accountIdB64: String) {
        val token = sessionTokenProvider.current() ?: return
        val cursor = noticeStore.cursor(accountIdB64)
        noticeStore.pruneOwn(accountIdB64, System.currentTimeMillis())
        val rows = mutableListOf<WireAccountNotice>()
        // A device with no cursor asks by time (the last 30 days) once, then
        // pages by the cursor the answer gives.
        val first = noticeQuery(cursor, System.currentTimeMillis())
        var after: Long? = first.after
        var sinceMs: Long? = first.sinceMs
        var nextAfter = cursor ?: 0L
        authClientFactory().use { client ->
            repeat(NOTICE_MAX_PAGES) {
                val page = client.accountNotices(token, after, sinceMs)
                sinceMs = null
                rows += page.notices.map { WireAccountNotice(id = it.id, kind = it.kind, atMs = it.at_ms) }
                nextAfter = maxOf(nextAfter, page.next_after)
                if (!page.more) return@use
                after = page.next_after
            }
        }
        val selection = selectAccountNotices(rows, nextAfter, cursor, noticeStore.ownActions(accountIdB64))
        noticeStore.setCursor(accountIdB64, selection.cursor)
        noticeStore.clearOwn(accountIdB64, selection.spentOwn)
        // A phrase was saved for the account (here or elsewhere): this
        // device's phrase-saved flag is read again.
        if (rows.any { it.kind == AccountNoticeKind.PhraseSaved.wire }) refreshRecoveryEmail()
        if (!secretsAreFor(accountIdB64) || selection.show.isEmpty()) return
        mutableAccountNotices.value = mutableAccountNotices.value + selection.show
    }

    /** The notice was answered (`It was me`, `OK`, or `It was not me`). */
    public fun dismissAccountNotice(id: Long) {
        mutableAccountNotices.value = mutableAccountNotices.value.filterNot { it.id == id }
    }

    /**
     * `It was not me` on a password / opened-by-email notice (UX 19.6): this
     * device holds the keys, so the new-password form asks nothing else for
     * the next ten minutes. Only a device with the account's keys.
     */
    public fun unlockPasswordChangeAfterNotice() {
        val accountIdB64 = signedInAccountIdB64() ?: return
        if (!canReplacePassword()) return
        noticePasswordUnlockUntilMs[accountIdB64] = System.currentTimeMillis() + FRESH_AUTH_WINDOW_MS
    }

    /** The "Protect your account" step was answered (any of its outcomes). */
    public fun finishProtectStep() {
        signedInAccountIdB64()?.let { protectionStore.setProtectStepOwed(it, false) }
        mutableProtectStepOwed.value = false
    }

    /** When the no-protection banner was dismissed on this device, or `null`. */
    public fun protectBannerDismissedAtMs(): Long? =
        signedInAccountIdB64()?.let { protectionStore.bannerDismissedAtMs(it) }

    public fun dismissProtectBanner() {
        signedInAccountIdB64()?.let { protectionStore.dismissBanner(it, System.currentTimeMillis()) }
    }

    /**
     * `POST /recovery-email/enrol/start` for the signed-in account: mail a
     * code to [address]. The address is passed through and kept nowhere.
     *
     * @throws FreshAuthRequired without a live proof (nothing was sent).
     * @throws AuthClientArgError for a guest account (no username).
     */
    public suspend fun recoveryEmailEnrolStart(address: String): RecoveryEmailEnrolStart {
        val session = mutableState.value as? AuthState.Session
            ?: throw AuthClientArgError("not signed in", me.lakko.app.l10n.UiText("common.error.session_not_ready"))
        if (session.account.username.isBlank()) {
            throw AuthClientArgError("a guest has no username", me.lakko.app.l10n.UiText("recovery_email.needs_username"))
        }
        val grant = freshGrant() ?: throw FreshAuthRequired()
        val token = sessionTokenProvider.current()
            ?: throw AuthClientArgError("not signed in", me.lakko.app.l10n.UiText("common.error.session_not_ready"))
        val named = resolveNamedSignIn(null, session.account.username, loginHandleSource)
        return try {
            authClientFactory().use {
                it.recoveryEmailEnrolStart(token, grant, named.handle, address, deviceLanguageTag())
            }
        } catch (err: AuthHttpError) {
            if (isFreshAuthRequired(err)) {
                dropFreshProof()
                throw FreshAuthRequired()
            }
            throw err
        }
    }

    /**
     * `POST /recovery-email/enrol/finish`: the mailed [code], and - only
     * while no phrase is saved ([enrolSealsVault]) - the email vault sealed
     * under the username and [address] with the start's salt. The seal runs
     * 64 MiB Argon2id and is done off the main thread here. An account whose
     * phrase was saved meanwhile (`409 phrase-saved`) is finished again
     * without a vault.
     */
    public suspend fun recoveryEmailEnrolFinish(start: RecoveryEmailEnrolStart, address: String, code: String) {
        val session = mutableState.value as? AuthState.Session
            ?: throw AuthClientArgError("not signed in", me.lakko.app.l10n.UiText("common.error.session_not_ready"))
        val accountIdB64 = session.account.accountIdB64
        val token = sessionTokenProvider.current()
            ?: throw AuthClientArgError("not signed in", me.lakko.app.l10n.UiText("common.error.session_not_ready"))
        val before = mutableRecoveryEmail.value ?: refreshRecoveryEmail()
        val phraseSaved = before?.phraseSaved ?: protectionStore.phraseSaved(accountIdB64)
        val sealed: ByteArray? = if (enrolSealsVault(phraseSaved)) {
            val named = resolveNamedSignIn(null, session.account.username, loginHandleSource)
            withContext(Dispatchers.Default) {
                val phrase = DevicePhrase.read(applicationContext, accountIdB64)
                    ?: throw AuthClientArgError("this device holds no phrase to seal")
                RecoveryMnemonic.fromPhrase(phrase).use {
                    uniffi.lakko_ffi_uniffi.emailVaultSeal(it, named.handle, address, start.vaultSalt)
                }
            }
        } else {
            null
        }
        val kind = if (before?.enabled == true) AccountNoticeKind.EmailReplaced else AccountNoticeKind.EmailAdded
        noticeStore.markOwn(accountIdB64, kind, System.currentTimeMillis())
        val vault = authClientFactory().use { client ->
            try {
                client.recoveryEmailEnrolFinish(token, start.challengeId, code, sealed)
            } catch (err: AuthHttpError) {
                if (err.code != "phrase-saved" || sealed == null) throw err
                protectionStore.setPhraseSaved(accountIdB64, true)
                client.recoveryEmailEnrolFinish(token, start.challengeId, code, null)
            }
        }
        mutableRecoveryEmail.value = RecoveryEmailStatus(
            available = true,
            enabled = true,
            vault = vault,
            phraseSaved = protectionStore.phraseSaved(accountIdB64),
            totp = before?.totp == true,
        )
        // The confirmed address is kept by the account's devices.
        keepRecoveryAddress(accountIdB64, address)
    }

    /** `DELETE /recovery-email` for the signed-in account.
     *  @throws FreshAuthRequired without a live proof. */
    public suspend fun recoveryEmailRemove() {
        val accountIdB64 = signedInAccountIdB64()
            ?: throw AuthClientArgError("not signed in", me.lakko.app.l10n.UiText("common.error.session_not_ready"))
        val grant = freshGrant() ?: throw FreshAuthRequired()
        val token = sessionTokenProvider.current()
            ?: throw AuthClientArgError("not signed in", me.lakko.app.l10n.UiText("common.error.session_not_ready"))
        noticeStore.markOwn(accountIdB64, AccountNoticeKind.EmailRemoved, System.currentTimeMillis())
        try {
            authClientFactory().use { it.recoveryEmailRemove(token, grant) }
        } catch (err: AuthHttpError) {
            if (isFreshAuthRequired(err)) {
                dropFreshProof()
                throw FreshAuthRequired()
            }
            throw err
        }
        mutableRecoveryEmail.value = mutableRecoveryEmail.value?.copy(enabled = false, vault = false)
        keepRecoveryAddress(accountIdB64, null)
    }

    // ---- the devices keep the address (amended 2026-10-13) ----------------

    /**
     * Keep [address] (`null` = removed) in the synced state's register, or
     * park the write until the state is open. The value is never logged.
     */
    private suspend fun keepRecoveryAddress(accountIdB64: String, address: String?) {
        val port = recoveryAddressPort
        val written = try {
            port != null && port.state().ready && secretsAreFor(accountIdB64) && port.write(address).let { true }
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w("lakko-auth", "recovery address not kept: ${cause.javaClass.simpleName}")
            false
        }
        if (written) {
            pendingAddressWrite.set(null)
            mutableRecoveryAddress.value = port?.state()?.address
        } else {
            pendingAddressWrite.set(accountIdB64 to address)
        }
    }

    /**
     * The register changed or became readable (the app root calls this on
     * every emission of the sync runtime's register): a parked write is
     * flushed, Settings is told, and the re-seal rule is evaluated again.
     */
    public suspend fun onRecoveryAddressState() {
        val port = recoveryAddressPort ?: return
        val accountIdB64 = mutableIdentitySecrets.value?.let { base64Encode(it.accountId) }
        if (accountIdB64 == null || !port.state().ready) {
            mutableRecoveryAddress.value = null
            return
        }
        pendingAddressWrite.get()?.let { parked ->
            if (parked.first == accountIdB64) {
                pendingAddressWrite.compareAndSet(parked, null)
                try {
                    port.write(parked.second)
                } catch (cause: Throwable) {
                    if (cause is kotlinx.coroutines.CancellationException) throw cause
                    Log.w("lakko-auth", "parked recovery address not kept: ${cause.javaClass.simpleName}")
                }
            } else {
                pendingAddressWrite.compareAndSet(parked, null)
            }
        }
        val held = port.state().address
        if (held != mutableRecoveryAddress.value) {
            mutableRecoveryAddress.value = held
            resealTried.remove(accountIdB64)
        }
        resealIfNeeded()
    }

    /**
     * The automatic re-seal ([resealNeeded]): seal the CURRENT phrase under
     * the held address with a fresh random salt, off the main thread, and
     * `PUT /recovery-email/vault`. Silent. One attempt in flight per
     * account and one per landing or register change; `vault-exists` and
     * `phrase-saved` read the status again; `address-mismatch` clears the
     * register (it is stale); anything else waits for the next landing.
     */
    private suspend fun resealIfNeeded() {
        val status = mutableRecoveryEmail.value ?: return
        val session = mutableState.value as? AuthState.Session ?: return
        val accountIdB64 = session.account.accountIdB64
        if (!secretsAreFor(accountIdB64) || session.account.username.isBlank()) return
        val port = recoveryAddressPort ?: return
        val held = port.state().takeIf { it.ready }?.address
        if (!resealNeeded(status.enabled, status.vault, status.phraseSaved, addressHeld = held != null)) return
        val address = held ?: return
        // The new words must be on this device: not while a reset's or a
        // rotation's publish is still owed.
        if (publishOwed()) return
        if (!resealTried.add(accountIdB64) || !resealInFlight.add(accountIdB64)) return
        val failure: Throwable? = try {
            val token = sessionTokenProvider.current() ?: return
            val named = resolveNamedSignIn(null, session.account.username, loginHandleSource)
            val salt = ByteArray(EMAIL_VAULT_SALT_LEN).also { SecureRandom().nextBytes(it) }
            val sealed = withContext(Dispatchers.Default) {
                val phrase = DevicePhrase.read(applicationContext, accountIdB64)
                    ?: throw AuthClientArgError("this device holds no phrase to seal")
                RecoveryMnemonic.fromPhrase(phrase).use {
                    uniffi.lakko_ffi_uniffi.emailVaultSeal(it, named.handle, address, salt)
                }
            }
            authClientFactory().use { it.recoveryEmailVaultPut(token, named.handle, address, salt, sealed) }
            null
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            cause
        } finally {
            resealInFlight.remove(accountIdB64)
        }
        when (resealOutcomeOf(failure)) {
            ResealOutcome.Settled -> refreshRecoveryEmail()
            ResealOutcome.AddressStale -> {
                Log.w("lakko-auth", "the held recovery address is not the account's any more; cleared")
                keepRecoveryAddress(accountIdB64, null)
            }
            ResealOutcome.RetryLater ->
                Log.w("lakko-auth", "recovery email vault not re-sealed: ${failure?.javaClass?.simpleName}")
        }
    }

    // ---- the own-username register and a rename (ADR-0076) ---------------

    private suspend fun writeOwnUsername(accountIdB64: String, username: String) {
        val port = ownUsernamePort
        val written = try {
            port != null && port.state().ready && port.write(username).let { true }
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w("lakko-auth", "own username not written to the register: ${cause.javaClass.simpleName}")
            false
        }
        if (written) pendingUsernameWrite.set(null) else pendingUsernameWrite.set(accountIdB64 to username)
    }

    /**
     * The own-username register changed or became readable (the app root
     * calls this on every emission): a parked write goes first, an account
     * from before the register seeds it once from its marker, and a name
     * that differs from the marker is ADOPTED - the account was renamed on
     * another device ([ownUsernameSync]).
     */
    public suspend fun onOwnUsernameState() {
        val port = ownUsernamePort ?: return
        val session = mutableState.value as? AuthState.Session ?: return
        val accountIdB64 = session.account.accountIdB64
        if (!secretsAreFor(accountIdB64)) return
        val parked = pendingUsernameWrite.get()?.takeIf { it.first == accountIdB64 }?.second
        val action = ownUsernameSync(
            register = port.state(),
            localUsername = session.account.username,
            parked = parked,
            renaming = renaming,
            // Seeding needs auth's word on the local name in THIS session.
            handleConfirmed = loginHandleRebinder.isConfirmed(session.account.username),
        )
        when (action) {
            OwnUsernameSync.Nothing -> if (parked != null && parked == port.state().username) pendingUsernameWrite.set(null)
            is OwnUsernameSync.Flush -> writeOwnUsername(accountIdB64, action.username)
            is OwnUsernameSync.Seed -> writeOwnUsername(accountIdB64, action.username)
            is OwnUsernameSync.Adopt -> adoptUsername(accountIdB64, action.username, renamedElsewhere = true)
        }
    }

    /** The marker and the live session take [username]; the handle is known
     *  for this session; the app root re-publishes the profile. */
    private fun adoptUsername(accountIdB64: String, username: String, renamedElsewhere: Boolean) {
        val session = mutableState.value as? AuthState.Session ?: return
        if (session.account.accountIdB64 != accountIdB64 || session.account.username == username) return
        sessionStore.save(PersistedSession(accountIdB64 = accountIdB64, username = username))
        sessionTokenProvider.peek()?.let { loginHandleRebinder.markBound(it.accessToken, username) }
        mutableState.value = AuthState.Session(session.account.copy(username = username))
        try {
            onUsernameChanged?.invoke(username, renamedElsewhere)
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "the new username was not re-published: ${cause.javaClass.simpleName}")
        }
    }

    /** When this device may rename again by its own stamp, or `null`. */
    public fun renameAllowedAtMs(): Long? {
        val accountIdB64 = signedInAccountIdB64() ?: return null
        return renameAllowedAtMs(renameStore.lastChangedAtMs(accountIdB64), System.currentTimeMillis())
    }

    private fun renameRunner(accountIdB64: String): UsernameRenameRunner =
        UsernameRenameRunner(
            accountIdB64 = accountIdB64,
            store = renameStore,
            ports = object : UsernameRenamePorts {
                override suspend fun bind(from: String, to: String) = renameBind(accountIdB64, from, to)

                override suspend fun replace(newUsername: String): ByteArray? {
                    val secrets = mutableIdentitySecrets.value ?: throw AuthClientArgError("not signed in")
                    val token = sessionTokenProvider.current() ?: secrets.sessionToken
                    return accountDirectory.replaceUsername(token, secrets.accountId, newUsername).certificate
                }

                override suspend fun adopt(newUsername: String, certificate: ByteArray?) {
                    val reach = me.lakko.app.reach.ReachManager.get(applicationContext)
                    if (certificate != null) reach.keepUsernameCertificate(certificate) else reach.dropUsernameCertificate()
                    renameStore.markChanged(accountIdB64, System.currentTimeMillis())
                    adoptUsername(accountIdB64, newUsername, renamedElsewhere = certificate == null)
                    writeOwnUsername(accountIdB64, newUsername)
                }
            },
            log = { Log.w("lakko-auth", it) },
        )

    /**
     * `PUT /auth/login-handle` moving the account from [from]'s handle to
     * [to]'s, with the grant and, when this device holds the address and the
     * account has a recovery email, the recovery email re-keyed: the address
     * and (no phrase saved) a vault freshly sealed off the main thread under
     * [to]'s handle with a new random salt ([renameRecoveryEmail]).
     */
    private suspend fun renameBind(accountIdB64: String, from: String, to: String) {
        val token = sessionTokenProvider.current() ?: throw AuthClientArgError("not signed in")
        val target = namedHandleOrRefuse(loginHandleSource, to)
        val previous = namedHandleOrRefuse(loginHandleSource, from)
        val status = mutableRecoveryEmail.value ?: refreshRecoveryEmail()
        val address = recoveryAddressPort?.state()?.takeIf { it.ready }?.address
        val plan = renameRecoveryEmail(
            hasRecoveryEmail = status?.enabled == true,
            addressHeld = address != null,
            phraseSaved = status?.phraseSaved ?: protectionStore.phraseSaved(accountIdB64),
        )
        val carried: LoginHandleRecoveryEmail? = when (plan) {
            RenameRecoveryEmail.None -> null
            RenameRecoveryEmail.AddressOnly -> LoginHandleRecoveryEmail(address = address.orEmpty())
            RenameRecoveryEmail.AddressAndVault -> {
                val salt = ByteArray(EMAIL_VAULT_SALT_LEN).also { SecureRandom().nextBytes(it) }
                val sealed = withContext(Dispatchers.Default) {
                    val phrase = DevicePhrase.read(applicationContext, accountIdB64)
                        ?: throw AuthClientArgError("this device holds no phrase to seal")
                    RecoveryMnemonic.fromPhrase(phrase).use {
                        uniffi.lakko_ffi_uniffi.emailVaultSeal(it, target.handle, address.orEmpty(), salt)
                    }
                }
                LoginHandleRecoveryEmail(
                    address = address.orEmpty(),
                    vault_salt = base64Encode(salt),
                    sealed_entropy = base64Encode(sealed),
                )
            }
        }
        val outcome = try {
            authClientFactory().use {
                it.bindLoginHandle(
                    sessionToken = token,
                    handle = target.handle,
                    freshGrant = freshGrant(),
                    previousHandle = previous.handle,
                    recoveryEmail = carried,
                )
            }
        } catch (err: AuthHttpError) {
            if (isFreshAuthRequired(err)) {
                dropFreshProof()
                throw FreshAuthRequired()
            }
            throw err
        }
        if (outcome == LoginHandleBinding.Taken) {
            throw AuthClientArgError("the username is taken", me.lakko.app.l10n.UiText("auth.signup.error.username_taken"))
        }
        // The handle of this session is known: the sync does not ask again.
        loginHandleRebinder.markBound(token, to)
    }

    /**
     * Change the account's username (ADR-0076 section 4): one resumable
     * operation ([UsernameRenameRunner]) - bind the new handle at auth, replace
     * the name at discovery (a refusal binds the old handle back), then the
     * marker, the session state, the register and the re-publish.
     */
    public suspend fun changeUsername(newUsername: String): RenameResult {
        val session = mutableState.value as? AuthState.Session
        if (session == null || session.account.username.isBlank() || !secretsAreFor(session.account.accountIdB64)) {
            return RenameResult.Refused(me.lakko.app.l10n.UiText("common.error.session_not_ready"))
        }
        if (freshGrant() == null) {
            return RenameResult.Refused(me.lakko.app.l10n.UiText("common.error.unavailable"), needsProof = true)
        }
        val named = try {
            namedHandleOrRefuse(loginHandleSource, newUsername)
        } catch (err: AuthClientArgError) {
            return RenameResult.Refused(err.text ?: me.lakko.app.l10n.UiText("username.error.unknown"))
        }
        renaming = true
        return try {
            renameRunner(session.account.accountIdB64).start(session.account.username, named.username)
        } finally {
            renaming = false
        }
    }

    /** A rename a crash left between its steps is finished or rolled back
     *  at the landing. Best effort; a record that cannot be settled stays. */
    private suspend fun resumeOwedRename(accountIdB64: String) {
        if (renameStore.load(accountIdB64) == null) return
        renaming = true
        try {
            renameRunner(accountIdB64).resume()
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w("lakko-auth", "owed username rename not settled: ${cause.javaClass.simpleName}")
        } finally {
            renaming = false
        }
    }

    // ---- a guest chooses a username and its first password ----------------

    /** Whether the signed-in account chose a username and still owes its
     *  first password (a restart between the two steps). */
    public fun firstPasswordOwed(): Boolean =
        signedInAccountIdB64()?.let { protectionStore.firstPasswordOwed(it) } == true

    /**
     * "Choose a username", the name step: the sign-up claim run later
     * ([claimUsername]: the login handle at auth, then discovery, the handle
     * released if discovery refuses), the certificate kept, the name stored
     * in the account marker and the live session, the handle marked bound.
     * The first password is owed from here on ([setFirstPassword]).
     *
     * @throws AuthClientArgError a taken or refused name (the form's line).
     * @throws AuthHttpError `429 handle-bind-limited`, others.
     */
    public suspend fun claimGuestUsername(username: String): String {
        val session = mutableState.value as? AuthState.Session
        val secrets = mutableIdentitySecrets.value
        if (session == null || secrets == null || session.account.username.isNotBlank()) {
            throw AuthClientArgError(
                "no guest session to name",
                me.lakko.app.l10n.UiText("common.error.session_not_ready"),
            )
        }
        val accountIdB64 = session.account.accountIdB64
        val token = sessionTokenProvider.current() ?: secrets.sessionToken
        val claimed = claimUsername(
            directory = accountDirectory,
            binder = loginHandleBinder,
            handles = loginHandleSource,
            sessionToken = token,
            accountId = secrets.accountId,
            username = username,
        )
        loginHandleRebinder.markBound(token, claimed.username)
        // Owed before the name shows anywhere: a restart reopens the screen
        // on the password.
        protectionStore.setFirstPasswordOwed(accountIdB64, true)
        sessionStore.save(PersistedSession(accountIdB64 = accountIdB64, username = claimed.username))
        claimed.certificate?.let {
            try {
                me.lakko.app.reach.ReachManager.get(applicationContext).keepUsernameCertificate(it)
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "username certificate not kept: ${cause.javaClass.simpleName}")
            }
        }
        mutableState.value = AuthState.Session(session.account.copy(username = claimed.username))
        writeOwnUsername(accountIdB64, claimed.username)
        return claimed.username
    }

    /**
     * "Choose a username", the password step: the device's seed-based
     * password change ([replacePassword]) with no proof block - the account
     * has no known password; this is its first. Allowed only while the first
     * password is owed.
     */
    public suspend fun setFirstPassword(newPassword: ByteArray) {
        val accountIdB64 = signedInAccountIdB64()
        if (accountIdB64 == null || !protectionStore.firstPasswordOwed(accountIdB64)) {
            throw AuthClientArgError(
                "no first password is owed",
                me.lakko.app.l10n.UiText("common.error.session_not_ready"),
            )
        }
        replacePassword(newPassword)
        protectionStore.setFirstPasswordOwed(accountIdB64, false)
    }

    /** The claim screen closed after its "done" line: the "Protect your
     *  account" step is offered once (the guest never saw it). */
    public fun finishUsernameClaim() {
        val accountIdB64 = signedInAccountIdB64() ?: return
        protectionStore.setProtectStepOwed(accountIdB64, true)
        mutableProtectStepOwed.value = true
    }

    // ---- Get back in > Email code (UX 19.3) -------------------------------

    /** Step 1: `recover/start` for the typed [username] and [address]. The
     *  answer is the same whatever was typed. */
    public suspend fun emailRecoverStart(username: String, address: String): EmailRecoverStartResponse {
        val named = resolveNamedSignIn(null, username, loginHandleSource)
        return authClientFactory().use { it.emailRecoverStart(named.handle, address, deviceLanguageTag()) }
    }

    public suspend fun emailRecoverVerify(challengeId: String, code: String): EmailRecoverVerifyResponse =
        authClientFactory().use { it.emailRecoverVerify(challengeId, code) }

    public suspend fun emailRecoverTotp(ticket: String, code: String): Unit =
        authClientFactory().use { it.emailRecoverTotp(ticket, code) }

    /** How [emailRecoverFinish] ended. */
    public enum class EmailRecoverOutcome {
        /** The vault opened and the phrase restore landed the session. */
        Restored,

        /** The new password was set and the session landed (a device
         *  without keys shows "Restore your keys" next). */
        PasswordSet,

        /** The vault did not open under the typed username and address. The
         *  ticket is spent; nothing changed on the server. */
        VaultNotOpened,
    }

    /**
     * Step 4 onward: ask `recover/open` under [ticket] and do what it says
     * ([emailRecoverAction]).
     *
     *   - the vault: opened with the login handle of [username] and
     *     [address] off the main thread (64 MiB Argon2id), the phrase is
     *     rebuilt and the EXISTING phrase restore runs with [newPassword]
     *     ([recover]: bundle check, recovery login, backup restore);
     *   - the password mode: `password/start|finish` under the ticket, then
     *     the usual ACCESS landing.
     */
    public suspend fun emailRecoverFinish(
        username: String,
        address: String,
        ticket: String,
        newPassword: ByteArray,
    ): EmailRecoverOutcome {
        val named = resolveNamedSignIn(null, username, loginHandleSource)
        val candidate = candidateAccount(named.username)
        val opened = authClientFactory().use { it.emailRecoverOpen(ticket) }
        return when (emailRecoverAction(opened)) {
            EmailRecoverAction.RestoreFromVault -> {
                opened as EmailRecoverOpen.Vault
                val phrase = try {
                    withContext(Dispatchers.Default) {
                        uniffi.lakko_ffi_uniffi.emailVaultOpen(
                            named.handle,
                            address,
                            opened.vaultSalt,
                            opened.sealedEntropy,
                        ).use { it.phrase() }
                    }
                } catch (_: FfiException) {
                    return EmailRecoverOutcome.VaultNotOpened
                } finally {
                    opened.sealedEntropy.fill(0)
                }
                // The server recorded "account opened by email" at `open`:
                // this device's own action.
                candidate?.let {
                    noticeStore.markOwn(it, AccountNoticeKind.AccountOpenedByEmail, System.currentTimeMillis())
                }
                recover(phrase, newPassword, named.username)
                // Reached from "Restore your keys": the parked sign-in without
                // keys is answered by the session that just landed.
                if (pendingRecovery.get() != null) clearPendingRecovery()
                signedInAccountIdB64()?.let { accountIdB64 ->
                    noticeStore.markOwn(accountIdB64, AccountNoticeKind.AccountOpenedByEmail, System.currentTimeMillis())
                    protectionStore.setPhraseSaved(accountIdB64, false)
                    // The person typed the address: this device keeps it.
                    keepRecoveryAddress(accountIdB64, address)
                }
                EmailRecoverOutcome.Restored
            }
            EmailRecoverAction.SetPassword -> {
                val result = authClientFactory().use {
                    it.emailRecoverSetPassword(ticket, newPassword, landingDeviceId(candidate))
                }
                val accountIdB64 = base64Encode(result.accountId)
                noticeStore.markOwn(accountIdB64, AccountNoticeKind.PasswordChangedByEmail, System.currentTimeMillis())
                protectionStore.setPhraseSaved(accountIdB64, true)
                mutableRestoreNotice.value = null
                // A mailed code proves the mailbox, nothing more (the answer
                // carries no fresh-auth grant).
                freshAuth.onSignInWithoutProof(accountIdB64)
                sessionLandingKind = SessionLanding.Other
                // The password was just chosen: no new-password step is owed.
                mutableNewPasswordOwed.value = false
                rebindLoginHandleInBackground(result.sessionToken, named.handle)
                landAfterAccessRecovery(
                    username = named.username,
                    accountIdBytes = result.accountId,
                    accountIdB64 = accountIdB64,
                    session = result.session,
                    pushContext = "email-recover",
                )
                // The person typed the address: this device keeps it (the
                // write is parked until the synced state is open).
                keepRecoveryAddress(accountIdB64, address)
                EmailRecoverOutcome.PasswordSet
            }
        }
    }

    // ---- taking the account back with the phrase (UX 19.7) ----------------

    /** What `recall/status` by login handle answered for a typed phrase. */
    public class PhraseRecallOffer(
        public val username: String,
        public val accountId: ByteArray,
        public val status: RecallStatus,
    )

    /**
     * Get back in > Recovery phrase, when the words are not the current
     * account's: ask `recall/status` by the login handle of [username] with
     * a statement signed by the key derived from [phrase]. Non-null only
     * when the words were this account's phrase before a reset in the
     * window AND the phrase arm is open; every other answer (and every
     * failure) is `null`, and the form keeps its "not this account's" line.
     */
    public suspend fun phraseRecallOffer(username: String, phrase: String): PhraseRecallOffer? {
        val named = try {
            resolveNamedSignIn(sessionStore.load()?.let { CachedAccount(it.accountIdB64, it.username) }, username, loginHandleSource)
        } catch (_: AuthClientArgError) {
            return null
        }
        val signingSeed = try {
            val seed = RecoveryMnemonic.fromPhrase(phrase).use { it.toSeed() }
            try {
                Identity.fromSeed(seed).use { it.ed25519Seed() }
            } finally {
                seed.fill(0)
            }
        } catch (_: FfiException) {
            return null
        }
        return try {
            val status = authClientFactory().use {
                it.recallStatus(ByteArray(0), signingSeed, handle = named.handle)
            }
            val accountId = status.accountId
            if (!status.phraseArm || accountId == null) null else PhraseRecallOffer(named.username, accountId, status)
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            null
        } finally {
            signingSeed.fill(0)
        }
    }

    /**
     * `Take the account back` from a device that holds no record: the recall
     * with `proof: "phrase"` (the OPAQUE login is made from the old recovery
     * seed), then the key reset's client order ([landKeyReset]): new keys, a
     * new phrase shown last, every other device signed out.
     *
     * @throws AuthHttpError / AuthNetworkError / RecallWrongPassword as the
     *   recall routes answer; nothing changed unless `finish` answered.
     */
    public suspend fun phraseRecallSubmit(offer: PhraseRecallOffer, oldPhrase: String, newPassword: ByteArray) {
        val accountIdB64 = base64Encode(offer.accountId)
        val oldRecoverySeed = RecoveryMnemonic.fromPhrase(oldPhrase).use { it.toSeed() }
        val oldAccountSeed = Identity.fromSeed(oldRecoverySeed).use { it.ed25519Seed() }
        val (newPhrase, newSeed) = RecoveryMnemonic.generate().use { it.phrase() to it.toSeed() }
        var handedOff = false
        try {
            val newSignaturePubkey = Identity.fromSeed(newSeed).use { it.signaturePublicKey() }
            val seedCopy = newSeed.copyOf()
            val recalled = try {
                authClientFactory().use {
                    it.recallAccount(
                        accountId = offer.accountId,
                        oldAccountSeed = oldAccountSeed,
                        anchorSeq = offer.status.anchorSeq,
                        oldPassword = ByteArray(0),
                        newPassword = newPassword,
                        newSeed = seedCopy,
                        newSignaturePubkey = newSignaturePubkey,
                        deviceId = landingDeviceId(accountIdB64),
                        oldRecoverySeed = oldRecoverySeed,
                    )
                }
            } finally {
                seedCopy.fill(0)
            }
            val nowMs = System.currentTimeMillis()
            freshAuth.put(accountIdB64, recalled.freshGrant, recalled.freshGrantExpiresAtMs, nowMs)
            sessionLandingKind = SessionLanding.Password
            mutableRecallDone.value = true
            handedOff = true
            // The phrase arm exists only for a phrase-protected account.
            protectionStore.setPhraseSaved(accountIdB64, true)
            landKeyReset(
                accountIdB64 = accountIdB64,
                accountIdBytes = offer.accountId,
                username = offer.username,
                seed = newSeed,
                phrase = newPhrase,
                newSignaturePubkey = newSignaturePubkey,
                reset = recalled.reset,
                context = "phrase-recall",
            )
        } finally {
            oldRecoverySeed.fill(0)
            oldAccountSeed.fill(0)
            if (!handedOff) newSeed.fill(0)
        }
    }

    // -----------------------------------------------------------------
    // The phrase on this device; choosing a new password
    // -----------------------------------------------------------------

    /** The recovery phrase stored on this device for the signed-in account,
     *  or `null` (Settings > Security > Recovery, "Show recovery phrase").
     *  Read from the device, never from a server. Call it off the main
     *  thread, and only after the device's own authentication prompt. */
    public fun devicePhrase(): String? {
        val accountIdB64 = (mutableState.value as? AuthState.Session)?.account?.accountIdB64 ?: return null
        return DevicePhrase.read(applicationContext, accountIdB64)
    }

    /**
     * Whether [password] is the signed-in account's password: the fallback
     * gate in front of "Show recovery phrase" on a device with no screen
     * lock. A password can only be checked with the server (one ordinary
     * sign-in exchange); the session it yields is discarded. `false` on a
     * wrong password; throws on a limit, when offline, or for an account
     * without a username.
     */
    public suspend fun verifyPassword(password: ByteArray): Boolean {
        val session = mutableState.value as? AuthState.Session
        if (session == null || session.account.username.isBlank()) {
            throw AuthClientArgError(
                "no signed-in account with a username",
                me.lakko.app.l10n.UiText("common.error.session_not_ready"),
            )
        }
        val named = resolveNamedSignIn(null, session.account.username, loginHandleSource)
        return try {
            // Nothing is re-bound and the session this yields is installed nowhere.
            // It still names this device, so it stays revocable with it.
            val result = authClientFactory().use {
                it.login(named.handle, password, landingDeviceId(session.account.accountIdB64))
            }
            try {
                base64Encode(result.accountId) == session.account.accountIdB64
            } finally {
                result.exportKey.fill(0)
                result.sessionKey.fill(0)
            }
        } catch (err: AuthNetworkError) {
            throw err
        } catch (err: AuthHttpError) {
            // The server's own refusals (a limit, an outage) are not an
            // answer about the password.
            if (err.status != HTTP_UNAUTHORIZED && err.status != HTTP_NOT_FOUND) throw err
            false
        } catch (_: FfiException) {
            // A wrong password fails the exchange on this side.
            false
        }
    }

    /** Whether [replacePassword] can run: a session with a username on a
     *  device that holds the account's keys (a guest has no login handle to
     *  address the reset by). */
    public fun canReplacePassword(): Boolean {
        val session = mutableState.value as? AuthState.Session ?: return false
        val secrets = mutableIdentitySecrets.value ?: return false
        return session.account.username.isNotBlank() &&
            secrets.accountIdentitySeed.size == ACCOUNT_IDENTITY_SEED_LENGTH
    }

    /** "Not now" on the new-password step. */
    public fun dismissNewPasswordOffer() {
        mutableNewPasswordOwed.value = false
    }

    /**
     * Replace the account's password from a device that holds its keys,
     * without typing the phrase. The cached 64-byte identity seed is the
     * recovery input `/password/reset` accepts, so the device proves it
     * holds the keys and the person types only the new password.
     *
     * Deliberately NOT [recover]: that is the fresh-device path, which
     * re-derives the device key from the new export key and replaces the
     * local chat state from the server backup. Here the stored device key
     * is kept and nothing local is deleted; only the session token and the
     * export-key cache are refreshed.
     *
     * @throws AuthClientArgError when there is no session with a username
     *   and keys, or the reset answered for another account.
     * @throws PasswordChangeNeedsProof without a live fresh-auth proof or a
     *   three-word check passed in the last ten minutes. Nothing was sent.
     */
    public suspend fun replacePassword(newPassword: ByteArray) {
        val session = mutableState.value as? AuthState.Session
        val secrets = mutableIdentitySecrets.value
        if (session == null || secrets == null || !canReplacePassword()) {
            throw AuthClientArgError(
                "no session that can replace the password",
                me.lakko.app.l10n.UiText("common.error.session_not_ready"),
            )
        }
        val accountIdB64 = session.account.accountIdB64
        // UX 17.8: a bare session may not set a password (whoever holds the
        // unlocked phone could, and then pass every proof).
        if (!passwordChangeUnlocked()) throw PasswordChangeNeedsProof()
        val named = resolveNamedSignIn(null, session.account.username, loginHandleSource)
        // A copy: the in-memory secret is never the buffer that is wiped.
        val seed = secrets.accountIdentitySeed.copyOf()
        try {
            val result = authClientFactory().use { client ->
                signInByName(
                    named = named,
                    call = {
                        client.recover(it.handle, seed, newPassword, landingDeviceId(session.account.accountIdB64))
                    },
                    sessionTokenOf = { it.sessionToken },
                    rebind = ::rebindLoginHandleInBackground,
                )
            }
            try {
                if (!result.accountId.contentEquals(secrets.accountId)) {
                    // The name's handle answered for another account. The
                    // proof was made from this account's seed, so this cannot
                    // verify; refuse rather than adopt another account's
                    // session.
                    throw AuthClientArgError("the password reset answered for another account")
                }
                try {
                    ExportKeyStore.putExportKey(applicationContext, accountIdB64, result.exportKey)
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "export-key persist (password change) failed", cause)
                }
                // Same account, same seed, same device key: only the token
                // and the export key change.
                installIdentitySecrets(
                    accountIdB64,
                    IdentitySecrets(
                        accountId = secrets.accountId.copyOf(),
                        exportKey = result.exportKey.copyOf(),
                        accountIdentitySeed = secrets.accountIdentitySeed.copyOf(),
                        session = result.session,
                    ),
                )
                mutableNewPasswordOwed.value = false
            } finally {
                result.exportKey.fill(0)
            }
        } finally {
            seed.fill(0)
        }
    }

    /**
     * ADR-0054 S5 - re-`PUT` the login handle after a successful sign-in by
     * name, in the background (idempotent: keeps a device that bound before a
     * rename in step). Never fails the sign-in; a `409` (another account holds
     * the handle) or any other failure is only logged, by status. Shares the
     * per-process (session, handle) dedup of [loginHandleRebinder], so the
     * session landing that follows does not bind the same pair again.
     */
    private fun rebindLoginHandleInBackground(sessionToken: String, handle: ByteArray) {
        loginHandleRebinder.bind(sessionToken, handle)
    }

    /**
     * The shared landing of every route that only signs in: the
     * authenticator code, a passkey, and the sign-in that follows the local
     * three-word check. "This device holds the keys" is decided here.
     * Mirror of `apps/web/src/auth/AuthContext.tsx::landAfterAccessRecovery`.
     *
     *   * persist the account marker (important on a fresh device);
     *   * **WARM same-device case** — a cached AccountIdentity (phrase)
     *     seed exists: bootstrap the [DeviceKeyStore] existence-gate
     *     from it ([bootstrapDeviceKeyFromSeedBestEffort], no password
     *     => no exportKey-derived key), build the live [IdentitySecrets]
     *     (exportKey EMPTY — these factors carry no password), clear any
     *     pending recovery, flip to [AuthState.Session];
     *   * **COLD fresh-device case** — no cached seed: park a
     *     [PendingRecovery] (exportKey EMPTY) and flip to
     *     [AuthState.NeedsRecoveryPhrase], where [provideRecoveryPhrase]
     *     / [completeFromPairing] complete the identity restore.
     *
     * Always issues a best-effort push subscribe (the ACCESS factor
     * succeeded; same posture as login / passkey), tagged with
     * [pushContext].
     *
     * FAIL-CLOSED note: the warm branch only fires for a seed read back
     * from THIS device's own [AccountIdentitySeedStore] (already
     * trusted), never a freshly-supplied one — so verify-before-persist
     * stays untouched.
     */
    private suspend fun landAfterAccessRecovery(
        username: String,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        session: SessionPair,
        pushContext: String,
        /** The passkey route's hook for the COLD case: it runs after the
         *  recovery context is parked and before the restore prompt is
         *  shown, and returns `true` when it landed the session itself (the
         *  passkey carried the keys). */
        restoreKeys: (suspend () -> Boolean)? = null,
    ) {
        // Persist the account marker — important on a fresh device
        // where no row existed (idempotent upsert otherwise).
        sessionStore.save(
            PersistedSession(
                accountIdB64 = accountIdB64,
                username = username,
            ),
        )

        // ADR-0017 identity acquisition. These factors carried NO
        // password, so the ONLY same-device shortcut is a cached
        // AccountIdentity (phrase) seed.
        val accountIdentitySeed: ByteArray? = try {
            AccountIdentitySeedStore.getAccountIdentitySeed(
                applicationContext,
                accountIdB64,
            )
        } catch (cause: Throwable) {
            // Decrypt / store failure — treat as "no cached seed" and
            // fall into the fail-safe rather than the wrong-identity
            // branch.
            Log.w(
                "lakko-auth",
                "account-identity-seed read failed (access recover)",
                cause,
            )
            null
        }

        if (accountIdentitySeed != null) {
            // Warm same-device case. Ensure the DeviceKeyStore
            // rehydration gate passes: no password => no exportKey-
            // derived device root key; if none exists, bootstrap it
            // from the cached (already-trusted) identity seed.
            bootstrapDeviceKeyFromSeedBestEffort(
                accountIdB64 = accountIdB64,
                accountIdentitySeed = accountIdentitySeed,
                context = "access recover",
            )
            // These routes involve no password, so the export key is whatever
            // an earlier password sign-in cached on this device, else empty.
            val cachedExportKey: ByteArray? = try {
                ExportKeyStore.getExportKey(applicationContext, accountIdB64)
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "export-key cache read failed (access recover)", cause)
                null
            }
            installIdentitySecrets(
                accountIdB64,
                IdentitySecrets(
                    accountId = accountIdBytes,
                    exportKey = cachedExportKey ?: ByteArray(0),
                    accountIdentitySeed = accountIdentitySeed,
                    session = session,
                ),
            )
            markSignedInNow(accountIdB64)
            pendingRecovery.set(null)
            mutableRestoreNotice.value = null
            mutableState.value = AuthState.Session(
                account = SessionAccount(
                    accountIdB64 = accountIdB64,
                    username = username,
                ),
            )
            // ADR-0073 section 4.2 - read the directory before any write (same
            // as a password login): stale keys land the signed-out screen.
            refreshPublishedIdentityInBackground(
                landing = IdentityLanding.SIGN_IN,
                accountIdB64 = accountIdB64,
                accountIdBytes = accountIdBytes,
                accountIdentitySeed = accountIdentitySeed,
            )
            // 1.20d — best-effort push subscribe (the ACCESS factor
            // succeeded; same posture as login / passkey).
            subscribePushBestEffort(pushContext)
            return
        }

        // Fresh-device fail-safe (ADR-0017): ACCESS succeeded,
        // identity restore is pending the phrase / a paired device.
        // exportKey EMPTY — there is no password on this path; the
        // completion paths bootstrap the device key from the VERIFIED
        // seed.
        pendingRecovery.set(
            PendingRecovery(
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
                exportKey = ByteArray(0),
                session = session,
                username = username,
            ),
        )
        installPendingSessionPair(accountIdB64, session)
        markSignedInNow(accountIdB64)
        if (restoreKeys != null && restoreKeys()) {
            // The passkey carried the keys and the session has landed.
            subscribePushBestEffort(pushContext)
            return
        }
        mutableState.value = AuthState.NeedsRecoveryPhrase(
            account = SessionAccount(
                accountIdB64 = accountIdB64,
                username = username,
            ),
        )
        subscribePushBestEffort(pushContext)
    }

    /**
     * ADR-0018 factor (c) — ensure the [DeviceKeyStore] rehydration
     * existence-gate passes on a no-password path: when NO device
     * root key exists for this account, derive one from the
     * AccountIdentity (phrase) seed via
     * [deriveDeviceRootKeyFromIdentitySeed] and persist it. A device
     * key that already exists is NEVER overwritten (a password login
     * may have derived it from the export key; replacing it is
     * pointless churn).
     *
     * MUST only be called with a seed that is already trusted: either
     * read back from this device's own [AccountIdentitySeedStore]
     * (warm TOTP case) or freshly verified against the published
     * bundle via [verifySeedAgainstPublishedIdentity] (the completion
     * paths) — NEVER before the fail-closed verification, so a
     * refusal leaves this device untouched.
     *
     * Best-effort (mirror of the web bootstrap, Android posture): on
     * Android nothing is wrapped under the device key (the encrypted
     * stores ride the Keystore master key), so the key is purely the
     * `rehydrate()` existence signal; a persistence failure just
     * means the next cold start asks the user to authenticate again.
     */
    private fun bootstrapDeviceKeyFromSeedBestEffort(
        accountIdB64: String,
        accountIdentitySeed: ByteArray,
        context: String,
    ) {
        try {
            val existing =
                deviceKeyStore.getDeviceKey(applicationContext, accountIdB64)
            if (existing != null) {
                // Only the existence signal matters — wipe immediately.
                existing.fill(0)
                return
            }
            val deviceRootKey = deriveDeviceRootKeyFromIdentitySeed(
                accountIdentitySeed,
                accountIdB64,
            )
            try {
                deviceKeyStore.putDeviceKey(
                    context = applicationContext,
                    accountIdB64 = accountIdB64,
                    rootKey = deviceRootKey,
                )
            } finally {
                deviceRootKey.fill(0)
            }
        } catch (cause: Throwable) {
            Log.w(
                "lakko-auth",
                "device-key bootstrap from identity seed ($context) failed",
                cause,
            )
        }
    }

    /**
     * ADR-0017 fresh-device fail-safe completion. Only valid while the
     * state is [AuthState.NeedsRecoveryPhrase]: the user typed their
     * recovery phrase, we derive the BIP39 seed, HARD-BLOCK verify it
     * against the account's published bundle, cache it under the device
     * key, build the canonical AccountIdentity from it (downstream via
     * `Identity.fromSeed`), and flip to a full [AuthState.Session].
     *
     * Mirror of `apps/web/src/auth/AuthContext.tsx::provideRecoveryPhrase`.
     *
     * FAIL CLOSED (lakko-security finding #1): the supplied phrase MUST
     * reproduce the account's PUBLISHED AccountIdentity key. This path
     * only runs on a fresh-device login of an ALREADY-ESTABLISHED
     * account, so the bundle must exist — a missing bundle (404) or an
     * unreachable directory is an anomaly, NOT a first-publish. We
     * therefore HARD-BLOCK on any failure-to-verify (mismatch, 404, or
     * network) rather than trust-on-first-use a possibly-wrong phrase: a
     * server-forced 404 must not let a socially-engineered wrong phrase
     * overwrite the published identity. On every refusal the derived
     * seed is zeroized.
     *
     * @throws PhraseMismatch when the derived pubkey does not match the
     *         account's published bundle.
     * @throws IdentityVerificationUnavailable when the bundle could not
     *         be fetched (404 / network) so the phrase cannot be
     *         verified — retryable, not a dead end.
     * @throws AuthClientArgError when there is no pending recovery.
     * @throws me.lakko.app.auth.FfiException via
     *         [RecoveryMnemonic.fromPhrase] when the BIP39 checksum
     *         fails.
     */
    public suspend fun provideRecoveryPhrase(mnemonicPhrase: String) {
        val pending = pendingRecovery.get()
            ?: throw AuthClientArgError("no pending identity recovery — sign in first")

        // Parse the phrase -> derive the BIP39 seed -> derive the
        // canonical AccountIdentity pubkey from it. `fromPhrase` throws
        // a typed FfiException on a bad checksum / unknown word
        // (preserves BIP39's no-probing property).
        // The canonical form (12 lower-case words, single spaces) is what the
        // device stores once the check below has passed.
        val (canonicalPhrase, accountIdentitySeed) =
            RecoveryMnemonic.fromPhrase(mnemonicPhrase).use { it.phrase() to it.toSeed() }
        var seedHandedOff = false
        try {
            verifySeedAgainstPublishedIdentity(
                accountIdBytes = pending.accountIdBytes,
                sessionToken = pending.sessionToken,
                accountIdentitySeed = accountIdentitySeed,
            )

            // ADR-0018 factor (c): a TOTP-recovered fresh device parked
            // with NO device root key (no password => no export key to
            // derive one from). Bootstrap it from the VERIFIED identity
            // seed — and ONLY here, strictly AFTER the fail-closed
            // verification above: a refusal must leave this device
            // untouched. No-op when a key already exists (password /
            // passkey paths).
            bootstrapDeviceKeyFromSeedBestEffort(
                accountIdB64 = pending.accountIdB64,
                accountIdentitySeed = accountIdentitySeed,
                context = "provideRecoveryPhrase",
            )

            // Cache the phrase seed under EncryptedSharedPreferences so
            // future warm logins skip the prompt. Best-effort.
            try {
                AccountIdentitySeedStore.putAccountIdentitySeed(
                    applicationContext,
                    pending.accountIdB64,
                    accountIdentitySeed,
                )
            } catch (cause: Throwable) {
                Log.w(
                    "lakko-auth",
                    "account-identity-seed persist (provideRecoveryPhrase) failed",
                    cause,
                )
            }
            // The device holds the keys now, so it holds the phrase (this is
            // also where a passkey that carried the keys ends).
            DevicePhrase.store(
                applicationContext,
                pending.accountIdB64,
                canonicalPhrase,
                DevicePhrase.LandingPath.ProvidePhrase,
            )

            // Finish the session: hand the verified phrase seed into the
            // live IdentitySecrets and flip to a full Session.
            installIdentitySecrets(
                pending.accountIdB64,
                IdentitySecrets(
                    accountId = pending.accountIdBytes,
                    exportKey = pending.exportKey.copyOf(),
                    accountIdentitySeed = accountIdentitySeed,
                    session = pendingRecovery.get()?.session ?: pending.session,
                ),
            )
            seedHandedOff = true
            pendingRecovery.set(null)
            // The keys are on this device now; nothing is left to explain.
            mutableRestoreNotice.value = null
            mutableState.value = AuthState.Session(
                account = SessionAccount(
                    accountIdB64 = pending.accountIdB64,
                    username = pending.username,
                ),
            )
        } finally {
            // On every refusal (mismatch, 404, network, FFI throw) the
            // seed never reached the live IdentitySecrets — zeroize it.
            if (!seedHandedOff) {
                accountIdentitySeed.fill(0)
            }
        }
    }

    /**
     * ADR-0073 "Reset account keys" (the successor of the ADR-0017
     * lost-everything fallback): give the account new keys and a new 12-word
     * phrase when the person recovered ACCESS via an ADR-0018 factor but has
     * NO recovery phrase and NO other device. The reset signs every other
     * device of the account out, deletes its passkeys, and is the ONLY path
     * that publishes a CHANGED AccountIdentity key — attested by auth, never
     * with a supersession proof (there is genuinely no old seed to sign
     * with). Contacts see a changed key and re-verify out of band.
     *
     * Reachable ONLY while a [PendingRecovery] is parked (post-ACCESS, from
     * [AuthState.NeedsRecoveryPhrase]) and ONLY via the explicit, typed
     * confirmation. Mirror of the web `AuthContext.mintFreshIdentity`.
     *
     * Order (ADR section 2.3, [KeyReset]):
     *   1. generate the phrase and the seed, derive the new identity;
     *   2. `reset/start` + `reset/finish`: the server step, one transaction,
     *      the point of no return. A failure here changes nothing and the
     *      generated phrase is discarded ([AuthHttpError] / [AuthNetworkError]
     *      propagate to the panel);
     *   3. persist immediately: the new pair (the old one is superseded), the
     *      seed, the phrase, the attestation ([KeyResetOwedStore]) and the
     *      `publish-owed` marker; the DeviceGroup of the old seed is dropped
     *      and the device binding re-minted at the next chat bootstrap;
     *   4. verify the attestation locally against the pinned attest key
     *      (fail closed: an attestation that does not verify is never sent);
     *   5. the directory upload with the attestation, then the DeviceGroup
     *      re-root with it. A step that fails is recorded on
     *      [freshPublishOwed] and retried ([retryFreshPublish], the next
     *      session landing) with the SAME attestation; nothing is minted again;
     *   6. park the phrase ([AuthState.SaveFreshPhrase]); the words are shown
     *      only once both steps landed.
     *
     * This device's own KeyPackage is re-published by the chat bootstrap of
     * the session landing that follows the phrase screen, with a device
     * binding re-signed by the NEW account key
     * ([SessionIdentities.deviceIdentity]).
     *
     * Proven now (UX 15.1a as amended 2026-10-06, section 17.2): both server
     * calls carry the fresh-auth proof a password or passkey sign-in mints.
     * This function never obtains one: the warning screen's proof block does
     * ([proveWithPassword] / [proveWithPasskey]) before it is called, or the
     * person signed in with the password or a passkey less than ten minutes
     * ago. With no live proof nothing is sent.
     *
     * @throws AuthClientArgError when there is no pending recovery.
     * @throws FreshAuthRequired no live proof, or the server refused the
     *   proof (`403 fresh-auth-required`). Nothing was reset.
     */
    public suspend fun mintFreshIdentity(): String {
        val pending = pendingRecovery.get()
            ?: throw AuthClientArgError("no pending identity recovery — sign in first")
        val proof = freshAuth.live(pending.accountIdB64, System.currentTimeMillis())
            ?: throw FreshAuthRequired()
        // ADR-0075 - whether the new words will be shown depends on the
        // account's mode, read with the parked session before the reset
        // (best effort: the flag as last learned otherwise).
        learnPhraseSaved(pending.accountIdB64, pending.sessionToken)

        // Generate a fresh 12-word recovery phrase + its seed (same client-
        // side generation signup uses), and the identity it seeds.
        val (phrase, seed) = RecoveryMnemonic.generate().use { mnemonic ->
            mnemonic.phrase() to mnemonic.toSeed()
        }
        val newSignaturePubkey = try {
            Identity.fromSeed(seed).use { it.signaturePublicKey() }
        } catch (cause: Throwable) {
            seed.fill(0)
            throw cause
        }

        // The server step. Nothing local has changed before it; a failure
        // discards the phrase.
        val reset = try {
            val seedCopy = seed.copyOf()
            try {
                authClientFactory().use {
                    it.resetAccountKeys(
                        sessionToken = sessionTokenProvider.current() ?: pending.sessionToken,
                        freshGrant = proof.grant,
                        seed = seedCopy,
                        newSignaturePubkey = newSignaturePubkey,
                    )
                }
            } finally {
                seedCopy.fill(0)
            }
        } catch (cause: Throwable) {
            seed.fill(0)
            if (isFreshAuthRequired(cause)) {
                // The proof expired between opening the screen and pressing
                // the button: nothing was reset; the screen shows the block.
                freshAuth.clear()
                throw FreshAuthRequired()
            }
            throw cause
        }
        // A key reset spends every proof of the old generation, and the pair
        // it returns carries none.
        freshAuth.clear()
        landKeyReset(
            accountIdB64 = pending.accountIdB64,
            accountIdBytes = pending.accountIdBytes,
            username = pending.username,
            seed = seed,
            phrase = phrase,
            newSignaturePubkey = newSignaturePubkey,
            reset = reset,
            context = "mintFreshIdentity",
        )
        return phrase
    }

    /**
     * Everything a key reset does on this device AFTER its server step
     * answered (ADR-0073 section 2.3 steps 3 to 6): the point of no return.
     * Shared by [mintFreshIdentity] (`reset/finish`) and by the recall
     * ([recallSubmit], `recall/finish`, ADR-0074 section 1.2: "from the
     * reset's client order onward the recall IS a reset"), so there is one
     * such path.
     *
     * Persists the new pair, seed, phrase, attestation and the publish-owed
     * marker before any further network call; drops the DeviceGroup and the
     * device binding of the old key; runs the attested publish (each step
     * recorded, retried from the owed record with the SAME attestation);
     * parks the phrase and flips to [AuthState.SaveFreshPhrase]. The words
     * are shown only once the publish landed. [seed] is owned by the parked
     * phrase from here on.
     */
    private suspend fun landKeyReset(
        accountIdB64: String,
        accountIdBytes: ByteArray,
        username: String,
        seed: ByteArray,
        phrase: String,
        newSignaturePubkey: ByteArray,
        reset: KeyResetResult,
        context: String,
    ) {
        // ---- the point of no return: persist before any further network call
        // The caller's old pair is superseded with everyone else's; install
        // the new one before the next request goes out.
        persistSessionPairBestEffort(accountIdB64, reset.session)
        installSessionPair(accountIdB64, reset.session)
        bootstrapDeviceKeyFromSeedBestEffort(
            accountIdB64 = accountIdB64,
            accountIdentitySeed = seed,
            context = context,
        )
        try {
            AccountIdentitySeedStore.putAccountIdentitySeed(applicationContext, accountIdB64, seed)
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "fresh-identity seed persist failed", cause)
        }
        DevicePhrase.store(applicationContext, accountIdB64, phrase, DevicePhrase.LandingPath.FreshIdentity)
        keyResetOwedStore.save(
            accountIdB64,
            KeyResetOwed(
                attestationB64 = base64Encode(reset.attestation),
                keyGeneration = reset.keyGeneration,
                // The mint time as the attestation carries it, read when it is
                // verified below; the response time is the fallback.
                signedAtMs = System.currentTimeMillis(),
                newSignaturePubkeyB64 = base64Encode(newSignaturePubkey),
            ),
        )
        owedStore.markPhraseOwed(accountIdB64, OwedPhraseOrigin.FreshIdentity)
        owedStore.markPublishOwed(accountIdB64)
        // ADR-0073 phase 2 / UX 16.6 — the channels this account owned under
        // the OLD keys are orphaned by the reset (their owner key derives from
        // the seed that just changed): recorded here, the one moment this
        // device knows, so the channels list says so before the person tries
        // to post. Best-effort.
        me.lakko.app.channels.OrphanedChannelsStore.recordAtReset(applicationContext, accountIdB64)
        // The DeviceGroup this device holds was founded under the OLD seed
        // and its chain on the DS belongs to the old root; the device binding
        // stored for the conversation identity was signed by the old key.
        me.lakko.app.devices.DeviceGroupLifecycle.discardForKeyReset(applicationContext, accountIdB64)
        try {
            DeviceIdentityStore.deleteDeviceBinding(applicationContext, accountIdB64)
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "stale device binding not deleted (re-minted on the next bootstrap)", cause)
        }
        // A conversation-group snapshot of the old identity is unreadable to
        // the new one (MLS forward secrecy); chats come back only from the
        // account's other devices, which the reset just signed out.
        markSignedInNow(accountIdB64)

        mutableResetThisSession.value = true

        // ---- the publish, with the attestation; each step recorded.
        var publishOwed = true
        try {
            runOwedPublish(accountIdB64, accountIdBytes, seed)
            publishOwed = owedStore.isPublishOwed(accountIdB64)
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w(
                "lakko-auth",
                "publishing the reset keys did not finish: " +
                    ((cause as? AuthHttpError)?.status?.toString()
                        ?: (cause as? me.lakko.app.discovery.DiscoveryHttpError)?.status?.toString()
                        ?: cause.javaClass.simpleName),
            )
        }

        // Stash the completion + phrase; flip to the save-phrase state.
        pendingFreshPhrase.set(
            PendingFreshPhrase(
                seed = seed,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
                session = reset.session,
                username = username,
                phrase = phrase,
                publishOwed = publishOwed,
            ),
        )
        pendingRecovery.set(null)
        mutableRestoreNotice.value = null
        mutableState.value = AuthState.SaveFreshPhrase(
            account = SessionAccount(
                accountIdB64 = accountIdB64,
                username = username,
            ),
        )
    }

    // -----------------------------------------------------------------
    // The fresh-auth proof (UX section 17)
    // -----------------------------------------------------------------

    /** The account a proof would be for: the signed-in one, or the one
     *  parked without its keys ([AuthState.NeedsRecoveryPhrase]). */
    private class ProofTarget(val accountIdBytes: ByteArray, val accountIdB64: String, val username: String)

    private fun proofTarget(): ProofTarget? {
        pendingRecovery.get()?.let { return ProofTarget(it.accountIdBytes, it.accountIdB64, it.username) }
        val session = mutableState.value as? AuthState.Session ?: return null
        val secrets = mutableIdentitySecrets.value ?: return null
        if (base64Encode(secrets.accountId) != session.account.accountIdB64) return null
        return ProofTarget(secrets.accountId, session.account.accountIdB64, session.account.username)
    }

    /** Whether a fresh-auth proof is live for this device's account: the
     *  surfaces of UX section 17 draw no proof block while it is. */
    public fun freshProofLive(): Boolean {
        val target = proofTarget() ?: return false
        return freshAuth.hasLive(target.accountIdB64, System.currentTimeMillis())
    }

    /** The live proof's opaque grant for this device's account, or `null`.
     *  Sent on the gated routes; never stored, never logged. */
    public fun freshGrant(): String? {
        val target = proofTarget() ?: return null
        return freshAuth.live(target.accountIdB64, System.currentTimeMillis())?.grant
    }

    /** The server refused the proof (`403 fresh-auth-required`): it is gone. */
    public fun dropFreshProof() {
        freshAuth.clear()
    }

    /** How this session was landed (UX 17.6). */
    public fun sessionLanding(): SessionLanding = sessionLandingKind

    /** Whether this device knows the username of the account a proof is for
     *  (a passkey sign-in that named no account does not): without it the
     *  proof block shows the sign-in screen's username field too. */
    public fun proofUsernameKnown(): Boolean = proofTarget()?.username?.isNotBlank() == true

    /** Whether the reset warning screen must show the proof block (UX
     *  17.2): no live fresh-auth proof is held for the parked account. */
    public fun resetNeedsProof(): Boolean = !freshProofLive()

    /**
     * Whether the account has at least one passkey, asked with the current
     * session; `null` when the server could not say. The proof block offers
     * `Use a passkey instead` only on `true`.
     */
    public suspend fun accountHasPasskey(): Boolean? {
        val token = sessionTokenProvider.current()
            ?: pendingRecovery.get()?.sessionToken
            ?: mutableIdentitySecrets.value?.sessionToken
            ?: return null
        return try {
            WebauthnClient().use { client ->
                client.setSessionToken(token)
                client.listCredentials().credentials.isNotEmpty()
            }
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w("lakko-auth", "could not list the account's passkeys: ${cause.javaClass.simpleName}")
            null
        }
    }

    /**
     * Prove who is asking with the account password (UX 17.1): the same
     * `/login/start` + `/login/finish` the sign-in screen runs, by the login
     * handle of the account's username (or, when this device does not know
     * it, of [typedUsername]). The pair it returns belongs to the same
     * account at the same generation and replaces the live (or parked) one;
     * NO other effect of a sign-in runs: no state change, no landing rule,
     * no wipe, no handle re-bind, no device-key rewrite, no push subscribe.
     * A name that signs in to a DIFFERENT account is answered as a wrong
     * password and nothing of that sign-in is installed.
     *
     * @throws FreshAuthWrongPassword the password is wrong. Nothing changed.
     * @throws AuthClientArgError no account to prove for, or no usable name.
     * @throws AuthHttpError a limit (`429`), an outage.
     * @throws AuthNetworkError offline.
     */
    public suspend fun proveWithPassword(password: ByteArray, typedUsername: String? = null) {
        val target = proofTarget() ?: throw AuthClientArgError(
            "no account to prove for",
            me.lakko.app.l10n.UiText("common.error.session_not_ready"),
        )
        if (password.isEmpty()) throw FreshAuthRequired()
        val parked = pendingRecovery.get()
        val named = resolveNamedSignIn(null, proofSignInName(target.username, typedUsername), loginHandleSource)
        val result = try {
            // The proof's sign-in replaces the session pair, so it must name
            // this device like the pair it replaces.
            authClientFactory().use { it.login(named.handle, password, landingDeviceId(target.accountIdB64)) }
        } catch (cause: FfiException) {
            // The OPAQUE finish refused the server's answer on this device:
            // the password does not open the envelope.
            throw FreshAuthWrongPassword()
        } catch (cause: AuthHttpError) {
            if (isWrongPasswordFailure(cause, localOpaqueFailure = false)) throw FreshAuthWrongPassword()
            throw cause
        }
        try {
            val accountIdB64 = base64Encode(result.accountId)
            if (accountIdB64 != target.accountIdB64) {
                // The name signs in to another account: that is not this
                // account's password. Its session is not installed and its
                // proof is not kept.
                Log.w("lakko-auth", "the proof's password sign-in named another account; refused")
                throw FreshAuthWrongPassword()
            }
            installProofSessionPair(accountIdB64, parked, result.session)
            freshAuth.put(accountIdB64, result.freshGrant, result.freshGrantExpiresAtMs, System.currentTimeMillis())
            if (!freshAuth.hasLive(accountIdB64, System.currentTimeMillis())) {
                throw AuthHttpError(200, "/login/finish", "no fresh-auth proof in the password login's answer")
            }
        } finally {
            result.exportKey.fill(0)
            result.sessionKey.fill(0)
        }
    }

    /**
     * Prove who is asking with a passkey (UX 17.1): the passkey sign-in
     * ceremony run again, limited to this account's passkeys. The pair it
     * returns replaces the live (or parked) one for the SAME account, with
     * no other effect of a sign-in. A passkey of another account installs
     * nothing.
     *
     * @throws PasskeyCancelled the person dismissed the sheet (not an error).
     * @throws FreshAuthOtherAccount the passkey belongs to another account.
     * @throws AuthClientArgError any other failure of the ceremony.
     */
    public suspend fun proveWithPasskey(activityContext: Context) {
        val target = proofTarget() ?: throw AuthClientArgError(
            "no account to prove for",
            me.lakko.app.l10n.UiText("common.error.session_not_ready"),
        )
        val parked = pendingRecovery.get()
        val assertion = runPasskeyCeremony(
            activityContext,
            accountIdB64Url = base64UrlEncodeNoPad(target.accountIdBytes),
        )
        try {
            if (!accountIdOfAssertion(assertion).contentEquals(target.accountIdBytes)) {
                Log.w("lakko-auth", "the proof's passkey belongs to another account; refused")
                throw FreshAuthOtherAccount()
            }
            installProofSessionPair(target.accountIdB64, parked, assertion.session)
            freshAuth.put(
                target.accountIdB64,
                assertion.freshGrant,
                assertion.freshGrantExpiresAtMs,
                System.currentTimeMillis(),
            )
            if (!freshAuth.hasLive(target.accountIdB64, System.currentTimeMillis())) {
                throw AuthClientArgError(
                    "no fresh-auth proof in the passkey sign-in's answer",
                    me.lakko.app.l10n.UiText("proof.passkey.failed"),
                )
            }
        } finally {
            assertion.prfOutput?.fill(0)
        }
    }

    /**
     * Swap in the session pair a proof's sign-in returned, for the same
     * account: the parked pair while the device waits for its keys, else the
     * live session's. If the context changed meanwhile (sign-out, a landing,
     * another account), nothing is installed and the proof is refused.
     */
    private fun installProofSessionPair(accountIdB64: String, parked: PendingRecovery?, pair: SessionPair) {
        if (parked != null) {
            if (!pendingRecovery.compareAndSet(parked, parked.copy(session = pair))) {
                val current = pendingRecovery.get()
                if (current == null || current.accountIdB64 != accountIdB64) {
                    throw AuthClientArgError("no pending identity recovery — sign in first")
                }
                pendingRecovery.set(current.copy(session = pair))
            }
            installPendingSessionPair(accountIdB64, pair)
            return
        }
        val live = mutableIdentitySecrets.value
        if (live == null || base64Encode(live.accountId) != accountIdB64 || signedInAccountIdB64() != accountIdB64) {
            throw AuthClientArgError(
                "the session changed while the proof was checked",
                me.lakko.app.l10n.UiText("common.error.session_not_ready"),
            )
        }
        persistSessionPairBestEffort(accountIdB64, pair)
        installSessionPair(accountIdB64, pair)
        mutableIdentitySecrets.value = live.copy(session = pair)
    }

    // -----------------------------------------------------------------
    // Changing the password needs a proof too (UX 17.8, first bullet)
    // -----------------------------------------------------------------

    /**
     * Whether the password may be replaced now: a live fresh-auth proof, or
     * the local three-word check passed in the last ten minutes
     * ([passwordChangeAllowed]). A bare session is not enough.
     */
    public fun passwordChangeUnlocked(): Boolean {
        val accountIdB64 = signedInAccountIdB64() ?: return false
        val now = System.currentTimeMillis()
        // UX 19.6 - "It was not me" on a device that holds the keys.
        if ((noticePasswordUnlockUntilMs[accountIdB64] ?: 0L) > now) return true
        // A guest that just chose a username sets its FIRST password: the
        // account has no known password to prove.
        if (protectionStore.firstPasswordOwed(accountIdB64)) return true
        return passwordChangeAllowed(
            proofLive = freshAuth.hasLive(accountIdB64, now),
            wordsCheckedAtMs = wordsCheckedAtMs[accountIdB64],
            nowMs = now,
        )
    }

    /**
     * The local three-word check as the gate of a password change, for the
     * person who forgot the password: the words are compared with the phrase
     * this device holds; no server takes part. `true` unlocks the change for
     * ten minutes and clears the budget; `false` is a miss (the caller counts
     * it with [recordThreeWordsMiss]); `null` when this device holds no
     * phrase for the signed-in account. Reads Keystore-backed storage: call
     * it off the main thread.
     */
    public fun checkThreeWordsForPasswordChange(positions: ByteArray, words: List<String>): Boolean? {
        val accountIdB64 = (mutableState.value as? AuthState.Session)?.account?.accountIdB64 ?: return null
        if (!protectionStore.phraseSaved(accountIdB64)) return null
        val phrase = DevicePhrase.read(applicationContext, accountIdB64) ?: return null
        val matched = RecoveryMnemonic.fromPhrase(phrase).use { phraseCheckWords(it, positions, words) }
        if (matched) {
            wordsCheckedAtMs[accountIdB64] = System.currentTimeMillis()
            threeWordsBudgetStore.clear(accountIdB64)
        }
        return matched
    }

    /** Whether this device holds the recovery phrase of the signed-in
     *  account (so the three-word check can be offered). Off the main thread. */
    public fun holdsPhraseForPasswordChange(): Boolean {
        val accountIdB64 = (mutableState.value as? AuthState.Session)?.account?.accountIdB64 ?: return false
        // ADR-0075 - three words exist only for a phrase-protected account.
        if (!threeWordsSurfaceOffered(protectionStore.phraseSaved(accountIdB64))) return false
        return DevicePhrase.read(applicationContext, accountIdB64) != null
    }

    /**
     * Verify a key-reset attestation on this device before it is handed to
     * discovery or the DS (ADR-0073 section 2.2): the signature under the
     * attest verifying key, and its binding to THIS account and the NEW key.
     * The build-time pinned key ([DeviceAttestKey.pinnedKey]) is used when
     * present — fail closed; when no key is pinned the JWKS endpoint is
     * consulted, as every other verifier of that key does, and a fetch that
     * fails leaves the attestation unused ([PublishOwedReason.Unverified],
     * retried later). Freshness is NOT checked here: the 30-day bound is
     * [attestationAgeAt]'s, and the server side checks none.
     *
     * Returns `null` when the attestation is usable, else the reason it is
     * not.
     */
    private suspend fun verifyResetAttestationLocally(
        attestation: ByteArray,
        accountIdBytes: ByteArray,
        newSignaturePubkey: ByteArray,
    ): ResetAttestationVerdict {
        val key = me.lakko.app.devices.DeviceAttestKey.pinnedKey() ?: try {
            me.lakko.app.devices.DeviceAttestKey.fetch()
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w("lakko-auth", "no pinned attest key and the JWKS could not be fetched; the reset attestation is unused")
            return ResetAttestationVerdict.Unusable(PublishOwedReason.Unverified(cause))
        }
        return try {
            val verified = uniffi.lakko_ffi_uniffi.verifyKeyResetAttestation(
                attestation = attestation,
                attestVerifyingKey = key,
                expectedAccountId = accountIdBytes,
                expectedPubkey = newSignaturePubkey,
            )
            ResetAttestationVerdict.Usable(signedAtMs = verified.signedAtMillis)
        } catch (cause: FfiException) {
            Log.w("lakko-auth", "the key-reset attestation does not verify on this device: ${cause.javaClass.simpleName}")
            ResetAttestationVerdict.Unusable(PublishOwedReason.Refused(cause))
        }
    }

    private sealed interface ResetAttestationVerdict {
        class Usable(val signedAtMs: Long) : ResetAttestationVerdict

        class Unusable(val reason: PublishOwedReason) : ResetAttestationVerdict
    }

    /**
     * ADR-0073 section 2.3 step 3: `POST /discovery/identity` with the new
     * bundle, the genesis self-signature and attestation as a first publish
     * carries, AND the key-reset attestation. `409 key-generation-not-newer`
     * on a retry means the upload already landed.
     */
    private suspend fun uploadResetBundle(
        accountIdBytes: ByteArray,
        accountIdentitySeed: ByteArray,
        attestation: ByteArray,
    ): KeyResetStepOutcome {
        val sessionToken = sessionTokenProvider.current()
            ?: mutableIdentitySecrets.value?.sessionToken
            ?: pendingFreshPhrase.get()?.sessionToken
            ?: return KeyResetStepOutcome.Retry(AuthClientArgError("no session to publish with"))
        val error: Throwable? = try {
            val discovery = discoveryClientFactory()
            try {
                discovery.setSessionToken(sessionToken)
                Identity.fromSeed(accountIdentitySeed).use { identity ->
                    val signaturePubkey = identity.signaturePublicKey()
                    discovery.uploadIdentityBundle(
                        accountId = accountIdBytes,
                        signaturePubkey = signaturePubkey,
                        x25519Pubkey = identity.x25519PublicKey(),
                        supersessionProofB64 = null,
                        leafAuth = signLeafGenesis(identity.ed25519Seed(), transparencyAccountKey(accountIdBytes)),
                        genesisAttest = mintGenesisAttestBestEffort(sessionToken, signaturePubkey),
                        keyResetAttest = attestation,
                    )
                }
            } finally {
                discovery.close()
            }
            null
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            cause
        }
        return discoveryResetOutcome(error)
    }

    /**
     * ADR-0073 section 2.3 step 4: found the DeviceGroup anew and re-root its
     * journal with the attested `Reset`
     * ([me.lakko.app.devices.DeviceGroupJournalAccess.reRootAfterKeyReset]).
     */
    private suspend fun reRootDeviceGroup(
        accountIdBytes: ByteArray,
        accountIdB64: String,
        accountIdentitySeed: ByteArray,
        attestationB64: String,
        keyGeneration: Long,
    ): KeyResetStepOutcome =
        try {
            me.lakko.app.ds.DsClient().use { ds ->
                me.lakko.app.devices.DeviceGroupJournalAccess.reRootAfterKeyReset(
                    context = applicationContext,
                    accountIdBytes = accountIdBytes,
                    accountIdB64 = accountIdB64,
                    accountIdentitySeed = accountIdentitySeed,
                    attestationB64 = attestationB64,
                    keyGeneration = keyGeneration,
                    ds = ds,
                )
            }
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            KeyResetStepOutcome.Retry(cause)
        }

    /**
     * "Try again" on the phrase screen while the publish of the reset is
     * owed (UX 15.4): repeats only the publishing steps that did not land,
     * with the stored attestation, and never resets the keys a second time.
     * Throws what the failing step threw (offline, `429`, `503`); on success
     * the phrase screen shows the words.
     */
    public suspend fun retryFreshPublish() {
        val pending = pendingFreshPhrase.get()
            ?: throw AuthClientArgError("no pending fresh-identity phrase")
        if (!pending.publishOwed) return
        val seed = pending.seed.copyOf()
        try {
            runOwedPublish(pending.accountIdB64, pending.accountIdBytes, seed)
        } finally {
            seed.fill(0)
        }
        if (owedStore.isPublishOwed(pending.accountIdB64)) return
        // Only clear the flag on the stash this call started from.
        pendingFreshPhrase.compareAndSet(pending, pending.copy(publishOwed = false))
    }

    /**
     * ADR-0017 lost-everything fallback — finish the session once the user
     * has acknowledged the new recovery phrase. Valid ONLY while a
     * [PendingFreshPhrase] is parked. Hands the fresh seed into the live
     * [IdentitySecrets] (empty exportKey — ACCESS-only) and flips to
     * [AuthState.Session]. Mirror of the web `confirmFreshPhraseSaved`.
     */
    /** The just-minted fresh recovery phrase, for the
     *  [AuthState.SaveFreshPhrase] screen to display. `null` outside that
     *  flow. Kept off the [AuthState] object so it is not threaded through
     *  the router (mirror of the web `freshPhrase()` accessor). */
    public fun freshPhrase(): String? = pendingFreshPhrase.get()?.phrase

    /**
     * ADR-0037 §6 (G-11) — suspend until the user decides whether to accept a
     * STALE backup head. Publishes the prompt on [staleOverridePrompt] and awaits
     * the UI's [resolveStaleOverride]. Returns `true` only on an explicit accept.
     * The `finally` clears the prompt so a cancelled coroutine never leaves a
     * dangling dialog. Mirror of the web `defaultStaleConfirm` seam — but never a
     * silent default: with no UI wired the deferred never completes, so the
     * restore stays blocked rather than auto-accepting (invariant #4).
     */
    private suspend fun awaitStaleOverride(reason: me.lakko.app.backup.RestoreReason): Boolean {
        val deferred = CompletableDeferred<Boolean>()
        staleOverrideDeferred.set(deferred)
        mutableStaleOverridePrompt.value = StaleOverridePrompt(reason)
        return try {
            deferred.await()
        } finally {
            mutableStaleOverridePrompt.value = null
            staleOverrideDeferred.compareAndSet(deferred, null)
        }
    }

    /**
     * ADR-0037 §6 (G-11) — resolve the pending stale-override decision from the
     * UI's destructive confirm dialog. `accepted = true` proceeds with the older
     * (verified-authentic) backup; `false` aborts the restore. No-op when no
     * decision is pending.
     */
    public fun resolveStaleOverride(accepted: Boolean) {
        val deferred = staleOverrideDeferred.getAndSet(null) ?: return
        mutableStaleOverridePrompt.value = null
        deferred.complete(accepted)
    }

    /** Dismiss the backup-rollback hard-refusal alarm banner (ADR-0037 §6). */
    public fun dismissRollbackAlert() {
        mutableRollbackAlert.value = null
    }

    /**
     * ADR-0073 section 4.2 — the directory check of a landing that holds the
     * phrase seed, fire-and-forget: read the published bundle BEFORE any
     * upload; equal keys re-publish ours (where the landing did before),
     * other keys at a higher generation sign this device out
     * ([onKeysSuperseded]), other keys at the same generation show the
     * blocking screen ([onPublishedKeyMismatch]). Never while a reset's
     * publish is owed on this device: the owed retry publishes then.
     *
     * The coroutine owns its copies of [accountIdBytes] and
     * [accountIdentitySeed] and zeroizes the seed. A logout (or a sign-in to
     * another account) while the read is out makes it stand down.
     */
    private fun refreshPublishedIdentityInBackground(
        landing: IdentityLanding,
        accountIdB64: String,
        accountIdBytes: ByteArray,
        accountIdentitySeed: ByteArray,
    ) {
        if (owedStore.isPublishOwed(accountIdB64)) return
        // A rotation this device runs publishes the identity itself, with
        // the proof and the attestation.
        if (rotationOwedStore.load(accountIdB64)?.let { it.commitSent || it.committed } == true) return
        val accountId = accountIdBytes.copyOf()
        val seed = accountIdentitySeed.copyOf()
        pushScope.launch {
            try {
                publishIdentityFor(
                    landing = landing,
                    accountIdB64 = accountIdB64,
                    accountId = accountId,
                    accountIdentitySeed = seed,
                )
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                // The refresh modes never throw on a directory failure; this
                // covers deriving the identity itself.
                Log.w("lakko-auth", "identity bundle refresh failed", cause)
            } finally {
                seed.fill(0)
            }
        }
    }

    /**
     * The silent restore's half (ADR-0073 section 3.3 and 4.2): refresh the
     * access token first - a reset the server already knows about is then
     * learned from the refresh refusal, before anything is read or written -
     * then run the same directory check a sign-in runs. Offline, the refresh
     * fails quietly and the check reads nothing (nothing is uploaded).
     */
    private fun refreshThenCheckPublishedIdentityInBackground(
        accountIdB64: String,
        accountIdBytes: ByteArray,
        accountIdentitySeed: ByteArray,
    ) {
        val accountId = accountIdBytes.copyOf()
        val seed = accountIdentitySeed.copyOf()
        pushScope.launch {
            try {
                sessionTokenProvider.refreshNow()
                if (mutableKeyResetSignOut.value != null) return@launch
                if (mutableIdentitySecrets.value?.accountId?.contentEquals(accountId) != true) return@launch
                if (owedStore.isPublishOwed(accountIdB64)) return@launch
                if (rotationOwedStore.load(accountIdB64)?.let { it.commitSent || it.committed } == true) {
                    return@launch
                }
                publishIdentityFor(
                    landing = IdentityLanding.SESSION_RESTORE,
                    accountIdB64 = accountIdB64,
                    accountId = accountId,
                    accountIdentitySeed = seed,
                )
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                Log.w("lakko-auth", "identity bundle check after restore failed", cause)
            } finally {
                seed.fill(0)
            }
        }
    }

    /**
     * Publish the identity bundle for [landing] (the policy lives on
     * [identityPublicationFor] / [publishIdentity]) from the canonical
     * AccountIdentity derived from [accountIdentitySeed]. The bearer is the
     * provider's current token for the account (the transport substitutes
     * it; the captured one only names the account).
     *
     * The short-lived `Identity` handle is safe HERE because the bundle carries
     * public keys only, and `Identity.fromSeed` is deterministic — the keys are
     * byte-identical to the long-lived Identity `ChatRepository` builds from
     * the same seed. Never mint a KeyPackage from this handle: its private keys
     * would die with it (`ChatRepository.republishKeyPackage` owns KeyPackages).
     *
     * Throws only for [IdentityPublication.GENESIS] (or when the identity cannot
     * be derived); the caller does not own [accountIdentitySeed]'s wipe here.
     */
    private suspend fun publishIdentityFor(
        landing: IdentityLanding,
        accountIdB64: String,
        accountId: ByteArray,
        accountIdentitySeed: ByteArray,
    ): IdentityPublicationResult {
        val pair = sessionTokenProvider.peek()
            ?: mutableIdentitySecrets.value?.session
            ?: throw AuthClientArgError("no session to publish with")
        val sessionToken = pair.accessToken
        val discovery = discoveryClientFactory()
        try {
            discovery.setSessionToken(sessionToken)
            return Identity.fromSeed(accountIdentitySeed).use { identity ->
                val ports = object : IdentityPublicationPorts {
                    override val signaturePubkey: ByteArray = identity.signaturePublicKey()
                    override val x25519Pubkey: ByteArray = identity.x25519PublicKey()
                    override val ownKeyGeneration: Long = pair.keyGeneration
                    override val ownPredecessors: List<ByteArray> = ownPredecessorKeys(accountIdB64)

                    override suspend fun fetchPublished(): PublishedIdentityKeys? =
                        discovery.identityFetch(accountId)?.let {
                            PublishedIdentityKeys(
                                signaturePubkey = base64Decode(it.signature_pubkey),
                                x25519Pubkey = base64Decode(it.x25519_pubkey),
                                keyGeneration = it.key_generation,
                            )
                        }

                    override suspend fun upload(withGenesisAuth: Boolean) {
                        // G-30 #2 (ADR-0019): a publish that may be the account's
                        // first log entry authorises it with a genesis
                        // self-signature over the (account_key, pubkey) binding;
                        // ADR-0036 adds the auth attestation (best-effort, so a
                        // mint failure never blocks the publish).
                        val leafAuth = if (withGenesisAuth) {
                            signLeafGenesis(identity.ed25519Seed(), transparencyAccountKey(accountId))
                        } else {
                            null
                        }
                        val genesisAttest = if (withGenesisAuth) {
                            mintGenesisAttestBestEffort(sessionToken, signaturePubkey)?.also {
                                // ADR-0074 - auth signed this key, so it is
                                // the recorded one.
                                genesisRecorded.markRecorded(accountIdB64, base64Encode(signaturePubkey))
                            }
                        } else {
                            null
                        }
                        discovery.uploadIdentityBundle(
                            accountId = accountId,
                            signaturePubkey = signaturePubkey,
                            x25519Pubkey = x25519Pubkey,
                            supersessionProofB64 = null,
                            leafAuth = leafAuth,
                            genesisAttest = genesisAttest,
                        )
                    }

                    override fun isStillCurrent(): Boolean =
                        mutableIdentitySecrets.value?.accountId?.contentEquals(accountId) == true

                    override fun onStaleKeys(verdict: LandingVerdict) {
                        when (verdict) {
                            LandingVerdict.StaleKeys -> onKeysSuperseded(accountIdB64)
                            LandingVerdict.KeyMismatch -> onPublishedKeyMismatch(accountIdB64)
                            is LandingVerdict.Proceed, LandingVerdict.OwnPredecessorPublished -> Unit
                        }
                    }
                }
                publishIdentity(
                    publication = identityPublicationFor(landing),
                    ports = ports,
                    log = { message, cause -> Log.w("lakko-auth", "$landing: $message", cause) },
                )
            }
        } finally {
            discovery.close()
        }
    }

    public fun confirmFreshPhraseSaved() {
        val parked = pendingFreshPhrase.get()
            ?: throw AuthClientArgError("no fresh identity to confirm")
        if (parked.publishOwed) {
            // The new keys are not published yet; the phrase screen offers
            // "Try again" and does not show the words.
            throw AuthClientArgError(
                "the new keys of the reset are not published yet",
                me.lakko.app.l10n.UiText("recovery_phrase.fresh.publish_owed"),
            )
        }
        // CONSUME the stash so the parked seed cannot be replayed.
        val pending = pendingFreshPhrase.getAndSet(null)
            ?: throw AuthClientArgError("no fresh identity to confirm")
        // The phrase was shown and confirmed on this screen.
        owedStore.clearPhraseOwed(pending.accountIdB64)
        mutablePublishOwedReason.value = null
        // A guest account has no username and so no password to replace; a
        // fresh identity reached through "Forgot password?" keeps the offer
        // (its recovery credential now belongs to the new phrase).
        if (pending.username.isEmpty()) mutableNewPasswordOwed.value = false
        installIdentitySecrets(
            pending.accountIdB64,
            IdentitySecrets(
                accountId = pending.accountIdBytes,
                exportKey = ByteArray(0),
                accountIdentitySeed = pending.seed,
                // The provider may have refreshed the pair since it was parked.
                session = sessionTokenProvider.peek()?.takeIf {
                    accountSegmentOfAccessToken(it.accessToken) == accountSegmentOfAccessToken(pending.session.accessToken)
                } ?: pending.session,
            ),
        )
        sessionStore.save(
            PersistedSession(
                accountIdB64 = pending.accountIdB64,
                username = pending.username,
            ),
        )
        mutableState.value = AuthState.Session(
            account = SessionAccount(
                accountIdB64 = pending.accountIdB64,
                username = pending.username,
            ),
        )
    }

    /**
     * ADR-0036 S6 (G-30 finding 2) — mint an auth-attested genesis
     * attestation for [pubkey] (bearer = [sessionToken]) via
     * `POST /auth/genesis/attest`, additive to the ADR-0019 self-signature.
     *
     * BEST-EFFORT: returns `null` on any failure so a genesis publish is
     * NEVER blocked. During rollout the transparency require-genesis-attest
     * flag defaults OFF — a genesis leaf without an attestation still
     * appends (the self-sig authorises it), and the attestation is attached
     * when the mint succeeds. Once every client mints, the operator can flip
     * the flag on. The bytes ride onto the identity-bundle upload next to the
     * self-sig.
     */
    private suspend fun mintGenesisAttestBestEffort(
        sessionToken: String,
        pubkey: ByteArray,
    ): ByteArray? =
        try {
            authClientFactory().use { it.genesisAttest(sessionToken, pubkey) }
        } catch (cause: Throwable) {
            Log.w(
                "lakko-auth",
                "ADR-0036: genesis attestation mint failed; publishing the " +
                    "genesis bundle WITHOUT it (additive — the transparency " +
                    "require-genesis-attest flag defaults OFF during rollout)",
                cause,
            )
            null
        }

    /**
     * Shared HARD-BLOCK verification (ADR-0017 + lakko-security
     * finding #1): the supplied AccountIdentity seed MUST reproduce
     * the account's PUBLISHED identity key. 404 (null bundle) or a
     * fetch error refuses ([IdentityVerificationUnavailable]); a
     * mismatch refuses ([PhraseMismatch]). Never trust-on-first-use.
     * Used by [provideRecoveryPhrase], [completeFromPairing] and
     * [completeFromDeviceRestore]; the CALLER owns zeroizing the
     * seed on refusal.
     */
    private suspend fun verifySeedAgainstPublishedIdentity(
        accountIdBytes: ByteArray,
        sessionToken: String?,
        accountIdentitySeed: ByteArray,
    ) {
        val derivedPubkey =
            Identity.fromSeed(accountIdentitySeed).use { it.signaturePublicKey() }
        val discovery = discoveryClientFactory()
        try {
            discovery.setSessionToken(sessionToken)
            val bundle = try {
                discovery.identityFetch(accountIdBytes)
            } catch (cause: Throwable) {
                // Could not reach the directory to verify
                // (network/parse). We cannot confirm the seed
                // reproduces the published key, so refuse rather
                // than cache a possibly-wrong identity.
                Log.w(
                    "lakko-auth",
                    "identity-seed verification fetch failed",
                    cause,
                )
                throw IdentityVerificationUnavailable(
                    "Could not reach the directory to verify the " +
                        "restored identity. Check your connection and " +
                        "try again.",
                )
            }
            if (bundle == null) {
                throw IdentityVerificationUnavailable(
                    "No published identity was found to verify against. " +
                        "Try again, or restore with your recovery phrase.",
                    missing = true,
                )
            }
            val published = base64Decode(bundle.signature_pubkey)
            if (!published.contentEquals(derivedPubkey)) {
                throw PhraseMismatch()
            }
        } finally {
            discovery.close()
        }
    }

    /**
     * Another device, on a signed-in device: finish the pending
     * [AuthState.NeedsRecoveryPhrase] with the keys received from ANOTHER
     * DEVICE over the end-to-end encrypted pairing channel instead of a
     * typed phrase. Same fail-closed gate as [provideRecoveryPhrase];
     * prefers the freshly REDEEMED session token when the grant redeem
     * succeeded.
     *
     * Recovery payload v2: what arrives is the 16-byte entropy of the
     * recovery phrase. The phrase and the 64-byte seed are rebuilt from it,
     * the seed is checked against the published identity, and only then are
     * the seed AND the phrase stored on this device.
     *
     * CONSUMES [phraseEntropy] (zeroized here; the derived seed is zeroized
     * on every refusal).
     *
     * @throws PhraseMismatch / IdentityVerificationUnavailable — same
     *         semantics as [provideRecoveryPhrase].
     * @throws AuthClientArgError when there is no pending recovery.
     */
    public suspend fun completeFromPairing(
        phraseEntropy: ByteArray,
        /** The pair the grant redeem issued, when it did; else the parked one. */
        session: SessionPair? = null,
    ) {
        val pending = pendingRecovery.get()
        if (pending == null) {
            phraseEntropy.fill(0)
            throw AuthClientArgError("no pending identity recovery — sign in first")
        }
        // ADR-0074 - the grant redeem named no device (it learned its account
        // from the answer): bind before the pair is used.
        @Suppress("NAME_SHADOWING")
        val session = session?.let { boundSession(pending.accountIdB64, it) }
        // Nothing is written before the check below.
        val keys = keysFromTransferredEntropy(phraseEntropy)
        val phraseSeed = keys.seed
        var seedHandedOff = false
        try {
            verifySeedAgainstPublishedIdentity(
                accountIdBytes = pending.accountIdBytes,
                sessionToken = session?.accessToken ?: pending.sessionToken,
                accountIdentitySeed = phraseSeed,
            )
            // A device parked by a route without a password has NO device
            // root key. Bootstrap it from the VERIFIED transferred seed,
            // strictly AFTER the fail-closed verification above — a refusal
            // must leave this device untouched. No-op when a key exists.
            bootstrapDeviceKeyFromSeedBestEffort(
                accountIdB64 = pending.accountIdB64,
                accountIdentitySeed = phraseSeed,
                context = "completeFromPairing",
            )
            try {
                AccountIdentitySeedStore.putAccountIdentitySeed(
                    applicationContext,
                    pending.accountIdB64,
                    phraseSeed,
                )
            } catch (cause: Throwable) {
                Log.w(
                    "lakko-auth",
                    "account-identity-seed persist (completeFromPairing) failed",
                    cause,
                )
            }
            // This device now holds the phrase like the one that confirmed it.
            DevicePhrase.store(
                applicationContext,
                pending.accountIdB64,
                keys.phrase,
                DevicePhrase.LandingPath.DevicePairing,
            )
            installIdentitySecrets(
                pending.accountIdB64,
                IdentitySecrets(
                    accountId = pending.accountIdBytes,
                    exportKey = pending.exportKey.copyOf(),
                    accountIdentitySeed = phraseSeed,
                    session = session ?: pendingRecovery.get()?.session ?: pending.session,
                ),
            )
            seedHandedOff = true
            pendingRecovery.set(null)
            mutableRestoreNotice.value = null
            mutableState.value = AuthState.Session(
                account = SessionAccount(
                    accountIdB64 = pending.accountIdB64,
                    username = pending.username,
                ),
            )
        } finally {
            if (!seedHandedOff) {
                phraseSeed.fill(0)
            }
        }
    }

    /**
     * ADR-0018 factor (a), path (b) — NO-PASSWORD restore: the fresh
     * device ran the anonymous pairing join and holds the transferred
     * phrase entropy (recovery payload v2; the phrase and the 64-byte seed
     * are rebuilt from it) + the REDEEMED session token (the only ACCESS on
     * this device — there is no password, hence no OPAQUE export key).
     *
     * FAIL CLOSED FIRST — the seed is verified against the published
     * identity BEFORE any persistence; a mismatched / unverifiable
     * seed leaves this device untouched.
     *
     * The device root key (the rehydration existence-gate) is derived
     * from the identity seed via [deriveDeviceRootKeyFromIdentitySeed]
     * — distinct HKDF domain from the exportKey-derived variant.
     * Unlike web, nothing is wrapped under the Android device key
     * (stores ride the Keystore master key), so a later password
     * login overwriting it with the exportKey-derived value is
     * harmless.
     *
     * `IdentitySecrets.exportKey` is set EMPTY on this path — no
     * consumer outside this repository reads it (grep-verified; the
     * canonical identity + backup signing are phrase-seeded per
     * ADR-0017), and a later password login replaces it.
     *
     * ADR-0056 security pass — after the seed verification and before ANY
     * persistence, the typed [username] must resolve at discovery (the
     * ordinary token-spending lookup, the token drawn with the redeemed
     * session) to [accountIdBytes]: a squatted auth login handle can route
     * the pairing request to another account, and discovery is a registry
     * auth does not control. A mismatch or an unknown name refuses with
     * [TypedNameOtherAccount], a lookup that could not run with
     * [TypedNameCheckUnavailable]; nothing is written and the session is
     * never installed. [persistJoin] is the pairing's held writes (device
     * seed, DeviceGroup snapshot, history, state bundle); it runs only once
     * both checks passed, before the session lands. The gates are
     * [runCheckedDeviceRestore].
     *
     * CONSUMES [phraseEntropy] (zeroized here; the derived seed is zeroized
     * on every refusal). [forgotPassword]: the restore was reached through
     * the recovery list, so the new-password step is offered once the
     * session lands.
     */
    public suspend fun completeFromDeviceRestore(
        accountIdBytes: ByteArray,
        accountIdB64: String,
        username: String,
        phraseEntropy: ByteArray,
        session: SessionPair,
        persistJoin: suspend () -> Unit = {},
        forgotPassword: Boolean = false,
    ) {
        // ADR-0074 - the grant redeem named no device: bind before use.
        @Suppress("NAME_SHADOWING")
        val session = boundSession(accountIdB64, session)
        // Nothing is written before the gates of [runCheckedDeviceRestore].
        val keys = keysFromTransferredEntropy(phraseEntropy)
        val phraseSeed = keys.seed
        var seedHandedOff = false
        try {
            runCheckedDeviceRestore(
                username = username,
                accountIdBytes = accountIdBytes,
                phraseSeed = phraseSeed,
                verifySeed = {
                    verifySeedAgainstPublishedIdentity(
                        accountIdBytes = accountIdBytes,
                        sessionToken = session.accessToken,
                        accountIdentitySeed = phraseSeed,
                    )
                },
                lookup = { name -> lookupWithSession(name, accountIdBytes, session.accessToken) },
                persistJoin = persistJoin,
                land = {
                    if (forgotPassword) mutableNewPasswordOwed.value = true
                    landDeviceRestore(
                        accountIdBytes = accountIdBytes,
                        accountIdB64 = accountIdB64,
                        username = username,
                        phraseSeed = phraseSeed,
                        phrase = keys.phrase,
                        session = session,
                    )
                    seedHandedOff = true
                },
            )
        } finally {
            if (!seedHandedOff) {
                phraseSeed.fill(0)
            }
        }
    }

    /**
     * The ordinary username lookup with a session that has not landed yet
     * (ADR-0056 security pass): the `discovery-lookup` token is drawn with
     * [sessionToken] explicitly and never pooled, and the lookup itself goes
     * out on the anonymous client (invariant #5).
     */
    private suspend fun lookupWithSession(
        username: String,
        accountIdBytes: ByteArray,
        sessionToken: String,
    ): ByteArray? = discoveryClientFactory().use { client ->
        client.useTokenSession(
            me.lakko.app.anon.AnonSession(
                accountKey = base64Encode(accountIdBytes),
                bearer = sessionToken,
            ),
        )
        client.lookup(username).account
    }

    /** The writes and the session of a device restore that passed every
     *  gate of [completeFromDeviceRestore]. */
    private fun landDeviceRestore(
        accountIdBytes: ByteArray,
        accountIdB64: String,
        username: String,
        phraseSeed: ByteArray,
        phrase: String,
        session: SessionPair,
    ) {
        val deviceRootKey =
            deriveDeviceRootKeyFromIdentitySeed(phraseSeed, accountIdB64)
        try {
            deviceKeyStore.putDeviceKey(
                context = applicationContext,
                accountIdB64 = accountIdB64,
                rootKey = deviceRootKey,
            )
        } catch (cause: Throwable) {
            Log.w(
                "lakko-auth",
                "device-key persist (completeFromDeviceRestore) failed",
                cause,
            )
        } finally {
            deviceRootKey.fill(0)
        }
        try {
            AccountIdentitySeedStore.putAccountIdentitySeed(
                applicationContext,
                accountIdB64,
                phraseSeed,
            )
        } catch (cause: Throwable) {
            Log.w(
                "lakko-auth",
                "account-identity-seed persist (completeFromDeviceRestore) failed",
                cause,
            )
        }
        // This device now holds the phrase like the one that confirmed it.
        DevicePhrase.store(applicationContext, accountIdB64, phrase, DevicePhrase.LandingPath.DeviceRestore)
        sessionStore.save(
            PersistedSession(
                accountIdB64 = accountIdB64,
                username = username,
            ),
        )
        installIdentitySecrets(
            accountIdB64,
            IdentitySecrets(
                accountId = accountIdBytes.copyOf(),
                exportKey = ByteArray(0),
                accountIdentitySeed = phraseSeed,
                session = session,
            ),
        )
        markSignedInNow(accountIdB64)
        pendingRecovery.set(null)
        mutableRestoreNotice.value = null
        mutableState.value = AuthState.Session(
            account = SessionAccount(
                accountIdB64 = accountIdB64,
                username = username,
            ),
        )
    }

    // -----------------------------------------------------------------
    // Passkey: sign-in, and the keys a passkey can carry
    // -----------------------------------------------------------------

    /**
     * Passkey sign-in. The ceremony is discoverable: it needs no account
     * cached on this device and no typed name. On a device that holds the
     * account's keys it lands signed in. On a device without them it
     * restores the keys when this passkey carries them (its PRF output opens
     * the blob stored for it and the phrase inside matches the account's
     * published identity, fail-closed), and otherwise parks in
     * [AuthState.NeedsRecoveryPhrase] with [restoreNotice] saying why.
     *
     * [usernameHint] names the account on a device that has none cached; it
     * is kept only when discovery resolves it to the signed-in account.
     * [forgotPassword] marks the sign-in as a forgot-password route, so the
     * new-password step is offered once the device holds the keys.
     * [activityContext] hosts the Credential Manager sheet.
     *
     * @throws PasskeyCancelled when the person dismissed the sheet (not an
     *   error: the form returns to idle).
     * @throws AuthClientArgError with the screen's sentence on any other
     *   failure of the ceremony.
     */
    public suspend fun loginWithPasskey(
        activityContext: Context = applicationContext,
        usernameHint: String? = null,
        forgotPassword: Boolean = false,
    ) {
        val assertion = runPasskeyCeremony(activityContext, accountIdB64Url = null)
        val accountIdBytes = accountIdOfAssertion(assertion)
        val accountIdB64 = base64Encode(accountIdBytes)
        // UX section 17 - a passkey sign-in is a fresh-auth proof.
        freshAuth.put(
            accountIdB64,
            assertion.freshGrant,
            assertion.freshGrantExpiresAtMs,
            System.currentTimeMillis(),
        )
        sessionLandingKind = SessionLanding.Passkey
        mutableRestoreNotice.value = null
        val cached = sessionStore.load()
        val username = if (cached != null && cached.accountIdB64 == accountIdB64) {
            cached.username
        } else {
            usernameNamingAccount(usernameHint, accountIdBytes, assertion.sessionToken)
        }
        if (forgotPassword) mutableNewPasswordOwed.value = true
        try {
            landAfterAccessRecovery(
                username = username,
                accountIdBytes = accountIdBytes,
                accountIdB64 = accountIdB64,
                session = assertion.session,
                pushContext = "passkey-login",
                restoreKeys = {
                    try {
                        val outcome = keysFromPasskeyAssertion(assertion)
                        if (outcome != PasskeyKeysOutcome.Restored) {
                            mutableRestoreNotice.value = restoreNoticeOf(outcome)
                        }
                        outcome == PasskeyKeysOutcome.Restored
                    } catch (cause: Throwable) {
                        // The vault or the directory could not be reached.
                        // The person is signed in; the restore prompt's
                        // passkey row retries.
                        Log.w("lakko-auth", "asking the passkey for the keys failed: ${cause.javaClass.simpleName}")
                        false
                    }
                },
            )
        } finally {
            assertion.prfOutput?.fill(0)
        }
    }

    /**
     * The restore prompt's passkey row: only valid in
     * [AuthState.NeedsRecoveryPhrase]. Runs the passkey ceremony for the
     * signed-in account and asks the passkey for the keys.
     * [PasskeyKeysOutcome.Restored] lands the session; the other outcomes
     * leave the prompt up and set [restoreNotice].
     */
    public suspend fun restoreKeysWithPasskey(activityContext: Context): PasskeyKeysOutcome {
        val pending = pendingRecovery.get() ?: throw AuthClientArgError(
            "no pending identity recovery",
            me.lakko.app.l10n.UiText("restore_identity.error.nothing"),
        )
        val assertion = runPasskeyCeremony(
            activityContext,
            accountIdB64Url = base64UrlEncodeNoPad(pending.accountIdBytes),
        )
        try {
            if (!accountIdOfAssertion(assertion).contentEquals(pending.accountIdBytes)) {
                throw AuthClientArgError(
                    "the passkey belongs to another account",
                    me.lakko.app.l10n.UiText("auth.passkey.error.wrong_account"),
                )
            }
            val outcome = keysFromPasskeyAssertion(assertion)
            if (outcome != PasskeyKeysOutcome.Restored) {
                mutableRestoreNotice.value = restoreNoticeOf(outcome)
            }
            return outcome
        } finally {
            assertion.prfOutput?.fill(0)
        }
    }

    /** The session token of the sign-in parked in
     *  [AuthState.NeedsRecoveryPhrase] (signed in, keys not here yet), else
     *  `null`. The restore prompt uses it to ask which passkeys carry keys. */
    public fun pendingSessionToken(): String? = pendingRecovery.get()?.sessionToken

    /**
     * The credential ids (base64url, no padding) of the account's passkeys
     * that restore keys, asked with [sessionToken]; `null` when the server
     * cannot say (it keeps no such keys, or the request failed): every
     * passkey then reads "Signs in only", which is what it can do.
     */
    public suspend fun passkeysWithKeys(sessionToken: String): List<String>? = try {
        authClientFactory().use { it.passkeyVaultList(sessionToken) }
    } catch (cause: Throwable) {
        if ((cause as? AuthHttpError)?.status != HTTP_SERVICE_UNAVAILABLE) {
            Log.w("lakko-auth", "could not list the passkeys that restore keys: ${cause.javaClass.simpleName}")
        }
        null
    }

    /**
     * "Turn on" for one passkey: store the account's keys for it, sealed
     * under the passkey's own secret. Opt-in, per passkey; nothing is stored
     * unless the person chose this. The rule is
     * [me.lakko.app.webauthn.turnOnPasskeyKeys]; this supplies the device's
     * phrase and seed, the limited ceremony, and the seal + upload. The
     * passkey's secret never leaves this device.
     *
     * @throws WebauthnUserCancelledError when the sheet was dismissed.
     */
    public suspend fun turnOnPasskeyKeys(
        activityContext: Context,
        credentialIdB64: String,
        registrationPrfOutput: ByteArray?,
        typedPhrase: String?,
    ): PasskeyKeysChoiceResult {
        val session = mutableState.value as? AuthState.Session
        val secrets = mutableIdentitySecrets.value
        if (session == null || secrets == null) {
            throw AuthClientArgError(
                "no session",
                me.lakko.app.l10n.UiText("common.error.session_not_ready"),
            )
        }
        return me.lakko.app.webauthn.turnOnPasskeyKeys(
            credentialIdB64 = credentialIdB64,
            storedPhrase = DevicePhrase.read(applicationContext, session.account.accountIdB64),
            typedPhrase = typedPhrase,
            identitySeed = secrets.accountIdentitySeed,
            registrationPrfOutput = registrationPrfOutput,
            seedOf = { phrase -> RecoveryMnemonic.fromPhrase(phrase).use { it.toSeed() } },
            evaluatePrf = {
                WebauthnClient().use { client ->
                    client.setSessionToken(secrets.sessionToken)
                    client.evaluatePrfFor(activityContext, credentialIdB64)
                }
            },
            sealAndStore = { phrase, prfOutput ->
                val sealed = RecoveryMnemonic.fromPhrase(phrase).use { passkeyVaultSeal(it, prfOutput) }
                try {
                    authClientFactory().use {
                        it.passkeyVaultPut(secrets.sessionToken, base64UrlDecodeNoPad(credentialIdB64), sealed)
                    }
                    true
                } catch (err: AuthHttpError) {
                    if (err.status == HTTP_SERVICE_UNAVAILABLE) false else throw err
                }
            },
        )
    }

    /** "Stop restoring keys": delete what is stored for one passkey. It then
     *  only signs in; keys already restored on other devices stay there. */
    public suspend fun stopPasskeyKeys(credentialIdB64: String): Boolean {
        val secrets = mutableIdentitySecrets.value ?: throw AuthClientArgError(
            "no session",
            me.lakko.app.l10n.UiText("common.error.session_not_ready"),
        )
        return authClientFactory().use { it.passkeyVaultDelete(secrets.sessionToken, credentialIdB64) }
    }

    /** Run the passkey ceremony (discoverable when [accountIdB64Url] is
     *  `null`, else limited to that account's passkeys), asking for the PRF
     *  evaluation that protects the keys. Failures are rethrown with the
     *  screen's sentence. */
    private suspend fun runPasskeyCeremony(
        activityContext: Context,
        accountIdB64Url: String?,
    ): PasskeyLoginResult {
        val assertion = try {
            WebauthnClient().use {
                it.loginWithPasskey(
                    activityContext,
                    accountIdB64 = accountIdB64Url,
                    // ADR-0074 - the account is known only when the ceremony
                    // was limited to it; a discoverable sign-in binds after.
                    deviceIdB64Url = accountIdB64Url
                        ?.let { url -> landingDeviceId(base64Encode(base64UrlDecodeNoPad(url))) }
                        ?.let(::base64UrlEncodeNoPad),
                )
            }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: WebauthnUserCancelledError) {
            throw PasskeyCancelled()
        } catch (cause: Throwable) {
            throw AuthClientArgError("passkey sign-in failed: ${cause.javaClass.simpleName}", passkeyFailureText(cause))
        }
        val landed = assertion ?: throw PasskeyCancelled()
        // ADR-0074 - a discoverable sign-in learns its account here: bind
        // the session to this device before anything uses it.
        val accountIdB64 = try {
            base64Encode(accountIdOfAssertion(landed))
        } catch (_: Throwable) {
            // The callers refuse a malformed account id themselves.
            return landed
        }
        return landed.withSession(boundSession(accountIdB64, landed.session))
    }

    private fun accountIdOfAssertion(assertion: PasskeyLoginResult): ByteArray {
        val bytes = try {
            base64UrlDecodeNoPad(assertion.accountIdB64)
        } catch (_: IllegalArgumentException) {
            null
        }
        if (bytes == null || bytes.size != ACCOUNT_ID_LENGTH) {
            throw AuthClientArgError(
                "malformed account id in the passkey sign-in",
                me.lakko.app.l10n.UiText("auth.passkey.error.generic"),
            )
        }
        return bytes
    }

    /** Ask the passkey that just signed in for the account's keys, while a
     *  recovery context is parked ([me.lakko.app.webauthn.keysFromPasskey]). */
    private suspend fun keysFromPasskeyAssertion(assertion: PasskeyLoginResult): PasskeyKeysOutcome {
        val credentialId = assertion.credentialId
        return me.lakko.app.webauthn.keysFromPasskey(
            prfOutput = assertion.prfOutput.takeIf { credentialId != null },
            fetchBlob = {
                try {
                    authClientFactory().use { it.passkeyVaultGet(assertion.sessionToken, credentialId!!) }
                } catch (err: AuthHttpError) {
                    if (err.status == HTTP_SERVICE_UNAVAILABLE) null else throw err
                }
            },
            open = { prfOutput, sealed ->
                try {
                    passkeyVaultOpen(prfOutput, sealed).use { it.phrase() }
                } catch (cause: FfiException) {
                    Log.w("lakko-auth", "the keys stored for this passkey did not open")
                    throw PasskeyVaultUnusable(cause)
                }
            },
            providePhrase = { phrase ->
                try {
                    provideRecoveryPhrase(phrase)
                } catch (_: PhraseMismatch) {
                    Log.w("lakko-auth", "the keys stored for this passkey are not this account's")
                    throw PasskeyPhraseNotThisAccount()
                }
            },
        )
    }

    /** [hint] when discovery resolves it to [accountIdBytes], else the empty
     *  name. A passkey assertion carries only the account id, and a typed
     *  name must not be saved for an account it does not name. */
    private suspend fun usernameNamingAccount(
        hint: String?,
        accountIdBytes: ByteArray,
        sessionToken: String,
    ): String {
        val typed = hint?.trim()?.lowercase().orEmpty()
        if (typed.isEmpty()) return ""
        return try {
            val found = lookupWithSession(typed, accountIdBytes, sessionToken)
            if (found != null && found.contentEquals(accountIdBytes)) typed else ""
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            Log.w(
                "lakko-auth",
                "could not check the typed name against the passkey's account: ${cause.javaClass.simpleName}",
            )
            ""
        }
    }

    /**
     * Drop the in-memory session. Keeps [SessionStore] and the
     * Keystore-wrapped device key intact — re-login on the same
     * device stays cheap.
     *
     * Mirror of the web's `logout()`. The cached username is
     * surfaced in [AuthState.Anon.cachedUsername] so the next
     * Login form pre-fills it.
     */
    public fun logout() {
        // ADR-0042, amended 2026-09-30 — before anything else: drop the push
        // wake binding (a wake that lands from here on is refused) and this
        // app's delivered notifications.
        me.lakko.app.push.PushSignOut.clearFirst(applicationContext)
        val cached = sessionStore.load()
        // Drop the persisted bearer token FIRST and synchronously. A relaunch
        // silently restores any session whose token is still on disk
        // (decideSessionRestore), so a logout that left it behind would be
        // undone by the next cold start. The device key + identity seed stay,
        // so re-login on this device stays cheap.
        if (cached != null) {
            deletePersistedSessionTokenBestEffort(cached.accountIdB64, "logout")
        }
        // Wipe the in-memory identity secrets — the user just
        // signed out of this session. The on-disk Keystore
        // envelope (device root key) survives; re-login on the
        // same device repopulates `identitySecrets` cheaply.
        // ADR-0054 - the push unregister below spends one push-register
        // token, drawn with THIS session after the live one is gone.
        val departingSession = currentAnonSession()
        // ADR-0074 section 2 - the session is ended at the server too, so a
        // copy of what this device held cannot keep using the account. Best
        // effort; offline, the server is told at the next launch.
        val departingPair = sessionTokenProvider.peek()
        val departingAccount = signedInAccountIdB64() ?: pendingRecovery.get()?.accountIdB64
        clearIdentitySecrets()
        sessionTokenProvider.clear()
        if (departingPair != null && departingAccount != null) {
            tellServerLoggedOut(departingAccount, departingPair, keepForRetry = true)
        }
        // The account's tokens stay on disk for its next sign-in; only the
        // memory goes (the unregister draws its token from disk).
        me.lakko.app.anon.AnonTokens.releaseSession()
        // ADR-0017: drop any parked fresh-device fail-safe context
        // (a NeedsRecoveryPhrase user who backed out via Sign out).
        clearPendingRecovery()
        mutableState.value = AuthState.Anon(cachedUsername = cached?.username)
        // 1.20d — fire-and-forget push unsubscribe. The
        // EncryptedSharedPreferences pseudonym + the server-
        // side token row both go; subsequent FCM wakes for
        // this device are no-ops until the next subscribe.
        unsubscribePushBestEffort("logout", departingSession)
    }

    /**
     * Drop the in-memory session **and** every piece of the account's local
     * data on this device.
     *
     * Forget device and Delete account both end here (through the one
     * orchestration in `MainActivity`, which stops the chat, contacts,
     * channels and device-sync runtimes first so nothing writes a store back).
     * The on-device wipe itself is [me.lakko.app.account.AccountLocalWipe]:
     * the one list of per-account stores, push binding and notifications
     * first. Distinct from [logout], which keeps all of it for the account's
     * next sign-in. After this the keys come back only from another device,
     * a passkey that restores keys, or the recovery phrase.
     */
    public fun forgetDevice() {
        // ADR-0042, amended 2026-09-30 — first, as in [logout] (the wipe
        // repeats it as its first step; idempotent).
        me.lakko.app.push.PushSignOut.clearFirst(applicationContext)
        val accountIdB64 = identitySecrets.value?.let { base64Encode(it.accountId) }
            ?: sessionStore.load()?.accountIdB64
        // ADR-0074 - read before the wipe: the session is ended at the
        // server as well (one attempt; nothing of it is kept on a device
        // that is forgetting the account).
        val departingPair = sessionTokenProvider.peek()
        if (accountIdB64 != null) {
            me.lakko.app.account.AccountLocalWipe.wipeAccountLocalData(applicationContext, accountIdB64)
        }
        // The signed-in pointer goes whatever account it names: this device is
        // no longer signed in to anything.
        sessionStore.clear()
        // ADR-0054 - the push unregister below spends one push-register
        // token, drawn with THIS session after the live one is gone.
        val departingSession = currentAnonSession()
        clearIdentitySecrets()
        sessionTokenProvider.clear()
        if (departingPair != null && accountIdB64 != null) {
            tellServerLoggedOut(accountIdB64, departingPair, keepForRetry = false)
        }
        // The wipe above removed this account's tokens; this drops only what
        // memory still holds (nothing on disk, no other account's tokens).
        me.lakko.app.anon.AnonTokens.releaseSession()
        // ADR-0017: drop any parked fresh-device fail-safe context.
        clearPendingRecovery()
        mutableProtectStepOwed.value = false
        mutableRecoveryEmail.value = null
        mutableAccountNotices.value = emptyList()
        mutableState.value = AuthState.Anon()
        // 1.20d — full push wipe. Fire-and-forget — the local
        // `DevicePushId.clear()` inside the subscriber runs
        // regardless of the server leg outcome, honouring
        // forgetDevice's "leave NOTHING per-account behind"
        // contract.
        unsubscribePushBestEffort("forgetDevice", departingSession)
    }

    // -----------------------------------------------------------------
    // ADR-0073 section 3.5 / 4.2: this device's keys are not the account's
    // keys any more
    // -----------------------------------------------------------------

    /**
     * A `401 key-generation-superseded` (from the refresh route, a bearer
     * route of auth or discovery) or a landing that found the directory
     * serving other keys at a higher generation: the account's keys were
     * reset on another device. Show the signed-out screen (the app root
     * replaces the app with it the moment [keyResetSignOut] is non-null) and
     * let the root run the full wipe ([wipeAfterKeyReset]) once every
     * runtime is stopped. Idempotent; a no-op for an account that is not
     * this device's.
     */
    public fun onKeysSuperseded(accountIdB64: String) {
        // ADR-0074 section 3.3 - a device that holds the account's keys does
        // not yet know whether this is a reset or a rotation that kept it:
        // the server does not say. It looks for the rotation first.
        if (ownRotationInFlight(accountIdB64)) {
            // This device's own rotation superseded its old pair; the
            // rotation's run installs the new one.
            Log.i("lakko-auth", "superseded by this device's own key rotation; nothing to do")
            return
        }
        val holdsKeys = mutableIdentitySecrets.value?.let { base64Encode(it.accountId) == accountIdB64 } == true
        if (!holdsKeys || mutableKeyResetSignOut.value != null) {
            signOutForKeyReset(accountIdB64, KeyResetSignOut.Kind.Reset)
            return
        }
        pushScope.launch {
            try {
                resolveSupersession(accountIdB64)
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                // Never a wipe on a failure of the looking itself.
                Log.w("lakko-auth", "superseded session not resolved: ${cause.javaClass.simpleName}")
            }
        }
    }

    /**
     * ADR-0073 section 4.2 rule 4 — the directory serves other keys at the
     * SAME generation: an anomaly, not a known reset. The same screen with
     * the mismatch line and the **Sign out** button; the wipe runs on the
     * tap ([wipeAfterKeyReset]), not by itself. Never an upload.
     */
    public fun onPublishedKeyMismatch(accountIdB64: String) {
        signOutForKeyReset(accountIdB64, KeyResetSignOut.Kind.Mismatch)
    }

    /**
     * ADR-0074 section 2.3 — another device of the account removed this
     * one: auth answered `401 device-revoked` (to the refresh, a bearer
     * route or the bind), or this device applied its own removal from the
     * account's DeviceGroup ([onRemovedFromDeviceGroup]). The removed screen
     * replaces the app and the full wipe runs BY ITSELF; no recall record
     * is kept (a removal changes no keys). Never called on a failed request
     * alone, and never on discovery's `403 device-revoked`.
     */
    public fun onDeviceRevoked(accountIdB64: String) {
        signOutForKeyReset(accountIdB64, KeyResetSignOut.Kind.Removed)
    }

    /** This device's own DeviceGroup turned inactive: proof of removal by a
     *  device of the account. Same consequence as [onDeviceRevoked]. */
    public fun onRemovedFromDeviceGroup() {
        val accountIdB64 = signedInAccountIdB64() ?: return
        signOutForKeyReset(accountIdB64, KeyResetSignOut.Kind.Removed)
    }

    private fun signOutForKeyReset(accountIdB64: String, kind: KeyResetSignOut.Kind) {
        // A key reset spends every fresh-auth proof of the old generation.
        if (freshAuth.hasLive(accountIdB64, System.currentTimeMillis())) freshAuth.clear()
        val ownAccount = signedInAccountIdB64()
            ?: pendingRecovery.get()?.accountIdB64
            ?: pendingFreshPhrase.get()?.accountIdB64
        if (ownAccount != accountIdB64) {
            // Signed out already, or another account: nothing of the named
            // account is live on this device, so there is nothing to wipe.
            Log.w("lakko-auth", "key-reset sign-out for an account that is not this device's live one; ignored")
            return
        }
        if (mutableKeyResetSignOut.value != null) return
        val username = sessionStore.load()?.takeIf { it.accountIdB64 == accountIdB64 }?.username
            ?: (mutableState.value as? AuthState.Session)?.account?.username
            ?: ""
        // ADR-0074 section 1.1 - a device that holds the account's keys
        // keeps a recall record through the wipe; one that never held them,
        // and a REMOVED device, keep nothing.
        val holdsKeys = mutableIdentitySecrets.value?.let { base64Encode(it.accountId) == accountIdB64 } == true ||
            pendingFreshPhrase.get()?.accountIdB64 == accountIdB64
        val wipe = supersededWipeFor(kind, holdsKeys)
        Log.w("lakko-auth", "this device's keys are no longer the account's ($kind, $wipe): signing out and wiping")
        mutableKeyResetSignOut.value = KeyResetSignOut(
            accountIdB64 = accountIdB64,
            username = username,
            kind = kind,
            phase = signOutPhaseFor(kind),
            keepsRecall = wipe == SupersededWipe.StagedKeepingRecall,
        )
    }

    // -----------------------------------------------------------------
    // ADR-0074 part 3 (Stage A): changing the account keys, devices kept
    // -----------------------------------------------------------------

    private val pendingRotationStore: PendingRotationStore by lazy { PendingRotationStore.onDevice(applicationContext) }
    private val ownPredecessorStore: OwnPredecessorStore by lazy { OwnPredecessorStore.onDevice(applicationContext) }
    private val preparedRotationStore: PreparedRotationStore by lazy {
        PreparedRotationStore.onDevice(applicationContext)
    }
    private val rotationOwedStore: me.lakko.app.devices.KeyRotationOwedStore by lazy {
        me.lakko.app.devices.KeyRotationOwedStore.onDevice(applicationContext)
    }
    private val rotationNoticeStorage: MarkerStorage by lazy {
        SharedPrefsMarkerStorage(applicationContext, ROTATION_NOTICE_PREFS)
    }

    /** UX 18.3 - this device was told its session is out of date and has
     *  not yet read (or been granted) the new keys: `Updating keys...` where
     *  "Synchronizing..." would be; sends wait. Never a full-screen state. */
    private val mutableKeysUpdating: MutableStateFlow<Boolean> = MutableStateFlow(false)
    public val keysUpdating: StateFlow<Boolean> = mutableKeysUpdating.asStateFlow()

    /** UX 18.3 - "Your account keys were changed on another device. This
     *  device has the new ones." until dismissed (kept across restarts). */
    private val mutableKeysRotatedNotice: MutableStateFlow<Boolean> = MutableStateFlow(false)
    public val keysRotatedNotice: StateFlow<Boolean> = mutableKeysRotatedNotice.asStateFlow()

    /** Bumped each time this device adopted rotated account keys: the app
     *  root re-publishes this device's KeyPackage (every directory row of
     *  the account was deleted by the rotation; the device id stays). */
    private val mutableKeysAdoptedRevision: MutableStateFlow<Long> = MutableStateFlow(0L)
    public val keysAdoptedRevision: StateFlow<Long> = mutableKeysAdoptedRevision.asStateFlow()

    private val supersessionInFlight = java.util.concurrent.atomic.AtomicBoolean(false)

    /** Read the kept-device notice for the signed-in account (at landing). */
    private fun restoreKeysRotatedNotice(accountIdB64: String) {
        mutableKeysRotatedNotice.value = try {
            rotationNoticeStorage.get(ROTATION_NOTICE_PREFIX + accountIdB64) != null
        } catch (_: Throwable) {
            false
        }
    }

    public fun dismissKeysRotatedNotice() {
        signedInAccountIdB64()?.let { accountIdB64 ->
            try {
                rotationNoticeStorage.remove(ROTATION_NOTICE_PREFIX + accountIdB64)
            } catch (_: Throwable) {
                // Shown again at the next launch.
            }
        }
        mutableKeysRotatedNotice.value = false
    }

    /** The keys of this account's own previous identities on this device. */
    private fun ownPredecessorKeys(accountIdB64: String): List<ByteArray> =
        try {
            val rows = ownPredecessorStore.load(accountIdB64)
            rows.map { it.signaturePubkey.copyOf() }.also { rows.forEach { row -> row.zero() } }
        } catch (_: Throwable) {
            emptyList()
        }

    /**
     * The 64-byte seed of the account identity the conversation leaves that
     * exist are bound to: the EARLIEST own predecessor after a rotation
     * (Stage A moves no leaf), else the current seed. The caller zeroes it.
     */
    public fun conversationAccountSeed(): ByteArray? {
        val secrets = mutableIdentitySecrets.value ?: return null
        val accountIdB64 = base64Encode(secrets.accountId)
        return try {
            ownPredecessorStore.conversationSeed(accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: secrets.accountIdentitySeed.copyOf()
    }

    /** This account's own previous identities (seeds included), earliest
     *  first; the caller zeroes each. Empty when it never rotated. */
    public fun ownPredecessors(): List<OwnPredecessor> {
        val accountIdB64 = signedInAccountIdB64() ?: return emptyList()
        return try {
            ownPredecessorStore.load(accountIdB64)
        } catch (_: Throwable) {
            emptyList()
        }
    }

    /** Move the read cursor of a previous identity's mailbox forward. */
    public fun advancePredecessorMailboxCursor(signaturePubkey: ByteArray, cursor: Long) {
        val accountIdB64 = signedInAccountIdB64() ?: return
        try {
            ownPredecessorStore.advanceMailboxCursor(accountIdB64, signaturePubkey, cursor)
        } catch (_: Throwable) {
            // Read again from the old cursor next time.
        }
    }

    /** Whether a rotation this device runs is between its commit and its
     *  local adoption: a `key-generation-superseded` seen then is this
     *  device's own doing and must never be read as a reset elsewhere. */
    private fun ownRotationInFlight(accountIdB64: String): Boolean {
        val owed = try {
            rotationOwedStore.load(accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return false
        return (owed.commitSent || owed.committed) && !owed.adopted
    }

    /** Whether the new words of a rotation this device ran are withheld
     *  (the publish is still owed, UX 18.1). */
    private fun rotationPublishOwed(accountIdB64: String): Boolean =
        try {
            rotationOwedStore.load(accountIdB64)?.let { me.lakko.app.devices.rotationWithholdsPhrase(it) } == true
        } catch (_: Throwable) {
            false
        }

    // ---- the rotating device (the ports of [KeyRotationRunner]) ----------

    private fun rotationSecrets(): IdentitySecrets =
        mutableIdentitySecrets.value ?: throw AuthClientArgError("not signed in")

    /**
     * ADR section 3.2 step 3: generate the new phrase and seed and sign the
     * supersession proof with the OLD account key (14 days), sealed on this
     * device. Idempotent while usable prepared material exists.
     */
    public fun rotationPrepare(nowMs: Long) {
        val secrets = rotationSecrets()
        val accountIdB64 = base64Encode(secrets.accountId)
        preparedRotationStore.load(accountIdB64)?.let { held ->
            val usable = preparedRotationUsable(held.preparedAtMs, nowMs)
            held.zero()
            if (usable) return
        }
        val (entropy, newSeed) = RecoveryMnemonic.generate().use { it.toEntropy() to it.toSeed() }
        try {
            val newEd = Identity.fromSeed(newSeed).use { it.ed25519Seed() }
            val oldEd = Identity.fromSeed(secrets.accountIdentitySeed).use { it.ed25519Seed() }
            val proof = try {
                val nowSecs = nowMs / 1000L
                uniffi.lakko_ffi_uniffi.signSupersessionProof(
                    oldEd,
                    newEd,
                    nowSecs,
                    nowSecs + ROTATION_PROOF_VALIDITY_SECS,
                )
            } finally {
                newEd.fill(0)
                oldEd.fill(0)
            }
            if (!preparedRotationStore.save(accountIdB64, PreparedRotation(entropy, proof, nowMs))) {
                throw IllegalStateException("the new keys could not be stored on this device")
            }
        } finally {
            entropy.fill(0)
            newSeed.fill(0)
        }
    }

    private fun preparedOrThrow(accountIdB64: String): PreparedRotation =
        preparedRotationStore.load(accountIdB64) ?: throw IllegalStateException("no prepared key rotation")

    /** The `key-rotation` envelope of the prepared material (192 bytes). */
    public fun rotationEnvelope(): ByteArray {
        val accountIdB64 = base64Encode(rotationSecrets().accountId)
        val prepared = preparedOrThrow(accountIdB64)
        try {
            return uniffi.lakko_ffi_uniffi.composeKeyRotationEnvelope(
                prepared.preparedAtMs.toULong(),
                prepared.newEntropy,
                prepared.supersessionProof,
            )
        } finally {
            prepared.zero()
        }
    }

    /**
     * ADR section 3.2 step 5: `rotate/start` and `rotate/finish`. On success
     * the new pair is persisted and served before this returns (the old one
     * is superseded). Throws what the client threw.
     */
    public suspend fun rotationCommit(
        freshGrant: String,
        revokeDeviceId: ByteArray?,
        /** ADR-0075 section 3: this rotation is "Save a recovery phrase". */
        phraseSaved: Boolean = false,
    ): me.lakko.app.devices.CommitAnswer.Committed {
        val secrets = rotationSecrets()
        val accountIdB64 = base64Encode(secrets.accountId)
        val prepared = preparedOrThrow(accountIdB64)
        val newSeed = RecoveryMnemonic.fromEntropy(prepared.newEntropy).use { it.toSeed() }
        try {
            val newPubkey = Identity.fromSeed(newSeed).use { it.signaturePublicKey() }
            val token = sessionTokenProvider.current() ?: secrets.sessionToken
            val result = authClientFactory().use {
                it.rotateAccountKeys(
                    sessionToken = token,
                    freshGrant = freshGrant,
                    seed = newSeed,
                    newSignaturePubkey = newPubkey,
                    supersessionProof = prepared.supersessionProof,
                    revokeDeviceIds = listOfNotNull(revokeDeviceId),
                    phraseSaved = phraseSaved,
                )
            }
            if (phraseSaved) {
                // The account is phrase-protected from this answer on, and
                // the notice it causes is this device's own action.
                protectionStore.setPhraseSaved(accountIdB64, true)
                noticeStore.markOwn(accountIdB64, AccountNoticeKind.PhraseSaved, System.currentTimeMillis())
                mutableRecoveryEmail.value = mutableRecoveryEmail.value?.copy(vault = false, phraseSaved = true)
            }
            // The grant the rotation used is of the previous generation and
            // is spent; the answer carries none.
            freshAuth.clear()
            persistSessionPairBestEffort(accountIdB64, result.session)
            sessionTokenProvider.update(result.session)
            return me.lakko.app.devices.CommitAnswer.Committed(
                attestationB64 = base64Encode(result.attestation),
                keyGeneration = result.keyGeneration,
            )
        } finally {
            newSeed.fill(0)
            prepared.zero()
        }
    }

    /**
     * A `rotate/finish` had no answer: ask auth whether the account is at
     * the prepared key, with the session upgrade only that key can sign.
     */
    public suspend fun rotationResolveLostCommit(): me.lakko.app.devices.LostCommit {
        val secrets = rotationSecrets()
        val accountIdB64 = base64Encode(secrets.accountId)
        val prepared = preparedRotationStore.load(accountIdB64) ?: return me.lakko.app.devices.LostCommit.NotCommitted
        val held = sessionTokenProvider.peek() ?: secrets.session
        val newEd = try {
            RecoveryMnemonic.fromEntropy(prepared.newEntropy).use { mnemonic ->
                val seed = mnemonic.toSeed()
                try {
                    Identity.fromSeed(seed).use { it.ed25519Seed() }
                } finally {
                    seed.fill(0)
                }
            }
        } finally {
            prepared.zero()
        }
        val outcome: UpgradeOutcome = try {
            UpgradeOutcome.Upgraded(
                authClientFactory().use { it.sessionUpgrade(held.refreshToken, secrets.accountId, newEd) },
            )
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            upgradeOutcomeOf(cause)
        } finally {
            newEd.fill(0)
        }
        return when (outcome) {
            is UpgradeOutcome.Upgraded -> {
                // A usable pair either way.
                persistSessionPairBestEffort(accountIdB64, outcome.pair)
                sessionTokenProvider.update(outcome.pair)
                if (upgradeProvesKey(held.keyGeneration, outcome.pair)) {
                    me.lakko.app.devices.LostCommit.Committed(outcome.pair.keyGeneration)
                } else {
                    me.lakko.app.devices.LostCommit.NotCommitted
                }
            }
            // The statement by the prepared key was not accepted: the
            // account is not at it.
            UpgradeOutcome.NotAccepted -> me.lakko.app.devices.LostCommit.NotCommitted
            UpgradeOutcome.DeviceRevoked, UpgradeOutcome.RefreshInvalid, is UpgradeOutcome.Unanswered ->
                me.lakko.app.devices.LostCommit.Unknown
        }
    }

    /** ADR section 3.2 step 6 on the rotating device. */
    public fun rotationAdopt() {
        val secrets = rotationSecrets()
        val accountIdB64 = base64Encode(secrets.accountId)
        val prepared = preparedOrThrow(accountIdB64)
        try {
            adoptRotatedKeys(
                accountIdB64 = accountIdB64,
                newEntropy = prepared.newEntropy,
                pair = null,
                asKeptDevice = false,
            )
        } finally {
            prepared.zero()
        }
    }

    /**
     * ADR section 3.5 "Discovery": the directory upload with the proof AND
     * the attestation (the proven-rotation arm). Without the proof it is the
     * attested reset arm (the fallback when the row no longer holds the key
     * the proof was signed by); without the attestation (a commit learned
     * from the session upgrade) the proof alone.
     */
    public suspend fun rotationPublishIdentity(
        attestationB64: String?,
        withProof: Boolean,
    ): me.lakko.app.devices.IdentityPublish {
        val secrets = rotationSecrets()
        val accountIdB64 = base64Encode(secrets.accountId)
        val prepared = preparedRotationStore.load(accountIdB64)
        val proofB64 = prepared?.let { base64Encode(it.supersessionProof) }
        prepared?.zero()
        if (withProof && proofB64 == null) {
            return me.lakko.app.devices.IdentityPublish.Retry(IllegalStateException("no supersession proof"))
        }
        val attestation = attestationB64?.let { base64Decode(it) }
        if (!withProof && attestation == null) {
            return me.lakko.app.devices.IdentityPublish.Retry(IllegalStateException("nothing to publish with"))
        }
        val sessionToken = sessionTokenProvider.current() ?: secrets.sessionToken
        val error: Throwable? = try {
            val discovery = discoveryClientFactory()
            try {
                discovery.setSessionToken(sessionToken)
                Identity.fromSeed(secrets.accountIdentitySeed).use { identity ->
                    val signaturePubkey = identity.signaturePublicKey()
                    if (attestation != null) {
                        val verdict = verifyResetAttestationLocally(attestation, secrets.accountId, signaturePubkey)
                        if (verdict is ResetAttestationVerdict.Unusable) {
                            return me.lakko.app.devices.IdentityPublish.Retry(
                                IllegalStateException("the attestation could not be verified on this device"),
                            )
                        }
                    }
                    discovery.uploadIdentityBundle(
                        accountId = secrets.accountId,
                        signaturePubkey = signaturePubkey,
                        x25519Pubkey = identity.x25519PublicKey(),
                        supersessionProofB64 = if (withProof) proofB64 else null,
                        // With the proof, discovery forwards it as the log
                        // leaf's authorisation; the attested-reset fallback
                        // is a first entry for this key and self-signs.
                        leafAuth = if (withProof) {
                            null
                        } else {
                            signLeafGenesis(identity.ed25519Seed(), transparencyAccountKey(secrets.accountId))
                        },
                        genesisAttest = null,
                        keyResetAttest = attestation,
                    )
                }
            } finally {
                discovery.close()
            }
            null
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            cause
        }
        return me.lakko.app.devices.rotationIdentityPublishOf(error)
    }

    /**
     * ADR section 3.5 "DS": move the DeviceGroup journal's root from the one
     * the OLD seed derives to the one the NEW seed derives, with the
     * attestation and the new account key's endorsement when an attestation
     * is in hand. Run by the rotating device and, without the attestation,
     * by a kept device when it adopts (whoever is first moves it; the other
     * finds it moved). `true` once the new root IS the journal's root.
     */
    public suspend fun rotationMoveDeviceGroupRoot(attestationB64: String?, keyGeneration: Long?): Boolean {
        val secrets = rotationSecrets()
        val accountIdB64 = base64Encode(secrets.accountId)
        val newSeed = secrets.accountIdentitySeed.copyOf()
        val oldSeed = ownPredecessorStore.load(accountIdB64).let { rows ->
            val last = rows.lastOrNull()?.phraseSeed?.copyOf()
            rows.forEach { it.zero() }
            last
        }
        try {
            if (oldSeed == null) return true
            val journalId = uniffi.lakko_ffi_uniffi.deviceGroupJournalId(secrets.accountId)
            val access = me.lakko.app.devices.DeviceGroupJournalAccess
            val oldRoot = access.rootOfSeed(oldSeed, journalId) ?: return false
            val newRoot = access.rootOfSeed(newSeed, journalId) ?: return false
            val evidence = if (attestationB64 != null && keyGeneration != null) {
                access.rootEvidence(newSeed, attestationB64, keyGeneration, journalId, newRoot.publicKey)
            } else {
                null
            }
            val moved = me.lakko.app.ds.DsClient().use { ds ->
                me.lakko.app.ds.transferRootOrConfirm(
                    journalId = journalId,
                    oldRoot = oldRoot,
                    newRoot = newRoot,
                    evidence = evidence,
                    put = { make -> ds.putStatement(make) },
                )
            }
            if (moved) me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.rebindRoot(newSeed)
            return moved
        } finally {
            newSeed.fill(0)
            oldSeed?.fill(0)
        }
    }

    /**
     * The seeds the steps that follow the seed outside MLS move between
     * (ADR section 3.5): the LATEST own predecessor's phrase seed and the
     * session's current one, as fresh copies the caller zeroes. `null` when
     * no predecessor is readable on this device; after an adoption that is
     * a failure of the store, and the callers keep their step owed.
     */
    public fun rotationSeedPair(): Pair<ByteArray, ByteArray>? {
        val secrets = rotationSecrets()
        val accountIdB64 = base64Encode(secrets.accountId)
        val previous = ownPredecessorStore.load(accountIdB64).let { rows ->
            val last = rows.lastOrNull()?.phraseSeed?.copyOf()
            rows.forEach { it.zero() }
            last
        } ?: return null
        return previous to secrets.accountIdentitySeed.copyOf()
    }

    /**
     * ADR section 3.5, "Channels": hand the owner key of every channel this
     * device publishes to from the predecessor's key to the current one.
     * `true` only when every channel has a verdict; throws when the channel
     * list could not be read. Run by the rotating device as a step of its
     * owed record and, idempotently, by a kept device when it adopts.
     */
    public suspend fun rotationMoveOwnedChannels(): Boolean {
        val accountIdB64 = base64Encode(rotationSecrets().accountId)
        // Called only after the new keys were adopted, when a predecessor
        // always exists: none readable is a failure, not "nothing to do".
        val (oldSeed, newSeed) = rotationSeedPair() ?: return false
        return try {
            withContext(Dispatchers.IO) {
                me.lakko.app.channels.KeyRotationChannels.handOverAll(applicationContext, accountIdB64, oldSeed, newSeed)
            }
        } finally {
            oldSeed.fill(0)
            newSeed.fill(0)
        }
    }

    /**
     * ADR section 3.5, "Backup": re-seal the backup under the phrase this
     * device holds NOW and upload it, awaited. Throws on anything but an
     * accepted upload, so the rotation keeps the step owed; nothing here is
     * best-effort. [knownGroupIdsHex] and [wholeStorageSnapshot] come from
     * the chat layer, [syncedState] from the DeviceGroup sync runtime.
     */
    public suspend fun rotationSealBackup(
        wholeStorageSnapshot: ByteArray,
        knownGroupIdsHex: List<String>,
        syncedState: ByteArray?,
    ) {
        val secrets = rotationSecrets()
        val accountIdB64 = base64Encode(secrets.accountId)
        val phrase = DevicePhrase.read(applicationContext, accountIdB64)
            ?: throw IllegalStateException("the new phrase is not readable on this device")
        withContext(Dispatchers.IO) {
            val phraseBytes = phrase.toByteArray(Charsets.UTF_8)
            val phraseSeed = RecoveryMnemonic.fromPhrase(phrase).use { it.toSeed() }
            try {
                // An envelope sealed under the OLD words would be exactly
                // the backup this step exists to replace.
                check(phraseSeed.contentEquals(secrets.accountIdentitySeed)) {
                    "the phrase on this device is not the rotated one"
                }
                val seed = me.lakko.app.backup.ed25519SeedFromIdentitySeed(phraseSeed)
                try {
                    val contacts =
                        me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext, accountIdB64).map { c ->
                            uniffi.lakko_ffi_uniffi.BackupContactRecord(
                                ed25519Pubkey = c.request.fromPubkey,
                                x25519Pubkey = ByteArray(32),
                                displayName = c.displayName ?: "",
                                accountId = c.request.fromAccountId,
                            )
                        }
                    me.lakko.app.backup.BackupClient().use { client ->
                        client.setSessionToken(secrets.sessionToken)
                        client.uploadNow(
                            accountId = secrets.accountId,
                            ed25519Seed = seed,
                            phraseBytes = phraseBytes,
                            contacts = contacts,
                            wholeStorageSnapshot = wholeStorageSnapshot,
                            context = applicationContext,
                            accountIdB64 = accountIdB64,
                            knownGroupIdsHex = knownGroupIdsHex,
                            syncedState = syncedState,
                        )
                    }
                } finally {
                    seed.fill(0)
                }
            } finally {
                phraseSeed.fill(0)
                phraseBytes.fill(0)
            }
            // The scheduled backup has nothing older to send.
            val now = System.currentTimeMillis()
            me.lakko.app.backup.AutoBackupStore.save(
                applicationContext,
                accountIdB64,
                me.lakko.app.backup.AutoBackup.markedUploaded(
                    me.lakko.app.backup.AutoBackupStore.load(applicationContext, accountIdB64),
                    now,
                ),
            )
        }
    }

    /** The publish landed far enough: the new words may be shown. */
    public fun rotationPhraseReleased() {
        mutablePublishOwedRevision.value = mutablePublishOwedRevision.value + 1
        reevaluateRecoveryEmail()
    }

    /**
     * The account keys changed (a rotation ran, here or on another device):
     * the server dropped the email vault, so the status is read again and
     * the automatic re-seal evaluated with the NEW phrase.
     */
    private fun reevaluateRecoveryEmail() {
        val accountIdB64 = mutableIdentitySecrets.value?.let { base64Encode(it.accountId) } ?: return
        resealTried.remove(accountIdB64)
        pushScope.launch {
            try {
                refreshRecoveryEmail()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                Log.w("lakko-auth", "recovery email not re-read: ${cause.javaClass.simpleName}")
            }
        }
    }

    public fun rotationDiscardPrepared() {
        reevaluateRecoveryEmail()
        val accountIdB64 = signedInAccountIdB64() ?: return
        try {
            preparedRotationStore.clear(accountIdB64)
        } catch (_: Throwable) {
            // Dropped by the wipe at the latest.
        }
    }

    // ---- a kept device ----------------------------------------------------

    /**
     * A `key-rotation` envelope dispatched from the account's OWN
     * DeviceGroup (the only place it means anything; the sync runtime is the
     * only caller). Verified with the account key this device holds and
     * stored as PENDING; nothing changes until the server side shows its
     * key as the account's.
     */
    public fun onKeyRotationEnvelope(plaintext: ByteArray) {
        val secrets = mutableIdentitySecrets.value ?: return
        val accountIdB64 = base64Encode(secrets.accountId)
        val nowMs = System.currentTimeMillis()
        val verified = try {
            val held = Identity.fromSeed(secrets.accountIdentitySeed).use { it.signaturePublicKey() }
            uniffi.lakko_ffi_uniffi.verifyKeyRotationEnvelope(plaintext, held, nowMs / 1000L)
        } catch (cause: Throwable) {
            // The code only; never a payload byte.
            Log.w("lakko-auth", "a key-rotation envelope was dropped: ${cause.javaClass.simpleName}")
            return
        }
        val incoming = PendingRotation(
            newSignaturePubkey = verified.newSignaturePubkey,
            newEntropy = verified.newEntropy,
            supersessionProof = verified.supersessionProof,
            preparedAtMs = verified.preparedAtMs.toLong(),
            proofExpiresAtUnixSecs = verified.proofExpiresAtUnixSecs,
        )
        try {
            val held = agedPendingRotations(accountIdB64, nowMs)
            val stored = pendingRotationStore.save(accountIdB64, withPendingRotation(held, incoming))
            held.forEach { it.zero() }
            if (!stored) Log.w("lakko-auth", "a key-rotation envelope could not be stored")
        } finally {
            incoming.zero()
        }
        // The commit may already be visible in the directory.
        pushScope.launch {
            try {
                checkPendingRotations()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                Log.w("lakko-auth", "pending key rotation not checked: ${cause.javaClass.simpleName}")
            }
        }
    }

    /** The pending envelopes, with the expired ones dropped (14 days). */
    private fun agedPendingRotations(accountIdB64: String, nowMs: Long): List<PendingRotation> {
        val held = pendingRotationStore.load(accountIdB64)
        if (held.isEmpty()) return held
        val kept = withoutExpiredRotations(held, nowMs) { preparedAtMs, now ->
            uniffi.lakko_ffi_uniffi.keyRotationPendingExpired(
                preparedAtMs.coerceAtLeast(0).toULong(),
                now.coerceAtLeast(0).toULong(),
            )
        }
        if (kept.size != held.size) {
            pendingRotationStore.save(accountIdB64, kept)
            held.filterNot { it in kept }.forEach { it.zero() }
        }
        return kept
    }

    /**
     * At a landing and after an envelope arrived: when the published
     * identity bundle carries a pending envelope's key, adopt it. Reads the
     * directory; changes nothing on a read failure.
     */
    public suspend fun checkPendingRotations() {
        val secrets = mutableIdentitySecrets.value ?: return
        val accountIdB64 = base64Encode(secrets.accountId)
        if (mutableKeyResetSignOut.value != null) return
        val pending = agedPendingRotations(accountIdB64, System.currentTimeMillis())
        if (pending.isEmpty()) return
        try {
            val published = try {
                val discovery = discoveryClientFactory()
                try {
                    sessionTokenProvider.peek()?.let { discovery.setSessionToken(it.accessToken) }
                    discovery.identityFetch(secrets.accountId)?.let { base64Decode(it.signature_pubkey) }
                } finally {
                    discovery.close()
                }
            } catch (cancel: kotlinx.coroutines.CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                return
            }
            val verdict = keptDeviceVerdict(pending, published, superseded = false, journalRead = true)
            if (verdict is KeptDeviceVerdict.Adopt) {
                resolveSupersession(accountIdB64, candidates = listOf(verdict.pending))
            }
        } finally {
            pending.forEach { it.zero() }
        }
    }

    /**
     * This device's session was answered `key-generation-superseded` (or the
     * directory serves another key at a higher generation), and it holds the
     * account's keys. Before it is treated as a reset: adopt a pending
     * rotation the server accepts, reading the DeviceGroup journal to its
     * head once when none is held. Never wipes on a failed request.
     */
    private suspend fun resolveSupersession(accountIdB64: String, candidates: List<PendingRotation>? = null) {
        if (!supersessionInFlight.compareAndSet(false, true)) return
        mutableKeysUpdating.value = true
        try {
            var journalRead = false
            var waitMs = SUPERSESSION_RETRY_FIRST_MS
            while (true) {
                val secrets = mutableIdentitySecrets.value ?: return
                if (base64Encode(secrets.accountId) != accountIdB64) return
                if (mutableKeyResetSignOut.value != null) return
                val pending = candidates ?: agedPendingRotations(accountIdB64, System.currentTimeMillis())
                val verdict = if (candidates != null) {
                    KeptDeviceVerdict.TryUpgrade(candidates)
                } else {
                    keptDeviceVerdict(pending, publishedKey = null, superseded = true, journalRead = journalRead)
                }
                when (verdict) {
                    is KeptDeviceVerdict.TryUpgrade, is KeptDeviceVerdict.Adopt -> {
                        val tries = (verdict as? KeptDeviceVerdict.TryUpgrade)?.candidates
                            ?: listOf((verdict as KeptDeviceVerdict.Adopt).pending)
                        when (val resolution = keptResolutionOf(tries.map { it to tryUpgrade(secrets, it) })) {
                            is KeptResolution.Adopted -> {
                                adoptRotatedKeys(
                                    accountIdB64 = accountIdB64,
                                    newEntropy = resolution.pending.newEntropy,
                                    pair = resolution.pair,
                                    asKeptDevice = true,
                                )
                                reevaluateRecoveryEmail()
                                return
                            }
                            KeptResolution.Removed -> {
                                signOutForKeyReset(accountIdB64, KeyResetSignOut.Kind.Removed)
                                return
                            }
                            KeptResolution.SignInAgain -> {
                                onRefreshInvalid(accountIdB64)
                                return
                            }
                            KeptResolution.Reset -> {
                                // A directory match that auth does not back
                                // decides nothing.
                                if (candidates != null) return
                                if (journalRead) {
                                    signOutForKeyReset(accountIdB64, KeyResetSignOut.Kind.Reset)
                                    return
                                }
                                journalRead = readDeviceGroupJournalOnce()
                            }
                            KeptResolution.Later -> {
                                if (candidates != null) return
                                kotlinx.coroutines.delay(waitMs)
                                waitMs = (waitMs * 2).coerceAtMost(SUPERSESSION_RETRY_MAX_MS)
                            }
                        }
                    }
                    KeptDeviceVerdict.ReadJournalOnce -> journalRead = readDeviceGroupJournalOnce()
                    KeptDeviceVerdict.TreatAsReset -> {
                        signOutForKeyReset(accountIdB64, KeyResetSignOut.Kind.Reset)
                        return
                    }
                    KeptDeviceVerdict.Nothing -> return
                }
            }
        } finally {
            mutableKeysUpdating.value = false
            supersessionInFlight.set(false)
        }
    }

    /** One upgrade trial: the refresh with a statement by [pending]'s key. */
    private suspend fun tryUpgrade(secrets: IdentitySecrets, pending: PendingRotation): UpgradeOutcome {
        val held = sessionTokenProvider.peek() ?: secrets.session
        val newEd = RecoveryMnemonic.fromEntropy(pending.newEntropy).use { mnemonic ->
            val seed = mnemonic.toSeed()
            try {
                Identity.fromSeed(seed).use { it.ed25519Seed() }
            } finally {
                seed.fill(0)
            }
        }
        return try {
            val pair = authClientFactory().use { it.sessionUpgrade(held.refreshToken, secrets.accountId, newEd) }
            // A credential that was not of a lower generation is refreshed
            // without the statement being read: that proves nothing.
            if (upgradeProvesKey(held.keyGeneration, pair)) {
                UpgradeOutcome.Upgraded(pair)
            } else {
                persistSessionPairBestEffort(base64Encode(secrets.accountId), pair)
                sessionTokenProvider.update(pair)
                UpgradeOutcome.NotAccepted
            }
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            upgradeOutcomeOf(cause)
        } finally {
            newEd.fill(0)
        }
    }

    /** ADR section 3.3 "Order on a superseded device": read the DeviceGroup
     *  journal to its head, once (the DS does not use session tokens). An
     *  unreadable journal counts as read: the device then decides without
     *  it, as the ADR says. */
    private suspend fun readDeviceGroupJournalOnce(): Boolean {
        try {
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.awaitHeadOnce(JOURNAL_HEAD_WAIT_MS)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "the device list's journal could not be read: ${cause.javaClass.simpleName}")
        }
        return true
    }

    /**
     * Adopt rotated account keys (ADR section 3.3 "Adopting", and step 6 of
     * 3.2 on the rotating device): the new seed and phrase replace the old
     * ones, the old identity is kept as this account's predecessor, and
     * NOTHING else local changes - every chat, contact and message stays,
     * the DeviceGroup stays, the device binding stays (Stage A re-binds no
     * leaf). The predecessor is stored BEFORE the seed is overwritten, so no
     * crash loses the key the existing conversations are bound to.
     */
    private fun adoptRotatedKeys(
        accountIdB64: String,
        newEntropy: ByteArray,
        pair: SessionPair?,
        asKeptDevice: Boolean,
    ) {
        val live = mutableIdentitySecrets.value?.takeIf { base64Encode(it.accountId) == accountIdB64 }
            ?: throw AuthClientArgError("no session to adopt the new keys into")
        val (phrase, newSeed) = RecoveryMnemonic.fromEntropy(newEntropy).use { it.phrase() to it.toSeed() }
        val oldSeed = live.accountIdentitySeed
        if (!oldSeed.contentEquals(newSeed)) {
            val oldPubkey = Identity.fromSeed(oldSeed).use { it.signaturePublicKey() }
            val newPubkey = Identity.fromSeed(newSeed).use { it.signaturePublicKey() }
            val kept = ownPredecessorStore.add(
                accountIdB64,
                OwnPredecessor(oldPubkey, oldSeed.copyOf(), System.currentTimeMillis()),
            )
            if (!kept) {
                newSeed.fill(0)
                throw IllegalStateException("the previous account key could not be kept; nothing was changed")
            }
            AccountIdentitySeedStore.putAccountIdentitySeed(applicationContext, accountIdB64, newSeed)
            DevicePhrase.store(applicationContext, accountIdB64, phrase, DevicePhrase.LandingPath.KeyRotation)
            // The same proven-predecessor rule this device applies to its
            // contacts, for its own account.
            me.lakko.app.contacts.ProvenPredecessors.recordProven(
                store = me.lakko.app.contacts.SharedPrefsProvenLinkStore(applicationContext),
                ownerAccountIdB64 = accountIdB64,
                accountIdB64 = accountIdB64,
                oldKeyB64 = base64Encode(oldPubkey),
                newKeyB64 = base64Encode(newPubkey),
            )
            // Auth moved its recorded identity key with the rotation: no
            // genesis mint for the new key.
            genesisRecorded.markRecorded(accountIdB64, base64Encode(newPubkey))
        }
        if (pair != null) {
            persistSessionPairBestEffort(accountIdB64, pair)
            sessionTokenProvider.update(pair)
        }
        val session = pair ?: sessionTokenProvider.peek() ?: live.session
        mutableIdentitySecrets.value = live.copy(accountIdentitySeed = newSeed, session = session)
        // The envelopes did their work (the adopted one, and any that named
        // a key that never became the account's).
        try {
            pendingRotationStore.clear(accountIdB64)
        } catch (_: Throwable) {
            // Aged out later.
        }
        if (asKeptDevice) {
            try {
                rotationNoticeStorage.set(ROTATION_NOTICE_PREFIX + accountIdB64, "1")
            } catch (_: Throwable) {
                // Shown for this process only.
            }
            mutableKeysRotatedNotice.value = true
            Log.i("lakko-auth", "the account keys were changed on another device; this device has the new ones")
            // The DeviceGroup journal's root follows the seed; whichever
            // device gets there first moves it.
            pushScope.launch {
                try {
                    rotationMoveDeviceGroupRoot(attestationB64 = null, keyGeneration = null)
                } catch (cause: Throwable) {
                    if (cause is kotlinx.coroutines.CancellationException) throw cause
                    Log.w("lakko-auth", "device list root not confirmed after adopting: ${cause.javaClass.simpleName}")
                }
            }
            // The channels this device publishes to follow the new owner
            // key: idempotent, whichever device gets there first hands each
            // over (ADR section 3.5, "Channels"). Not this device's owed
            // record: a kept device has none, and the rotating device's
            // record carries the step.
            pushScope.launch {
                try {
                    if (!rotationMoveOwnedChannels()) {
                        Log.w("lakko-auth", "a channel owner handover got no verdict after adopting; the rotating device's step covers it")
                    }
                } catch (cause: Throwable) {
                    if (cause is kotlinx.coroutines.CancellationException) throw cause
                    Log.w("lakko-auth", "channel owner handover not run after adopting: ${cause.javaClass.simpleName}")
                }
            }
        }
        mutableKeysAdoptedRevision.value = mutableKeysAdoptedRevision.value + 1
    }

    /** The refresh credential is gone (`refresh-invalid`): the sign-in
     *  screen, nothing wiped - the keys are still this account's. */
    private fun onRefreshInvalid(accountIdB64: String) {
        if (signedInAccountIdB64() != accountIdB64 && pendingRecovery.get()?.accountIdB64 != accountIdB64) return
        if (mutableKeyResetSignOut.value != null) return
        Log.w("lakko-auth", "the session could not be refreshed: signing out (nothing wiped)")
        logout()
    }

    /**
     * The wipe of the signed-out screen (ADR-0073 section 3.5): the FULL
     * forget-device wipe, run by the app root after it stopped every
     * runtime, with two differences from the tapped Forget device - nothing
     * is published or deleted server-side first (the KeyPackage row is
     * already gone with the reset, and the token is superseded), and the
     * only network call is the best-effort push unregister (nothing else can
     * tear those rows down, G-36) with the anonymous tokens the account
     * still holds. Moves the screen to its "removed" phase when done.
     */
    public fun wipeAfterKeyReset() {
        val signOut = mutableKeyResetSignOut.value ?: return
        if (signOut.phase == KeyResetSignOut.Phase.Removed) return
        val accountIdB64 = signOut.accountIdB64
        me.lakko.app.push.PushSignOut.clearFirst(applicationContext)
        // The unregister draws its token from THIS session's pool, read
        // before the live secrets go.
        val departingSession = currentAnonSession()
            ?: pendingRecovery.get()?.let {
                me.lakko.app.anon.AnonSession(accountKey = it.accountIdB64, bearer = it.sessionToken)
            }
        // ADR-0074 section 1.1 - the recall record is written BEFORE
        // anything is deleted (the seed it holds is about to go). A record
        // that could not be written means the full wipe: nothing half-kept.
        val recallRecord = if (signOut.keepsRecall) writeRecallRecord(accountIdB64, signOut.username) else null
        if (recallRecord != null) {
            me.lakko.app.account.AccountLocalWipe.wipeKeepingRecallRecord(applicationContext, accountIdB64)
        } else {
            me.lakko.app.account.AccountLocalWipe.wipeAccountLocalData(applicationContext, accountIdB64)
        }
        if (sessionStore.load()?.accountIdB64 == accountIdB64) sessionStore.clear()
        clearIdentitySecrets()
        sessionTokenProvider.clear()
        me.lakko.app.anon.AnonTokens.releaseSession()
        clearPendingRecovery()
        pendingFreshPhrase.getAndSet(null)?.seed?.fill(0)
        mutableState.value = AuthState.Anon()
        unsubscribePushBestEffort("key-reset", departingSession)
        if (recallRecord != null) {
            // "Was it you?" replaces the signed-out screen.
            mutableRecall.value = RecallUi(
                accountIdB64 = accountIdB64,
                username = recallRecord.username,
                learnedAtMs = recallRecord.learnedAtMs,
            )
            recallRecord.zero()
            mutableKeyResetSignOut.value = null
        } else {
            mutableKeyResetSignOut.value = signOut.copy(phase = KeyResetSignOut.Phase.Removed)
        }
    }

    // -----------------------------------------------------------------
    // ADR-0074 part 1: "Was it you?" and taking the account back
    // -----------------------------------------------------------------

    /**
     * Build and store the recall record of [accountIdB64] from the seed this
     * device still holds: the 32-byte Ed25519 seed of the account identity,
     * NOT the 64-byte phrase seed. `null` when no seed is held or the record
     * could not be stored (the caller then runs the full wipe).
     */
    private fun writeRecallRecord(accountIdB64: String, username: String): RecallRecord? {
        val phraseSeed = mutableIdentitySecrets.value
            ?.takeIf { base64Encode(it.accountId) == accountIdB64 }?.accountIdentitySeed?.copyOf()
            ?: pendingFreshPhrase.get()?.takeIf { it.accountIdB64 == accountIdB64 }?.seed?.copyOf()
            ?: try {
                AccountIdentitySeedStore.getAccountIdentitySeed(applicationContext, accountIdB64)
            } catch (_: Throwable) {
                null
            }
            ?: return null
        return try {
            val signingSeed = Identity.fromSeed(phraseSeed).use { it.ed25519Seed() }
            val record = RecallRecord(
                accountId = base64Decode(accountIdB64),
                oldAccountSeed = signingSeed,
                username = username,
                learnedAtMs = System.currentTimeMillis(),
            )
            if (recallStore.save(record)) {
                record
            } else {
                record.zero()
                Log.w("lakko-auth", "the recall record could not be stored; running the full wipe")
                null
            }
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "the recall record could not be built: ${cause.javaClass.simpleName}")
            null
        } finally {
            phraseSeed.fill(0)
        }
    }

    /**
     * The launch half: a recall record on disk brings "Was it you?" back
     * (or, past the window by this device's clock, finishes the wipe).
     * `true` when a record was found.
     */
    private fun restoreRecallAtLaunch(): Boolean {
        val record = try {
            recallStore.loadAny()
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "reading the recall record failed: ${cause.javaClass.simpleName}")
            null
        } ?: return false
        val ui = RecallUi(
            accountIdB64 = record.accountIdB64,
            username = record.username,
            learnedAtMs = record.learnedAtMs,
        )
        record.zero()
        if (recallExpired(ui.learnedAtMs, null, System.currentTimeMillis())) {
            endRecall(ui, RecallEnd.NotPossible)
        } else {
            mutableRecall.value = ui
        }
        return true
    }

    /**
     * End the recall without the account being taken back: destroy the
     * record and hand over to the signed-out screen, whose wipe (the FULL
     * one, run by the app root as for any reset) finishes the deletion and
     * then offers `Sign in`.
     */
    private fun endRecall(ui: RecallUi, reason: RecallEnd) {
        Log.w("lakko-auth", "the recall ended without taking the account back ($reason)")
        recallStore.delete(ui.accountIdB64)
        mutableKeyResetSignOut.value = KeyResetSignOut(
            accountIdB64 = ui.accountIdB64,
            username = ui.username,
            kind = KeyResetSignOut.Kind.Reset,
            phase = KeyResetSignOut.Phase.Removing,
            keepsRecall = false,
            notice = recallEndLine(reason),
        )
        mutableRecall.value = null
    }

    private fun applyRecallStep(step: RecallStep, basedOn: RecallUi) {
        when (step) {
            is RecallStep.Stay -> mutableRecall.compareAndSet(basedOn, step.ui)
            is RecallStep.End -> endRecall(basedOn, step.reason)
        }
    }

    private suspend fun readRecallStatus(accountIdB64: String): RecallStatusOutcome {
        val record = recallStore.load(accountIdB64) ?: return RecallStatusOutcome.Refused
        return try {
            RecallStatusOutcome.Open(
                authClientFactory().use { it.recallStatus(record.accountId, record.oldAccountSeed) },
            )
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            recallStatusOutcomeOf(cause)
        } finally {
            record.zero()
        }
    }

    /**
     * Ask the server whether the account can be taken back from this device
     * and apply the answer ([recallAfterStatus]). Returns how long to wait
     * before asking again; `null` when the recall is over.
     */
    public suspend fun recallCheck(): Long? {
        val ui = mutableRecall.value ?: return null
        if (ui.working) return RECALL_BUSY_RECHECK_MS
        // A `finish` that was sent and not answered may have landed: a
        // status asked now could read "no anchor" and destroy the record
        // while the answer can still be asked again. Wait for that first.
        pendingRecallFinish.get()?.let { parked ->
            if (parked.accountIdB64 == ui.accountIdB64 &&
                recallFinishReplayable(parked.firstSentAtMs, System.currentTimeMillis())
            ) {
                return RECALL_BUSY_RECHECK_MS
            }
        }
        val outcome = readRecallStatus(ui.accountIdB64)
        // The person may have moved on (the form, a submit) meanwhile.
        val current = mutableRecall.value ?: return null
        if (current.working) return RECALL_BUSY_RECHECK_MS
        applyRecallStep(recallAfterStatus(current, outcome, System.currentTimeMillis()), current)
        return if (mutableRecall.value == null) null else recallRecheckDelayMs(outcome)
    }

    /** "Yes, it was me" / "Go back" / "No, take the account back" / "Back". */
    public fun recallShow(stage: RecallStage) {
        val ui = mutableRecall.value ?: return
        if (ui.working) return
        if (stage == RecallStage.Form && !recallNoEnabled(ui)) return
        mutableRecall.compareAndSet(ui, ui.copy(stage = stage, line = null))
    }

    /** "Delete and sign in": local only, nothing is sent (ADR 1.1). */
    public fun recallConfirmOwn() {
        val ui = mutableRecall.value ?: return
        if (ui.working) return
        endRecall(ui, RecallEnd.ConfirmedOwn)
    }

    /**
     * "Take the account back": prove this device's old account key and the
     * password the account had before the reset, set [newPassword], and
     * from the server's answer onward run the key reset's client order
     * ([landKeyReset]): a new identity, a new phrase shown last.
     *
     * A failure leaves its line on the form ([recallAfterFailure]) or ends
     * the recall; nothing is thrown for the screen to map.
     */
    public suspend fun recallSubmit(
        oldPassword: ByteArray,
        newPassword: ByteArray,
        /** ADR-0075 section 8 / UX 19.7: `Use my recovery phrase instead` -
         *  the 12 words the account had before the reset, in place of the
         *  password. A phrase that does not parse is a wrong proof. */
        oldPhrase: String? = null,
    ) {
        val oldRecoverySeed: ByteArray? = if (oldPhrase == null) {
            null
        } else {
            try {
                RecoveryMnemonic.fromPhrase(oldPhrase).use { it.toSeed() }
            } catch (_: FfiException) {
                mutableRecall.value?.let { ui ->
                    applyRecallStep(
                        recallAfterFailure(ui, RecallFailure.WrongPassword, null, System.currentTimeMillis()),
                        ui,
                    )
                }
                return
            }
        }
        try {
            recallSubmitWith(oldPassword, newPassword, oldRecoverySeed)
        } finally {
            oldRecoverySeed?.fill(0)
        }
    }

    private suspend fun recallSubmitWith(oldPassword: ByteArray, newPassword: ByteArray, oldRecoverySeed: ByteArray?) {
        val ui = mutableRecall.value ?: return
        val open = ui.availability as? RecallAvailability.Open ?: return
        if (ui.working) return
        val working = ui.copy(working = true, line = null)
        if (!mutableRecall.compareAndSet(ui, working)) return
        val record = recallStore.load(ui.accountIdB64)
        if (record == null) {
            endRecall(working, RecallEnd.Closed)
            return
        }
        // ADR-0074 T5 item 25 - a `finish` of an earlier press that was sent
        // and never answered is asked again FIRST, with the same bytes: if
        // it landed, the account already has the phrase generated then.
        var phrase: String? = null
        var seed: ByteArray? = null
        var result: Pair<ByteArray, RecallResult>? = null
        val parked = pendingRecallFinish.get()?.takeIf { it.accountIdB64 == ui.accountIdB64 }
        if (parked != null) {
            if (!recallFinishReplayable(parked.firstSentAtMs, System.currentTimeMillis())) {
                pendingRecallFinish.compareAndSet(parked, null)
                parked.seed.fill(0)
            } else {
                try {
                    val again = authClientFactory().use { it.recallFinishResend(parked.request) }
                    pendingRecallFinish.compareAndSet(parked, null)
                    phrase = parked.phrase
                    seed = parked.seed
                    result = parked.newSignaturePubkey to again
                } catch (cause: Throwable) {
                    if (cause is kotlinx.coroutines.CancellationException) {
                        record.zero()
                        mutableRecall.compareAndSet(working, working.copy(working = false))
                        throw cause
                    }
                    if (recallFinishAnswerLost(cause)) {
                        // Still no answer: the request and its phrase stay.
                        record.zero()
                        applyRecallStep(
                            recallAfterFailure(working, RecallFailure.Offline, null, System.currentTimeMillis()),
                            working,
                        )
                        return
                    }
                    // A real answer: that finish did not land (or its ten
                    // minutes are over). Its phrase names nothing; carry on
                    // with a new attempt from what was typed now.
                    Log.w("lakko-auth", "the unanswered recall finish did not land: ${cause.javaClass.simpleName}")
                    pendingRecallFinish.compareAndSet(parked, null)
                    parked.seed.fill(0)
                }
            }
        }
        if (result == null) {
            // A new phrase and identity, as a reset makes them: the old key
            // authorises the recall and does not return.
            val (newPhrase, newSeed) = RecoveryMnemonic.generate().use { mnemonic ->
                mnemonic.phrase() to mnemonic.toSeed()
            }
            var newPubkey: ByteArray? = null
            result = try {
                val newSignaturePubkey = Identity.fromSeed(newSeed).use { it.signaturePublicKey() }
                newPubkey = newSignaturePubkey
                val seedCopy = newSeed.copyOf()
                try {
                    newSignaturePubkey to authClientFactory().use {
                        it.recallAccount(
                            accountId = record.accountId,
                            oldAccountSeed = record.oldAccountSeed,
                            anchorSeq = open.status.anchorSeq,
                            oldPassword = oldPassword,
                            newPassword = newPassword,
                            newSeed = seedCopy,
                            newSignaturePubkey = newSignaturePubkey,
                            deviceId = landingDeviceId(ui.accountIdB64),
                            oldRecoverySeed = oldRecoverySeed,
                        )
                    }
                } finally {
                    seedCopy.fill(0)
                }
            } catch (cause: Throwable) {
                record.zero()
                val pubkey = newPubkey
                if (cause is RecallFinishUnanswered && pubkey != null) {
                    // Sent, and no answer was read: the account may already
                    // be at the new keys. Keep the request and the phrase
                    // until the answer is in hand (this process only).
                    pendingRecallFinish.getAndSet(
                        PendingRecallFinish(
                            accountIdB64 = ui.accountIdB64,
                            request = cause.request,
                            firstSentAtMs = cause.firstSentAtMs,
                            seed = newSeed,
                            phrase = newPhrase,
                            newSignaturePubkey = pubkey,
                        ),
                    )?.seed?.fill(0)
                    Log.w("lakko-auth", "the recall finish was sent and not answered; it is asked again on the next try")
                    applyRecallStep(
                        recallAfterFailure(working, RecallFailure.Offline, null, System.currentTimeMillis()),
                        working,
                    )
                    return
                }
                newSeed.fill(0)
                if (cause is kotlinx.coroutines.CancellationException) {
                    mutableRecall.compareAndSet(working, working.copy(working = false))
                    throw cause
                }
                val failure = recallFailureOf(cause)
                Log.w("lakko-auth", "taking the account back did not happen: ${failure.javaClass.simpleName}")
                // A wrong password: how many tries are left comes from a
                // fresh status.
                val statusAfter =
                    if (failure == RecallFailure.WrongPassword) readRecallStatus(ui.accountIdB64) else null
                applyRecallStep(recallAfterFailure(working, failure, statusAfter, System.currentTimeMillis()), working)
                return
            }
            phrase = newPhrase
            seed = newSeed
        }
        val (newSignaturePubkey, recalled) = result
        val landedSeed = seed ?: return
        val landedPhrase = phrase ?: return
        // `finish` answered: the account is this device's again and the
        // record has done its work.
        record.zero()
        recallStore.delete(ui.accountIdB64)
        // ADR-0075 - the staged wipe dropped this device's phrase-saved
        // flag; the anchor's phrase arm says whether the account had a
        // saved phrase, and so whether the new words are shown.
        protectionStore.setPhraseSaved(ui.accountIdB64, open.status.phraseArm)
        val nowMs = System.currentTimeMillis()
        freshAuth.put(ui.accountIdB64, recalled.freshGrant, recalled.freshGrantExpiresAtMs, nowMs)
        sessionLandingKind = SessionLanding.Password
        mutableRecallDone.value = true
        try {
            landKeyReset(
                accountIdB64 = ui.accountIdB64,
                accountIdBytes = base64Decode(ui.accountIdB64),
                username = ui.username,
                seed = landedSeed,
                phrase = landedPhrase,
                newSignaturePubkey = newSignaturePubkey,
                reset = recalled.reset,
                context = "recall",
            )
        } finally {
            mutableRecall.value = null
        }
    }

    /** The signed-out screen's one button: to the sign-in screen. */
    public fun dismissKeyResetSignOut() {
        val signOut = mutableKeyResetSignOut.value ?: return
        if (signOut.phase != KeyResetSignOut.Phase.Removed) return
        mutableKeyResetSignOut.value = null
    }

    /**
     * Phase 2a1d — fetch + decrypt + parse the prior backup
     * envelope for this account, if any. Best-effort; logs
     * every non-`restored` outcome and continues.
     *
     * Called from [recover] BEFORE [bootstrapBackupBestEffort]
     * so the new baseline upload can carry the merged set
     * instead of overwriting the server blob with an empty
     * list. On Android the application of restored contacts
     * into [me.lakko.app.contacts.ContactsRepository] is not
     * yet wired — the contacts list is in-memory only and
     * there's no persistent target to upsert into (mirror of
     * iOS); this helper logs the parsed-contact count so the
     * wire-decryption end of the flow is exercised today, and
     * the merge-into-state layer lands alongside the Android
     * contact-persistence sub-sprint.
     */
    /**
     * Returns the prior MLS whole-storage snapshot (if any) so
     * the bootstrap helper can re-publish it instead of nuking
     * the server-side snapshot with an empty one. Empty
     * `ByteArray` when there is no prior backup or the prior
     * backup pre-dates 2a3d.
     */
    private suspend fun restoreFromServerBackupBestEffort(
        sessionToken: String,
        accountId: ByteArray,
        accountIdB64: String,
        phrase: String,
        context: String,
    ): RestoredBackup {
        val phraseBytes = phrase.toByteArray(Charsets.UTF_8)
        val client = me.lakko.app.backup.BackupClient()
        try {
            client.setSessionToken(sessionToken)
            // ADR-0037 §6 (G-11): arm the rollback gate. A HARD refusal alarms +
            // applies no backup; a STALE head suspends on the user's destructive
            // confirm (never silent-accept, invariant #4); a clean proceed /
            // fail-open opens the envelope + advances the monotone floor.
            val gate = me.lakko.app.backup.RestoreGateContext(
                accountId = accountId,
                accountIdB64 = accountIdB64,
                confirmStaleOverride = { reason -> awaitStaleOverride(reason) },
            )
            val parsed = when (
                val outcome = client.restore(applicationContext, phraseBytes, gate)
            ) {
                is me.lakko.app.backup.GatedRestoreOutcome.Restored -> outcome.payload
                is me.lakko.app.backup.GatedRestoreOutcome.NoBackup -> {
                    Log.i("lakko-auth", "backup restore ($context): no prior backup")
                    return RestoredBackup(ByteArray(0), emptyList(), ByteArray(0))
                }
                is me.lakko.app.backup.GatedRestoreOutcome.WrongPhraseOrTampered -> {
                    Log.w("lakko-auth", "backup restore ($context): wrong phrase or tampered")
                    return RestoredBackup(ByteArray(0), emptyList(), ByteArray(0))
                }
                is me.lakko.app.backup.GatedRestoreOutcome.RollbackBlocked -> {
                    if (outcome.hard) {
                        // Unambiguous tamper / corruption — alarm + refuse (no
                        // backup applied). Raise the dismissible security banner.
                        Log.e(
                            "lakko-auth",
                            "backup restore ($context): ROLLBACK BLOCKED (hard) - ${outcome.reason.diagnostic}",
                        )
                        mutableRollbackAlert.value = BackupRollbackAlert(outcome.reason)
                    } else {
                        // Stale head the user DECLINED to override.
                        Log.w(
                            "lakko-auth",
                            "backup restore ($context): stale head override declined - ${outcome.reason.diagnostic}",
                        )
                    }
                    return RestoredBackup(ByteArray(0), emptyList(), ByteArray(0))
                }
                is me.lakko.app.backup.GatedRestoreOutcome.TransportFailed -> {
                    Log.w("lakko-auth", "backup restore ($context) failed", outcome.cause)
                    return RestoredBackup(ByteArray(0), emptyList(), ByteArray(0))
                }
            }
            Log.i(
                "lakko-auth",
                "backup restore ($context): restored ${parsed.acceptedContacts.size} contact(s) " +
                    "from snapshot at ${parsed.createdAtMs}, " +
                    "MLS snapshot ${parsed.wholeStorageSnapshot.size} bytes",
            )
            // The returned `wholeStorageSnapshot` IS applied locally by the
            // recover caller: it writes the blob to `IdentitySnapshotStore`
            // before flipping to `AuthState.Session`, so `ChatRepository`'s
            // rehydrate path restores the same MLS group state the
            // upload-side device had (mirror of the iOS recover flow). The
            // caller also re-publishes it on the next /backup PUT so a
            // further device-recover sees it too.
            //
            // Contact-in-backup restore (ADR-0021): reconstruct the accepted
            // contacts (with their account_id) and MERGE into the persistent
            // store — skip a pubkey already present so a contact accepted after
            // the backup is not clobbered. The backup does not carry the inbox
            // metadata, so reconstruct an IncomingContactRequest with sentinels:
            // a negative messageId (never collides with a real DS id), a zero
            // pseudonym, no intro.
            try {
                // The RESTORED account's list, whatever session is bound.
                val existing =
                    me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext, accountIdB64)
                val restored =
                    me.lakko.app.contacts.reconstructRestoredContacts(
                        existing,
                        parsed.acceptedContacts,
                        parsed.createdAtMs.toULong(),
                    )
                if (restored.isNotEmpty()) {
                    me.lakko.app.contacts.AcceptedContactsStore.save(
                        applicationContext,
                        accountIdB64,
                        existing + restored,
                    )
                    Log.i(
                        "lakko-auth",
                        "backup restore ($context): merged ${restored.size} contact(s)",
                    )
                }
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "backup restore ($context): contact merge failed", cause)
            }
            // The index the snapshot cannot supply: `restoreStorage` lays
            // down a provider holding every group, but nothing can enumerate
            // it, so `ChatRepository.bootstrap` rebuilds handles from a
            // separately persisted set of ids. Without this the restored
            // device held every group and listed none.
            val groupIdsHex = parsed.knownGroupIds.map { id ->
                id.joinToString("") { b -> "%02x".format(b) }
            }
            if (groupIdsHex.isNotEmpty()) {
                Log.i(
                    "lakko-auth",
                    "backup restore ($context): ${groupIdsHex.size} known group(s)",
                )
            }
            // ADR-0058 section 5.2 — the folded synced state (folders, pins,
            // mutes, archive, read cursors, drafts, order). Seeded into the
            // store the sync runtime reads at bootstrap, merged with anything
            // this device already held, so a restore with no other device
            // alive still gets them back.
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.seedFromBackup(
                context = applicationContext,
                accountIdB64 = accountIdB64,
                bytes = parsed.syncedState,
            )
            return RestoredBackup(parsed.wholeStorageSnapshot, groupIdsHex, parsed.syncedState)
        } finally {
            client.close()
            phraseBytes.fill(0)
        }
    }

    /** What a restore recovered: the provider snapshot and the group index
     *  that snapshot cannot supply on its own. */
    private data class RestoredBackup(
        val snapshot: ByteArray,
        val knownGroupIdsHex: List<String>,
        /** ADR-0058 — `SyncedState.toBytes()` from the backup, empty when none. */
        val syncedState: ByteArray,
    )

    /**
     * Phase 2a1c — post a baseline backup envelope. Called from [signup]
     * and [recover] right after the state flip (the landing itself already
     * stored the phrase, [DevicePhrase]). Best-effort end-to-end; every step logs
     * and continues on failure so a transient transport hiccup
     * does not block the user from landing in a working session.
     *
     * Synchronous in the suspend context — the caller is
     * already on the IO dispatcher.
     */
    private suspend fun bootstrapBackupBestEffort(
        accountId: ByteArray,
        accountIdB64: String,
        exportKey: ByteArray,
        sessionToken: String,
        phrase: String,
        context: String,
        wholeStorageSnapshot: ByteArray = ByteArray(0),
        syncedState: ByteArray? = null,
    ) {
        val phraseBytes = phrase.toByteArray(Charsets.UTF_8)
        val backupClient = me.lakko.app.backup.BackupClient()
        try {
            backupClient.setSessionToken(sessionToken)
            // ADR-0017: sign the backup with the phrase-derived
            // AccountIdentity ed25519 seed (stable across password
            // changes), matching web/iOS — NOT the password exportKey.
            // bootstrapBackupBestEffort already holds the phrase, so derive
            // the identity seed here rather than threading a new param.
            val accountIdentitySeed =
                RecoveryMnemonic.fromPhrase(phrase).use { it.toSeed() }
            val seed = try {
                me.lakko.app.backup.ed25519SeedFromIdentitySeed(accountIdentitySeed)
            } catch (cause: Throwable) {
                accountIdentitySeed.fill(0)
                Log.w("lakko-auth", "backup seed-derive ($context) failed", cause)
                return
            }
            accountIdentitySeed.fill(0)
            // Contact-in-backup (ADR-0021): include the accepted contacts so a
            // restore on a fresh device rebuilds the list + resolves group-call
            // roster names. The native AcceptedContact carries no X25519 pubkey,
            // so zero-fill it (matches web's legacy-contact handling); the
            // backup carries ed25519 + display name + the 16-byte account_id.
            // displayName stays null/"" when unset so a restore does not promote
            // the fingerprint fallback into a "chosen" name.
            val contactRecords =
                me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext, accountIdB64).map { c ->
                    uniffi.lakko_ffi_uniffi.BackupContactRecord(
                        ed25519Pubkey = c.request.fromPubkey,
                        x25519Pubkey = ByteArray(32),
                        displayName = c.displayName ?: "",
                        accountId = c.request.fromAccountId,
                    )
                }
            try {
                backupClient.uploadNow(
                    accountId = accountId,
                    ed25519Seed = seed,
                    phraseBytes = phraseBytes,
                    contacts = contactRecords,
                    wholeStorageSnapshot = wholeStorageSnapshot,
                    // ADR-0037 (G-11): mint the auth-signed head + persist the
                    // monotonic version floor under the Keystore-backed store.
                    context = applicationContext,
                    accountIdB64 = accountIdB64,
                    syncedState = syncedState,
                )
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "backup baseline upload ($context) failed", cause)
            } finally {
                seed.fill(0)
            }
        } finally {
            backupClient.close()
            phraseBytes.fill(0)
        }
    }

    /**
     * After a recover: re-publish the bundle from the UNCHANGED phrase seed
     * (ADR-0017: a recover rotates the password, never the identity), read
     * before write ([IdentityPublication.REPUBLISH]): a directory serving
     * other keys at a higher generation means the account was reset on
     * another device and THIS phrase is the old one - the signed-out screen,
     * never an upload. Best-effort: the session is already live. The
     * KeyPackage is NOT published from the short-lived handle; the chat
     * bootstrap after a recover does that.
     */
    private suspend fun republishBundleAfterRecover(
        accountIdB64: String,
        accountId: ByteArray,
        accountIdentitySeed: ByteArray,
    ) {
        if (owedStore.isPublishOwed(accountIdB64)) return
        val outcome = try {
            publishIdentityFor(
                landing = IdentityLanding.RECOVER,
                accountIdB64 = accountIdB64,
                accountId = accountId,
                accountIdentitySeed = accountIdentitySeed,
            )
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException) throw cause
            Log.w("lakko-auth", "post-recover identity bundle publish failed", cause)
            return
        }
        if (outcome is IdentityPublicationResult.Uploaded) {
            Log.i(
                "lakko-auth",
                "post-recover: re-published identity bundle for " +
                    "$accountIdB64 WITHOUT a supersession proof " +
                    "(ADR-0017 — phrase-seeded identity unchanged)",
            )
        }
    }

    /** 1.20d — Fire ``PushSubscriber.subscribe`` on the
     *  application-scoped [pushScope] (not the suspend caller's
     *  scope, so cancellation of the auth flow doesn't strand
     *  the subscribe in flight). Errors are logged inside the
     *  subscriber + here, never thrown past the auth flow —
     *  push failure must not block sign-in. */
    private fun subscribePushBestEffort(context: String) {
        // ADR-0042, amended 2026-09-30 — the pseudonym is registered FOR the
        // account signed in right now, and the wake binding names it. Read
        // here, synchronously, so the account cannot change under the launch.
        val accountIdB64 = signedInAccountIdB64() ?: run {
            Log.w("lakko-auth", "push subscribe ($context) skipped: no signed-in account")
            return
        }
        pushScope.launch {
            try {
                PushSubscriber(applicationContext).subscribe(
                    accountIdB64 = accountIdB64,
                    stillSignedIn = { signedInAccountIdB64() == accountIdB64 },
                )
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "push subscribe ($context) failed", cause)
            }
        }
    }

    /** The account of the current session, whichever post-ACCESS state it
     *  is in, or `null` when signed out. */
    private fun signedInAccountIdB64(): String? =
        when (val state = mutableState.value) {
            is AuthState.Session -> state.account.accountIdB64
            is AuthState.NeedsRecoveryPhrase -> state.account.accountIdB64
            is AuthState.SaveFreshPhrase -> state.account.accountIdB64
            AuthState.Loading, is AuthState.Anon -> null
        }

    /** 1.20d — Mirror of [subscribePushBestEffort] for the
     *  unregister path. */
    private fun unsubscribePushBestEffort(
        context: String,
        session: me.lakko.app.anon.AnonSession?,
    ) {
        pushScope.launch {
            try {
                PushSubscriber(applicationContext).unsubscribe(session)
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "push unsubscribe ($context) failed", cause)
            }
        }
    }

    /** Wipe the stashed export key bytes + drop the flow value.
     *  Called from [logout] / [forgetDevice]. The `.fill(0)` is
     *  best-effort: the buffer may still live elsewhere in heap
     *  if callers cloned it, but our local copy goes away
     *  promptly. */
    /** The anonymous-token session of the signed-in account, or `null`. */
    private fun currentAnonSession(): me.lakko.app.anon.AnonSession? =
        identitySecrets.value?.let {
            me.lakko.app.anon.AnonSession(
                accountKey = base64Encode(it.accountId),
                bearer = it.sessionToken,
            )
        }

    private fun clearIdentitySecrets() {
        mutableIdentitySecrets.value?.let { secrets ->
            secrets.exportKey.fill(0)
            // ADR-0017: the AccountIdentity (phrase) seed is the
            // highest-sensitivity in-memory secret — wipe it too.
            secrets.accountIdentitySeed.fill(0)
            secrets.accountId.fill(0)
        }
        mutableIdentitySecrets.value = null
        // ADR-0075 - what was read for the departing session goes with it.
        mutableRecoveryEmail.value = null
        mutableRecoveryAddress.value = null
        loginHandleRebinder.forgetConfirmed()
        mutableAccountNotices.value = emptyList()
        mutableProtectStepOwed.value = false
        // No session, no contact list: an account-less read is empty and an
        // account-less write is refused until the next landing binds again.
        me.lakko.app.contacts.AcceptedContactsStore.unbind()
    }

    /** ADR-0017 — drop any parked fresh-device fail-safe context,
     *  wiping the export-key copy it retained. Called from
     *  [logout] / [forgetDevice] so a backed-out NeedsRecoveryPhrase
     *  user leaves no secret material behind. */
    private fun clearPendingRecovery() {
        pendingRecovery.getAndSet(null)?.let { pending ->
            pending.exportKey.fill(0)
            pending.accountIdBytes.fill(0)
        }
        pendingFreshPhrase.getAndSet(null)?.let { it.seed.fill(0) }
        // The fresh-auth proof belongs to the sign-in that is ending.
        freshAuth.clear()
        sessionLandingKind = SessionLanding.Other
        wordsCheckedAtMs.clear()
        // What only makes sense inside the sign-in that is ending. The owed
        // markers stay on disk for a logout (the phrase is shown again at the
        // next session); Forget device removes them with the account's data.
        mutableSignupPhrase.value = null
        mutableRestoreNotice.value = null
        mutableNewPasswordOwed.value = false
        mutablePublishOwedReason.value = null
        mutableResetThisSession.value = false
    }
}

// =============================================================================
// State types
// =============================================================================

/** Discriminated union for the auth state — drives the UI router. */
public sealed interface AuthState {
    /** Initial state before [AuthRepository] finishes rehydrating
     *  from disk. Practically invisible because rehydration is
     *  synchronous, but kept for symmetry with the web side. */
    public data object Loading : AuthState

    /** No on-device session. Optionally carries the cached username
     *  so the Login form (1.15d5) can pre-fill it. */
    public data class Anon(val cachedUsername: String? = null) : AuthState

    /** Active session. */
    public data class Session(val account: SessionAccount) : AuthState

    /** ADR-0017 fresh-device fail-safe. The user authenticated
     *  successfully (password, passkey, authenticator code) but this device has
     *  NO cached AccountIdentity (phrase) seed, so we cannot rebuild
     *  the canonical identity. We MUST NOT derive it from the export
     *  key (wrong identity now) nor silently mint a fresh one (breaks
     *  every contact's pin). The UI renders the restore prompt (another
     *  device, a passkey that carries keys, the phrase, or a new identity);
     *  [AuthRepository.provideRecoveryPhrase] and its siblings complete the
     *  session. */
    public data class NeedsRecoveryPhrase(val account: SessionAccount) : AuthState

    /** ADR-0073 "Reset account keys" (the ADR-0017 lost-everything
     *  fallback). The user recovered ACCESS via an ADR-0018 factor but has
     *  NO recovery phrase and NO other device, and reset the account keys:
     *  the server step is done, the new phrase is minted, and the new
     *  identity is published with the auth attestation (or its publish is
     *  still owed, [AuthRepository.freshPublishOwed]); the UI must show the
     *  phrase for the user to save before the session lands.
     *  [AuthRepository.confirmFreshPhraseSaved] finishes it. Also the guest
     *  signup's phrase step (ADR-0032). */
    public data class SaveFreshPhrase(val account: SessionAccount) : AuthState
}

/**
 * ADR-0037 §6 (G-11) — the pending stale-backup-head override request. Surfaced
 * on [AuthRepository.staleOverridePrompt] while a restore is blocked awaiting the
 * user's decision; resolved via [AuthRepository.resolveStaleOverride].
 */
public data class StaleOverridePrompt(
    /** Why the head was flagged stale; the dialog shows its catalog phrase. */
    val reason: me.lakko.app.backup.RestoreReason,
)

/**
 * ADR-0037 §6 (G-11) — a security alarm raised when a restore was HARD-refused
 * by the rollback gate (an unambiguous tamper / corruption of the served backup
 * head). Surfaced on [AuthRepository.rollbackAlert] as a dismissible banner; no
 * backup was applied.
 */
public data class BackupRollbackAlert(
    /** Why the head was rejected; the banner shows its catalog phrase. */
    val reason: me.lakko.app.backup.RestoreReason,
)

/** Why the restore prompt opened after a passkey sign-in (its optional
 *  first line). */
public enum class RestoreNotice {
    /** The passkey signed in and carries no keys (no PRF output, or nothing
     *  stored for it). */
    PasskeyNoKeys,

    /** Keys are stored for the passkey but did not open, or the phrase they
     *  gave is not the account's. Nothing was changed. */
    PasskeyUnusable,
}

internal fun restoreNoticeOf(outcome: PasskeyKeysOutcome): RestoreNotice? = when (outcome) {
    PasskeyKeysOutcome.Restored -> null
    PasskeyKeysOutcome.NoKeys -> RestoreNotice.PasskeyNoKeys
    PasskeyKeysOutcome.Unusable -> RestoreNotice.PasskeyUnusable
}

/** The account "Three words from your phrase" signs in on this device. */
public class ThreeWordsAccount(
    public val accountIdB64: String,
    public val username: String,
)

/** The person dismissed the passkey sheet. Not an error: the form goes back
 *  to idle. */
public class PasskeyCancelled : Exception("passkey sheet dismissed")

/** The sentence for a passkey ceremony that failed (not a dismissal). */
internal fun passkeyFailureText(cause: Throwable): me.lakko.app.l10n.UiText = when {
    cause is WebauthnHttpError && cause.status == HTTP_SERVICE_UNAVAILABLE ->
        me.lakko.app.l10n.UiText("auth.passkey.error.server_disabled")
    cause is WebauthnHttpError && cause.status == HTTP_NOT_FOUND ->
        me.lakko.app.l10n.UiText("auth.passkey.error.not_registered")
    cause is WebauthnHttpError ->
        me.lakko.app.l10n.UiText("auth.passkey.error.failed_status", texts = mapOf("status" to cause.status.toString()))
    cause is java.io.IOException -> me.lakko.app.l10n.UiText("auth.passkey.error.network")
    cause is androidx.credentials.exceptions.NoCredentialException ->
        me.lakko.app.l10n.UiText("auth.passkey.error.not_registered")
    cause is androidx.credentials.exceptions.GetCredentialProviderConfigurationException ||
        cause is androidx.credentials.exceptions.GetCredentialUnsupportedException ->
        me.lakko.app.l10n.UiText("recovery.passkey.unsupported")
    else -> me.lakko.app.l10n.UiText("auth.passkey.error.generic")
}

private const val HTTP_UNAUTHORIZED: Int = 401

/** The kept-device notice of UX 18.3, per account (a flag, no secret). */
private const val ROTATION_NOTICE_PREFS: String = "lakko-key-rotation-notice"
private const val ROTATION_NOTICE_PREFIX: String = "notice-v1:"

/** The waits between tries of an upgrade that had no usable answer
 *  (chosen, not measured). */
private const val SUPERSESSION_RETRY_FIRST_MS: Long = 5_000L
private const val SUPERSESSION_RETRY_MAX_MS: Long = 60_000L

/** How long a superseded device waits for its DeviceGroup journal to be
 *  read to the head (chosen: longer than one long poll). */
private const val JOURNAL_HEAD_WAIT_MS: Long = 40_000L
private const val HTTP_NOT_FOUND: Int = 404
private const val HTTP_SERVICE_UNAVAILABLE: Int = 503
private const val ACCOUNT_ID_LENGTH: Int = 16

/** How many pages of account notices one landing reads at most. */
private const val NOTICE_MAX_PAGES: Int = 5

/** Account record exposed to the UI when a session is active.
 *  Mirror of `apps/web`'s `SessionAccount`. */
public data class SessionAccount(
    /** Base64 of the raw 16-byte UUIDv7. The canonical id we send
     *  to the server on every subsequent request. */
    val accountIdB64: String,
    /** Human-visible handle. Local-only until 1.15e. */
    val username: String,
)

/**
 * Identity secrets held in memory for the current session.
 *
 * Mirror of `apps/web/src/auth/AuthContext.tsx::IdentitySecrets`.
 * Populated whenever the user has just authenticated (signup,
 * login, recover) — those flows have the OPAQUE export key in
 * scope. Empty after a session restore from disk: the export
 * key is not persisted, so the secrets cannot be re-derived
 * without a password.
 *
 * Consumers that need to sign on behalf of the user
 * (`ContactsClient.sendContactRequest`, MLS commits) MUST check
 * `AuthRepository.identitySecrets.value` and surface a "please
 * re-enter your password" prompt when it's `null`.
 *
 * The bytes themselves are held — not a UniFFI `Identity`
 * instance — because:
 *   * they're small and clone-friendly;
 *   * a UniFFI handle carries lifetime baggage (the Rust-side
 *     Arc) and an explicit `.close()` requirement;
 *   * downstream callers reconstruct an `Identity` on demand
 *     via `Identity.fromSeed(accountIdentitySeed)` (ADR-0017 —
 *     was `exportKey` pre-ADR-0017) — each reconstruction is
 *     sub-millisecond (HKDF + Ed25519 + X25519 derivation).
 */
public data class IdentitySecrets(
    /** Raw 16-byte UUIDv7. */
    val accountId: ByteArray,
    /** OPAQUE password export key (64 bytes for `LakkoCipherSuite`).
     *  ADR-0017: NO LONGER the seed for the canonical
     *  AccountIdentity — it is retained only as an auth factor and
     *  as the device-root-key seed (`deriveDeviceRootKey`). */
    val exportKey: ByteArray,
    /** ADR-0017: the BIP39 recovery-phrase seed
     *  (`RecoveryMnemonic.toSeed()`, 64 bytes). This is the seed for
     *  the canonical AccountIdentity (`Identity.fromSeed`) — stable
     *  across password changes and recovery, so the pubkey contacts
     *  pin never churns on a routine recover. Every `fromSeed` site
     *  that builds the canonical identity reads THIS, not
     *  `exportKey`. */
    val accountIdentitySeed: ByteArray,
    /** ADR-0073: the session pair issued at the landing (the access token
     *  every client captures as its bearer - the transport substitutes the
     *  provider's current one -, the refresh credential and the key
     *  generation). The provider's refresh updates it in place. */
    val session: SessionPair,
) {
    /** The access token of [session]. Threaded into `DiscoveryClient` and
     *  the others so every protected write carries the `Authorization:
     *  Bearer <token>` header; the transport refreshes it on the wire. */
    val sessionToken: String get() = session.accessToken

    /** The access token's expiry as a unix-millisecond timestamp. */
    val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is IdentitySecrets) return false
        return accountId.contentEquals(other.accountId) &&
            exportKey.contentEquals(other.exportKey) &&
            accountIdentitySeed.contentEquals(other.accountIdentitySeed) &&
            session == other.session
    }

    override fun hashCode(): Int {
        var h = accountId.contentHashCode()
        h = 31 * h + exportKey.contentHashCode()
        h = 31 * h + accountIdentitySeed.contentHashCode()
        h = 31 * h + session.hashCode()
        return h
    }
}

/**
 * ADR-0073 section 3.5 / 4.2 — the signed-out screen's state: this device's
 * keys are not the account's keys any more. Shown by the app root instead
 * of the app, in every auth state, until its one button is tapped.
 */
public data class KeyResetSignOut(
    val accountIdB64: String,
    /** The account's `@name` for the copy, or empty for a guest. */
    val username: String,
    val kind: Kind,
    val phase: Phase,
    /** ADR-0074 section 1.1 - the wipe keeps the recall record and the
     *  screen that follows is "Was it you?" ([supersededWipeFor]). Only ever
     *  `true` for [Kind.Reset] on a device that holds the account's keys. */
    val keepsRecall: Boolean = false,
    /** A line the screen carries once the wipe is done: why the account can
     *  no longer be taken back ([recallEndLine]). */
    val notice: me.lakko.app.l10n.UiText? = null,
) {
    public enum class Kind {
        /** The account's keys were reset on another device (a known reset:
         *  the wipe runs by itself). */
        Reset,

        /** The directory serves other keys at the same generation (an
         *  anomaly: the wipe runs on the tap). */
        Mismatch,

        /** ADR-0074: another device of the account removed this one (the
         *  wipe runs by itself; nothing is kept to take the account back
         *  with, since a removal changes no keys). */
        Removed,
    }

    public enum class Phase {
        /** The wipe is running ("Removing..."). */
        Removing,

        /** The wipe is done: the one button is "Sign in". */
        Removed,

        /** [Kind.Mismatch]: the one button is "Sign out", which runs the
         *  wipe. */
        AwaitingTap,
    }
}

/**
 * The phase a sign-out screen opens in: a known reset and a removal by
 * another device (ADR-0074) delete everything BY THEMSELVES; only the
 * same-generation mismatch waits for the person's tap.
 */
public fun signOutPhaseFor(kind: KeyResetSignOut.Kind): KeyResetSignOut.Phase = when (kind) {
    KeyResetSignOut.Kind.Reset, KeyResetSignOut.Kind.Removed -> KeyResetSignOut.Phase.Removing
    KeyResetSignOut.Kind.Mismatch -> KeyResetSignOut.Phase.AwaitingTap
}

/**
 * The screen a refused session leads to (ADR-0074 section 2.3, UX 18.7):
 * three answers, three screens, three deletions. `null` = no full-screen
 * state: the sign-in screen with nothing wiped (a logout at the server, an
 * expired refresh credential), or one refresh and one retry.
 */
public fun signOutKindFor(refusal: SessionRefusal): KeyResetSignOut.Kind? = when (refusal) {
    SessionRefusal.KeysReset -> KeyResetSignOut.Kind.Reset
    SessionRefusal.DeviceRevoked -> KeyResetSignOut.Kind.Removed
    SessionRefusal.RefreshInvalid, SessionRefusal.Unauthorized -> null
}

/** Result of a sign-up. The recovery phrase is generated and stored on the
 *  device but NOT handed to the screen (ADR-0075 D2): it is shown only when
 *  the person saves it (UX 19.5). */
public data class SignupOutcome(
    val accountIdB64: String,
)

/**
 * ADR-0017 fresh-device fail-safe: everything needed to finish a
 * session once the user supplies their recovery phrase. The auth
 * factor (password / passkey) already succeeded; only the
 * AccountIdentity (phrase) seed is missing on this device. Read back
 * by [AuthRepository.provideRecoveryPhrase].
 *
 * Mirror of `apps/web/src/auth/AuthContext.tsx::PendingRecovery`.
 */
internal data class PendingRecovery(
    val accountIdBytes: ByteArray,
    val accountIdB64: String,
    /** OPAQUE export key from the just-completed auth — retained for
     *  the in-memory `exportKey`. Owns an independent `copyOf()`. */
    val exportKey: ByteArray,
    /** The pair the auth factor landed (refreshed in place by the provider
     *  while the device is parked). */
    val session: SessionPair,
    val username: String,
) {
    val sessionToken: String get() = session.accessToken

    val sessionExpiresAtMs: Long get() = session.accessExpiresAtMs

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is PendingRecovery) return false
        return accountIdBytes.contentEquals(other.accountIdBytes) &&
            accountIdB64 == other.accountIdB64 &&
            exportKey.contentEquals(other.exportKey) &&
            session == other.session &&
            username == other.username
    }

    override fun hashCode(): Int {
        var h = accountIdBytes.contentHashCode()
        h = 31 * h + accountIdB64.hashCode()
        h = 31 * h + exportKey.contentHashCode()
        h = 31 * h + session.hashCode()
        h = 31 * h + username.hashCode()
        return h
    }
}

/**
 * ADR-0017 lost-everything fallback — the completion context stashed by
 * [AuthRepository.mintFreshIdentity] between minting the fresh identity
 * (already published) and the user acknowledging the new phrase. Holds no
 * password material (there is none on this path); the seed is the
 * just-minted fresh AccountIdentity seed, zeroized if the flow is abandoned.
 *
 * Mirror of the web `pendingFreshPhrase` signal.
 */
internal data class PendingFreshPhrase(
    val seed: ByteArray,
    val accountIdBytes: ByteArray,
    val accountIdB64: String,
    /** The pair the reset issued at the new generation. */
    val session: SessionPair,
    val username: String,
    val phrase: String,
    /** `true` while the new keys of a reset are not yet published to the
     *  directory and the device list (ADR-0073 section 2.3). The phrase is
     *  not shown and the session does not land until it is `false`. Always
     *  `false` for a guest signup, which published its genesis bundle. */
    val publishOwed: Boolean,
) {
    val sessionToken: String get() = session.accessToken

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is PendingFreshPhrase) return false
        return seed.contentEquals(other.seed) &&
            accountIdBytes.contentEquals(other.accountIdBytes) &&
            accountIdB64 == other.accountIdB64 &&
            session == other.session &&
            username == other.username &&
            phrase == other.phrase &&
            publishOwed == other.publishOwed
    }

    override fun hashCode(): Int {
        var h = seed.contentHashCode()
        h = 31 * h + accountIdBytes.contentHashCode()
        h = 31 * h + accountIdB64.hashCode()
        h = 31 * h + session.hashCode()
        h = 31 * h + username.hashCode()
        h = 31 * h + phrase.hashCode()
        h = 31 * h + publishOwed.hashCode()
        return h
    }
}

/**
 * ADR-0017 — thrown by [AuthRepository.provideRecoveryPhrase] when the
 * typed recovery phrase derives an AccountIdentity pubkey that does NOT
 * match the account's published bundle. Surfaced to the UI as "that
 * phrase doesn't match this account" — we refuse rather than cache a
 * forked identity that would break every contact's pin.
 *
 * Mirror of the web `SupersessionPhraseMismatch`.
 */
public class PhraseMismatch : IllegalStateException(
    "That recovery phrase doesn't match this account's identity. " +
        "Check the phrase, or pair an existing device instead.",
)

/**
 * ADR-0017 (lakko-security finding #1) — thrown by
 * [AuthRepository.provideRecoveryPhrase] when the account's published
 * bundle could NOT be fetched (404 / network) so the supplied phrase
 * cannot be verified against it. We HARD-BLOCK rather than
 * trust-on-first-use a possibly-wrong phrase: this path only runs for
 * an already-established account, so a missing bundle is an anomaly (a
 * withholding or unreachable directory), and a server-forced 404 must
 * not be able to push a socially-engineered wrong phrase into the
 * published identity. Surfaced as a retryable error, not a dead end.
 *
 * Mirror of the web `IdentityVerificationUnavailable`.
 */
public class IdentityVerificationUnavailable(
    message: String,
    /** `true` when the directory answered with no published identity;
     *  `false` when it could not be reached. Picks the screen sentence. */
    public val missing: Boolean = false,
) : IllegalStateException(message)
