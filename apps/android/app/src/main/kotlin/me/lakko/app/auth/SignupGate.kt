// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/*
 * Sign-up when the beta is full (`docs/ux/beta-account-cap.md`).
 *
 * The server holds a fixed number of accounts with a username. Two answers
 * say so: `GET /signup/open` (a hint, asked before the proof of work, which
 * can be stale) and `403 registration-closed` from the first login-handle
 * bind (final). This file is the one place that reads them; the screens
 * branch on the pure decisions below.
 */

/** The error code of a first login-handle bind refused at the cap. */
public const val REGISTRATION_CLOSED_CODE: String = "registration-closed"

private const val HTTP_FORBIDDEN: Int = 403

/** Whether [err] is the server's `403 registration-closed`. */
public fun isRegistrationClosed(err: AuthHttpError): Boolean =
    err.status == HTTP_FORBIDDEN && err.code == REGISTRATION_CLOSED_CODE

/**
 * The first login-handle bind was refused: the beta is full. Thrown by the
 * username claim ([claimUsername]) before discovery is asked, so no name is
 * left in the directory.
 */
public open class RegistrationClosed(message: String = "registration closed") : RuntimeException(message)

/** The error code of a first bind refused because the supporter place it
 *  carried was already spent by another account (ADR-0097). */
public const val SUPPORTER_CODE_USED_CODE: String = "supporter-code-used"

/** Whether [err] is the server's `403 supporter-code-used`. */
public fun isSupporterCodeUsed(err: AuthHttpError): Boolean =
    err.status == HTTP_FORBIDDEN && err.code == SUPPORTER_CODE_USED_CODE

/**
 * The first bind carried a supporter place (`docs/ux/supporters.md` section
 * 1.2) and the server says that place was already used for another account.
 * A [RegistrationClosed]: the account just made is deleted and wiped as for a
 * plain refusal; the full state then shows the "already been used" line.
 */
public class SupporterCodeUsed : RegistrationClosed("supporter code used")

/** What the sign-up screen draws. */
public enum class SignupGate {
    /** The ordinary form. */
    FORM,

    /** "Lakko is full for now": the form is replaced. */
    CLOSED,
}

/** The sign-up screen for an answer of `/signup/open` ([AuthClient.signupOpen]
 *  already reads a failed question as open). */
public fun signupGate(open: Boolean): SignupGate = if (open) SignupGate.FORM else SignupGate.CLOSED

/** Whether a failure of the sign-up (or the guest claim) is the full state:
 *  the bind refused at the cap. */
public fun isFullState(err: Throwable): Boolean =
    err is RegistrationClosed || (err is AuthHttpError && isRegistrationClosed(err))

/** What the guest question after a call shows (`docs/ux/call-links.md`
 *  section 9, `beta-account-cap.md` section 3). */
public enum class GuestOfferMode {
    /** `Create account` / `No, thanks`, as before. */
    ASK,

    /** The closed question: one line and `OK`, which does what
     *  `No, thanks` does. */
    CLOSED,
}

/**
 * The guest question for the answer of `/signup/open` when it is about to
 * show ([open]) and whether a `Create account` was refused at the bind
 * ([refusedAtBind]). A refusal outranks a later open answer: the bind is
 * final, the question can be stale.
 */
public fun guestOfferMode(open: Boolean, refusedAtBind: Boolean): GuestOfferMode =
    if (refusedAtBind || !open) GuestOfferMode.CLOSED else GuestOfferMode.ASK
