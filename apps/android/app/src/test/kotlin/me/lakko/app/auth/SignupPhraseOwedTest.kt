// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A signup's recovery phrase stays owed across a restart until the person
 * acknowledges it (`SignupPhraseOwed.kt`), the marker half of the web
 * `phrase-gate.ts`. The guest who chose `Create account` is held to the same
 * rule.
 */
class SignupPhraseOwedTest {

    private class MemoryStorage : MarkerStorage {
        val rows = LinkedHashMap<String, String>()

        override fun getItem(key: String): String? = rows[key]

        override fun setItem(key: String, value: String) {
            rows[key] = value
        }

        override fun removeItem(key: String) {
            rows.remove(key)
        }
    }

    private object ThrowingStorage : MarkerStorage {
        override fun getItem(key: String): String? = throw IllegalStateException("blocked")

        override fun setItem(key: String, value: String): Unit = throw IllegalStateException("blocked")

        override fun removeItem(key: String): Unit = throw IllegalStateException("blocked")
    }

    @Test
    fun `nothing is owed until it is marked, and only for that account`() {
        val storage = MemoryStorage()
        assertFalse(isSignupPhraseOwed(storage, "acct"))
        assertFalse(isSignupPhraseOwed(null, "acct"))
        markSignupPhraseOwed(storage, "acct")
        assertTrue(isSignupPhraseOwed(storage, "acct"))
        assertFalse(isSignupPhraseOwed(storage, "other"))
    }

    @Test
    fun `acknowledging clears the marker and leaves no row`() {
        val storage = MemoryStorage()
        markSignupPhraseOwed(storage, "acct")
        clearSignupPhraseOwed(storage, "acct")
        assertFalse(isSignupPhraseOwed(storage, "acct"))
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `the marker never holds the phrase`() {
        val storage = MemoryStorage()
        markSignupPhraseOwed(storage, "acct")
        assertEquals(listOf("1"), storage.rows.values.toList())
    }

    @Test
    fun `the marker never throws when storage refuses`() {
        markSignupPhraseOwed(ThrowingStorage, "acct")
        clearSignupPhraseOwed(ThrowingStorage, "acct")
        assertFalse(isSignupPhraseOwed(ThrowingStorage, "acct"))
    }

    @Test
    fun `a restart shows an owed phrase again`() {
        val storage = MemoryStorage()
        markSignupPhraseOwed(storage, "acct")
        val logged = mutableListOf<String>()
        val phrase = owedSignupPhrase(storage, "acct", readPhrase = { "abandon ability able" }) { m, _ -> logged += m }
        assertEquals("abandon ability able", phrase)
        assertTrue(logged.isEmpty())
    }

    @Test
    fun `a restart with nothing owed never reads the phrase`() {
        var read = false
        val phrase = owedSignupPhrase(MemoryStorage(), "acct", readPhrase = { read = true; "words" }) { _, _ -> }
        assertNull(phrase)
        assertFalse(read)
    }

    @Test
    fun `an owed phrase that cannot be read is reported and blocks nothing`() {
        val storage = MemoryStorage()
        markSignupPhraseOwed(storage, "acct")
        val logged = mutableListOf<String>()
        assertNull(owedSignupPhrase(storage, "acct", readPhrase = { null }) { m, _ -> logged += m })
        assertNull(
            owedSignupPhrase(storage, "acct", readPhrase = { throw IllegalStateException("keystore") }) { m, _ ->
                logged += m
            },
        )
        assertEquals(2, logged.size)
    }
}
