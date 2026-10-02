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
    fun `a failed retry of the recovery credential reads by its kind`() {
        assertEquals(UiText("common.error.offline"), freshCredentialFailure(AuthNetworkError("/x", IOException("down"))))
        assertEquals(
            UiText("recovery_phrase.fresh.credential_error"),
            freshCredentialFailure(AuthHttpError(503, "/recovery/credential/start", "")),
        )
        assertEquals(
            UiText("recovery_phrase.fresh.credential_error"),
            freshCredentialFailure(IllegalStateException()),
        )
    }
}
