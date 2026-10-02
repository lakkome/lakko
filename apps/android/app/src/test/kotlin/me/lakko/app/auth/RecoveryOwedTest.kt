// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The two debts a landing can leave for an account on this device. */
class RecoveryOwedTest {

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    @Test
    fun `an owed phrase is showable until it is confirmed`() {
        val store = RecoveryOwedStore(MapStorage())
        assertFalse(store.phraseShowable("a"))
        store.markPhraseOwed("a")
        assertTrue(store.isPhraseOwed("a"))
        assertTrue(store.phraseShowable("a"))
        assertFalse(store.isPhraseOwed("b"))
        store.clearPhraseOwed("a")
        assertFalse(store.phraseShowable("a"))
    }

    @Test
    fun `a phrase whose recovery credential is not replaced yet is held back`() {
        val store = RecoveryOwedStore(MapStorage())
        store.markPhraseOwed("a")
        store.markCredentialOwed("a")
        assertTrue(store.isCredentialOwed("a"))
        assertFalse("the new words would not sign in yet", store.phraseShowable("a"))
        store.clearCredentialOwed("a")
        assertTrue(store.phraseShowable("a"))
    }

    @Test
    fun `the markers survive a new store over the same storage and go with the account`() {
        val storage = MapStorage()
        RecoveryOwedStore(storage).apply {
            markPhraseOwed("a")
            markCredentialOwed("a")
            markPhraseOwed("b")
        }
        val reopened = RecoveryOwedStore(storage)
        assertTrue(reopened.isPhraseOwed("a"))
        assertTrue(reopened.isCredentialOwed("a"))
        reopened.clearAccount("a")
        assertFalse(reopened.isPhraseOwed("a"))
        assertFalse(reopened.isCredentialOwed("a"))
        assertTrue(reopened.isPhraseOwed("b"))
    }
}
