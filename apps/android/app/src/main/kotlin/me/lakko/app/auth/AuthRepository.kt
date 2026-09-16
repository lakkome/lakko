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
import kotlinx.coroutines.launch
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.push.PushSubscriber
import me.lakko.app.webauthn.PasskeyLoginResult
import me.lakko.app.webauthn.WebauthnClient
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.RecoveryMnemonic
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
 * 1.15d4 ships **signup-only**. Login and recover land in
 * 1.15d5/d6 and add the corresponding methods to this class.
 *
 * ## Discovery
 *
 * Like the web `AuthContext`, signup claims the `@username` in discovery
 * ([claimUsernameAtSignup]), login on a device that does not know the account
 * resolves the username there ([resolveLoginAccount]), and every landing that
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
) {
    /** 1.20d — dedicated scope for fire-and-forget push
     *  subscribe / unsubscribe coroutines. `SupervisorJob` so
     *  one failed leg doesn't tear down the others; bound to
     *  `Dispatchers.IO` because both legs are network-heavy. The
     *  scope's lifetime is the process — `AuthRepository` is a
     *  process-singleton in `LakkoApp.kt`. */
    private val pushScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** `@username` register (signup) and lookup (login on a device that does
     *  not know the account), over short-lived discovery clients. */
    private val accountDirectory: AccountDirectory =
        DiscoveryAccountDirectory(discoveryClientFactory)

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

    /**
     * ADR-0018 factor (d) — between [recoverWithContactStart] (which
     * sends the one-time code) and [recoverWithContactFinish] (which
     * submits it), we stash the resolved `(accountId, username, kind)`
     * here so the finish step targets the exact `(account, kind)` the
     * code was sent for without re-resolving the username. Cleared on
     * a successful finish (the code is single-use server-side; a retry
     * must re-send). Single-slot — only one recovery flow at a time.
     */
    private val pendingContactRecover: AtomicReference<PendingContactRecover?> =
        AtomicReference(null)

    /** ADR-0017 lost-everything fallback — the minted fresh phrase +
     *  completion context, parked between [mintFreshIdentity] and
     *  [confirmFreshPhraseSaved]. */
    private val pendingFreshPhrase: AtomicReference<PendingFreshPhrase?> =
        AtomicReference(null)

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
        // Discovery's rate-limited reads (OPRF / lookup / channel lookup) are
        // charged per account when they carry this session's token, per IP
        // otherwise; most discovery clients are built without a token, so
        // they read it from here. Only those three reads use it.
        me.lakko.app.discovery.AmbientSessionToken.install {
            identitySecrets.value?.sessionToken
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
                mutableIdentitySecrets.value = decision.secrets
                mutableState.value = AuthState.Session(account = decision.account)
                // ADR-0017 defence-in-depth — the same background self-check a
                // password login runs (is the published identity still ours?),
                // re-publishing only when the directory is missing the bundle
                // or holds other keys, so a relaunch does not pay a
                // proof-of-work solve for a no-op.
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
        mutableIdentitySecrets.value = secrets
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
     *   2a. Claim the username in discovery ([claimUsernameAtSignup]); a
     *      taken name fails signup with the form's message.
     *   3. Save the `(accountId, normalisedUsername)` marker to [SessionStore].
     *   4. Derive the device root key via [deriveDeviceRootKey].
     *   5. Wrap + persist via [DeviceKeyStore.putDeviceKey].
     *   6. Zeroize the OPAQUE export key + the derived root key.
     *   6a. Publish the identity bundle (genesis, fail loud).
     *   7. Flip state to [AuthState.Session].
     *   8. Return the 24-word recovery phrase for the UI to
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
    ): SignupOutcome {
        val trimmed = username.trim()
        if (trimmed.isEmpty()) {
            throw AuthClientArgError("username must be non-empty")
        }
        return authClientFactory().use { client ->
            val result = client.signup(password, enrolRecovery = true)
            try {
                val accountIdB64 = base64Encode(result.accountId)
                // Claim the username in discovery FIRST, before anything is
                // persisted on this device (web order). Without this binding
                // nobody can find the account by name — no contact request,
                // no add-to-group, no sign-in on another device. A taken name
                // fails the signup with the form's message; the OPAQUE account
                // then exists server-side with no username, which web accepts
                // too (the user retries with another name).
                val normalisedUsername = claimUsernameAtSignup(
                    directory = accountDirectory,
                    sessionToken = result.sessionToken,
                    accountId = result.accountId,
                    username = trimmed,
                )
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
                // Phase 2a1c — post-signup backup bootstrap.
                // Caches the phrase in EncryptedSharedPreferences
                // + posts a baseline /backup envelope. Best-
                // effort; failures log and continue.
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
     * @return the 24-word recovery phrase (also surfaced by [freshPhrase]).
     */
    public suspend fun signupGuest(): SignupOutcome {
        // 32 random bytes the user NEVER sees — recovery is phrase-only.
        val randomPassword = ByteArray(32).also { SecureRandom().nextBytes(it) }
        return authClientFactory().use { client ->
            // Same PoW-gated OPAQUE registration signup uses — no bypass.
            val result = client.signup(randomPassword, enrolRecovery = true)
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
                    ),
                )
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
     *   1. Resolve the typed username to an `accountId`
     *      ([resolveLoginAccount]): the on-device [SessionStore] marker
     *      answers when its username equals the typed one (no round
     *      trip); otherwise — a fresh device, or a different account —
     *      a discovery lookup binds it. An unregistered username fails
     *      as `invalid credentials`, the same error as a wrong password.
     *      (This used to refuse whenever the device held no cached
     *      account, so an account could only ever be used on the device
     *      that created it.)
     *   2. Call [AuthClient.login] with `(accountId, password)`.
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
        // The marker answers a same-account sign-in without a round trip; a
        // fresh device (or another account) resolves the typed username
        // through discovery. Only the TARGET account is decided here — the
        // ADR-0017 identity acquisition below is unchanged, so a device with
        // no cached phrase seed still lands in NeedsRecoveryPhrase.
        val account = resolveLoginAccount(
            cached = sessionStore.load()?.let {
                CachedAccount(accountIdB64 = it.accountIdB64, username = it.username)
            },
            username = trimmed,
            directory = accountDirectory,
            encodeAccountId = ::base64Encode,
        )
        val accountIdB64 = account.accountIdB64
        val resolvedUsername = account.username
        val accountIdBytes = base64Decode(accountIdB64)

        authClientFactory().use { client ->
            val result = client.login(accountIdBytes, password)
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
                // Upsert the marker with the RESOLVED account, so the next sign-in on
                // this device skips the lookup (and a cross-device first sign-in
                // leaves the device knowing whose account it holds).
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
     *      newPasswordBytes)` — one PoW solve, parallel OPAQUE
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
        // Resolve the account_id the same way `login` does (web parity)
        // so recover works on a FRESH device: a local cache hit wins;
        // otherwise a discovery.lookup on the supplied username binds
        // it. Without either we cannot target the recover. Extracted to
        // the pure [resolveRecoverAccount] helper (G-29) so the
        // cross-device behaviour is locked by a unit test.
        val cached = sessionStore.load()?.let {
            CachedAccount(accountIdB64 = it.accountIdB64, username = it.username)
        }
        val (accountIdB64, resolvedUsername) = resolveRecoverAccount(
            cached = cached,
            username = username,
            lookup = { name ->
                val result = discoveryClientFactory().use { it.lookup(name) }
                RecoverAccountLookup(
                    account = result.account,
                    normalisedUsername = result.normalisedUsername,
                )
            },
            encodeAccountId = ::base64Encode,
        )
        val accountIdBytes = base64Decode(accountIdB64)

        // G-4 (ADR-0016 / ADR-0017) — capture the OLD OPAQUE export
        // key BEFORE the recover rotates the device key + overwrites
        // the cached export key (the `ExportKeyStore.putExportKey`
        // below). Retained for diagnostics + symmetry with the iOS /
        // web flow; under ADR-0017 the canonical AccountIdentity is
        // phrase-seeded, so a routine recover does NOT rotate the
        // account key and no supersession proof is minted (the G-4
        // emit below compares old-vs-new account pubkeys and SKIPS the
        // proof when equal — the routine case).
        val oldExportKey: ByteArray? =
            ExportKeyStore.getExportKey(applicationContext, accountIdB64)

        // Parse the mnemonic up front. `fromPhrase` collapses
        // unknown-word and bad-checksum into one FfiException by
        // design (preserves BIP39's no-probing property). Burn
        // the seed buffer at the end — it's the only thing
        // standing between an attacker and the OPAQUE export key
        // we'll derive next.
        val seed = RecoveryMnemonic.fromPhrase(mnemonicPhrase).use { mnemonic ->
            mnemonic.toSeed()
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
                val result = client.recover(accountIdBytes, seed, newPassword)
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
                    me.lakko.app.devices.StateBundle.markRestored(applicationContext)
                    val restoredSnapshot = restoreFromServerBackupBestEffort(
                        sessionToken = result.sessionToken,
                        accountId = accountIdBytes,
                        accountIdB64 = accountIdB64,
                        phrase = mnemonicPhrase,
                        context = "recover",
                    )
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
                    // bootstrap: re-cache the phrase + post a
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
            // G-4 — the captured OLD export key is secret material
            // (the seed for the old AccountIdentity); wipe it too.
            oldExportKey?.fill(0)
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
     * @throws AuthClientArgError on a blank username or an unknown
     *         username ("username not found"), or a malformed code.
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
            )
        }
        // Resolve the username -> account_id exactly like the Recover
        // AnotherDevice mode: a local cache hit wins, else a
        // discovery.lookup round-trip binds it.
        val cached = sessionStore.load()
        val accountIdBytes: ByteArray
        val resolvedUsername: String
        if (cached != null && cached.username == trimmed) {
            accountIdBytes = base64Decode(cached.accountIdB64)
            resolvedUsername = cached.username
        } else {
            val lookup = discoveryClientFactory().use { it.lookup(trimmed) }
            val account = lookup.account
                ?: throw AuthClientArgError("username not found")
            accountIdBytes = account
            resolvedUsername = lookup.normalisedUsername
        }
        val accountIdB64 = base64Encode(accountIdBytes)

        val result = authClientFactory().use { client ->
            client.totpRecover(accountIdBytes, code)
        }

        // Shared ADR-0018 ACCESS-only landing (warm cached-seed finish
        // OR cold fail-safe park) — identical to the cascade's other
        // ACCESS factor (factor (d) phone/email OTP).
        landAfterAccessRecovery(
            username = resolvedUsername,
            accountIdBytes = accountIdBytes,
            accountIdB64 = accountIdB64,
            sessionToken = result.sessionToken,
            sessionExpiresAtMs = result.sessionExpiresAtMs,
            pushContext = "totp-recover",
        )
    }

    /**
     * ADR-0018 factor (d), step 1 — send a one-time code to a
     * previously-enrolled phone / email. Mirror of
     * `apps/web/src/auth/AuthContext.tsx::recoverWithContactStart`.
     *
     * ACCESS only: [recoverWithContactFinish] verifies the code and
     * issues the session triple; identity acquisition runs the
     * ADR-0017 state machine afterward.
     *
     * ENUMERATION-SAFE: the server returns the same `{ sent: true }`
     * whether or not the contact matches a confirmed row, so a
     * non-throwing return here only means "advance to the code step".
     * The resolved `(accountId, username, kind)` is stashed for the
     * finish step.
     *
     * @param kind     [ContactKind.EMAIL] (1) or [ContactKind.PHONE] (2).
     * @param contact  The raw phone / email plaintext.
     * @throws AuthClientArgError on a blank / unknown username or a
     *         malformed contact.
     * @throws AuthHttpError 503 when the factor is disabled
     *         server-side, 400 on a malformed contact.
     * @throws AuthNetworkError on transport failure.
     */
    public suspend fun recoverWithContactStart(
        username: String,
        kind: Int,
        contact: String,
    ) {
        val trimmed = username.trim()
        if (trimmed.isEmpty()) {
            throw AuthClientArgError(
                "enter the username of the account to recover",
            )
        }
        // Resolve the username -> account_id exactly like
        // [recoverWithTotp]: a local cache hit wins, else a
        // discovery.lookup round-trip binds it.
        val cached = sessionStore.load()
        val accountIdBytes: ByteArray
        val resolvedUsername: String
        if (cached != null && cached.username == trimmed) {
            accountIdBytes = base64Decode(cached.accountIdB64)
            resolvedUsername = cached.username
        } else {
            val lookup = discoveryClientFactory().use { it.lookup(trimmed) }
            val account = lookup.account
                ?: throw AuthClientArgError("username not found")
            accountIdBytes = account
            resolvedUsername = lookup.normalisedUsername
        }

        // Send the code. ENUMERATION-SAFE server-side: a non-throwing
        // return only means "advance to the code step".
        authClientFactory().use { client ->
            client.recoveryContactRecoverStart(accountIdBytes, kind, contact)
        }

        // Stash the resolved context so [recoverWithContactFinish] can
        // submit the code without re-resolving. The kind is carried
        // verbatim so the finish targets the exact (account, kind) the
        // code was sent for.
        pendingContactRecover.set(
            PendingContactRecover(
                accountIdBytes = accountIdBytes,
                accountIdB64 = base64Encode(accountIdBytes),
                username = resolvedUsername,
                kind = kind,
            ),
        )
    }

    /**
     * ADR-0018 factor (d), step 2 — submit the delivered code. Mirror
     * of `apps/web/src/auth/AuthContext.tsx::recoverWithContactFinish`.
     *
     * Uses the `(accountId, username, kind)` stashed by
     * [recoverWithContactStart]. On success the session triple drives
     * the shared [landAfterAccessRecovery] (warm same-device finish OR
     * cold fail-safe park), and the stash is consumed (the code is
     * single-use server-side; a retry must re-send via step 1).
     *
     * @throws AuthClientArgError when no code was requested first, or a
     *         malformed code.
     * @throws AuthHttpError 401 on a wrong/expired/replayed code
     *         (enumeration-safe), 429 under lockout, 503 when disabled.
     * @throws AuthNetworkError on transport failure.
     */
    public suspend fun recoverWithContactFinish(code: String) {
        val pending = pendingContactRecover.get()
            ?: throw AuthClientArgError(
                "request a code first (no pending phone / email recovery)",
            )
        val result = authClientFactory().use { client ->
            client.recoveryContactRecoverFinish(
                pending.accountIdBytes,
                pending.kind,
                code,
            )
        }
        // Consume the stash — the code is single-use server-side.
        pendingContactRecover.set(null)
        // Shared ADR-0018 ACCESS-only landing — identical to the
        // cascade's other ACCESS factor (factor (c) TOTP).
        landAfterAccessRecovery(
            username = pending.username,
            accountIdBytes = pending.accountIdBytes,
            accountIdB64 = pending.accountIdB64,
            sessionToken = result.sessionToken,
            sessionExpiresAtMs = result.sessionExpiresAtMs,
            pushContext = "contact-recover",
        )
    }

    /**
     * ADR-0018 — the shared post-ACCESS landing for the password-less
     * recovery factors (factor (c) TOTP, factor (d) phone/email OTP).
     * Mirror of `apps/web/src/auth/AuthContext.tsx::landAfterAccessRecovery`.
     *
     * Extracted verbatim from the original [recoverWithTotp] tail — the
     * warm / cold branches are byte-for-byte the prior behavior:
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
    private fun landAfterAccessRecovery(
        username: String,
        accountIdBytes: ByteArray,
        accountIdB64: String,
        sessionToken: String,
        sessionExpiresAtMs: Long,
        pushContext: String,
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
            installIdentitySecrets(
                accountIdB64,
                IdentitySecrets(
                    accountId = accountIdBytes,
                    exportKey = ByteArray(0),
                    accountIdentitySeed = accountIdentitySeed,
                    sessionToken = sessionToken,
                    sessionExpiresAtMs = sessionExpiresAtMs,
                ),
            )
            pendingRecovery.set(null)
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
        val accountIdentitySeed = RecoveryMnemonic.fromPhrase(mnemonicPhrase).use {
            it.toSeed()
        }
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
     * button. Mirror of the web `AuthContext.mintFreshIdentity`. Does NOT
     * land the session here — it flips to [AuthState.SaveFreshPhrase] and
     * returns the new phrase so the UI can show it; [confirmFreshPhraseSaved]
     * lands the session after the user acknowledges.
     *
     * @throws AuthClientArgError when there is no pending recovery.
     */
    public suspend fun mintFreshIdentity(): String {
        val pending = pendingRecovery.get()
            ?: throw AuthClientArgError("no pending identity recovery — sign in first")

        // Generate a fresh 24-word recovery phrase + its seed (same client-
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
        publishIdentityFor(
            landing = IdentityLanding.FRESH_IDENTITY,
            accountId = pending.accountIdBytes,
            accountIdentitySeed = seed,
            sessionToken = pending.sessionToken,
        )

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
            ),
        )
        pendingRecovery.set(null)
        mutableState.value = AuthState.SaveFreshPhrase(
            account = SessionAccount(
                accountIdB64 = pending.accountIdB64,
                username = pending.username,
            ),
        )
        return phrase
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
    private suspend fun awaitStaleOverride(reason: String): Boolean {
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
        val pending = pendingFreshPhrase.getAndSet(null)
            ?: throw AuthClientArgError("no fresh identity to confirm")
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
     * ADR-0018 factor (a), path (a) — complete a pending
     * `NeedsRecoveryPhrase` login using the identity seed received
     * from ANOTHER DEVICE over the E2EE pairing channel (instead of
     * typing the 24 words). Same fail-closed gate as
     * [provideRecoveryPhrase]; prefers the freshly REDEEMED session
     * token when the joiner's grant redeem succeeded.
     *
     * Takes ownership of `phraseSeed` (zeroized on every refusal).
     *
     * @throws PhraseMismatch / IdentityVerificationUnavailable — same
     *         semantics as [provideRecoveryPhrase].
     * @throws AuthClientArgError when there is no pending recovery.
     */
    public suspend fun completeFromPairing(
        phraseSeed: ByteArray,
        sessionToken: String? = null,
        sessionExpiresAtMs: Long? = null,
    ) {
        val pending = pendingRecovery.get()
            ?: throw AuthClientArgError("no pending identity recovery — sign in first")
        var seedHandedOff = false
        try {
            verifySeedAgainstPublishedIdentity(
                accountIdBytes = pending.accountIdBytes,
                sessionToken = sessionToken ?: pending.sessionToken,
                accountIdentitySeed = phraseSeed,
            )
            // ADR-0018 factor (c): a TOTP-recovered fresh device parked
            // with NO device root key. Bootstrap it from the VERIFIED
            // transferred seed, strictly AFTER the fail-closed
            // verification above — a refusal must leave this device
            // untouched. No-op when a key already exists.
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
     * identity seed + the REDEEMED session token (the only ACCESS on
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
     * Takes ownership of `phraseSeed` (zeroized on every refusal).
     */
    public suspend fun completeFromDeviceRestore(
        accountIdBytes: ByteArray,
        accountIdB64: String,
        username: String,
        phraseSeed: ByteArray,
        sessionToken: String,
        sessionExpiresAtMs: Long,
    ) {
        var seedHandedOff = false
        try {
            verifySeedAgainstPublishedIdentity(
                accountIdBytes = accountIdBytes,
                sessionToken = sessionToken,
                accountIdentitySeed = phraseSeed,
            )
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
            seedHandedOff = true
            pendingRecovery.set(null)
            mutableState.value = AuthState.Session(
                account = SessionAccount(
                    accountIdB64 = accountIdB64,
                    username = username,
                ),
            )
        } finally {
            if (!seedHandedOff) {
                phraseSeed.fill(0)
            }
        }
    }

    /**
     * Phase 2a2c2-android — sign in with a WebAuthn passkey on
     * a device that has already been seen by this account at
     * least once (signup / login / recover ran here in the past
     * + populated the export-key cache).
     *
     * Drives the Credential Manager discoverable-credential
     * flow (no username / password typed in the UI). On a
     * successful assertion, resolves the cached export key from
     * [ExportKeyStore] and reconstructs the full
     * [IdentitySecrets] pipeline — same shape as
     * [login(username:password)] minus the OPAQUE round-trip.
     *
     * Fresh-device UX is **unsupported** by design: without a
     * locally-cached export key the passkey gives us only a
     * session token, but no path to derive Identity / device
     * key / MLS bootstrap. The recovery phrase remains the only
     * fresh-device entry route. The Login screen gates the
     * "Sign in with passkey" button on the cached marker so
     * this error path is rare in practice.
     *
     * @throws AuthClientArgError when there is no cached
     *         account, the passkey belongs to a different
     *         account than the cached one, or the export-key
     *         cache is empty / malformed.
     */
    public suspend fun loginWithPasskey() {
        val cached = sessionStore.load()
            ?: throw AuthClientArgError(
                "No cached account on this device. Sign in with " +
                    "password or recover with your phrase first.",
            )

        val webauthnClient = WebauthnClient()
        val assertion: PasskeyLoginResult? = try {
            webauthnClient.loginWithPasskey(applicationContext)
        } finally {
            webauthnClient.close()
        }
        // Credential Manager returned null only on user cancel
        // — surface as an arg error so the form layer can show
        // a generic "cancelled" message.
        assertion ?: throw AuthClientArgError(
            "Cancelled. No passkey sign-in happened.",
        )

        // Compare via raw bytes — the cached `accountIdB64`
        // rides as Foundation/JVM-standard padded base64, while
        // the server's auth_finish response is URL-safe base64
        // without padding (URL_SAFE_NO_PAD on the wire). A
        // string comparison would always mismatch.
        val cachedAccountBytes: ByteArray = try {
            android.util.Base64.decode(
                cached.accountIdB64,
                android.util.Base64.NO_WRAP,
            )
        } catch (_: IllegalArgumentException) {
            throw AuthClientArgError("Malformed cached account_id.")
        }
        val assertionAccountBytes: ByteArray = try {
            android.util.Base64.decode(
                assertion.accountIdB64,
                android.util.Base64.URL_SAFE or
                    android.util.Base64.NO_WRAP or
                    android.util.Base64.NO_PADDING,
            )
        } catch (_: IllegalArgumentException) {
            throw AuthClientArgError("Malformed account_id in passkey assertion.")
        }
        if (!cachedAccountBytes.contentEquals(assertionAccountBytes)) {
            throw AuthClientArgError(
                "This passkey belongs to a different account than the " +
                    "one cached on this device. Sign in with password " +
                    "first to enable passkey on this device.",
            )
        }

        val exportKey = ExportKeyStore.getExportKey(
            applicationContext,
            cached.accountIdB64,
        ) ?: throw AuthClientArgError(
            "No cached credentials for passkey sign-in on this device. " +
                "Sign in with password or recover with your phrase first.",
        )

        // Re-derive the device root key from the cached export
        // key. OPAQUE export key is deterministic — the upsert
        // here is a no-op for an unchanged password but keeps
        // the row's last-write timestamp moving.
        val deviceRootKey = deriveDeviceRootKey(exportKey, cached.accountIdB64)
        try {
            deviceKeyStore.putDeviceKey(
                context = applicationContext,
                accountIdB64 = cached.accountIdB64,
                rootKey = deviceRootKey,
            )
        } finally {
            deviceRootKey.fill(0)
        }
        // Re-upsert the cache too so any future eviction policy
        // sees the row as recently touched.
        try {
            ExportKeyStore.putExportKey(
                applicationContext,
                cached.accountIdB64,
                exportKey,
            )
        } catch (cause: Throwable) {
            Log.w("lakko-auth", "export-key persist (passkey login) failed", cause)
        }
        sessionStore.save(cached)

        // ADR-0017: build the canonical identity from the cached
        // AccountIdentity (phrase) seed, NOT the export key. Passkey
        // resume only runs on a device that already cached the export
        // key, so the phrase seed is normally present too — but a
        // device that last logged in before this cache existed may
        // lack it, in which case we fail closed into the
        // NeedsRecoveryPhrase state (never derive from exportKey,
        // never mint fresh).
        val accountIdentitySeed: ByteArray? = try {
            AccountIdentitySeedStore.getAccountIdentitySeed(
                applicationContext,
                cached.accountIdB64,
            )
        } catch (cause: Throwable) {
            Log.w(
                "lakko-auth",
                "account-identity-seed read failed (passkey login)",
                cause,
            )
            null
        }

        if (accountIdentitySeed == null) {
            pendingRecovery.set(
                PendingRecovery(
                    accountIdBytes = cachedAccountBytes,
                    accountIdB64 = cached.accountIdB64,
                    exportKey = exportKey.copyOf(),
                    sessionToken = assertion.sessionToken,
                    sessionExpiresAtMs = assertion.sessionExpiresAtMs,
                    username = cached.username,
                ),
            )
            mutableState.value = AuthState.NeedsRecoveryPhrase(
                account = SessionAccount(
                    accountIdB64 = cached.accountIdB64,
                    username = cached.username,
                ),
            )
            // Best-effort wipe of the local export-key copy in the
            // suspend frame. The PendingRecovery slot above carries an
            // independent copyOf().
            exportKey.fill(0)
            subscribePushBestEffort("passkey-login")
            return
        }

        installIdentitySecrets(
            cached.accountIdB64,
            IdentitySecrets(
                accountId = cachedAccountBytes,
                exportKey = exportKey.copyOf(),
                accountIdentitySeed = accountIdentitySeed,
                sessionToken = assertion.sessionToken,
                sessionExpiresAtMs = assertion.sessionExpiresAtMs,
            ),
        )
        mutableState.value = AuthState.Session(
            account = SessionAccount(
                accountIdB64 = cached.accountIdB64,
                username = cached.username,
            ),
        )
        // ADR-0017 defence-in-depth + directory refresh, in the background
        // (same as a password login): alert on a mismatch, then re-publish.
        refreshPublishedIdentityInBackground(
            landing = IdentityLanding.SIGN_IN,
            accountIdBytes = cachedAccountBytes,
            accountIdentitySeed = accountIdentitySeed,
            sessionToken = assertion.sessionToken,
        )
        // Best-effort wipe of the local export-key copy in the
        // suspend frame. The `IdentitySecrets.exportKey` slot
        // above carries an independent `copyOf()` for the
        // ChatRepository / ContactsClient lifetime.
        exportKey.fill(0)
        subscribePushBestEffort("passkey-login")
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
        clearIdentitySecrets()
        // ADR-0017: drop any parked fresh-device fail-safe context
        // (a NeedsRecoveryPhrase user who backed out via Sign out).
        clearPendingRecovery()
        mutableState.value = AuthState.Anon(cachedUsername = cached?.username)
        // 1.20d — fire-and-forget push unsubscribe. The
        // EncryptedSharedPreferences pseudonym + the server-
        // side token row both go; subsequent FCM wakes for
        // this device are no-ops until the next subscribe.
        unsubscribePushBestEffort("logout")
    }

    /**
     * Drop the in-memory session **and** the on-device material.
     *
     * Mirror of the web's `forgetDevice` semantics. Distinct from
     * [logout]: this wipes both the [SessionStore] record and the
     * Keystore-wrapped device key envelope. After `forgetDevice`
     * the only way back into the account is via the recovery
     * passphrase (1.15d6 `/recover` flow).
     */
    public fun forgetDevice() {
        val cached = sessionStore.load()
        if (cached != null) {
            // The persisted bearer token goes too — a stale row would let a
            // later cold start re-attach to this account's session.
            deletePersistedSessionTokenBestEffort(cached.accountIdB64, "forgetDevice")
            try {
                deviceKeyStore.deleteDeviceKey(applicationContext, cached.accountIdB64)
            } catch (_: Throwable) {
                // Best-effort wipe; SessionStore.clear() below is
                // still worth running.
            }
            // Phase 2a1c — drop the cached recovery phrase. The
            // EncryptedSharedPreferences row is independent of
            // the device key envelope above, so we wipe it
            // explicitly. Best-effort.
            try {
                me.lakko.app.backup.PhraseStore.deletePhrase(
                    applicationContext,
                    cached.accountIdB64,
                )
            } catch (_: Throwable) {
                // best-effort
            }
            // ADR-0037 (G-11) — drop the backup-head version floor. Its
            // EncryptedSharedPreferences row is independent of the phrase / key
            // above; a stale floor would confuse a future signup on the same
            // accountId. Best-effort.
            try {
                me.lakko.app.backup.FloorStore.deleteFloor(
                    applicationContext,
                    cached.accountIdB64,
                )
            } catch (_: Throwable) {
                // best-effort
            }
            // Phase 2a2c2 — drop the cached export key too. Same
            // clean-wipe rationale as the phrase + device-key
            // rows above.
            try {
                ExportKeyStore.deleteExportKey(
                    applicationContext,
                    cached.accountIdB64,
                )
            } catch (_: Throwable) {
                // best-effort
            }
            // Phase 2a3b0 — drop the cached DeviceIdentity
            // seed. The leaf in the user's DeviceGroup that
            // this seed signed for is orphaned; sibling
            // devices clean it up via 2a3c's remove-device
            // flow.
            try {
                DeviceIdentityStore.deleteDeviceIdentitySeed(
                    applicationContext,
                    cached.accountIdB64,
                )
            } catch (_: Throwable) {
                // best-effort
            }
            // ADR-0017 — drop the cached AccountIdentity (phrase)
            // seed. It is the highest-sensitivity on-device secret;
            // a stale row would confuse a future signup on the same
            // accountId. After this wipe the only way back into the
            // account is the recovery phrase (or a paired device).
            try {
                AccountIdentitySeedStore.deleteAccountIdentitySeed(
                    applicationContext,
                    cached.accountIdB64,
                )
            } catch (_: Throwable) {
                // best-effort
            }
            // Phase 2a3b4 — drop the per-account DeviceGroup
            // snapshot envelope. Without the snapshot the local
            // OpenMLS provider has no DeviceGroup state to
            // restore, mirroring the iOS / web `forgetDevice`
            // surface. The Keystore wrap key under
            // `DeviceGroupSnapshotStore.WRAP_KEY_ALIAS` is
            // shared across accounts so we leave it intact;
            // only the per-account envelope file is removed.
            try {
                me.lakko.app.devices.DeviceGroupSnapshotStore.deleteSnapshot(
                    applicationContext,
                    cached.accountIdB64,
                )
            } catch (_: Throwable) {
                // best-effort
            }
        }
        sessionStore.clear()
        clearIdentitySecrets()
        // ADR-0017: drop any parked fresh-device fail-safe context.
        clearPendingRecovery()
        mutableState.value = AuthState.Anon()
        // 1.20d — full push wipe. Fire-and-forget — the local
        // `DevicePushId.clear()` inside the subscriber runs
        // regardless of the server leg outcome, honouring
        // forgetDevice's "leave NOTHING per-account behind"
        // contract.
        unsubscribePushBestEffort("forgetDevice")
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
    ): ByteArray {
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
                    return ByteArray(0)
                }
                is me.lakko.app.backup.GatedRestoreOutcome.WrongPhraseOrTampered -> {
                    Log.w("lakko-auth", "backup restore ($context): wrong phrase or tampered")
                    return ByteArray(0)
                }
                is me.lakko.app.backup.GatedRestoreOutcome.RollbackBlocked -> {
                    if (outcome.hard) {
                        // Unambiguous tamper / corruption — alarm + refuse (no
                        // backup applied). Raise the dismissible security banner.
                        Log.e(
                            "lakko-auth",
                            "backup restore ($context): ROLLBACK BLOCKED (hard) - ${outcome.reason}",
                        )
                        mutableRollbackAlert.value = BackupRollbackAlert(outcome.reason)
                    } else {
                        // Stale head the user DECLINED to override.
                        Log.w(
                            "lakko-auth",
                            "backup restore ($context): stale head override declined - ${outcome.reason}",
                        )
                    }
                    return ByteArray(0)
                }
                is me.lakko.app.backup.GatedRestoreOutcome.TransportFailed -> {
                    Log.w("lakko-auth", "backup restore ($context) failed", outcome.cause)
                    return ByteArray(0)
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
                val existing =
                    me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext)
                val restored =
                    me.lakko.app.contacts.reconstructRestoredContacts(
                        existing,
                        parsed.acceptedContacts,
                        parsed.createdAtMs.toULong(),
                    )
                if (restored.isNotEmpty()) {
                    me.lakko.app.contacts.AcceptedContactsStore.save(
                        applicationContext,
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
            return parsed.wholeStorageSnapshot
        } finally {
            client.close()
            phraseBytes.fill(0)
        }
    }

    /**
     * Phase 2a1c — cache the phrase + post a baseline backup
     * envelope. Called from [signup] and [recover] right after
     * the state flip. Best-effort end-to-end; every step logs
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
    ) {
        val phraseBytes = phrase.toByteArray(Charsets.UTF_8)
        val backupClient = me.lakko.app.backup.BackupClient()
        try {
            backupClient.setSessionToken(sessionToken)
            try {
                backupClient.cachePhrase(applicationContext, accountIdB64, phraseBytes)
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "backup phrase cache ($context) failed", cause)
                // Continue — baseline envelope upload can still
                // proceed without the cache.
            }
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
                me.lakko.app.contacts.AcceptedContactsStore.load(applicationContext).map { c ->
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
        pushScope.launch {
            try {
                PushSubscriber(applicationContext).subscribe()
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "push subscribe ($context) failed", cause)
            }
        }
    }

    /** 1.20d — Mirror of [subscribePushBestEffort] for the
     *  unregister path. */
    private fun unsubscribePushBestEffort(context: String) {
        pushScope.launch {
            try {
                PushSubscriber(applicationContext).unsubscribe()
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
    private fun clearIdentitySecrets() {
        mutableIdentitySecrets.value?.let { secrets ->
            secrets.exportKey.fill(0)
            // ADR-0017: the AccountIdentity (phrase) seed is the
            // highest-sensitivity in-memory secret — wipe it too.
            secrets.accountIdentitySeed.fill(0)
            secrets.accountId.fill(0)
        }
        mutableIdentitySecrets.value = null
    }

    /** ADR-0017 — drop any parked fresh-device fail-safe context,
     *  wiping the export-key copy it retained. Called from
     *  [logout] / [forgetDevice] so a backed-out NeedsRecoveryPhrase
     *  user leaves no secret material behind. Also drops any pending
     *  ADR-0018 phone/email recovery routing context (no secrets, but
     *  it should not survive a sign-out). */
    private fun clearPendingRecovery() {
        pendingRecovery.getAndSet(null)?.let { pending ->
            pending.exportKey.fill(0)
            pending.accountIdBytes.fill(0)
        }
        pendingContactRecover.set(null)
        pendingFreshPhrase.getAndSet(null)?.let { it.seed.fill(0) }
        mutableIdentityAlert.value = null
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
     *  successfully (password or passkey ACCESS) but this device has
     *  NO cached AccountIdentity (phrase) seed, so we cannot rebuild
     *  the canonical identity. We MUST NOT derive it from the export
     *  key (wrong identity now) nor silently mint a fresh one (breaks
     *  every contact's pin). The UI renders a recovery-phrase prompt;
     *  [AuthRepository.provideRecoveryPhrase] completes the session. */
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
    /** Human-readable reason the head was flagged stale, shown in the dialog. */
    val reason: String,
)

/**
 * ADR-0037 §6 (G-11) — a security alarm raised when a restore was HARD-refused
 * by the rollback gate (an unambiguous tamper / corruption of the served backup
 * head). Surfaced on [AuthRepository.rollbackAlert] as a dismissible banner; no
 * backup was applied.
 */
public data class BackupRollbackAlert(
    /** Human-readable reason the head was rejected, shown in the banner. */
    val reason: String,
)

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
 *  the freshly-generated 24-word recovery passphrase exactly once. */
public data class SignupOutcome(
    /** The 24-word BIP39 recovery phrase. The UI MUST display this
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
 * ADR-0018 factor (d) — the resolved `(accountId, username, kind)`
 * stashed between [AuthRepository.recoverWithContactStart] (which
 * sends the one-time code) and [AuthRepository.recoverWithContactFinish]
 * (which submits it). Carries NO secret material — just the routing
 * context for the finish call.
 *
 * Mirror of the web `AuthContext`'s `pendingContactRecover` signal.
 */
internal data class PendingContactRecover(
    val accountIdBytes: ByteArray,
    val accountIdB64: String,
    val username: String,
    /** [ContactKind.EMAIL] (1) or [ContactKind.PHONE] (2). */
    val kind: Int,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is PendingContactRecover) return false
        return accountIdBytes.contentEquals(other.accountIdBytes) &&
            accountIdB64 == other.accountIdB64 &&
            username == other.username &&
            kind == other.kind
    }

    override fun hashCode(): Int {
        var h = accountIdBytes.contentHashCode()
        h = 31 * h + accountIdB64.hashCode()
        h = 31 * h + username.hashCode()
        h = 31 * h + kind
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
            phrase == other.phrase
    }

    override fun hashCode(): Int {
        var h = seed.contentHashCode()
        h = 31 * h + accountIdBytes.contentHashCode()
        h = 31 * h + accountIdB64.hashCode()
        h = 31 * h + sessionToken.hashCode()
        h = 31 * h + sessionExpiresAtMs.hashCode()
        h = 31 * h + username.hashCode()
        h = 31 * h + phrase.hashCode()
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
public class IdentityVerificationUnavailable(message: String) :
    IllegalStateException(message)
