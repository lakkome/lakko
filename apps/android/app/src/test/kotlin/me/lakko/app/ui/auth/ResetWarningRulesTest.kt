// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.ResetPasswordRequired
import me.lakko.app.auth.ResetWrongPassword
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/** UX 15.1 + 15.1a: the reset warning's state decisions. */
class ResetWarningRulesTest {

    @Test
    fun `with a live grant the word alone enables the button`() {
        assertTrue(resetButtonEnabled("RESET", needsPassword = false, password = ""))
        assertTrue(resetButtonEnabled(" RESET ", needsPassword = false, password = ""))
        assertFalse(resetButtonEnabled("reset", needsPassword = false, password = ""))
        assertFalse(resetButtonEnabled("", needsPassword = false, password = "hunter2"))
    }

    @Test
    fun `without a live grant the button waits for both the password and the word`() {
        assertFalse(resetButtonEnabled("RESET", needsPassword = true, password = ""))
        assertFalse(resetButtonEnabled("", needsPassword = true, password = "hunter2"))
        assertFalse(resetButtonEnabled("RESE", needsPassword = true, password = "hunter2"))
        assertTrue(resetButtonEnabled("RESET", needsPassword = true, password = "hunter2"))
    }

    @Test
    fun `the field is hidden by default and never pre-filled`() {
        val state = RestoreIdentityUiState()
        assertFalse(state.resetNeedsPassword)
        assertEquals("", state.resetPassword)
        assertNull(state.resetPasswordError)
    }

    @Test
    fun `a missing or refused grant is the password-required failure`() {
        assertSame(ResetFailure.PasswordRequired, resetFailureOf(ResetPasswordRequired()))
        assertSame(
            ResetFailure.PasswordRequired,
            resetFailureOf(AuthHttpError(403, "/account/keys/reset/finish", "{}", code = "password-required")),
        )
        assertSame(ResetFailure.WrongPassword, resetFailureOf(ResetWrongPassword()))
    }

    @Test
    fun `a limit keeps the sign-in screen's line and offline keeps the reset's`() {
        val limit = resetFailureOf(
            AuthHttpError(429, "/login/start", "{}", code = "too-many-attempts", retryAfterSecs = 300),
        ) as ResetFailure.Other
        assertEquals("common.error.too_many_attempts_minutes", limit.text.key)
        val offline = resetFailureOf(AuthNetworkError("/login/start", IOException("down"))) as ResetFailure.Other
        assertEquals("restore_identity.reset.error.offline", offline.text.key)
        val other = resetFailureOf(AuthHttpError(403, "/account/keys/reset/start", "{}", code = "nope")) as ResetFailure.Other
        assertEquals("restore_identity.reset.error.generic", other.text.key)
    }

    @Test
    fun `password required reveals the empty field with no error styling`() {
        val before = RestoreIdentityUiState(busy = true, error = UiText("x"), resetPassword = "stale")
        val after = afterResetFailure(before, ResetFailure.PasswordRequired, needsPasswordNow = true)
        assertFalse(after.busy)
        assertTrue(after.resetNeedsPassword)
        assertEquals("", after.resetPassword)
        assertNull(after.resetPasswordError)
        assertNull(after.error)
    }

    @Test
    fun `a wrong password puts its sentence under the field and nowhere else`() {
        val before = RestoreIdentityUiState(busy = true, resetNeedsPassword = true, resetPassword = "typo")
        val after = afterResetFailure(before, ResetFailure.WrongPassword, needsPasswordNow = true)
        assertFalse(after.busy)
        assertTrue(after.resetNeedsPassword)
        assertEquals("restore_identity.reset.error.wrong_password", after.resetPasswordError?.key)
        assertNull(after.error)
        // Kept for the person to correct.
        assertEquals("typo", after.resetPassword)
    }

    @Test
    fun `a failure after the password was accepted does not ask for it again`() {
        val before = RestoreIdentityUiState(busy = true, resetNeedsPassword = true, resetPassword = "right")
        val failure = ResetFailure.Other(UiText("restore_identity.reset.error.offline"))
        val granted = afterResetFailure(before, failure, needsPasswordNow = false)
        assertFalse(granted.resetNeedsPassword)
        assertEquals("", granted.resetPassword)
        assertEquals("restore_identity.reset.error.offline", granted.error?.key)
        // The sign-in itself failed (offline, a limit): the field stays, with
        // what was typed.
        val notGranted = afterResetFailure(before, failure, needsPasswordNow = true)
        assertTrue(notGranted.resetNeedsPassword)
        assertEquals("right", notGranted.resetPassword)
    }

