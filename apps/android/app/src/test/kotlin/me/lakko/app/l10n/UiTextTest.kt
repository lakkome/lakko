// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import me.lakko.app.profile.DisplayNameProblem
import me.lakko.app.profile.UsernameCheck
import me.lakko.app.profile.UsernameProblem
import me.lakko.app.profile.usernameCanonicalHintText
import me.lakko.app.profile.usernameFieldErrorText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0061 - what the sign-in screens hand the catalog. */
class UiTextTest {

    @Test
    fun englishPositionsListHasNoSerialComma() {
        assertEquals("", listPositions(emptyList(), "en"))
        assertEquals("7", listPositions(listOf(7), "en"))
        assertEquals("3 and 7", listPositions(listOf(3, 7), "en"))
        assertEquals("3, 7 and 15", listPositions(listOf(3, 7, 15), "en"))
    }

    @Test
    fun theNameRulesCarryTheirKeysAndBounds() {
        assertEquals(UiText("username.error.too_short", counts = mapOf("count" to 3uL)), UsernameProblem.TOO_SHORT.text)
        assertEquals(UiText("username.error.too_long", counts = mapOf("count" to 32uL)), UsernameProblem.TOO_LONG.text)
        assertEquals(UiText("display_name.error.too_long", counts = mapOf("count" to 64uL)), DisplayNameProblem.TOO_LONG.text)
        assertEquals(UiText("username.error.unknown"), UsernameProblem.UNUSABLE.text)
    }

    @Test
    fun theFieldLineIsTheRuleOrTheCanonicalForm() {
        assertNull(usernameFieldErrorText(UsernameCheck.Invalid(UsernameProblem.EMPTY), "  "))
        assertEquals(
            UsernameProblem.INVALID_CHARACTERS.text,
            usernameFieldErrorText(UsernameCheck.Invalid(UsernameProblem.INVALID_CHARACTERS), "a-b"),
        )
        assertEquals(
            UiText("username.hint.shown_as", texts = mapOf("username" to "@alice")),
            usernameCanonicalHintText(UsernameCheck.Valid("alice"), "Alice"),
        )
        assertNull(usernameCanonicalHintText(UsernameCheck.Valid("alice"), "@alice"))
    }
}
