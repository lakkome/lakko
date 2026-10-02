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
    fun `a phrase whose recovery credential is not replaced yet is held back`() {
        val store = RecoveryOwedStore(MapStorage())
        store.markPhraseOwed("a", OwedPhraseOrigin.Signup)
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
            markPhraseOwed("a", OwedPhraseOrigin.Signup)
            markCredentialOwed("a")
            markPhraseOwed("b", OwedPhraseOrigin.Signup)
        }
        val reopened = RecoveryOwedStore(storage)
        assertTrue(reopened.isPhraseOwed("a"))
        assertTrue(reopened.isCredentialOwed("a"))
        reopened.clearAccount("a")
        assertFalse(reopened.isPhraseOwed("a"))
        assertFalse(reopened.isCredentialOwed("a"))
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
    fun `show recovery phrase withholds the words while a credential replace is owed`() {
        assertEquals(
            PhraseRevealGate.CredentialPending,
            phraseRevealGate(credentialOwed = true, phraseOnDevice = true),
        )
        assertEquals(
            PhraseRevealGate.CredentialPending,
            phraseRevealGate(credentialOwed = true, phraseOnDevice = false),
        )
        assertEquals(PhraseRevealGate.Show, phraseRevealGate(credentialOwed = false, phraseOnDevice = true))
        assertEquals(PhraseRevealGate.NotSaved, phraseRevealGate(credentialOwed = false, phraseOnDevice = false))
    }
}
