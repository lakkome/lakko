// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

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
    private val mutableState: MutableStateFlow<AuthState> =
        MutableStateFlow(AuthState.Loading)

    public val state: StateFlow<AuthState> = mutableState.asStateFlow()

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
                mutableState.value = AuthState.Session(
                    account = SessionAccount(
                        accountIdB64 = accountIdB64,
                        username = trimmed,
                    ),
                )
                val phrase = result.recoveryPhrase ?: throw IllegalStateException(
                    "internal: signup completed without a recovery phrase",
                )
                SignupOutcome(recoveryPhrase = phrase)
            } finally {
                // The OPAQUE export key drops out of scope here.
                // Best-effort wipe before GC sees the buffer.
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
                mutableState.value = AuthState.Session(
                    account = SessionAccount(
                        accountIdB64 = cached.accountIdB64,
                        username = cached.username,
                    ),
                )
            } finally {
                // Best-effort wipe of the export key + session key
                // before they drop out of scope.
                result.exportKey.fill(0)
                result.sessionKey.fill(0)
            }
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
        mutableState.value = AuthState.Anon(cachedUsername = cached?.username)
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
        }
        sessionStore.clear()
        mutableState.value = AuthState.Anon()
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
