// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import me.lakko.app.auth.AuthClientArgError
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import me.lakko.app.auth.ThreeWordsAccount
import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/** Small pure rules of the recovery screens. */
class RecoveryRulesTest {

    @Test
    fun `the note under the list names three words only where the row is`() {
        assertEquals("recovery.access_only_note_words", accessOnlyNoteKey(threeWords = true))
        assertEquals("recovery.access_only_note", accessOnlyNoteKey(threeWords = false))
    }

    @Test
    fun `the three word form submits only three word-shaped answers`() {
        val form = ThreeWordsUi(accountIdB64 = "a", username = "alice", positions = listOf(7, 3, 12))
        assertFalse(form.complete)
        assertFalse(form.copy(typed = listOf("abandon", "", "zoo")).complete)
        assertFalse(form.copy(typed = listOf("abandon", "12", "zoo")).complete)
        assertTrue(form.copy(typed = listOf("abandon", " Able ", "zoo")).complete)
        // The positions keep the order they were drawn in.
        assertEquals(listOf(7, 3, 12), form.positions)
        assertEquals("a", ThreeWordsAccount("a", "alice").accountIdB64)
    }

    @Test
    fun `a new password must match and be long enough before anything is sent`() {
        assertEquals(UiText("auth.password.error.mismatch"), newPasswordProblem("longenough", "different"))
        assertEquals("auth.password.error.too_short", newPasswordProblem("short", "short")?.key)
        assertNull(newPasswordProblem("longenough", "longenough"))
    }

    @Test
    fun `a failed password change reads by its kind`() {
        assertEquals(UiText("common.error.offline"), newPasswordFailure(AuthNetworkError("/x", IOException("down"))))
        assertEquals(
            UiText("recovery.new_password.error.generic"),
            newPasswordFailure(AuthHttpError(401, "/password/reset/finish", "")),
        )
        assertEquals(
            "common.error.too_many_attempts",
            newPasswordFailure(AuthHttpError(429, "/password/reset/start", "", code = "too-many-attempts")).key,
        )
        val own = UiText("common.error.session_not_ready")
        assertEquals(own, newPasswordFailure(AuthClientArgError("no session", own)))
        assertEquals(UiText("recovery.new_password.error.generic"), newPasswordFailure(IllegalStateException()))
    }

    @Test
    fun `a failed retry of the reset publish reads by its kind`() {
        assertEquals(UiText("common.error.offline"), freshPublishFailure(AuthNetworkError("/x", IOException("down"))))
        assertEquals(
            UiText("common.error.offline"),
            freshPublishFailure(me.lakko.app.discovery.DiscoveryNetworkError("/discovery/identity", IOException("down"))),
        )
        assertEquals(
            UiText("recovery_phrase.fresh.publish_error"),
            freshPublishFailure(AuthHttpError(503, "/account/keys/reset/start", "")),
        )
        assertEquals(
            UiText("recovery_phrase.fresh.publish_error"),
            freshPublishFailure(IllegalStateException()),
        )
    }

    @Test
    fun `the reset button takes the typed word RESET and nothing else`() {
        assertTrue(resetConfirmationMatches("RESET"))
        assertTrue(resetConfirmationMatches("  RESET "))
        assertFalse(resetConfirmationMatches("reset"))
        assertFalse(resetConfirmationMatches("RESE"))
        assertFalse(resetConfirmationMatches(""))
        assertFalse(resetConfirmationMatches("RESET account keys"))
        assertEquals("RESET", RESET_CONFIRMATION_WORD)
    }

    @Test
    fun `a reset that failed before anything changed reads offline, limited, or try later`() {
        assertEquals(
            UiText("restore_identity.reset.error.offline"),
            resetFailureText(AuthNetworkError("/account/keys/reset/start", IOException("down"))),
        )
        assertEquals(
            "common.error.too_many_attempts",
            resetFailureText(
                AuthHttpError(429, "/account/keys/reset/start", "", code = "too-many-attempts", retryAfterSecs = 30),
            ).key,
        )
        assertEquals(
            UiText("restore_identity.reset.error.generic"),
            resetFailureText(AuthHttpError(503, "/account/keys/reset/finish", "", code = "key-reset-disabled")),
        )
        assertEquals(
            UiText("restore_identity.reset.error.generic"),
            resetFailureText(AuthHttpError(401, "/account/keys/reset/finish", "")),
        )
        assertEquals(UiText("restore_identity.reset.error.generic"), resetFailureText(IllegalStateException()))
    }
}
