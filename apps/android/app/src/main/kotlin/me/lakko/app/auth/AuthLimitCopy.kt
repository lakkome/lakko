// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/*
 * ADR-0054 S4, UX `docs/ux/anti-abuse.md` sections 1-2: what a person reads
 * when `services/auth` refuses by a limit. A limit is the server's decision:
 * it never signs anyone out, never reads as "invalid", and says when to try
 * again. The strings are the spec's, verbatim, identical on the three clients.
 */

/** `too-many-attempts` with no stated wait, or one of a minute or less. */
public const val TOO_MANY_ATTEMPTS_COPY: String = "Too many attempts. Wait a moment, then try again."

/** `recovery-code-send-limited`. */
public const val RECOVERY_CODE_SEND_LIMITED_COPY: String =
    "We sent a code recently. Wait a few minutes before asking for another."

/** `device-request-limited`. */
public const val DEVICE_REQUEST_LIMITED_COPY: String =
    "Too many requests to your other devices. Try again in a few minutes."

/** `signup-rate-limited` (signup start or its challenge). */
public const val SIGNUP_RATE_LIMITED_COPY: String =
    "Too many new accounts from this network right now. Try again later."

/** The button under [SIGNUP_RATE_LIMITED_COPY]. */
public const val SIGNUP_TRY_AGAIN: String = "Try again"

/** Signup working state (UX section 1). */
public const val SIGNUP_WORKING_COPY: String = "Creating your account..."

/** The one line under the working row. */
public const val SIGNUP_WORKING_LINE: String =
    "This takes a little while, once. You will not wait like this again."

/** The guest join's working row (a call link). */
public const val GUEST_JOIN_WORKING_COPY: String = "Getting you into the call..."

/** `too-many-attempts` with a stated wait of more than a minute. */
public fun tooManyAttemptsInMinutesCopy(retryAfterSecs: Long): String {
    val minutes = (retryAfterSecs + SECONDS_PER_MINUTE - 1) / SECONDS_PER_MINUTE
    return "Too many attempts. Try again in $minutes minutes."
}

/**
 * The limit sentence for [err], or `null` when [err] is not a limit (the
 * caller then keeps its own copy; a `503 rate-limiter-unavailable` is not a
 * limit either and falls to the caller's generic failure).
 *
 * A `429` whose code this client does not know reads as the attempts
 * sentence, so a limit is never shown as a wrong password.
 */
public fun authLimitCopy(err: AuthHttpError): String? {
    if (err.status != HTTP_TOO_MANY_REQUESTS) return null
    return when (err.code) {
        "recovery-code-send-limited" -> RECOVERY_CODE_SEND_LIMITED_COPY
        "device-request-limited" -> DEVICE_REQUEST_LIMITED_COPY
        "signup-rate-limited" -> SIGNUP_RATE_LIMITED_COPY
        else -> {
            val wait = err.retryAfterSecs
            if (wait != null && wait > SECONDS_PER_MINUTE) {
                tooManyAttemptsInMinutesCopy(wait)
            } else {
                TOO_MANY_ATTEMPTS_COPY
            }
        }
    }
}

private const val HTTP_TOO_MANY_REQUESTS: Int = 429
private const val SECONDS_PER_MINUTE: Long = 60L
