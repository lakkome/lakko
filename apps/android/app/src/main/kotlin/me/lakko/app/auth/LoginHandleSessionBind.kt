// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException
import java.security.MessageDigest

/*
 * ADR-0054 S5 - keep the login handle bound for every session this device
 * holds, not only the ones a sign-in by name or a signup produced.
 *
 * Sign-in by name needs `PUT /auth/login-handle` to have run for the account
 * at least once. Signup binds it and a sign-in by name re-binds it, but an
 * account whose session came from anywhere else (a cold-start restore of a
 * session opened before S5, a passkey, a pairing or another-device
 * confirmation) would never bind, and after a logout that person could not
 * sign in by name again. So whenever a session lands with a username known on
 * this device, the handle is re-bound once in the background.
 */

/** A session that has a username to bind: what [LoginHandleSessionRebinder]
 *  needs from the live auth state. */
public class LoginHandleBindTarget(
    public val sessionToken: String,
    public val username: String,
) {
    /** Never print the bearer token into a log line. */
    override fun toString(): String = "LoginHandleBindTarget(sessionToken=<redacted>, username=$username)"
}

/**
 * The bind target of the current auth state, or `null` when there is nothing
 * to bind: no session, no secrets yet (or already cleared), secrets that
 * belong to another account than the session (the instant between two
 * writes during an account switch), or an account with no username (a guest).
 */
public fun loginHandleBindTarget(state: AuthState, secrets: IdentitySecrets?): LoginHandleBindTarget? {
    if (state !is AuthState.Session || secrets == null) return null
    if (base64Encode(secrets.accountId) != state.account.accountIdB64) return null
    val username = state.account.username.trim()
    if (username.isEmpty()) return null
    return LoginHandleBindTarget(sessionToken = secrets.sessionToken, username = username)
}

/**
 * Best-effort, fire-and-forget `PUT /auth/login-handle`, at most once per
 * (session, login handle) per process. The handle is a pure function of the
 * canonical username, so this is once per (session, username).
 *
 * Nothing here ever reaches the user: a `409` (another account holds the
 * handle) and every other failure are logged by status only, and a
 * `503 login-by-name-disabled` (the server has no handle pepper) is silent.
 * A pair is spent when its bind is launched, not when it succeeds, so a
 * failure is not retried within the process (the next process start, or the
 * next session, tries again).
 *
 * Only a digest of each (session, handle) pair is kept, so the set does not
 * retain bearer tokens past a logout.
 */
public class LoginHandleSessionRebinder(
    private val source: LoginHandleSource,
    private val binder: LoginHandleBinder,
    /** Runs a bind off the caller; production launches on the auth
     *  repository's process-lifetime scope. */
    private val launch: (suspend () -> Unit) -> Unit,
    /** Diagnostic sink (status only, never a token, never a handle). */
    private val log: (String) -> Unit,
    /** Told when auth confirmed a handle in this session (ADR-0068). */
    private val onConfirmed: () -> Unit = {},
) {
    private val spent: MutableSet<String> = HashSet()

    /** Digests of the handles auth confirmed as the account's in this
     *  session: the check answered `same`, or this device bound the handle
     *  itself. Only then may a device seed the own-username register. */
    private val confirmed: MutableSet<String> = HashSet()

    /** Whether auth confirmed [username]'s handle in this session. */
    public fun isConfirmed(username: String): Boolean {
        val handle = handleOrNull(username) ?: return false
        val key = handleDigest(handle)
        handle.fill(0)
        return synchronized(confirmed) { key in confirmed }
    }

    /** The session ended: what it confirmed is forgotten. */
    public fun forgetConfirmed() {
        synchronized(confirmed) { confirmed.clear() }
    }

    private fun confirm(handle: ByteArray) {
        val key = handleDigest(handle)
        val added = synchronized(confirmed) { confirmed.add(key) }
        if (added) onConfirmed()
    }

    private fun handleDigest(handle: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(handle).joinToString("") { "%02x".format(it) }

    /** A session with a known username is available: bind its handle unless
     *  this (session, username) pair was already bound in this process. */
    public fun onSessionAvailable(target: LoginHandleBindTarget) {
        val handle = handleOrNull(target.username) ?: return
        bind(target.sessionToken, handle)
    }

    /** Bind [handle] for [sessionToken] unless already done in this process.
     *  Takes ownership of [handle] and zeroes it afterwards. */
    public fun bind(sessionToken: String, handle: ByteArray) {
        if (!claim(sessionToken, handle)) {
            handle.fill(0)
            return
        }
        launch {
            try {
                // ADR-0068 section 2 - never a blind PUT: the account may
                // have been renamed on another device, and a bind here would
                // revert it at auth. Bind only when the account holds none.
                when (handleSyncAction(binder.check(sessionToken, handle))) {
                    HandleSyncAction.Nothing -> confirm(handle)
                    HandleSyncAction.WaitForRegister ->
                        log("login-handle sync: the account holds another handle; waiting for the name")
                    HandleSyncAction.Bind ->
                        if (binder.bind(sessionToken, handle) == LoginHandleBinding.Taken) {
                            log("login-handle bind: status $HTTP_CONFLICT")
                        } else {
                            confirm(handle)
                        }
                }
            } catch (cause: CancellationException) {
                throw cause
            } catch (cause: AuthHttpError) {
                if (!isLoginByNameDisabled(cause)) {
                    log("login-handle bind: status ${cause.status}")
                }
            } catch (cause: Throwable) {
                log("login-handle bind failed: ${cause.javaClass.simpleName}")
            } finally {
                handle.fill(0)
            }
        }
    }

    /** Record that [username]'s handle is already bound for [sessionToken]
     *  (a signup just did it), so the session landing does not repeat it. */
    public fun markBound(sessionToken: String, username: String) {
        val handle = handleOrNull(username) ?: return
        claim(sessionToken, handle)
        // This device bound it itself, or adopted the account's name.
        confirm(handle)
        handle.fill(0)
    }

    private fun handleOrNull(username: String): ByteArray? =
        try {
            source.handleFor(username).handle
        } catch (cause: CancellationException) {
            throw cause
        } catch (cause: Throwable) {
            // A name this device holds that fails the username rule (it
            // predates the rule): nothing to bind.
            log("login-handle bind skipped: ${cause.javaClass.simpleName}")
            null
        }

    /** `true` the first time the pair is seen in this process. */
    private fun claim(sessionToken: String, handle: ByteArray): Boolean {
        val key = pairDigest(sessionToken, handle)
        return synchronized(spent) { spent.add(key) }
    }

    private fun pairDigest(sessionToken: String, handle: ByteArray): String {
        val token = sessionToken.toByteArray(Charsets.UTF_8)
        val digest = MessageDigest.getInstance("SHA-256")
        digest.update(PAIR_DOMAIN)
        digest.update(lengthPrefix(token.size))
        digest.update(token)
        digest.update(lengthPrefix(handle.size))
        digest.update(handle)
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun lengthPrefix(n: Int): ByteArray =
        byteArrayOf((n ushr 24).toByte(), (n ushr 16).toByte(), (n ushr 8).toByte(), n.toByte())

    private companion object {
        val PAIR_DOMAIN: ByteArray = "lakko/android/login-handle-bound/v1".toByteArray(Charsets.UTF_8)
        const val HTTP_CONFLICT: Int = 409
    }
}
