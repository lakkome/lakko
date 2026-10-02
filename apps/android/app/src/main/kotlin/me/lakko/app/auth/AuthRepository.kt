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
        )

    private val mutableState: MutableStateFlow<AuthState> =
        MutableStateFlow(AuthState.Loading)

    public val state: StateFlow<AuthState> = mutableState.asStateFlow()

    /**
     * ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) —
     * non-null when this device's canonical AccountIdentity did NOT match
     * the account's currently-published bundle at login (someone replaced
     * it; the ACCESS-hijack signature). Surfaced as a dismissible banner.
     */
    private val mutableIdentityAlert: MutableStateFlow<IdentityAlert?> =
        MutableStateFlow(null)

    public val identityAlert: StateFlow<IdentityAlert?> =
        mutableIdentityAlert.asStateFlow()

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

    /** ADR-0017 lost-everything fallback — the minted fresh phrase +
     *  completion context, parked between [mintFreshIdentity] and
     *  [confirmFreshPhraseSaved]. */
    private val pendingFreshPhrase: AtomicReference<PendingFreshPhrase?> =
        AtomicReference(null)

    /** The debts a landing can leave: a phrase still to be shown, a recovery
     *  credential still to be replaced ([RecoveryOwedStore]). */
    private val owedStore: RecoveryOwedStore by lazy { RecoveryOwedStore.onDevice(applicationContext) }

    /** The local three-word guess budget, per account. */
    private val threeWordsBudgetStore: ThreeWordsBudgetStore by lazy {
        ThreeWordsBudgetStore.onDevice(applicationContext)
    }

    /**
     * The recovery phrase a sign-up (or a fresh identity whose screen was
     * left) generated, until the person confirms they saved it; `null`
     * otherwise. While non-null the app root shows the phrase step instead
     * of the app. The fact that it is owed survives a restart
     * ([RecoveryOwedStore]); the phrase itself is read back from the phrase
     * store when the next session for the account lands.
     */
    private val mutableSignupPhrase: MutableStateFlow<String?> = MutableStateFlow(null)

    public val signupPhrase: StateFlow<String?> = mutableSignupPhrase.asStateFlow()

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
     * `true` while a freshly created identity is published but its recovery
     * phrase is not yet the account's recovery credential on the server (the
     * replace failed). The phrase screen then shows an explanation and "Try
     * again" instead of the words; [retryFreshRecoveryCredential] clears it.
     */
    private val mutableFreshCredentialPending: MutableStateFlow<Boolean> = MutableStateFlow(false)

    public val freshRecoveryCredentialPending: StateFlow<Boolean> =
        mutableFreshCredentialPending.asStateFlow()

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
        restoreScope.launch { rehydrate() }
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
                // Secrets first, then state: an observer that reacts to the
                // Session flip must already find the secrets it needs.
                publishIdentitySecrets(decision.secrets)
                mutableState.value = AuthState.Session(account = decision.account)
                // ADR-0054 UX section 3 - a push registration that failed
                // (a used-up allowance, no network) is retried silently at the
                // next launch, which is this restore.
                subscribePushBestEffort("restore")
                // ADR-0017 defence-in-depth — the same background self-check a
                // password login runs (is the published identity still ours?),
                // re-publishing only when the directory is missing the bundle
                // or holds other keys, so a relaunch does not pay an
                // upload for a no-op.
                refreshPublishedIdentityInBackground(
                    landing = IdentityLanding.SESSION_RESTORE,
                    accountIdBytes = decision.secrets.accountId,
                    accountIdentitySeed = decision.secrets.accountIdentitySeed,
                    sessionToken = decision.secrets.sessionToken,
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

        override fun loadSessionToken(accountIdB64: String): PersistedSessionToken? =
            SessionTokenStore.getSessionToken(applicationContext, accountIdB64)

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
        try {
            SessionTokenStore.putSessionToken(
                context = applicationContext,
                accountIdB64 = accountIdB64,
                token = secrets.sessionToken,
                expiresAtMs = secrets.sessionExpiresAtMs,
            )
        } catch (cause: Throwable) {
            Log.w(
                "lakko-auth",
                "session-token persist failed; the next launch will require sign-in",
                cause,
            )
        }
        publishIdentitySecrets(secrets)
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
        mutableIdentitySecrets.value = secrets
        settleOwedRecoveryCredential(accountIdB64, secrets.accountIdentitySeed, secrets.sessionToken)
    }

    /**
     * A phrase generated on this device and never confirmed as saved is
     * shown again at the next session for that account, read back from the
     * phrase store. Not while a recovery-credential replace is outstanding:
     * that phrase signs in nowhere yet ([settleOwedRecoveryCredential] calls
     * back here once the replace succeeds). A phrase that is owed but no
     * longer readable cannot be shown and is logged rather than blocking the
     * account.
     */
    private fun restoreOwedPhrase(accountIdB64: String) {
        if (mutableSignupPhrase.value != null) return
        if (!owedStore.phraseShowable(accountIdB64)) return
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
     * A fresh identity whose recovery-credential replace did not succeed
     * leaves a marker. When a session for that account lands, the device
     * holds the new seed, so the replace is tried again here, in the
     * background. Success clears the marker (and shows the phrase if it was
     * never confirmed); a failure is logged and waits for the next landing.
     *
     * Safe against a stale seed: the marker is set only after the new seed
     * was stored on this device, and every other way a seed reaches this
     * device is checked against the published (new) identity first.
     */
    private fun settleOwedRecoveryCredential(
        accountIdB64: String,
        accountIdentitySeed: ByteArray,
        sessionToken: String,
    ) {
        if (!owedStore.isCredentialOwed(accountIdB64)) return
        val seed = accountIdentitySeed.copyOf()
        pushScope.launch {
            try {
                authClientFactory().use { it.replaceRecoveryCredential(sessionToken, seed) }
                owedStore.clearCredentialOwed(accountIdB64)
                if (signedInAccountIdB64() == accountIdB64) restoreOwedPhrase(accountIdB64)
            } catch (cause: Throwable) {
                Log.w(
                    "lakko-auth",
                    "owed recovery-credential replace failed; retried at the next session landing: " +
                        ((cause as? AuthHttpError)?.status?.toString() ?: cause.javaClass.simpleName),
                )
            } finally {
                seed.fill(0)
            }
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
     *   8. Return the 12-word recovery phrase for the UI to
     *      display **once**.
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
            val result = client.signup(password, enrolRecovery = true, onProofSolved = onProofSolved)
            try {
                val accountIdB64 = base64Encode(result.accountId)
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
                try {
                    publishIdentityFor(
                        landing = IdentityLanding.SIGNUP,
                        accountId = result.accountId,
                        accountIdentitySeed = accountIdentitySeed,
                        sessionToken = result.sessionToken,
                    )
                } catch (cause: Throwable) {
                    // The seed never reaches the live session on this path.
                    accountIdentitySeed.fill(0)
                    throw cause
                }
                // The phrase is owed to the person until they confirm they
                // saved it: up before the session lands (the app root shows
                // the phrase step instead of the app), and recorded so a
                // restart shows it again.
                owedStore.markPhraseOwed(accountIdB64)
                mutableSignupPhrase.value = phrase
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
                        sessionToken = result.sessionToken,
                        sessionExpiresAtMs = result.sessionExpiresAtMs,
                    ),
                )
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
                SignupOutcome(recoveryPhrase = phrase)
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
     * account. It then parks the recovery phrase and flips to
     * [AuthState.SaveFreshPhrase] (like [mintFreshIdentity]) instead of landing
     * the session directly, so the existing SaveFreshPhraseScreen +
     * [confirmFreshPhraseSaved] render + land it unchanged.
     *
     * SECURITY: guest creation routes through the SAME PoW-gated signup — no
     * bypass. The password is 32 random bytes; recovery is phrase-only,
     * matching the ACCESS-only convention the recovery cascade already uses.
     * No username is claimed; the account is username-less until a later
     * "claim your username" onboarding step.
     *
     * @return the 12-word recovery phrase (also surfaced by [freshPhrase]).
     */
    public suspend fun signupGuest(onProofSolved: () -> Unit = {}): SignupOutcome {
        // 32 random bytes the user NEVER sees — recovery is phrase-only.
        val randomPassword = ByteArray(32).also { SecureRandom().nextBytes(it) }
        return authClientFactory().use { client ->
            // Same PoW-gated OPAQUE registration signup uses — no bypass.
            val result = client.signup(randomPassword, enrolRecovery = true, onProofSolved = onProofSolved)
            try {
                val accountIdB64 = base64Encode(result.accountId)
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
                publishIdentityFor(
                    landing = IdentityLanding.SIGNUP,
                    accountId = result.accountId,
                    accountIdentitySeed = accountIdentitySeed,
                    sessionToken = result.sessionToken,
                )
                // Owed until confirmed on the phrase screen. The session is
                // persisted now, so a restart before that restores it and
                // shows the phrase first: a guest has no password to sign in
                // with again, and would otherwise be left with an account
                // whose only way back (the phrase) was never shown.
                try {
                    SessionTokenStore.putSessionToken(
                        context = applicationContext,
                        accountIdB64 = accountIdB64,
                        token = result.sessionToken,
                        expiresAtMs = result.sessionExpiresAtMs,
                    )
                } catch (cause: Throwable) {
                    Log.w("lakko-auth", "session-token persist (signupGuest) failed", cause)
                }
                owedStore.markPhraseOwed(accountIdB64)
                // Park the completion context + flip to SaveFreshPhrase so the
                // UI shows the recovery phrase before the session lands (the
                // SaveFreshPhraseScreen -> confirmFreshPhraseSaved path lands
                // the canonical session; the seed is owned by the pending
                // record thereafter, so it is not wiped here).
                pendingFreshPhrase.set(
                    PendingFreshPhrase(
                        seed = accountIdentitySeed,
                        accountIdBytes = result.accountId,
                        accountIdB64 = accountIdB64,
                        sessionToken = result.sessionToken,
                        sessionExpiresAtMs = result.sessionExpiresAtMs,
                        username = "",
                        phrase = phrase,
                        // A guest signup registers its recovery credential
                        // itself; nothing is left to replace.
                        recoveryCredentialOwed = false,
                    ),
                )
                mutableFreshCredentialPending.value = false
                mutableState.value = AuthState.SaveFreshPhrase(
                    account = SessionAccount(accountIdB64 = accountIdB64, username = ""),
                )
                SignupOutcome(recoveryPhrase = phrase)
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
                call = { client.login(it.handle, password) },
                sessionTokenOf = { it.sessionToken },
                rebind = ::rebindLoginHandleInBackground,
            )
            val accountIdBytes = result.accountId
            val accountIdB64 = base64Encode(accountIdBytes)
            val resolvedUsername = named.username
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
                            sessionToken = result.sessionToken,
                            sessionExpiresAtMs = result.sessionExpiresAtMs,
                            username = resolvedUsername,
                        ),
                    )
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
                        sessionToken = result.sessionToken,
                        sessionExpiresAtMs = result.sessionExpiresAtMs,
                    ),
                )
                mutableState.value = AuthState.Session(
                    account = SessionAccount(
                        accountIdB64 = accountIdB64,
                        username = resolvedUsername,
                    ),
                )
                // ADR-0017 defence-in-depth + directory refresh, in the
                // background: check the published identity is still ours
                // (alert on a mismatch), THEN re-publish the bundle from the
                // canonical identity. Before this, a password login never
                // published the bundle, so an account whose bundle was missing
                // stayed unverifiable however often its owner signed in.
                refreshPublishedIdentityInBackground(
                    landing = IdentityLanding.SIGN_IN,
                    accountIdBytes = accountIdBytes,
                    accountIdentitySeed = accountIdentitySeed,
                    sessionToken = result.sessionToken,
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
                    call = { client.recover(it.handle, seed, newPassword) },
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
                            sessionToken = result.sessionToken,
                            sessionExpiresAtMs = result.sessionExpiresAtMs,
                        ),
                    )
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
                        accountId = accountIdBytes,
                        accountIdentitySeed = accountIdentitySeed,
                        sessionToken = result.sessionToken,
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
                call = { client.totpRecover(it.handle, code) },
                sessionTokenOf = { it.sessionToken },
                rebind = ::rebindLoginHandleInBackground,
            )
        }
        val accountIdBytes = result.accountId
        val accountIdB64 = base64Encode(accountIdBytes)
        val resolvedUsername = named.username

        mutableRestoreNotice.value = null
        // A forgot-password route: the new-password step is owed once this
        // device holds the keys.
        mutableNewPasswordOwed.value = true
        // Shared ADR-0018 ACCESS-only landing (warm cached-seed finish
        // OR cold fail-safe park).
        landAfterAccessRecovery(
            username = resolvedUsername,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            sessionToken = result.sessionToken,
            sessionExpiresAtMs = result.sessionExpiresAtMs,
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
     * username, and the device holds both its recovery phrase and its keys.
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
        if (phrase == null || !offersThreeWords(cached.username, phraseOnDevice = true, keysOnDevice = keysOnDevice)) {
            return null
        }
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
                    call = { client.recoveryLogin(it.handle, seed) },
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
            landAfterAccessRecovery(
                username = named.username,
                accountIdBytes = result.accountId,
                accountIdB64 = accountIdB64,
                sessionToken = result.sessionToken,
                sessionExpiresAtMs = result.sessionExpiresAtMs,
                pushContext = "three-words",
            )
            threeWordsBudgetStore.clear(accountIdB64)
            return true
        } finally {
            seed.fill(0)
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
            val result = authClientFactory().use { it.login(named.handle, password) }
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
        val named = resolveNamedSignIn(null, session.account.username, loginHandleSource)
        // A copy: the in-memory secret is never the buffer that is wiped.
        val seed = secrets.accountIdentitySeed.copyOf()
        try {
            val result = authClientFactory().use { client ->
                signInByName(
                    named = named,
                    call = { client.recover(it.handle, seed, newPassword) },
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
                        sessionToken = result.sessionToken,
                        sessionExpiresAtMs = result.sessionExpiresAtMs,
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
        sessionToken: String,
        sessionExpiresAtMs: Long,
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
                    sessionToken = sessionToken,
                    sessionExpiresAtMs = sessionExpiresAtMs,
                ),
            )
            pendingRecovery.set(null)
            mutableRestoreNotice.value = null
            mutableState.value = AuthState.Session(
                account = SessionAccount(
                    accountIdB64 = accountIdB64,
                    username = username,
                ),
            )
            // ADR-0017 defence-in-depth + directory refresh, in the background
            // (same as a password login): alert on a mismatch, then re-publish.
            refreshPublishedIdentityInBackground(
                landing = IdentityLanding.SIGN_IN,
                accountIdBytes = accountIdBytes,
                accountIdentitySeed = accountIdentitySeed,
                sessionToken = sessionToken,
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
                sessionToken = sessionToken,
                sessionExpiresAtMs = sessionExpiresAtMs,
                username = username,
            ),
        )
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
                    sessionToken = pending.sessionToken,
                    sessionExpiresAtMs = pending.sessionExpiresAtMs,
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
     * ADR-0017 lost-everything fallback. Mint a brand-new recovery phrase
     * + AccountIdentity for the SAME account when the user recovered ACCESS
     * via an ADR-0018 factor but has NO recovery phrase and NO other device.
     *
     * This is the ONLY sanctioned path that publishes a CHANGED
     * AccountIdentity key WITHOUT a supersession proof — correct because
     * there is genuinely no old seed to sign with (the user lost it). The
     * user's contacts then see a changed key with no proof and re-verify
     * out of band (the accepted, documented degradation).
     *
     * Reachable ONLY while a [PendingRecovery] is parked (i.e. post-ACCESS,
     * from [AuthState.NeedsRecoveryPhrase]) and ONLY via the explicit UI
     * button. Mirror of the web `AuthContext.mintFreshIdentity`.
     *
     * Order: generate phrase + seed -> publish the identity bundle (a
     * failure leaves this device untouched) -> local writes (device key,
     * seed, session, phrase store, the "phrase owed" marker) -> replace the
     * server's recovery credential with one made from the new seed (a
     * failure there is recorded and retried, never a reason to mint again).
     *
     * Does NOT
     * land the session here — it flips to [AuthState.SaveFreshPhrase] and
     * returns the new phrase so the UI can show it; [confirmFreshPhraseSaved]
     * lands the session after the user acknowledges.
     *
     * @throws AuthClientArgError when there is no pending recovery.
     */
    public suspend fun mintFreshIdentity(): String {
        val pending = pendingRecovery.get()
            ?: throw AuthClientArgError("no pending identity recovery — sign in first")

        // Generate a fresh 12-word recovery phrase + its seed (same client-
        // side generation signup uses).
        val (phrase, seed) = RecoveryMnemonic.generate().use { mnemonic ->
            mnemonic.phrase() to mnemonic.toSeed()
        }

        // Publish the NEW bundle WITHOUT a supersession proof FIRST, and FAIL
        // the flow if it does not land — otherwise a transient network error
        // would leave the device with a cached fresh seed while the server
        // still serves the OLD bundle, a torn state the user cannot self-heal
        // (provideRecoveryPhrase with the new phrase would then hard-block
        // against the stale published key). Publishing before any local
        // mutation keeps a failure fully recoverable: the device is untouched
        // and the user can retry.
        try {
            publishIdentityFor(
                landing = IdentityLanding.FRESH_IDENTITY,
                accountId = pending.accountIdBytes,
                accountIdentitySeed = seed,
                sessionToken = pending.sessionToken,
            )
        } catch (cause: Throwable) {
            // Nothing local changed; the seed of the identity that was not
            // published goes.
            seed.fill(0)
            throw cause
        }

        // Bootstrap the device-key existence gate from the fresh seed (no
        // password on this path). No-op if a key already exists.
        bootstrapDeviceKeyFromSeedBestEffort(
            accountIdB64 = pending.accountIdB64,
            accountIdentitySeed = seed,
            context = "mintFreshIdentity",
        )

        // Cache the fresh seed locally (best-effort) so warm relaunch works.
        try {
            AccountIdentitySeedStore.putAccountIdentitySeed(
                applicationContext,
                pending.accountIdB64,
                seed,
            )
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "fresh-identity seed persist failed", cause)
        }
        // The pending session was never persisted (the device was parked
        // without keys); land it now so a relaunch re-attaches.
        try {
            SessionTokenStore.putSessionToken(
                context = applicationContext,
                accountIdB64 = pending.accountIdB64,
                token = pending.sessionToken,
                expiresAtMs = pending.sessionExpiresAtMs,
            )
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "session-token persist (fresh identity) failed", cause)
        }

        // The device holds the new keys, so it holds the new phrase (the
        // local three-word check, the hand-over to a new device, "Show
        // recovery phrase").
        DevicePhrase.store(applicationContext, pending.accountIdB64, phrase, DevicePhrase.LandingPath.FreshIdentity)
        // A restart before the phrase is confirmed lands a session from the
        // keys just stored; the owed marker makes that session show the
        // phrase first.
        owedStore.markPhraseOwed(pending.accountIdB64)

        // The server's recovery credential was registered from the OLD
        // phrase. Replace it with one made from the new seed, so the new 12
        // words run the recovery login and the password reset and the old
        // ones stop. The identity is already published, so a failure here is
        // NOT a reason to mint again: the debt is recorded (it survives a
        // restart and is retried when a session lands), the phrase screen
        // shows an explanation with "Try again" instead of the words, and
        // [retryFreshRecoveryCredential] repeats only this step.
        owedStore.markCredentialOwed(pending.accountIdB64)
        var recoveryCredentialOwed = true
        run {
            val seedCopy = seed.copyOf()
            try {
                authClientFactory().use { it.replaceRecoveryCredential(pending.sessionToken, seedCopy) }
                owedStore.clearCredentialOwed(pending.accountIdB64)
                recoveryCredentialOwed = false
            } catch (cause: Throwable) {
                Log.w(
                    "lakko-auth",
                    "replacing the recovery credential after a fresh identity failed: " +
                        ((cause as? AuthHttpError)?.status?.toString() ?: cause.javaClass.simpleName),
                )
            } finally {
                seedCopy.fill(0)
            }
        }

        // Stash the completion + phrase; flip to the save-phrase state.
        pendingFreshPhrase.set(
            PendingFreshPhrase(
                seed = seed,
                accountIdBytes = pending.accountIdBytes,
                accountIdB64 = pending.accountIdB64,
                sessionToken = pending.sessionToken,
                sessionExpiresAtMs = pending.sessionExpiresAtMs,
                username = pending.username,
                phrase = phrase,
                recoveryCredentialOwed = recoveryCredentialOwed,
            ),
        )
        mutableFreshCredentialPending.value = recoveryCredentialOwed
        pendingRecovery.set(null)
        mutableRestoreNotice.value = null
        mutableState.value = AuthState.SaveFreshPhrase(
            account = SessionAccount(
                accountIdB64 = pending.accountIdB64,
                username = pending.username,
            ),
        )
        return phrase
    }

    /**
     * Retry the recovery-credential replace for the identity
     * [mintFreshIdentity] already published. Never mints again. Throws what
     * the request threw (offline, `429`, `503`); on success the phrase
     * screen shows the words.
     */
    public suspend fun retryFreshRecoveryCredential() {
        val pending = pendingFreshPhrase.get()
            ?: throw AuthClientArgError("no pending fresh-identity phrase")
        if (!pending.recoveryCredentialOwed) return
        // The identity is already published: only the credential is retried.
        val seed = pending.seed.copyOf()
        try {
            authClientFactory().use { it.replaceRecoveryCredential(pending.sessionToken, seed) }
        } finally {
            seed.fill(0)
        }
        owedStore.clearCredentialOwed(pending.accountIdB64)
        // Only clear the flag on the stash this call started from.
        if (pendingFreshPhrase.compareAndSet(pending, pending.copy(recoveryCredentialOwed = false))) {
            mutableFreshCredentialPending.value = false
        }
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

    /** Dismiss the identity-changed security banner. */
    public fun dismissIdentityAlert() {
        mutableIdentityAlert.value = null
    }

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
     * ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) plus
     * the directory refresh, fire-and-forget from a landing that holds the
     * phrase seed: is the account's published AccountIdentity still OURS, and
     * is the directory serving it? A mismatch means someone replaced it (the
     * ACCESS-hijack signature — an attacker minted a fresh identity via the
     * lost-everything path) and raises the alert. The self-check reads BEFORE
     * the re-publish writes, so a replaced bundle is seen before it is
     * overwritten ([publishIdentity]). A 404 or a fetch error never alerts.
     *
     * Before this the check only read, and no sign-in ever published the
     * bundle, so an account that had none could not become verifiable.
     *
     * The coroutine owns its copies of [accountIdBytes] and
     * [accountIdentitySeed] and zeroizes the seed. A logout (or a sign-in to
     * another account) while the read is out makes it stand down.
     */
    private fun refreshPublishedIdentityInBackground(
        landing: IdentityLanding,
        accountIdBytes: ByteArray,
        accountIdentitySeed: ByteArray,
        sessionToken: String,
    ) {
        val accountId = accountIdBytes.copyOf()
        val seed = accountIdentitySeed.copyOf()
        pushScope.launch {
            try {
                publishIdentityFor(
                    landing = landing,
                    accountId = accountId,
                    accountIdentitySeed = seed,
                    sessionToken = sessionToken,
                )
            } catch (cause: Throwable) {
                // The refresh modes never throw on a directory failure; this
                // covers deriving the identity itself.
                Log.w("lakko-auth", "identity bundle refresh failed", cause)
            } finally {
                seed.fill(0)
            }
        }
    }

    /**
     * Publish the identity bundle for [landing] (the policy lives on
     * [identityPublicationFor] / [publishIdentity]) from the canonical
     * AccountIdentity derived from [accountIdentitySeed].
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
        accountId: ByteArray,
        accountIdentitySeed: ByteArray,
        sessionToken: String,
    ): IdentityPublicationResult {
        val discovery = discoveryClientFactory()
        try {
            discovery.setSessionToken(sessionToken)
            return Identity.fromSeed(accountIdentitySeed).use { identity ->
                val ports = object : IdentityPublicationPorts {
                    override val signaturePubkey: ByteArray = identity.signaturePublicKey()
                    override val x25519Pubkey: ByteArray = identity.x25519PublicKey()

                    override suspend fun fetchPublished(): PublishedIdentityKeys? =
                        discovery.identityFetch(accountId)?.let {
                            PublishedIdentityKeys(
                                signaturePubkey = base64Decode(it.signature_pubkey),
                                x25519Pubkey = base64Decode(it.x25519_pubkey),
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
                            mintGenesisAttestBestEffort(sessionToken, signaturePubkey)
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

                    override fun reportPublishedIdentity(changed: Boolean) {
                        mutableIdentityAlert.value =
                            if (changed) IdentityAlert.PublishedIdentityChanged else null
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
        if (parked.recoveryCredentialOwed) {
            // The new phrase is not the account's recovery credential yet; the
            // phrase screen offers "Try again" and does not show the words.
            throw AuthClientArgError(
                "the recovery credential of the fresh identity is not replaced yet",
                me.lakko.app.l10n.UiText("recovery_phrase.fresh.credential_pending"),
            )
        }
        // CONSUME the stash so the parked seed cannot be replayed.
        val pending = pendingFreshPhrase.getAndSet(null)
            ?: throw AuthClientArgError("no fresh identity to confirm")
        // The phrase was shown and confirmed on this screen.
        owedStore.clearPhraseOwed(pending.accountIdB64)
        mutableFreshCredentialPending.value = false
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
                sessionToken = pending.sessionToken,
                sessionExpiresAtMs = pending.sessionExpiresAtMs,
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
        sessionToken: String? = null,
        sessionExpiresAtMs: Long? = null,
    ) {
        val pending = pendingRecovery.get()
        if (pending == null) {
            phraseEntropy.fill(0)
            throw AuthClientArgError("no pending identity recovery — sign in first")
        }
        // Nothing is written before the check below.
        val keys = keysFromTransferredEntropy(phraseEntropy)
        val phraseSeed = keys.seed
        var seedHandedOff = false
        try {
            verifySeedAgainstPublishedIdentity(
                accountIdBytes = pending.accountIdBytes,
                sessionToken = sessionToken ?: pending.sessionToken,
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
                    sessionToken = sessionToken ?: pending.sessionToken,
                    sessionExpiresAtMs = sessionExpiresAtMs
                        ?: pending.sessionExpiresAtMs,
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
        sessionToken: String,
        sessionExpiresAtMs: Long,
        persistJoin: suspend () -> Unit = {},
        forgotPassword: Boolean = false,
    ) {
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
                        sessionToken = sessionToken,
                        accountIdentitySeed = phraseSeed,
                    )
                },
                lookup = { name -> lookupWithSession(name, accountIdBytes, sessionToken) },
                persistJoin = persistJoin,
                land = {
                    if (forgotPassword) mutableNewPasswordOwed.value = true
                    landDeviceRestore(
                        accountIdBytes = accountIdBytes,
                        accountIdB64 = accountIdB64,
                        username = username,
                        phraseSeed = phraseSeed,
                        phrase = keys.phrase,
                        sessionToken = sessionToken,
                        sessionExpiresAtMs = sessionExpiresAtMs,
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
        sessionToken: String,
        sessionExpiresAtMs: Long,
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
                sessionToken = sessionToken,
                sessionExpiresAtMs = sessionExpiresAtMs,
            ),
        )
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
                sessionToken = assertion.sessionToken,
                sessionExpiresAtMs = assertion.sessionExpiresAtMs,
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
            WebauthnClient().use { it.loginWithPasskey(activityContext, accountIdB64 = accountIdB64Url) }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: WebauthnUserCancelledError) {
            throw PasskeyCancelled()
        } catch (cause: Throwable) {
            throw AuthClientArgError("passkey sign-in failed: ${cause.javaClass.simpleName}", passkeyFailureText(cause))
        }
        return assertion ?: throw PasskeyCancelled()
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
        clearIdentitySecrets()
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
        // The wipe above removed this account's tokens; this drops only what
        // memory still holds (nothing on disk, no other account's tokens).
        me.lakko.app.anon.AnonTokens.releaseSession()
        // ADR-0017: drop any parked fresh-device fail-safe context.
        clearPendingRecovery()
        mutableState.value = AuthState.Anon()
        // 1.20d — full push wipe. Fire-and-forget — the local
        // `DevicePushId.clear()` inside the subscriber runs
        // regardless of the server leg outcome, honouring
        // forgetDevice's "leave NOTHING per-account behind"
        // contract.
        unsubscribePushBestEffort("forgetDevice", departingSession)
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
     * ADR-0017 — re-publish the identity bundle after a recover,
     * PROOF-LESS. (The KeyPackage is NOT published here — see the note in
     * the body; `ChatRepository.republishKeyPackage` owns it.)
     *
     * The G-4 / ADR-0016 supersession EMIT path is now **DORMANT**.
     * With a phrase-seeded AccountIdentity, a routine recover does NOT
     * rotate the account key: the recovery phrase is unchanged, so the
     * NEW account signature pubkey EQUALS the OLD one. We derive both
     * from the same [accountIdentitySeed] and compare them explicitly.
     *
     *   * **Equal (the only case on a routine recover):** SKIP the
     *     supersession proof mint + upload entirely (emitting a proof
     *     for a key nobody's pin changed would be spurious) and
     *     re-publish the bundle WITHOUT a proof. The public keys are
     *     identical; the upsert clears any stale server-side proof
     *     column.
     *   * **Different (impossible on a phrase-seeded recover):** FAIL
     *     LOUD — log an error and ABORT the emit. Publishing a changed
     *     account key with no proof would break every contact's pin
     *     (they would fail closed). A genuine identity rotation is a
     *     separate explicit action (ADR-0017 "rotate my identity"),
     *     out of scope for Part 1.
     *
     * "old == new account pubkey => skip" is the load-bearing ADR-0017
     * emit invariant. Best-effort transport: a transient upload failure
     * logs and continues (the session is already live).
     */
    private suspend fun republishBundleAfterRecover(
        accountId: ByteArray,
        accountIdentitySeed: ByteArray,
        sessionToken: String,
    ) {
        val accountIdB64 = base64Encode(accountId)

        // Defence-in-depth (ADR-0017): derive the OLD and NEW account
        // signature pubkeys. Both come from the UNCHANGED phrase seed,
        // so they MUST match. We assert it rather than assume it — if
        // it ever fails, it means the phrase that authenticated the
        // OPAQUE recover differs from the one seeding the identity, a
        // logic bug. Fail loud rather than silently publish a changed
        // key with no proof.
        val oldAccountPubkey: ByteArray
        val newAccountPubkey: ByteArray
        try {
            oldAccountPubkey =
                Identity.fromSeed(accountIdentitySeed).use { it.signaturePublicKey() }
            newAccountPubkey =
                Identity.fromSeed(accountIdentitySeed).use { it.signaturePublicKey() }
        } catch (cause: Throwable) {
            Log.e(
                "lakko-auth",
                "post-recover: failed to derive AccountIdentity pubkeys " +
                    "for the ADR-0017 emit gate — ABORTING the bundle " +
                    "re-publish (will not publish without verifying the " +
                    "account key is unchanged)",
                cause,
            )
            return
        }
        if (!oldAccountPubkey.contentEquals(newAccountPubkey)) {
            // Impossible on a phrase-seeded recover (same seed => same
            // key). If it ever happens, fail loud — never publish a
            // changed key with no proof.
            Log.e(
                "lakko-auth",
                "post-recover: ADR-0017 invariant violated — recover " +
                    "produced a different AccountIdentity pubkey than " +
                    "expected (same phrase => same identity). ABORTING the " +
                    "bundle re-publish to avoid breaking every contact's pin.",
            )
            return
        }

        // Proof-less re-publish (account key unchanged), which also clears any
        // stale server proof column. It carries a genesis self-signature
        // ([IdentityPublication.REPUBLISH]): ignored when the key is already
        // logged, and what authorises the log entry when a recover is the
        // directory's first sight of the account. Best-effort — the session is
        // already live. The KeyPackage is NOT published from this short-lived
        // handle; the chat bootstrap after a recover does that.
        val outcome = try {
            publishIdentityFor(
                landing = IdentityLanding.RECOVER,
                accountId = accountId,
                accountIdentitySeed = accountIdentitySeed,
                sessionToken = sessionToken,
            )
        } catch (cause: Throwable) {
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
        mutableIdentityAlert.value = null
        // What only makes sense inside the sign-in that is ending. The owed
        // markers stay on disk for a logout (the phrase is shown again at the
        // next session); Forget device removes them with the account's data.
        mutableSignupPhrase.value = null
        mutableRestoreNotice.value = null
        mutableNewPasswordOwed.value = false
        mutableFreshCredentialPending.value = false
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

    /** ADR-0017 lost-everything fallback. The user recovered ACCESS via
     *  an ADR-0018 factor but has NO recovery phrase and NO other device,
     *  and chose to mint a brand-new identity. The new bundle is already
     *  published (without a supersession proof) and the new phrase minted;
     *  the UI must show the phrase for the user to save before the session
     *  lands. [AuthRepository.confirmFreshPhraseSaved] finishes it. */
    public data class SaveFreshPhrase(val account: SessionAccount) : AuthState
}

/**
 * ADR-0017 defence-in-depth (lakko-security Phase-3A follow-up (i)) — a
 * security alert raised when this device's canonical AccountIdentity did
 * not match the account's currently-published bundle at login. Mirror of
 * the web / iOS `IdentityAlert`.
 */
public enum class IdentityAlert {
    /** Someone published a DIFFERENT identity for this account (the
     *  ACCESS-hijack signature — an attacker minted a fresh identity via
     *  the lost-everything path). */
    PublishedIdentityChanged,
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
private const val HTTP_NOT_FOUND: Int = 404
private const val HTTP_SERVICE_UNAVAILABLE: Int = 503
private const val ACCOUNT_ID_LENGTH: Int = 16

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
    /** Phase 1.5b: bearer token issued by the auth service on the
     *  most recent signup/login/recover. Threaded into
     *  `DiscoveryClient` so every protected write carries the
     *  `Authorization: Bearer <token>` header. */
    val sessionToken: String,
    /** Token expiry as a unix-millisecond timestamp. */
    val sessionExpiresAtMs: Long,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is IdentitySecrets) return false
        return accountId.contentEquals(other.accountId) &&
            exportKey.contentEquals(other.exportKey) &&
            accountIdentitySeed.contentEquals(other.accountIdentitySeed) &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs
    }

    override fun hashCode(): Int {
        var h = accountId.contentHashCode()
        h = 31 * h + exportKey.contentHashCode()
        h = 31 * h + accountIdentitySeed.contentHashCode()
        h = 31 * h + sessionToken.hashCode()
        h = 31 * h + sessionExpiresAtMs.hashCode()
        return h
    }
}

