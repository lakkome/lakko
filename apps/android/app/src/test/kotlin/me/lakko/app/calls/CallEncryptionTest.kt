// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` sections 4 and 5 — the lock chip, and the safety number
 * behind it.
 *
 * ## The defect these pin
 *
 * The call screen printed the sixteen-digit safety number across its top,
 * permanently. The rule now is that the code is never shown unless the user asks
 * for it, and that the chip appears only when the call's encryption is really
 * armed — so the first two tests are the two halves of "never show the digits on
 * the call screen".
 */
class CallEncryptionTest {

    private val number = "0123 4567 8901 2345"

    @Test
    fun `an armed call shows the chip`() {
        assertTrue(showsCallEncryptionChip(armed = true, safetyNumber = number))
    }

    @Test
    fun `an unarmed call shows no chip at all`() {
        // Not a greyed-out lock and not an ellipsis: a call that cannot arm its
        // key refuses to start, so there is nothing to indicate.
        assertFalse(showsCallEncryptionChip(armed = false, safetyNumber = number))
        assertFalse(showsCallEncryptionChip(armed = false, safetyNumber = null))
    }

    @Test
    fun `a chip is never offered with an empty sheet behind it`() {
        assertFalse(showsCallEncryptionChip(armed = true, safetyNumber = null))
        assertFalse(showsCallEncryptionChip(armed = true, safetyNumber = "   "))
    }

    @Test
    fun `the chip says one word, and it is not a number`() {
        assertEquals("Encrypted", CALL_ENCRYPTION_CHIP_LABEL.en)
        assertTrue(CALL_ENCRYPTION_CHIP_LABEL.en.none { it.isDigit() })
    }

    @Test
    fun `the sheet names the other person on a one-to-one call`() {
        for (others in listOf(0, 1)) {
            assertEquals(
                "Only you and Anna can hear this call.",
                callEncryptionSentence("Anna", groupCall = false, othersInCall = others).en,
            )
        }
    }

    @Test
    fun `an unnamed peer gets a sentence that is still true`() {
        // No invented "Someone": that would read as a claim about a person.
        for (missing in listOf(null, "", "   ")) {
            assertEquals(GROUP_SENTENCE, callEncryptionSentence(missing, groupCall = false, othersInCall = 1).en)
        }
    }

    @Test
    fun `a group or link call never names one person, whatever its size`() {
        // The 2026-11-07 defect: the sheet read "Only you and <one participant>"
        // on a call with several people in it.
        for (others in listOf(0, 1, 2, 7)) {
            assertEquals(GROUP_SENTENCE, callEncryptionSentence("Anna", groupCall = true, othersInCall = others).en)
        }
        assertEquals(GROUP_SENTENCE, callEncryptionSentence("Anna", groupCall = false, othersInCall = 2).en)
    }

    @Test
    fun `the line under the number follows the kind of call`() {
        assertEquals(ONE_TO_ONE_HINT, callEncryptionHint(groupCall = false, othersInCall = 1).en)
        assertEquals(ONE_TO_ONE_HINT, callEncryptionHint(groupCall = false, othersInCall = 0).en)
        for (others in listOf(0, 1, 4)) {
            assertEquals(GROUP_HINT, callEncryptionHint(groupCall = true, othersInCall = others).en)
        }
        assertEquals(GROUP_HINT, callEncryptionHint(groupCall = false, othersInCall = 3).en)
        for (hint in listOf(ONE_TO_ONE_HINT, GROUP_HINT)) {
            assertFalse(hint.contains("phone", ignoreCase = true))
        }
    }

    private companion object {
        const val GROUP_SENTENCE = "Only the people on this call can hear it."
        const val ONE_TO_ONE_HINT =
            "Read the number aloud to each other. If it matches on both devices, no one else is listening."
        const val GROUP_HINT =
            "Everyone in this call sees the same number. Read it aloud. If it matches on every device, " +
                "no one else is listening."
    }

    @Test
    fun `the safety number is shown in four groups of four`() {
        assertEquals(listOf("0123", "4567", "8901", "2345"), safetyNumberGroups(number))
    }

    @Test
    fun `a number that arrives unspaced is grouped anyway`() {
        // The core renders it spaced and pins that with a golden vector; the
        // sheet re-groups rather than trusting it, because an ungrouped run of
        // sixteen digits is the exact failure this change removes.
        assertEquals(listOf("0123", "4567", "8901", "2345"), safetyNumberGroups("0123456789012345"))
    }

    @Test
    fun `no number means nothing to show`() {
        assertTrue(safetyNumberGroups(null).isEmpty())
        assertTrue(safetyNumberGroups("").isEmpty())
        assertTrue(safetyNumberGroups("  ").isEmpty())
    }

    @Test
    fun `the spoken label pauses between groups`() {
        assertEquals("Safety number: 0123, 4567, 8901, 2345", safetyNumberSpokenLabel(number, EnglishForTests.strings))
    }

    @Test
    fun `a missing number is spoken as missing rather than as silence`() {
        assertEquals("Safety number unavailable", safetyNumberSpokenLabel(null, EnglishForTests.strings))
    }

    @Test
    fun `the sheet explains what the number is for without naming a protocol`() {
        val banned = listOf("MLS", "SFrame", "exporter", "epoch", "E2EE", "key")
        for (groupCall in listOf(false, true)) {
            val hint = callEncryptionHint(groupCall, othersInCall = 1).en
            for (word in banned) {
                assertFalse("$word must not appear in `$hint`", hint.contains(word, ignoreCase = true))
            }
        }
        assertEquals("This call is encrypted", CALL_ENCRYPTION_SHEET_TITLE.en)
    }
}
