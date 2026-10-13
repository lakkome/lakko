// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException
import me.lakko.app.profile.usernameProblemOf
import uniffi.lakko_ffi_uniffi.loginHandle
import uniffi.lakko_ffi_uniffi.validateUsername

/*
 * ADR-0054 S5 - sign in and recover BY NAME.
 *
 * Before S5 a sign-in on a device that did not know the account looked the
 * username up at discovery to learn the account id. Since S3 that lookup
 * spends an anonymous token, and a token needs a session, so a fresh device
 * could no longer find the account to sign into. The auth routes now take the
 * login handle of the name (`loginHandle(username)`, 32 bytes, base64url on
 * the wire) and hand the account id back only in the finish, after a proof.
 *
 * Nothing here looks anything up: the handle is a pure function of the name.
 * A name nobody holds takes the server's ordinary path on a stand-in account,
 * so it fails exactly like a wrong password / wrong code.
 */

/** Minimal shape of a locally-cached `(accountId, username)` record. */
public data class CachedAccount(
    val accountIdB64: String,
    val username: String,
)

/** A username resolved to what the name routes need. */
public class NamedHandle(
    /** The canonical username (the core's username rule): what to persist
     *  and show. */
    public val username: String,
    /** The 32-byte login handle. */
    public val handle: ByteArray,
)

/** Username to [NamedHandle]. The production source is the native core; a host
 *  test passes a fake. */
public fun interface LoginHandleSource {
    /** @throws uniffi.lakko_ffi_uniffi.FfiException a typed username refusal. */
    public fun handleFor(username: String): NamedHandle
}

/** [LoginHandleSource] over the core's `validateUsername` + `loginHandle`. */
public object UniffiLoginHandleSource : LoginHandleSource {
    override fun handleFor(username: String): NamedHandle =
        NamedHandle(username = validateUsername(username), handle = loginHandle(username))
}

/**
 * [LoginHandleSource.handleFor], with a name that fails the username rule
 * mapped to an [AuthClientArgError] carrying the rule's own copy (UX
 * `docs/ux/profile-names.md` section 4). Any other failure propagates.
 */
internal fun namedHandleOrRefuse(source: LoginHandleSource, username: String): NamedHandle =
    try {
        source.handleFor(username)
    } catch (e: CancellationException) {
        throw e
    } catch (e: Exception) {
        val problem = usernameProblemOf(e) ?: throw e
        throw AuthClientArgError(problem.copy, problem.text)
    }

/**
 * The name a sign-in or recovery targets: the typed [typed] name when there
 * is one, otherwise the account cached on this device ([cached]); the request
 * always carries the handle of that name, never an account id.
 *
 * @throws AuthClientArgError when there is neither (a fresh device with the
 *   username field left blank), or the name fails the username rule.
 */
public fun resolveNamedSignIn(
    cached: CachedAccount?,
    typed: String?,
    source: LoginHandleSource,
): NamedHandle {
    val trimmed = typed?.trim().orEmpty()
    val name = when {
        trimmed.isNotEmpty() -> trimmed
        cached != null && cached.username.isNotEmpty() -> cached.username
        else -> throw AuthClientArgError(NO_NAME_COPY, me.lakko.app.l10n.UiText("recovery.username_required_to_recover"))
    }
    return namedHandleOrRefuse(source, name)
}

/** The typed-nothing refusal. */
public const val NO_NAME_COPY: String = "Enter your username to recover the account."

/** The two login-handle routes behind an interface, so the claim order and
 *  the re-bind after a sign-in by name are host-testable. */
public interface LoginHandleBinder {
    /** `PUT /auth/login-handle`. */
    public suspend fun bind(sessionToken: String, handle: ByteArray): LoginHandleBinding

    /** `DELETE /auth/login-handle`. */
    public suspend fun unbind(sessionToken: String): Boolean

    /**
     * `POST /auth/login-handle/check` (ADR-0076 section 2): what the account
     * holds relative to [handle]. The default answers "none" (bind), which is
     * what a first bind at sign-up is.
     */
    public suspend fun check(sessionToken: String, handle: ByteArray): LoginHandleState = LoginHandleState.None
}

/** [LoginHandleBinder] over a short-lived [AuthClient] per call. */
public class AuthClientLoginHandleBinder(
    private val clientFactory: () -> AuthClient,
) : LoginHandleBinder {
    override suspend fun bind(sessionToken: String, handle: ByteArray): LoginHandleBinding =
        clientFactory().use { it.bindLoginHandle(sessionToken, handle) }

    override suspend fun unbind(sessionToken: String): Boolean =
        clientFactory().use { it.unbindLoginHandle(sessionToken) }

    override suspend fun check(sessionToken: String, handle: ByteArray): LoginHandleState =
        clientFactory().use { it.checkLoginHandle(sessionToken, handle) }
}

/**
 * Run a sign-in (or recovery) by name: resolve the handle, run [call] with it,
 * and on success hand the new session and the handle to [rebind] (ADR-0054 S5:
 * re-`PUT` the handle after every successful sign-in by name, idempotent, so a
 * device that bound before a rename stays in step). [rebind] is fire-and-forget
 * in production; nothing about the sign-in waits on it or fails with it.
 */
public suspend fun <R> signInByName(
    cached: CachedAccount?,
    typed: String?,
    source: LoginHandleSource,
    call: suspend (NamedHandle) -> R,
    sessionTokenOf: (R) -> String,
    rebind: (sessionToken: String, handle: ByteArray) -> Unit,
): Pair<NamedHandle, R> {
    val named = resolveNamedSignIn(cached, typed, source)
    return named to signInByName(named, call, sessionTokenOf, rebind)
}

/** [signInByName] for a name already resolved (the second step of a two-step
 *  recovery, or a recover whose name was resolved before its inputs were
 *  parsed). */
public suspend fun <R> signInByName(
    named: NamedHandle,
    call: suspend (NamedHandle) -> R,
    sessionTokenOf: (R) -> String,
    rebind: (sessionToken: String, handle: ByteArray) -> Unit,
): R {
    val result = call(named)
    rebind(sessionTokenOf(result), named.handle.copyOf())
    return result
}

/** The generic failure a `503 login-by-name-disabled` shows (the server has
 *  no handle pepper): the caller's ordinary "something went wrong" copy. */
public fun isLoginByNameDisabled(err: AuthHttpError): Boolean =
    err.status == HTTP_SERVICE_UNAVAILABLE && err.code == LOGIN_BY_NAME_DISABLED

/** `503` body code on every name route when the server has no pepper. */
public const val LOGIN_BY_NAME_DISABLED: String = "login-by-name-disabled"

private const val HTTP_SERVICE_UNAVAILABLE: Int = 503
