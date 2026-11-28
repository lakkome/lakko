// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/*
 * The fresh-auth proof (`docs/ux/identity-recovery.md` section 17; ADR-0073
 * as amended 2026-10-06). Reset account keys, delete the account, add a
 * device, add a passkey and change the password each need the person to
 * prove who they are NOW: a signed-in session alone is refused
 * (`403 fresh-auth-required`).
 *
 * The proof is an opaque grant the server returns as `fresh_grant` +
 * `fresh_grant_expires_at_ms` from the password sign-in, the passkey sign-in
 * and the signup finish (ten minutes). One grant serves any number of the
 * gated actions within its lifetime, except that a key reset spends every
 * grant of the old generation.
 *
 * It lives in memory only: never in durable storage, never in a log line. It
 * is bound to the account it was issued for and dropped on sign-out, on the
 * account wipe, on expiry, on an account change and after a key reset.
 */

/** A fresh-auth proof as a sign-in returned it. Never printed. */
public class FreshAuthProof(
    /** Standard base64 of the account id the proof was issued for. */
    public val accountIdB64: String,
    /** The opaque grant; sent as `fresh_grant`, never parsed. */
    public val grant: String,
    /** Unix milliseconds; the proof is unusable at or after this. */
    public val expiresAtMs: Long,
    /** ADR-0073, amended 2026-11-26: the proof was minted by a PASSWORD
     *  sign-in (or the sign-up), not by a passkey. A key reset's factor
     *  routes accept only such a proof; delete account, add a device and
     *  add a passkey accept either. */
    public val byPassword: Boolean = true,
) {
    override fun toString(): String = "FreshAuthProof(grant=<redacted>, expiresAtMs=$expiresAtMs)"
}

/**
 * The one in-memory slot for the fresh-auth proof. At most one is held (the
 * newest sign-in's); every read checks the account and the expiry, and an
 * expired proof is dropped by the read that finds it so.
 */
public class FreshAuthHolder {
    private var held: FreshAuthProof? = null

    /** Keep the proof a sign-in returned, replacing any older one. A blank
     *  grant, or one already expired, is not kept (and clears the slot: the
     *  older proof belongs to an older sign-in). */
    @Synchronized
    public fun put(
        accountIdB64: String,
        grant: String?,
        expiresAtMs: Long?,
        nowMs: Long,
        byPassword: Boolean = true,
    ) {
        held = if (grant.isNullOrEmpty() || expiresAtMs == null || expiresAtMs <= nowMs) {
            null
        } else {
            FreshAuthProof(
                accountIdB64 = accountIdB64,
                grant = grant,
                expiresAtMs = expiresAtMs,
                byPassword = byPassword,
            )
        }
    }

    /** The live proof only when a PASSWORD minted it (the key reset's
     *  factor routes, ADR-0073 amended 2026-11-26). */
    public fun livePassword(accountIdB64: String, nowMs: Long): FreshAuthProof? =
        live(accountIdB64, nowMs)?.takeIf { it.byPassword }

    /**
     * A sign-in of [accountIdB64] by a route that mints no proof (an
     * authenticator code, three words, another device): it does not revoke
     * the proof a password typed a minute ago earned for the SAME account,
     * and it drops a proof held for ANOTHER account (an account change).
     */
    @Synchronized
    public fun onSignInWithoutProof(accountIdB64: String) {
        if (held?.accountIdB64 != accountIdB64) held = null
    }

    /** The live proof of [accountIdB64], or `null`: none held, held for
     *  another account, or expired (then dropped). */
    @Synchronized
    public fun live(accountIdB64: String, nowMs: Long): FreshAuthProof? {
        val current = held ?: return null
        if (current.expiresAtMs <= nowMs) {
            held = null
            return null
        }
        return current.takeIf { it.accountIdB64 == accountIdB64 }
    }

    /** Whether a live proof is held for [accountIdB64]. */
    public fun hasLive(accountIdB64: String, nowMs: Long): Boolean = live(accountIdB64, nowMs) != null

    /** Drop whatever is held: the server refused it, a key reset spent it,
     *  the person signed out, or the account was wiped. */
    @Synchronized
    public fun clear() {
        held = null
    }
}

/** The `error` code of the `403` every gated route answers without a usable
 *  proof (missing, malformed, expired, another account's, stale generation). */
public const val FRESH_AUTH_REQUIRED: String = "fresh-auth-required"

