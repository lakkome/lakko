// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/*
 * UX `identity-recovery.md` 19.1b - setting up an authenticator app, the
 * same screen at sign-up and in Settings > Security > Recovery. The pure
 * decisions of that screen: how the key is shown, when the code is sent
 * (on the sixth digit), and what each failure of the two calls means.
 */

/** The manual-entry key in groups of four, as the screen shows it
 *  ("ABCD EFGH ..."). What `Copy` puts on the clipboard is the key itself,
 *  without the spaces. */
public fun groupedTotpKey(secretB32: String): String =
    secretB32.filter { !it.isWhitespace() }.chunked(TOTP_KEY_GROUP).joinToString(" ")

private const val TOTP_KEY_GROUP: Int = 4

/** The six digits to check, or `null` while fewer are typed: the code is
 *  checked on the sixth digit, with no separate button. */
public fun totpCodeToCheck(typed: String): String? = normalizeTotpCode(typed)

/** What a failed call of the setup means for the screen (the 19.1b table). */
public sealed interface TotpSetupFailure {
    /** No live proof (or the server refused the one sent): the 17.1 block
     *  appears, nothing went wrong. */
    public data object ProofRequired : TotpSetupFailure

    /** Wrong or expired code: the digits clear and keep focus. */
    public data object WrongCode : TotpSetupFailure

    /** Too many tries: the countdown; the field waits until zero. */
    public data class Wait(val retryAfterSecs: Long?) : TotpSetupFailure

    /** The account already has an authenticator. */
    public data object AlreadyEnabled : TotpSetupFailure

    /** Offline: the QR and the key stay. */
    public data object Offline : TotpSetupFailure

    /** Anything else: the generic line with `Try again`, which starts over
     *  with a new key. */
    public data object Generic : TotpSetupFailure
}

private const val TOTP_HTTP_UNAUTHORIZED: Int = 401
private const val TOTP_HTTP_CONFLICT: Int = 409
private const val TOTP_HTTP_TOO_MANY: Int = 429

/** Classify a failure of `/totp/enroll/start` ([finishing] `false`) or
 *  `/totp/enroll/finish` (`true`). */
public fun totpSetupFailureOf(finishing: Boolean, err: Throwable): TotpSetupFailure = when {
    err is FreshAuthRequired || isFreshAuthRequired(err) -> TotpSetupFailure.ProofRequired
    err is AuthNetworkError || err is java.io.IOException -> TotpSetupFailure.Offline
    err is AuthHttpError && err.status == TOTP_HTTP_TOO_MANY -> TotpSetupFailure.Wait(err.retryAfterSecs)
    err is AuthHttpError && err.status == TOTP_HTTP_CONFLICT -> TotpSetupFailure.AlreadyEnabled
    finishing && err is AuthHttpError && err.status == TOTP_HTTP_UNAUTHORIZED -> TotpSetupFailure.WrongCode
    else -> TotpSetupFailure.Generic
}