    @Test
    fun `the screen state never prints the typed password`() {
        assertFalse(RestoreIdentityUiState(resetPassword = "hunter2", phrase = "abandon").toString().contains("hunter2"))
    }

    @Test
    fun `the username field shows only with the password field and only when the name is unknown`() {
        assertTrue(resetShowsUsername(needsPassword = true, usernameKnown = false))
        assertFalse(resetShowsUsername(needsPassword = true, usernameKnown = true))
        // A live grant: section 15.1 exactly, no field at all.
        assertFalse(resetShowsUsername(needsPassword = false, usernameKnown = false))
        assertFalse(resetShowsUsername(needsPassword = false, usernameKnown = true))
    }

    @Test
    fun `with an unknown username the button waits for the name too`() {
        assertFalse(
            resetButtonEnabled("RESET", needsPassword = true, password = "hunter2", usernameKnown = false, username = ""),
        )
        assertFalse(
            resetButtonEnabled("RESET", needsPassword = true, password = "hunter2", usernameKnown = false, username = "  "),
        )
        assertFalse(
            resetButtonEnabled("RESET", needsPassword = true, password = "", usernameKnown = false, username = "alice"),
        )
        assertFalse(
            resetButtonEnabled("", needsPassword = true, password = "hunter2", usernameKnown = false, username = "alice"),
        )
        assertTrue(
            resetButtonEnabled("RESET", needsPassword = true, password = "hunter2", usernameKnown = false, username = "alice"),
        )
    }

    @Test
    fun `a known username or a live grant never waits for a typed name`() {
        assertTrue(
            resetButtonEnabled("RESET", needsPassword = true, password = "hunter2", usernameKnown = true, username = ""),
        )
        assertTrue(
            resetButtonEnabled("RESET", needsPassword = false, password = "", usernameKnown = false, username = ""),
        )
    }

    @Test
    fun `opening the warning starts with empty fields`() {
        val before = RestoreIdentityUiState(
            resetPassword = "stale",
            resetUsername = "stale",
            resetPasswordError = UiText("x"),
            error = UiText("y"),
        )
        val after = afterResetOpened(before, needsPassword = true, alreadyOpen = false)
        assertTrue(after.resetNeedsPassword)
        assertEquals("", after.resetPassword)
        assertEquals("", after.resetUsername)
        assertNull(after.resetPasswordError)
        assertNull(after.error)
    }

    @Test
    fun `a configuration change keeps the typed password and username`() {
        val typed = RestoreIdentityUiState(
            resetNeedsPassword = true,
            resetPassword = "hunter2",
            resetUsername = "alice",
            resetPasswordError = UiText("restore_identity.reset.error.wrong_password"),
        )
        val after = afterResetOpened(typed, needsPassword = true, alreadyOpen = true)
        assertEquals("hunter2", after.resetPassword)
        assertEquals("alice", after.resetUsername)
        assertEquals("restore_identity.reset.error.wrong_password", after.resetPasswordError?.key)
        assertTrue(after.resetNeedsPassword)
    }

    @Test
    fun `a field shown before a configuration change does not vanish, and an expired grant reveals it`() {
        val shown = RestoreIdentityUiState(resetNeedsPassword = true, resetPassword = "hunter2")
        assertTrue(afterResetOpened(shown, needsPassword = false, alreadyOpen = true).resetNeedsPassword)
        val hidden = RestoreIdentityUiState(resetNeedsPassword = false)
        assertTrue(afterResetOpened(hidden, needsPassword = true, alreadyOpen = true).resetNeedsPassword)
        assertFalse(afterResetOpened(hidden, needsPassword = false, alreadyOpen = true).resetNeedsPassword)
    }

    @Test
    fun `a failed reset keeps the typed username`() {
        val before = RestoreIdentityUiState(busy = true, resetNeedsPassword = true, resetPassword = "typo", resetUsername = "alice")
        assertEquals("alice", afterResetFailure(before, ResetFailure.WrongPassword, needsPasswordNow = true).resetUsername)
        assertEquals("alice", afterResetFailure(before, ResetFailure.PasswordRequired, needsPasswordNow = true).resetUsername)
    }

    @Test
    fun `the screen state never prints the typed username`() {
        assertFalse(RestoreIdentityUiState(resetUsername = "alice").toString().contains("alice"))
    }
}