/** The header `DELETE /account` carries the grant in (the route has no body). */
public const val FRESH_GRANT_HEADER: String = "x-lakko-fresh-grant"

/** Whether [err] is a gated route's `403 fresh-auth-required` (or the
 *  client-side refusal to send without a proof, [FreshAuthRequired]). */
public fun isFreshAuthRequired(err: Throwable): Boolean = when (err) {
    is FreshAuthRequired -> true
    is AuthHttpError -> err.status == HTTP_FORBIDDEN_STATUS && err.code == FRESH_AUTH_REQUIRED
    is me.lakko.app.webauthn.WebauthnHttpError ->
        err.status == HTTP_FORBIDDEN_STATUS && errorCodeOfBody(err.body) == FRESH_AUTH_REQUIRED
    else -> false
}

/** The `error` code of a JSON error body (`{"error":"..."}`), or `null`.
 *  The body is the server's and may be anything; nothing here throws. */
public fun errorCodeOfBody(body: String): String? =
    ERROR_CODE_PATTERN.find(body)?.groupValues?.getOrNull(1)

private val ERROR_CODE_PATTERN = Regex("\"error\"\\s*:\\s*\"([a-z0-9-]{1,64})\"")

/**
 * The action needs a proof given now: none is live and none was supplied, or
 * the server refused the one sent (`403 fresh-auth-required`, expired between
 * opening the screen and pressing the button). Nothing was done. The surface
 * shows the proof block; this is not an error to the person.
 */
public class FreshAuthRequired : RuntimeException("the action needs a fresh proof of who is asking")

/** The password typed into the proof block is wrong. Nothing was done. */
public class FreshAuthWrongPassword : RuntimeException("wrong password")

/** The passkey used for the proof belongs to a different account. Nothing of
 *  that sign-in is installed. */
public class FreshAuthOtherAccount : RuntimeException("the passkey belongs to another account")

/**
 * Whether a failure of the proof block's password sign-in means "wrong
 * password": the server's `401` (indistinguishable from an unknown account,
 * by design), or the OPAQUE finish failing on this device before anything
 * is sent ([localOpaqueFailure]). A limit, a `503` and a transport failure
 * are not.
 */
public fun isWrongPasswordFailure(err: Throwable, localOpaqueFailure: Boolean): Boolean =
    localOpaqueFailure || (err is AuthHttpError && err.status == HTTP_UNAUTHORIZED_LOGIN_STATUS)

/**
 * The name the proof block's password sign-in derives its login handle
 * from: the username this device knows for the account, else the one typed
 * on the screen. A known name is never overridden by a typed one. Blank when
 * neither exists (the caller refuses).
 */
public fun proofSignInName(knownUsername: String, typedUsername: String?): String =
    knownUsername.trim().ifEmpty { typedUsername?.trim().orEmpty() }

/**
 * How the session on this device was landed, kept in memory with the session
 * (UX 17.6). It decides which row of the reset screen opens first: a person
 * who got in with the authenticator code and has no passkey is shown the
 * "cannot reset" panel before any field.
 */
public enum class SessionLanding { Password, Passkey, Authenticator, Other }

/**
 * Whether the password of an account whose keys this device holds may be
 * replaced now (UX 17.8, first bullet): only behind a live fresh-auth proof
 * (the current password or a passkey), or the local three-word check of the
 * recovery phrase passed within the last ten minutes (the person who forgot
 * the password). A bare session is not enough.
 */
public fun passwordChangeAllowed(proofLive: Boolean, wordsCheckedAtMs: Long?, nowMs: Long): Boolean =
    proofLive || wordsCheckLive(wordsCheckedAtMs, nowMs)

/** Whether a local three-word check made at [checkedAtMs] still counts. A
 *  clock set back never extends it. */
public fun wordsCheckLive(checkedAtMs: Long?, nowMs: Long): Boolean =
    checkedAtMs != null && nowMs >= checkedAtMs && nowMs - checkedAtMs < FRESH_AUTH_WINDOW_MS

/** Ten minutes: the window of the local three-word check (the server states
 *  its own expiry for a grant). */
public const val FRESH_AUTH_WINDOW_MS: Long = 10L * 60L * 1000L

/** The password of an account may not be replaced on a bare session. */
public class PasswordChangeNeedsProof : RuntimeException("changing the password needs a fresh proof")

private const val HTTP_UNAUTHORIZED_LOGIN_STATUS: Int = 401
private const val HTTP_FORBIDDEN_STATUS: Int = 403
