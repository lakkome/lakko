// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.IOException
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0075 section 7 / UX 19.3 - the step machine of Get back in > Email code. */
class EmailRecoverFlowTest {

    private fun http(status: Int, code: String?, retryAfterSecs: Long? = null) =
        AuthHttpError(status = status, endpoint = "/recovery-email/x", body = "", code = code, retryAfterSecs = retryAfterSecs)

    @Test
    fun `step 2 is the same whatever was typed`() {
        // `recover/start` answers alike for a matching pair, a wrong pair, an
        // unknown username and an account without an email; the step is
        // built from that answer alone, so it cannot differ.
        val forMatch = afterEmailRecoverStart("chal-1", resendAfterSecs = 45, expiresInSecs = 600, nowMs = 1_000L)
        val forMiss = afterEmailRecoverStart("chal-1", resendAfterSecs = 45, expiresInSecs = 600, nowMs = 1_000L)
        assertEquals(forMatch, forMiss)
        val step = forMatch.step as EmailRecoverStep.Code
        assertEquals("chal-1", step.challengeId)
        assertEquals(46_000L, step.resendAtMs)
        assertEquals(601_000L, step.expiresAtMs)
        assertNull(forMatch.error)
        assertFalse(forMatch.busy)
    }

    @Test
    fun `a wrong code and a code that was never sent get the same line`() {
        // Both are the server's one `401 code-refused`.
        val failure = emailRecoverFailureOf(EmailRecoverCall.Verify, http(401, "code-refused"))
        assertEquals(EmailRecoverFailure.WrongCode(UiText("recovery_email.code.wrong")), failure)
        assertTrue(emailFailureClearsCode(failure))
        val code = afterEmailRecoverStart("c", 45, 600, 0L)
        val after = afterEmailRecoverFailure(code.copy(busy = true), failure, nowMs = 5L)
        // The step stays, one try spent: five tries per code.
        assertEquals((code.step as EmailRecoverStep.Code).copy(wrongTries = 1), after.step)
        assertEquals(UiText("recovery_email.code.wrong"), after.error)
        assertFalse(after.busy)
    }

    @Test
    fun `the authenticator step appears only when the account has one`() {
        assertEquals(EmailRecoverStep.NewPassword("t"), afterEmailRecoverVerify("t", totpRequired = false).step)
        assertEquals(EmailRecoverStep.Totp("t"), afterEmailRecoverVerify("t", totpRequired = true).step)
        // There is no way past it but its code.
        assertEquals(EmailRecoverStep.NewPassword("t"), afterEmailRecoverTotp("t").step)
        val wrong = emailRecoverFailureOf(EmailRecoverCall.Totp, http(401, "code-refused"))
        assertEquals(EmailRecoverFailure.WrongCode(UiText("recovery.totp.error.wrong")), wrong)
        val stays = afterEmailRecoverFailure(EmailRecoverState(step = EmailRecoverStep.Totp("t")), wrong, 0L)
        assertEquals(EmailRecoverStep.Totp("t"), stays.step)
    }

    @Test
    fun `the flow ends on the new password and every account reaches the same step`() {
        // start -> code -> (authenticator) -> NEW PASSWORD, and nothing
        // after it: a mailed code gives a new password and a session, never
        // the keys (the email vault was removed 2026-11-26).
        val code = afterEmailRecoverStart("c", 45, 600, 0L)
        assertTrue(code.step is EmailRecoverStep.Code)
        val withoutTotp = afterEmailRecoverVerify("t", totpRequired = false)
        assertEquals(EmailRecoverStep.NewPassword("t"), withoutTotp.step)
        val withTotp = afterEmailRecoverVerify("t", totpRequired = true)
        assertEquals(EmailRecoverStep.Totp("t"), withTotp.step)
        assertEquals(EmailRecoverStep.NewPassword("t"), afterEmailRecoverTotp("t").step)
        // With or without an authenticator: the identical password step.
        assertEquals(withoutTotp, afterEmailRecoverTotp("t"))
    }

    @Test
    fun `the fifth wrong code goes back to the address step`() {
        // The server answers a used-up code like a wrong one, so the count
        // is this device's.
        val failure = emailRecoverFailureOf(EmailRecoverCall.Verify, http(401, "code-refused"))
        var state = afterEmailRecoverStart("c", 45, 600, 0L)
        repeat(EMAIL_CODE_MAX_TRIES - 1) {
            state = afterEmailRecoverFailure(state, failure, 0L)
            assertTrue(state.step is EmailRecoverStep.Code)
            assertEquals(UiText("recovery_email.code.wrong"), state.error)
        }
        state = afterEmailRecoverFailure(state, failure, 0L)
        assertEquals(EmailRecoverStep.Address, state.step)
        assertEquals(UiText("recovery_email.code.used_up"), state.error)
        // A new code starts a new count.
        assertEquals(0, (afterEmailRecoverStart("d", 45, 600, 0L).step as EmailRecoverStep.Code).wrongTries)
    }

