// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * ADR-0048 §4 — the UI's reading of the core's username and profile-name
 * refusals.
 *
 * The rule itself is the core's; these tests pin the MAPPING from its typed
 * refusal to the UX spec's copy, and the search-field decision that no lookup
 * is ever produced for a failing name. The core functions are stand-ins that
 * throw the same `FfiException` subclasses the generated binding throws
 * (plain Kotlin classes, no `.so`), and the stand-in username rule applies the
 * ADR's own rule (trim, strip `@`, lowercase, `[a-z0-9_]`, 3..32) so each
 * refusal is reached by a realistic input rather than by a switch in the test.
 */
class NameRulesTest {

    /** Stand-in for `validateUsername` applying the ADR-0048 §4 rule. */
    private fun fakeValidate(input: String): String {
        val n = input.trim().removePrefix("@").lowercase()
        if (n.isEmpty()) throw FfiException.UsernameEmpty("empty")
        if (!n.all { it in 'a'..'z' || it in '0'..'9' || it == '_' }) {
            throw FfiException.UsernameInvalidCharacters("chars")
        }
        if (n.length < 3) throw FfiException.UsernameTooShort("short")
        if (n.length > 32) throw FfiException.UsernameTooLong("long")
        return n
    }

    @Test
    fun `each username refusal maps to the spec copy`() {
        assertEquals(
            UsernameCheck.Invalid(UsernameProblem.INVALID_CHARACTERS),
            checkUsername("алиса", ::fakeValidate),
        )
        assertEquals(UsernameCheck.Invalid(UsernameProblem.TOO_SHORT), checkUsername("al", ::fakeValidate))
        assertEquals(
            UsernameCheck.Invalid(UsernameProblem.TOO_LONG),
            checkUsername("a".repeat(33), ::fakeValidate),
        )
        assertEquals(UsernameCheck.Invalid(UsernameProblem.EMPTY), checkUsername("  @ ", ::fakeValidate))
        assertEquals("Only Latin letters, digits and _", UsernameProblem.INVALID_CHARACTERS.copy)
        assertEquals("At least 3 characters", UsernameProblem.TOO_SHORT.copy)
        assertEquals("At most 32 characters", UsernameProblem.TOO_LONG.copy)
    }

    @Test
    fun `a refusal the core did not classify fails closed`() {
        val check = checkUsername("alice") { throw FfiException.Discovery("voprf") }
        assertEquals(UsernameCheck.Invalid(UsernameProblem.UNUSABLE), check)
    }

    @Test
    fun `a valid name carries its canonical form and a lowercase hint`() {
        val check = checkUsername("@Alice", ::fakeValidate)
        assertEquals(UsernameCheck.Valid("alice"), check)
        assertEquals(
            me.lakko.app.l10n.UiText("username.hint.shown_as", texts = mapOf("username" to "@alice")),
            usernameCanonicalHintText(check, "@Alice"),
        )
        assertNull(usernameCanonicalHintText(checkUsername("alice", ::fakeValidate), "alice"))
    }

    @Test
    fun `the field shows no error for a blank field and the reason otherwise`() {
        assertNull(usernameFieldErrorText(checkUsername("", ::fakeValidate), ""))
        assertEquals(
            me.lakko.app.l10n.UiText("username.error.invalid_characters"),
            usernameFieldErrorText(checkUsername("a-b-c", ::fakeValidate), "a-b-c"),
        )
        assertNull(usernameFieldErrorText(checkUsername("alice", ::fakeValidate), "alice"))
    }

    @Test
    fun `a search field never produces a lookup for a failing name`() {
        val invalid = listOf("", "ab", "a-lice", "аlice", "a".repeat(40), "al ice")
        for (input in invalid) {
            val decision = searchDecision(input, ::fakeValidate)
            assertTrue("'$input' must refuse, got $decision", decision is SearchDecision.Refuse)
        }
        assertEquals(SearchDecision.Lookup("alice"), searchDecision(" @Alice ", ::fakeValidate))
    }

    @Test
    fun `each display name refusal maps to its copy and blank is no name`() {
        fun fakeNormalize(input: String): String? {
            val t = input.trim()
            if (t.isEmpty()) return null
            if ('\n' in t) throw FfiException.DisplayNameLineBreak("lb")
            if ('‮' in t) throw FfiException.DisplayNameControlOrFormatCharacter("cf")
            if (t.length > 64) throw FfiException.DisplayNameTooLong("long")
            return t
        }
        assertEquals(DisplayNameCheck.Valid(null), checkDisplayName("   ", ::fakeNormalize))
        assertEquals(DisplayNameCheck.Valid("Alice"), checkDisplayName(" Alice ", ::fakeNormalize))
        assertEquals(
            DisplayNameCheck.Invalid(DisplayNameProblem.LINE_BREAK),
            checkDisplayName("A\nB", ::fakeNormalize),
        )
        assertEquals(
            DisplayNameCheck.Invalid(DisplayNameProblem.CONTROL_OR_FORMAT),
            checkDisplayName("Alice‮", ::fakeNormalize),
        )
        assertEquals(
            DisplayNameCheck.Invalid(DisplayNameProblem.TOO_LONG),
            checkDisplayName("x".repeat(65), ::fakeNormalize),
        )
        assertEquals(
            DisplayNameProblem.TOO_MANY_BYTES,
            displayNameProblemOf(FfiException.DisplayNameTooManyBytes("b")),
        )
        assertNull(displayNameProblemOf(FfiException.UsernameEmpty("u")))
    }
}
