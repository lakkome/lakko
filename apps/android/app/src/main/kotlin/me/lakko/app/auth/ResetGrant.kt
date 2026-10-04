// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/*
 * ADR-0065, amended 2026-10-04 - the password-fresh reset grant.
 *
 * `POST /account/keys/reset/{start,finish}` are refused (`403
 * password-required`) without a reset grant, and only the PASSWORD login's
 * `/login/finish` mints one (ten minutes, one use). A stolen session, or an
 * unlocked phone in someone else's hands, cannot reset the account keys.
 *
 * The grant is an opaque string. It lives in memory only: never in durable
 * storage, never in a log line. It is bound to the account it was issued
 * for and dropped on sign-out, on the account wipe, when used and when
 * expired. UX: `docs/ux/identity-recovery.md` section 15.1a.
 */

/** A reset grant as the password login returned it. Never printed. */
public class ResetGrant(
    /** Standard base64 of the account id the grant was issued for. */
    public val accountIdB64: String,
    /** The opaque grant; sent as `reset_grant`, never parsed. */
    public val grant: String,
    /** Unix milliseconds; the grant is unusable at or after this. */
    public val expiresAtMs: Long,
) {
    override fun toString(): String = "ResetGrant(grant=<redacted>, expiresAtMs=$expiresAtMs)"
}

/**
 * The one in-memory slot for the reset grant. At most one grant is held
 * (the newest password login's); every read checks the account and the
 * expiry, and an expired grant is dropped by the read that finds it so.
 */
public class ResetGrantHolder {
    private var held: ResetGrant? = null

    /** Keep the grant a password login returned, replacing any older one. A
     *  blank grant, or one already expired, is not kept (and clears the
     *  slot: the older grant belongs to an older sign-in). */
    @Synchronized
    public fun put(accountIdB64: String, grant: String?, expiresAtMs: Long?, nowMs: Long) {
        held = if (grant.isNullOrEmpty() || expiresAtMs == null || expiresAtMs <= nowMs) {
            null
        } else {
            ResetGrant(accountIdB64 = accountIdB64, grant = grant, expiresAtMs = expiresAtMs)
        }
    }

    /** The live grant of [accountIdB64], or `null`: none held, held for
     *  another account, or expired (then dropped). */
    @Synchronized
    public fun live(accountIdB64: String, nowMs: Long): ResetGrant? {
        val current = held ?: return null
        if (current.expiresAtMs <= nowMs) {
            held = null
            return null
        }
        return current.takeIf { it.accountIdB64 == accountIdB64 }
    }

    /** Whether a live grant is held for [accountIdB64]. */
    public fun hasLive(accountIdB64: String, nowMs: Long): Boolean = live(accountIdB64, nowMs) != null

    /** Drop whatever is held: the grant was spent or refused, the person
     *  signed out, or the account was wiped. */
    @Synchronized
    public fun clear() {
        held = null
    }
}

/** The `error` code of the `403` both reset calls answer without a usable
 *  grant (missing, malformed, expired, another account's, stale generation). */
public const val RESET_PASSWORD_REQUIRED: String = "password-required"

/** Whether [err] is the reset's `403 password-required`. */
public fun isResetPasswordRequired(err: Throwable): Boolean =
    err is AuthHttpError && err.status == HTTP_FORBIDDEN_STATUS && err.code == RESET_PASSWORD_REQUIRED

/**
 * The reset needs the password proven now: no live grant is held and none
 * was supplied, or the server refused the grant (`403 password-required`,
 * expired between opening the screen and pressing the button). Nothing was
 * reset. The screen reveals the password field with its helper; this is not
 * an error to the person.
 */
public class ResetPasswordRequired : RuntimeException("the reset needs the password proven now")

/** The password typed on the reset screen is wrong. Nothing was reset. */
public class ResetWrongPassword : RuntimeException("wrong password")

/**
 * Whether a failure of the reset screen's password sign-in means "wrong
 * password": the server's `401` (indistinguishable from an unknown account,
 * by design), or the OPAQUE finish failing on this device before anything
 * is sent ([localOpaqueFailure]). A limit, a `503` and a transport failure
 * are not.
 */
public fun isWrongPasswordFailure(err: Throwable, localOpaqueFailure: Boolean): Boolean =
    localOpaqueFailure || (err is AuthHttpError && err.status == HTTP_UNAUTHORIZED_LOGIN_STATUS)

/**
 * The name the reset screen's password sign-in derives its login handle
 * from: the username this device knows for the parked account, else the one
 * typed on the warning screen. A known name is never overridden by a typed
 * one. Blank when neither exists (the caller refuses).
 */
public fun resetSignInName(knownUsername: String, typedUsername: String?): String =
    knownUsername.trim().ifEmpty { typedUsername?.trim().orEmpty() }

private const val HTTP_UNAUTHORIZED_LOGIN_STATUS: Int = 401
private const val HTTP_FORBIDDEN_STATUS: Int = 403
