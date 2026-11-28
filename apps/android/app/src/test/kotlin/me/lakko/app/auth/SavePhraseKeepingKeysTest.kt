// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import java.io.IOException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * UX 19.5 (amended 2026-11-26) - the last step of "Save a recovery phrase":
 * no key changes; the phrase counts as saved once the account's protection
 * was published with it.
 */
class SavePhraseKeepingKeysTest {

    private class FakePorts(
        private val live: Boolean = true,
        private val refusal: Throwable? = null,
    ) : PhraseKeptPorts {
        val steps = mutableListOf<String>()
        override fun proofLive(): Boolean = live
        override suspend fun publishWithPhrase() {
            steps += "publish"
            refusal?.let { throw it }
        }
        override fun storeSaved() {
            steps += "saved"
        }
    }

    @Test
    fun `the phrase is saved only after the account's protection was published with it`() = runTest {
        val ports = FakePorts()
        assertEquals(PhraseSavedOutcome.Saved, savePhraseKeepingKeys(ports))
        assertEquals(listOf("publish", "saved"), ports.steps)
    }

    @Test
    fun `nothing counts as saved when the publish did not land`() = runTest {
        val offline = FakePorts(refusal = IOException("offline"))
        assertEquals(PhraseSavedOutcome.Failed, savePhraseKeepingKeys(offline))
        assertFalse("saved" in offline.steps)

        val stale = FakePorts(refusal = SealAnnouncementStale())
        assertEquals(PhraseSavedOutcome.Failed, savePhraseKeepingKeys(stale))
        assertFalse("saved" in stale.steps)
    }

    @Test
    fun `without a live proof nothing is published and the block is asked for`() = runTest {
        val ports = FakePorts(live = false)
        assertEquals(PhraseSavedOutcome.ProofRequired, savePhraseKeepingKeys(ports))
        assertTrue("nothing is sent without a proof", ports.steps.isEmpty())
    }

    @Test(expected = kotlinx.coroutines.CancellationException::class)
    fun `a cancellation is not read as a failure`() = runTest {
        savePhraseKeepingKeys(FakePorts(refusal = kotlinx.coroutines.CancellationException("left")))
    }
}
