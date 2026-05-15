// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import android.util.Log
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import me.lakko.app.push.PushSubscriber
import uniffi.lakko_ffi_uniffi.RecoveryMnemonic

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
 * ## What this class deliberately doesn't do
 *
 * The web `AuthContext` also handles username registration on the
 * discovery service, KeyPackage upload, MLS state rehydration,
 * and ChatContext fan-out. None of that surface exists on the
 * Android side yet (discovery client lands in 1.15e, MLS chat
 * surface in 1.15f). When those crates show up we extend this
 * repository — the shape is intentional, not vestigial.
 */
public class AuthRepository(
    private val applicationContext: Context,
    private val sessionStore: SessionStore = SessionStore(applicationContext),
    private val deviceKeyStore: DeviceKeyStore = DeviceKeyStore,
    /** Lazy because constructing a Ktor [AuthClient] eagerly costs
     *  ~50 ms (OkHttp engine + thread pool). The login / signup
     *  screens only need it on submit, not on first paint. */
    private val authClientFactory: () -> AuthClient = { AuthClient() },
) {
    /** 1.20d — dedicated scope for fire-and-forget push
     *  subscribe / unsubscribe coroutines. `SupervisorJob` so
     *  one failed leg doesn't tear down the others; bound to
     *  `Dispatchers.IO` because both legs are network-heavy. The
     *  scope's lifetime is the process — `AuthRepository` is a
     *  process-singleton in `LakkoApp.kt`. */
    private val pushScope: CoroutineScope =
        CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val mutableState: MutableStateFlow<AuthState> =
        MutableStateFlow(AuthState.Loading)

    public val state: StateFlow<AuthState> = mutableState.asStateFlow()

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
     * **NOT populated on a session restore from disk**: the
     * export key is not persisted (only the device root key,
     * which is a one-way HKDF derivation — the export key cannot
     * be recovered from the device key bytes). A returning user
     * whose session rehydrates from `SessionStore` lands in a
     * state where `state` is `Session` but `identitySecrets` is
     * `null`. Operations that need to sign on behalf of the user
     * (contact-request send, MLS commits, …) MUST check this and
     * surface a "please re-enter your password" prompt when it's
     * `null`.
     *
     * The shape mirrors the web `IdentitySecrets` precisely so
     * the cross-platform mental model stays uniform: same
     * lifecycle, same null-after-rehydrate, same wipe-on-logout.
     */
    private val mutableIdentitySecrets: MutableStateFlow<IdentitySecrets?> =
        MutableStateFlow(null)

    public val identitySecrets: StateFlow<IdentitySecrets?> =
        mutableIdentitySecrets.asStateFlow()

    init {
        // Rehydrate synchronously — both reads are tiny and SharedPreferences
        // is on-disk-but-process-cached. If we ever switch to DataStore
        // (which is suspend-only) move this to a coroutine in a `viewModelScope`.
        mutableState.value = rehydrate()
    }

    /** Inspect persisted state and resolve the initial [AuthState]:
     *
     *   * No `SessionStore` record → fresh device, [AuthState.Anon].
     *   * Record present but device key absent (Keystore master
     *     deleted, file truncated, etc.) → [AuthState.Anon] with
     *     `cachedUsername` so the Login form can pre-fill.
     *   * Record + device key both present → [AuthState.Session]. */
    private fun rehydrate(): AuthState {
        val cached = sessionStore.load() ?: return AuthState.Anon()
        // We don't *use* the device key bytes here; we just check
        // they exist. Reading them through Keystore on cold start
        // would warm up the cipher but isn't free; the
        // file-exists() probe is enough for the gate.
        val keyBytes = try {
            deviceKeyStore.getDeviceKey(applicationContext, cached.accountIdB64)
        } catch (cause: Throwable) {
            // Keystore failure (revoked master, corrupted blob,
            // etc.) — treat as "no key on this device" so the
            // user re-enters their password rather than seeing
            // the app crash.
            null
        }
        if (keyBytes == null) {
            return AuthState.Anon(cachedUsername = cached.username)
        }
        // Wipe immediately — we never need the bytes here, only
        // the existence signal. The actual consumer (1.15f's MLS
        // storage layer) will re-fetch via `getDeviceKey` on
        // demand.
        keyBytes.fill(0)
        return AuthState.Session(
            account = SessionAccount(
                accountIdB64 = cached.accountIdB64,
                username = cached.username,
            ),
        )
    }

    /**
     * Drive the full OPAQUE-PAKE signup flow.
     *
     * Steps:
     *   1. Validate the username (non-empty after trim).
     *   2. Call [AuthClient.signup] with `enrolRecovery = true`.
     *   3. Save the `(accountId, username)` marker to [SessionStore].
     *   4. Derive the device root key via [deriveDeviceRootKey].
     *   5. Wrap + persist via [DeviceKeyStore.putDeviceKey].
     *   6. Zeroize the OPAQUE export key + the derived root key.
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
                sessionStore.save(
                    PersistedSession(
                        accountIdB64 = accountIdB64,
                        username = trimmed,
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
                // Stash the export key for ContactsClient / future
                // ChatClient + the like. The bytes survive until
                // logout / forgetDevice clears the flow; signup is
                // a one-shot ceremony so we don't expect any
                // surprises here.
                mutableIdentitySecrets.value = IdentitySecrets(
                    accountId = result.accountId,
                    exportKey = result.exportKey.copyOf(),
                    sessionToken = result.sessionToken,
                    sessionExpiresAtMs = result.sessionExpiresAtMs,
                )
                mutableState.value = AuthState.Session(
                    account = SessionAccount(
                        accountIdB64 = accountIdB64,
                        username = trimmed,
                    ),
                )
                // 1.20d — best-effort push subscribe. Errors are
                // logged inside the subscriber + never thrown
                // past the auth flow.
                subscribePushBestEffort("signup")
                val phrase = result.recoveryPhrase ?: throw IllegalStateException(
                    "internal: signup completed without a recovery phrase",
                )
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
     * Drive the full OPAQUE-PAKE login flow.
     *
     * Mirror of `apps/web/src/auth/AuthContext.tsx::login`. Steps:
     *   1. Resolve the typed username to an `accountId`. In 1.15d5
     *      the only resolver is the on-device [SessionStore] cache
     *      — if the typed username matches the cached entry, we
     *      use the cached `accountId` and skip a round-trip; if it
     *      doesn't, we throw `invalid credentials` (the same error
     *      as a wrong password — indistinguishable failure preserves
     *      OPAQUE's no-probing property). Cross-device login via
     *      the discovery service is 1.15e's job; until then a fresh
     *      device requires running signup first.
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
        val cached = sessionStore.load()
            ?: throw AuthHttpError(
                status = 401,
                endpoint = "/login/start",
                body = "no account on this device — sign up first",
            )
        if (cached.username != trimmed) {
            // Cross-device login resolves via the discovery service
            // (1.15e); until then a mismatched username is the same
            // failure mode as a wrong password.
            throw AuthHttpError(
                status = 401,
                endpoint = "/login/start",
                body = "invalid credentials",
            )
        }
        val accountIdBytes = base64Decode(cached.accountIdB64)

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
                val deviceRootKey = deriveDeviceRootKey(result.exportKey, cached.accountIdB64)
                try {
                    deviceKeyStore.putDeviceKey(
                        context = applicationContext,
                        accountIdB64 = cached.accountIdB64,
                        rootKey = deviceRootKey,
                    )
                } finally {
                    deviceRootKey.fill(0)
                }
                sessionStore.save(cached)  // upsert — schema-versioned marker
                // Stash the export key + session token for
                // ContactsClient / DiscoveryClient.
                mutableIdentitySecrets.value = IdentitySecrets(
                    accountId = accountIdBytes,
                    exportKey = result.exportKey.copyOf(),
                    sessionToken = result.sessionToken,
                    sessionExpiresAtMs = result.sessionExpiresAtMs,
                )
                mutableState.value = AuthState.Session(
                    account = SessionAccount(
                        accountIdB64 = cached.accountIdB64,
                        username = cached.username,
                    ),
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
    ) {
        if (mnemonicPhrase.isBlank()) {
            throw AuthClientArgError("recovery phrase must be non-empty")
        }
        if (newPassword.isEmpty()) {
            throw AuthClientArgError("new password must be non-empty")
        }
        val cached = sessionStore.load()
            ?: throw AuthClientArgError(
                "no account on this device — cross-device recovery " +
                    "via the discovery service arrives in 1.15e",
            )
        val accountIdBytes = base64Decode(cached.accountIdB64)

        // Parse the mnemonic up front. `fromPhrase` collapses
        // unknown-word and bad-checksum into one FfiException by
        // design (preserves BIP39's no-probing property). Burn
        // the seed buffer at the end — it's the only thing
        // standing between an attacker and the OPAQUE export key
        // we'll derive next.
        val seed = RecoveryMnemonic.fromPhrase(mnemonicPhrase).use { mnemonic ->
            mnemonic.toSeed()
        }
        try {
            authClientFactory().use { client ->
                val result = client.recover(accountIdBytes, seed, newPassword)
                try {
                    val deviceRootKey =
                        deriveDeviceRootKey(result.exportKey, cached.accountIdB64)
                    try {
                        deviceKeyStore.putDeviceKey(
                            context = applicationContext,
                            accountIdB64 = cached.accountIdB64,
                            rootKey = deviceRootKey,
                        )
                    } finally {
                        deviceRootKey.fill(0)
                    }
                    sessionStore.save(cached) // idempotent upsert
                    // Stash the NEW export key — the recovery
                    // flow rotates the password envelope so this
                    // is fresh material, not a reuse of the prior
                    // signup/login result.
                    mutableIdentitySecrets.value = IdentitySecrets(
                        accountId = accountIdBytes,
                        exportKey = result.exportKey.copyOf(),
                        sessionToken = result.sessionToken,
                        sessionExpiresAtMs = result.sessionExpiresAtMs,
                    )
                    mutableState.value = AuthState.Session(
                        account = SessionAccount(
                            accountIdB64 = cached.accountIdB64,
                            username = cached.username,
                        ),
                    )
                    // Phase 2a1c — post-recover backup bootstrap.
                    // Re-caches the phrase under the freshly-
                    // rotated device root key + posts a baseline
                    // /backup envelope. Best-effort.
                    bootstrapBackupBestEffort(
                        accountId = accountIdBytes,
                        accountIdB64 = cached.accountIdB64,
                        exportKey = result.exportKey,
                        sessionToken = result.sessionToken,
                        phrase = mnemonicPhrase,
                        context = "recover",
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
        val cached = sessionStore.load()
        // Wipe the in-memory identity secrets — the user just
        // signed out of this session. The on-disk Keystore
        // envelope (device root key) survives; re-login on the
        // same device repopulates `identitySecrets` cheaply.
        clearIdentitySecrets()
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
        }
        sessionStore.clear()
        clearIdentitySecrets()
        mutableState.value = AuthState.Anon()
        // 1.20d — full push wipe. Fire-and-forget — the local
        // `DevicePushId.clear()` inside the subscriber runs
        // regardless of the server leg outcome, honouring
        // forgetDevice's "leave NOTHING per-account behind"
        // contract.
        unsubscribePushBestEffort("forgetDevice")
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
            val seed = try {
                me.lakko.app.backup.ed25519SeedFromExportKey(exportKey)
            } catch (cause: Throwable) {
                Log.w("lakko-auth", "backup seed-derive ($context) failed", cause)
                return
            }
            try {
                backupClient.uploadNow(
                    accountId = accountId,
                    ed25519Seed = seed,
                    phraseBytes = phraseBytes,
                    contacts = emptyList(),  // 2a3 wires the cross-device sync
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
            secrets.accountId.fill(0)
        }
        mutableIdentitySecrets.value = null
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
}

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
 *   * they're small (16 + 64 = 80 bytes) and clone-friendly;
 *   * a UniFFI handle carries lifetime baggage (the Rust-side
 *     Arc) and an explicit `.close()` requirement;
 *   * downstream callers reconstruct an `Identity` on demand
 *     via `Identity.fromSeed(exportKey)` — each reconstruction
 *     is sub-millisecond (HKDF + Ed25519 + X25519 derivation).
 */
public data class IdentitySecrets(
    /** Raw 16-byte UUIDv7. */
    val accountId: ByteArray,
    /** OPAQUE password export key (64 bytes for `LakkoCipherSuite`).
     *  Use as the seed for `Identity.fromSeed`. */
    val exportKey: ByteArray,
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
            sessionToken == other.sessionToken &&
            sessionExpiresAtMs == other.sessionExpiresAtMs
    }

    override fun hashCode(): Int {
        var h = accountId.contentHashCode()
        h = 31 * h + exportKey.contentHashCode()
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