/** Result handed back to the signup ViewModel so it can show
 *  the freshly-generated 12-word recovery passphrase exactly once. */
public data class SignupOutcome(
    /** The 12-word BIP39 recovery phrase. The UI MUST display this
     *  to the user and require an explicit "I have written it
     *  down" acknowledgement before navigating away. Re-display
     *  is impossible — the words live in memory only and drop the
     *  moment the user moves on. */
    val recoveryPhrase: String,
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
    val sessionToken: String,
    val sessionExpiresAtMs: Long,
    val username: String,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is PendingRecovery) return false
        return accountIdBytes.contentEquals(other.accountIdBytes) &&
            accountIdB64 == other.accountIdB64 &&
            exportKey.contentEquals(other.exportKey) &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs &&
            username == other.username
    }

    override fun hashCode(): Int {
        var h = accountIdBytes.contentHashCode()
        h = 31 * h + accountIdB64.hashCode()
        h = 31 * h + exportKey.contentHashCode()
        h = 31 * h + sessionToken.hashCode()
        h = 31 * h + sessionExpiresAtMs.hashCode()
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
    val sessionToken: String,
    val sessionExpiresAtMs: Long,
    val username: String,
    val phrase: String,
    /** `true` while the server's recovery credential still belongs to the
     *  OLD phrase (the fresh identity is published but the credential
     *  replace has not succeeded yet). The phrase is not shown and the
     *  session does not land until it is `false`. Always `false` for a
     *  guest signup, which registers its credential itself. */
    val recoveryCredentialOwed: Boolean,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is PendingFreshPhrase) return false
        return seed.contentEquals(other.seed) &&
            accountIdBytes.contentEquals(other.accountIdBytes) &&
            accountIdB64 == other.accountIdB64 &&
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs &&
            username == other.username &&
            phrase == other.phrase &&
            recoveryCredentialOwed == other.recoveryCredentialOwed
    }

    override fun hashCode(): Int {
        var h = seed.contentHashCode()
        h = 31 * h + accountIdBytes.contentHashCode()
        h = 31 * h + accountIdB64.hashCode()
        h = 31 * h + sessionToken.hashCode()
        h = 31 * h + sessionExpiresAtMs.hashCode()
        h = 31 * h + username.hashCode()
        h = 31 * h + phrase.hashCode()
        h = 31 * h + recoveryCredentialOwed.hashCode()
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
