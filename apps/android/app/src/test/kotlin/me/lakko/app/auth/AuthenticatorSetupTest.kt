// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** UX 19.1b - setting up an authenticator app, at sign-up and in Settings. */
class AuthenticatorSetupTest {

    private fun http(status: Int, code: String? = null, retry: Long? = null) =
        AuthHttpError(status, "/totp/enroll/finish", "{}", code = code, retryAfterSecs = retry)

    @Test
    fun `the key is shown in groups of four`() {
        assertEquals("ABCD EFGH IJKL MNOP", groupedTotpKey("ABCDEFGHIJKLMNOP"))
        assertEquals("ABCD EFGH IJ", groupedTotpKey("ABCDEFGHIJ"))
        assertEquals("ABCD EFGH", groupedTotpKey("ABCD EFGH"))
        assertEquals("", groupedTotpKey(""))
    }

    @Test
    fun `the code is checked on the sixth digit and not before`() {
        assertNull(totpCodeToCheck("12345"))
        assertEquals("123456", totpCodeToCheck("123456"))
        assertEquals("123456", totpCodeToCheck(" 123456 "))
        assertNull(totpCodeToCheck("1234567"))
        assertNull(totpCodeToCheck("12345a"))
    }

    @Test
    fun `a missing or refused proof shows the block, with no error`() {
        for (finishing in listOf(false, true)) {
            assertEquals(TotpSetupFailure.ProofRequired, totpSetupFailureOf(finishing, FreshAuthRequired()))
            assertEquals(
                TotpSetupFailure.ProofRequired,
                totpSetupFailureOf(finishing, http(403, FRESH_AUTH_REQUIRED)),
            )
        }
    }

    @Test
    fun `a wrong code is one only when finishing`() {
        assertEquals(TotpSetupFailure.WrongCode, totpSetupFailureOf(finishing = true, err = http(401)))
        assertEquals(TotpSetupFailure.Generic, totpSetupFailureOf(finishing = false, err = http(401)))
    }

    @Test
    fun `a limit is a wait, an existing authenticator is said, offline keeps the screen`() {
        assertEquals(TotpSetupFailure.Wait(120), totpSetupFailureOf(true, http(429, "too-many-attempts", 120)))
        assertEquals(TotpSetupFailure.Wait(null), totpSetupFailureOf(false, http(429)))
        assertEquals(TotpSetupFailure.AlreadyEnabled, totpSetupFailureOf(false, http(409)))
        assertEquals(TotpSetupFailure.AlreadyEnabled, totpSetupFailureOf(true, http(409)))
        assertEquals(TotpSetupFailure.Offline, totpSetupFailureOf(true, AuthNetworkError("/x", IOException("down"))))
        assertEquals(TotpSetupFailure.Offline, totpSetupFailureOf(false, IOException("down")))
    }

    @Test
    fun `anything else starts over with a new key`() {
        assertEquals(TotpSetupFailure.Generic, totpSetupFailureOf(true, http(500)))
        assertEquals(TotpSetupFailure.Generic, totpSetupFailureOf(false, http(503)))
        assertEquals(TotpSetupFailure.Generic, totpSetupFailureOf(true, IllegalStateException("x")))
    }
}
