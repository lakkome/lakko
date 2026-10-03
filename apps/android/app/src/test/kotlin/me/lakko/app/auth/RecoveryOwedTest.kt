// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import org.junit.Assert.assertEquals
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
        store.markPhraseOwed("a", OwedPhraseOrigin.Signup)
        assertTrue(store.isPhraseOwed("a"))
        assertTrue(store.phraseShowable("a"))
        assertFalse(store.isPhraseOwed("b"))
        store.clearPhraseOwed("a")
        assertFalse(store.phraseShowable("a"))
    }

    @Test
    fun `a phrase whose new keys are not published yet is held back`() {
        val store = RecoveryOwedStore(MapStorage())
        store.markPhraseOwed("a", OwedPhraseOrigin.FreshIdentity)
        store.markPublishOwed("a")
        assertTrue(store.isPublishOwed("a"))
        assertFalse("nobody can find the new keys yet", store.phraseShowable("a"))
        store.clearPublishOwed("a")
        assertTrue(store.phraseShowable("a"))
    }

    @Test
    fun `the markers survive a new store over the same storage and go with the account`() {
        val storage = MapStorage()
        RecoveryOwedStore(storage).apply {
            markPhraseOwed("a", OwedPhraseOrigin.Signup)
            markPublishOwed("a")
            markPhraseOwed("b", OwedPhraseOrigin.Signup)
        }
        val reopened = RecoveryOwedStore(storage)
        assertTrue(reopened.isPhraseOwed("a"))
        assertTrue(reopened.isPublishOwed("a"))
        reopened.clearAccount("a")
        assertFalse(reopened.isPhraseOwed("a"))
        assertFalse(reopened.isPublishOwed("a"))
        assertTrue(reopened.isPhraseOwed("b"))
    }

    @Test
    fun `the owed phrase remembers whether a sign-up or a fresh identity made it`() {
        val storage = MapStorage()
        val store = RecoveryOwedStore(storage)
        assertEquals(null, store.owedPhraseOrigin("a"))
        store.markPhraseOwed("a", OwedPhraseOrigin.Signup)
        store.markPhraseOwed("b", OwedPhraseOrigin.FreshIdentity)
        assertEquals(OwedPhraseOrigin.Signup, store.owedPhraseOrigin("a"))
        assertEquals(OwedPhraseOrigin.FreshIdentity, RecoveryOwedStore(storage).owedPhraseOrigin("b"))
        assertTrue(store.isPhraseOwed("b"))
        // A row written before the origin was recorded reads as a sign-up.
        storage.rows["phrase-owed:c"] = "1"
        assertEquals(OwedPhraseOrigin.Signup, store.owedPhraseOrigin("c"))
        store.clearPhraseOwed("b")
        assertEquals(null, store.owedPhraseOrigin("b"))
    }

    @Test
    fun `show recovery phrase withholds the words while a publish is owed`() {
        assertEquals(
            PhraseRevealGate.PublishPending,
            phraseRevealGate(publishOwed = true, phraseOnDevice = true),
        )
        assertEquals(
            PhraseRevealGate.PublishPending,
            phraseRevealGate(publishOwed = true, phraseOnDevice = false),
        )
        assertEquals(PhraseRevealGate.Show, phraseRevealGate(publishOwed = false, phraseOnDevice = true))
        assertEquals(PhraseRevealGate.NotSaved, phraseRevealGate(publishOwed = false, phraseOnDevice = false))
    }
}
