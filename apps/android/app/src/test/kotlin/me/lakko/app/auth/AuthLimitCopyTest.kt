// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0054 S4 / UX `docs/ux/anti-abuse.md` section 2 - every limit code to
 *  its sentence, verbatim, including the minutes form. */
class AuthLimitCopyTest {

    private fun err(status: Int, code: String?, wait: Long? = null) =
        AuthHttpError(status, "/x", "", code = code, retryAfterSecs = wait)

    @Test
    fun tooManyAttemptsWithoutOrWithAShortWait() {
        assertEquals("Too many attempts. Wait a moment, then try again.", authLimitCopy(err(429, "too-many-attempts")))
        assertEquals(
            "Too many attempts. Wait a moment, then try again.",
            authLimitCopy(err(429, "too-many-attempts", wait = 60)),
        )
    }

    @Test
    fun tooManyAttemptsInMinutesRoundsUp() {
        assertEquals("Too many attempts. Try again in 2 minutes.", authLimitCopy(err(429, "too-many-attempts", 61)))
        assertEquals("Too many attempts. Try again in 15 minutes.", authLimitCopy(err(429, "too-many-attempts", 900)))
        assertEquals("Too many attempts. Try again in 16 minutes.", authLimitCopy(err(429, "too-many-attempts", 901)))
    }

    @Test
    fun theOtherCodes() {
        assertEquals(
            "We sent a code recently. Wait a few minutes before asking for another.",
            authLimitCopy(err(429, "recovery-code-send-limited", 600)),
        )
        assertEquals(
            "Too many requests to your other devices. Try again in a few minutes.",
            authLimitCopy(err(429, "device-request-limited", 600)),
        )
        assertEquals(
            "Too many new accounts from this network right now. Try again later.",
            authLimitCopy(err(429, "signup-rate-limited", 3600)),
        )
        assertEquals("Try again", SIGNUP_TRY_AGAIN)
    }

    @Test
    fun anUnknown429IsStillALimitNeverAWrongPassword() {
        assertEquals("Too many attempts. Wait a moment, then try again.", authLimitCopy(err(429, null)))
    }

    @Test
    fun notALimitIsLeftToTheCaller() {
        assertNull(authLimitCopy(err(503, "rate-limiter-unavailable")))
        assertNull(authLimitCopy(err(401, null)))
    }

    @Test
    fun theWorkingStateCopy() {
        assertEquals("Creating your account...", SIGNUP_WORKING_COPY)
        assertEquals("This takes a little while, once. You will not wait like this again.", SIGNUP_WORKING_LINE)
        assertEquals("Getting you into the call...", GUEST_JOIN_WORKING_COPY)
    }
}
