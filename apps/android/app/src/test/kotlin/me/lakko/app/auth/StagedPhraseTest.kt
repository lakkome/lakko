// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0017 lost-everything fallback: the phrase of a fresh identity is in its
 * account's row BEFORE the publish that makes it the only way back, and the
 * row is put back as it was when the publish does not happen.
 */
class StagedPhraseTest {

    private class Row(var stored: ByteArray? = null) : PhraseRow {
        var failWrites = false
        var writes = 0

        override fun read(): ByteArray? = stored?.copyOf()

        override fun write(phrase: ByteArray) {
            if (failWrites) throw IllegalStateException("keystore refused")
            writes += 1
            stored = phrase.copyOf()
        }

        override fun delete() {
            stored = null
        }
    }

    private fun bytes(text: String) = text.toByteArray(Charsets.UTF_8)

    @Test
    fun `staging writes the phrase where a killed process finds it again`() {
        val row = Row()
        val staged = StagedPhrase(row, "new phrase")
        staged.stage()
        assertArrayEquals(bytes("new phrase"), row.stored)
        assertTrue(staged.isHeld())
    }

    @Test
    fun `a row that cannot be written refuses before anything irreversible`() {
        val row = Row(bytes("old phrase")).apply { failWrites = true }
        val staged = StagedPhrase(row, "new phrase")
        try {
            staged.stage()
            fail("the mint went on without a stored phrase")
        } catch (_: IllegalStateException) {
            // expected: the caller refuses the mint here
        }
        assertArrayEquals(bytes("old phrase"), row.stored)
        assertFalse(staged.isHeld())
    }

    @Test
    fun `the publish did not happen - the previous phrase is back`() {
        val row = Row(bytes("old phrase"))
        val staged = StagedPhrase(row, "new phrase")
        staged.stage()
        assertTrue(staged.rollback())
        assertArrayEquals(bytes("old phrase"), row.stored)
    }

    @Test
    fun `the publish did not happen and there was no phrase before - the row is gone`() {
        val row = Row()
        val staged = StagedPhrase(row, "new phrase")
        staged.stage()
        assertTrue(staged.rollback())
        assertNull(row.stored)
    }

    @Test
    fun `a row wiped in between is not written back`() {
        // Forget device ran while the publish was out.
        val row = Row(bytes("old phrase"))
        val staged = StagedPhrase(row, "new phrase")
        staged.stage()
        row.stored = null
        val writesBefore = row.writes

        assertFalse(staged.isHeld())
        assertTrue(staged.rollback())

        assertNull(row.stored)
        assertEquals(writesBefore, row.writes)
    }

    @Test
    fun `a row rewritten in between is left as it is`() {
        val row = Row()
        val staged = StagedPhrase(row, "new phrase")
        staged.stage()
        row.stored = bytes("typed at another sign-in")
        assertTrue(staged.rollback())
        assertArrayEquals(bytes("typed at another sign-in"), row.stored)
    }

    @Test
    fun `a rollback that cannot write says so`() {
        val row = Row(bytes("old phrase"))
        val staged = StagedPhrase(row, "new phrase")
        staged.stage()
        row.failWrites = true
        assertFalse(staged.rollback())
    }

    @Test
    fun `closing wipes the copies and leaves the row`() {
        val row = Row(bytes("old phrase"))
        val staged = StagedPhrase(row, "new phrase")
        staged.stage()
        staged.close()
        assertArrayEquals(bytes("new phrase"), row.stored)
    }
}