    @Test
    fun `too many letters is a wait with a countdown, never a wrong code`() {
        val failure = emailRecoverFailureOf(EmailRecoverCall.Start, http(429, "email-rate-limited", retryAfterSecs = 90))
        assertEquals(EmailRecoverFailure.Wait(90), failure)
        val after = afterEmailRecoverFailure(EmailRecoverState(busy = true), failure, nowMs = 1_000L)
        assertEquals(91_000L, after.waitUntilMs)
        assertNull(after.error)
        assertEquals(EmailRecoverStep.Address, after.step)
        // A limit on the verify route that states a wait is a wait too.
        assertEquals(
            EmailRecoverFailure.Wait(30),
            emailRecoverFailureOf(EmailRecoverCall.Verify, http(429, "too-many-attempts", retryAfterSecs = 30)),
        )
    }

    @Test
    fun `a dead ticket or an expired code starts over`() {
        for (code in listOf("ticket-refused", "code-expired", "totp-required")) {
            assertEquals(
                code,
                EmailRecoverFailure.Restart(UiText("recovery_email.code.expired")),
                emailRecoverFailureOf(EmailRecoverCall.Password, http(401, code)),
            )
        }
        val step = EmailRecoverStep.Code("c", resendAtMs = 10L, expiresAtMs = 100L)
        assertFalse(emailCodeExpired(step, 99L))
        assertTrue(emailCodeExpired(step, 100L))
    }

    @Test
    fun `mail not set up, offline and an invalid address each have their line`() {
        assertEquals(
            EmailRecoverFailure.Unavailable,
            emailRecoverFailureOf(EmailRecoverCall.Start, http(503, RECOVERY_EMAIL_DISABLED)),
        )
        assertEquals(
            UiText("recovery_email.unavailable"),
            afterEmailRecoverFailure(EmailRecoverState(), EmailRecoverFailure.Unavailable, 0L).error,
        )
        assertEquals(
            EmailRecoverFailure.Line(UiText("common.error.offline")),
            emailRecoverFailureOf(EmailRecoverCall.Start, AuthNetworkError("/x", IOException("down"))),
        )
        assertEquals(
            EmailRecoverFailure.Line(UiText("recovery_email.invalid")),
            emailRecoverFailureOf(EmailRecoverCall.Start, http(400, "address-invalid")),
        )
        // Anything unknown is the unavailable line, never a wrong code.
        assertEquals(
            EmailRecoverFailure.Line(UiText("recovery.error.unavailable")),
            emailRecoverFailureOf(EmailRecoverCall.Password, http(500, null)),
        )
    }

    @Test
    fun `the state never prints what was typed`() {
        val text = afterEmailRecoverVerify("secret-ticket", totpRequired = true).toString()
        assertFalse(text.contains("secret-ticket"))
    }

    @Test
    fun `the enrol routes map the table of 19_2`() {
        assertEquals(
            EmailRecoverFailure.Line(UiText("recovery_email.send_failed")),
            recoveryEmailEnrolFailureOf(finishing = false, err = http(502, "mail-send-failed")),
        )
        assertEquals(
            EmailRecoverFailure.Line(UiText("recovery_email.needs_username")),
            recoveryEmailEnrolFailureOf(finishing = false, err = http(409, "username-required")),
        )
        assertEquals(
            EmailRecoverFailure.Wait(60),
            recoveryEmailEnrolFailureOf(finishing = false, err = http(429, "email-rate-limited", 60)),
        )
        assertEquals(
            EmailRecoverFailure.WrongCode(UiText("recovery_email.code.wrong")),
            recoveryEmailEnrolFailureOf(finishing = true, err = http(401, "code-refused")),
        )
        assertEquals(
            EmailRecoverFailure.WrongCode(UiText("recovery_email.code.expired")),
            recoveryEmailEnrolFailureOf(finishing = true, err = http(410, "code-expired")),
        )
        assertEquals(
            EmailRecoverFailure.Wait(null),
            recoveryEmailEnrolFailureOf(finishing = true, err = http(429, "too-many-attempts")),
        )
        assertEquals(
            EmailRecoverFailure.Unavailable,
            recoveryEmailEnrolFailureOf(finishing = false, err = http(503, RECOVERY_EMAIL_DISABLED)),
        )
        assertEquals(
            EmailRecoverFailure.Line(UiText("common.error.offline")),
            recoveryEmailEnrolFailureOf(finishing = true, err = AuthNetworkError("/x", IOException("down"))),
        )
    }
}
