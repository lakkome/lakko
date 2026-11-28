// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.FreshAuthRequired
import me.lakko.app.auth.ResetFactorRequired
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/** UX 15.1 + 15.1b: what a failed reset leaves on the warning screen. The
 *  factor section's own rules are `me.lakko.app.auth.ResetFactorTest`. */
class ResetWarningRulesTest {

    @Test
    fun `the typed word must be RESET, surrounding whitespace ignored`() {
        assertTrue(resetConfirmationMatches("RESET"))
        assertTrue(resetConfirmationMatches(" RESET "))
        assertFalse(resetConfirmationMatches("reset"))
        assertFalse(resetConfirmationMatches(""))
    }

    @Test
    fun `a missing or refused confirmation opens the factor section again`() {
        // No live confirmation on this device.
        assertSame(ResetFailure.FactorRequired, resetFailureOf(ResetFactorRequired()))
        // The server refused the one sent.
        assertSame(
            ResetFailure.FactorRequired,
            resetFailureOf(AuthHttpError(403, "/account/keys/reset/finish", "{}", code = "reset-factor-required")),
        )
        // The password proof behind it ran out.
        assertSame(ResetFailure.FactorRequired, resetFailureOf(FreshAuthRequired()))
        assertSame(
            ResetFailure.FactorRequired,
            resetFailureOf(AuthHttpError(403, "/account/keys/reset/start", "{}", code = "fresh-auth-required")),
        )
    }

    @Test
    fun `a limit keeps the sign-in screen's line and offline keeps the reset's`() {
        val limit = resetFailureOf(
            AuthHttpError(429, "/account/keys/reset/start", "{}", code = "too-many-attempts", retryAfterSecs = 300),
        ) as ResetFailure.Other
        assertEquals("common.error.too_many_attempts_minutes", limit.text.key)
        val offline = resetFailureOf(AuthNetworkError("/account/keys/reset/start", IOException("down"))) as ResetFailure.Other
        assertEquals("restore_identity.reset.error.offline", offline.text.key)
        val other = resetFailureOf(AuthHttpError(403, "/account/keys/reset/start", "{}", code = "nope")) as ResetFailure.Other
        assertEquals("restore_identity.reset.error.generic", other.text.key)
    }

    @Test
    fun `factor required leaves the screen idle with no error styling`() {
        val before = RestoreIdentityUiState(busy = true, error = UiText("stale"))
        val after = afterResetFailure(before, ResetFailure.FactorRequired)
        assertFalse(after.busy)
        assertNull(after.error)
    }

    @Test
    fun `any other failure puts its sentence on the form`() {
        val before = RestoreIdentityUiState(busy = true)
        val after = afterResetFailure(before, ResetFailure.Other(UiText("restore_identity.reset.error.offline")))
        assertFalse(after.busy)
        assertEquals("restore_identity.reset.error.offline", after.error?.key)
    }

    @Test
    fun `the screen state never prints the typed phrase`() {
        assertFalse(RestoreIdentityUiState(phrase = "abandon ability").toString().contains("abandon"))
    }

    @Test
    fun `restore your keys lists no email row, and nothing optional when only the earlier keys are missing`() {
        assertEquals(RestoreRows(passkey = true, phrase = true), restoreRowsFor(false, passkeyOffered = true, phrase = true))
        assertEquals(RestoreRows(passkey = false, phrase = false), restoreRowsFor(false, passkeyOffered = false, phrase = false))
        // UX 8a: a passkey and the phrase bring the keys this device has.
        assertEquals(RestoreRows(passkey = false, phrase = false), restoreRowsFor(true, passkeyOffered = true, phrase = true))
    }

    @Test
    fun `the cannot panel lists its three ways out in order`() {
        assertEquals(
            listOf(
                "proof.reset.cannot.option_device",
                "proof.reset.cannot.option_phrase",
                "proof.reset.cannot.option_passkey",
            ),
            RESET_CANNOT_OPTION_KEYS,
        )
    }

    @Test
    fun `setting up an authenticator has its own proof helper`() {
        assertEquals("proof.add_totp.help", proofCopy(ProofSurface.AddTotp).helper)
    }
}
