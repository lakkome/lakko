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
 * ADR-0075 section 3 as amended 2026-11-01 / UX 19.5 - which way "Save a
 * recovery phrase" goes, and the last step of the way that keeps the keys.
 */
class SavePhraseKeepingKeysTest {

    private class FakePorts(
        private val grant: String? = "grant",
        private val refusal: Throwable? = null,
    ) : PhraseKeptPorts {
        val steps = mutableListOf<String>()
        override fun freshGrant(): String? = grant
        override fun markOwnAction() {
            steps += "own"
        }
        override suspend fun markSaved(freshGrant: String) {
            steps += "post:$freshGrant"
            refusal?.let { throw it }
        }
        override fun storeSaved() {
            steps += "saved"
        }
    }

    private fun http(status: Int, code: String?) = AuthHttpError(status, "/account/phrase/saved", "{}", code = code)

    @Test
    fun `rotation_needed keeps the rotation path whatever the device holds`() {
        assertEquals(
            PhraseSavePath.Rotate,
            phraseSavePath(rotationNeeded = true, holdsPhrase = true, rotationInFlight = false),
        )
        assertEquals(
            PhraseSavePath.Rotate,
            phraseSavePath(rotationNeeded = true, holdsPhrase = false, rotationInFlight = false),
        )
    }

    @Test
    fun `a device without the phrase rotates`() {
        assertEquals(
            PhraseSavePath.Rotate,
            phraseSavePath(rotationNeeded = false, holdsPhrase = false, rotationInFlight = false),
        )
    }

    @Test
    fun `a rotation that may have reached the server is finished, not sidestepped`() {
        assertEquals(
            PhraseSavePath.Rotate,
            phraseSavePath(rotationNeeded = false, holdsPhrase = true, rotationInFlight = true),
        )
    }

    @Test
    fun `a phrase that was never in a vault and is on this device keeps the keys`() {
        assertEquals(
            PhraseSavePath.KeepKeys,
            phraseSavePath(rotationNeeded = false, holdsPhrase = true, rotationInFlight = false),
        )
    }

    @Test
    fun `the action is marked as this device's own before the server hears it, and saved only after`() = runTest {
        val ports = FakePorts()
        assertEquals(PhraseSavedOutcome.Saved, savePhraseKeepingKeys(ports))
        assertEquals(listOf("own", "post:grant", "saved"), ports.steps)
    }

    @Test
    fun `nothing counts as saved when the server did not say so`() = runTest {
        val offline = FakePorts(refusal = AuthNetworkError("/account/phrase/saved", IOException("offline")))
        assertEquals(PhraseSavedOutcome.Failed, savePhraseKeepingKeys(offline))
        assertFalse("saved" in offline.steps)

        val refused = FakePorts(refusal = http(500, null))
        assertEquals(PhraseSavedOutcome.Failed, savePhraseKeepingKeys(refused))
        assertFalse("saved" in refused.steps)
    }

    @Test
    fun `a 409 sends the person to the rotation path`() = runTest {
        val ports = FakePorts(refusal = http(409, PHRASE_SAVE_NEEDS_ROTATION))
        assertEquals(PhraseSavedOutcome.NeedsRotation, savePhraseKeepingKeys(ports))
        assertFalse("saved" in ports.steps)
    }

    @Test
    fun `another 409 is not read as needing a rotation`() {
        assertEquals(PhraseSavedOutcome.Failed, phraseSavedOutcomeOf(http(409, "phrase-saved")))
    }

    @Test
    fun `a proof that ran out asks for it again`() = runTest {
        val refused = FakePorts(refusal = http(403, FRESH_AUTH_REQUIRED))
        assertEquals(PhraseSavedOutcome.ProofRequired, savePhraseKeepingKeys(refused))
        assertFalse("saved" in refused.steps)

        val noGrant = FakePorts(grant = null)
        assertEquals(PhraseSavedOutcome.ProofRequired, savePhraseKeepingKeys(noGrant))
        assertTrue("nothing is sent without a proof", noGrant.steps.isEmpty())
    }
}
