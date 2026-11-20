// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.AuthHttpError
import me.lakko.app.auth.AuthNetworkError
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/**
 * Every passkey restores keys, with nothing to choose (UX identity-recovery
 * 10.2, amended 2026-11-11): what happens right after a passkey is created,
 * and what a row's "Restore keys with this passkey" says. The real store
 * rule ([turnOnPasskeyKeys]) runs over fake ports: the phrase, the second
 * prompt and the vault.
 */
class PasskeyAddKeysTest {

    private val prf = ByteArray(32) { 1 }
    private val seed = ByteArray(64) { 4 }

    private class Ports {
        var ceremonies = 0
        var stored: Pair<String, ByteArray>? = null
    }

    /** Create-then-store, as [addPasskeyWithKeys] runs it. */
    private suspend fun addAndStore(
        ports: Ports,
        creationPrf: ByteArray? = prf,
        prfEnabled: Boolean? = null,
        storedPhrase: String? = "phrase",
        ceremony: suspend () -> PasskeyCeremonyExtras = { PasskeyCeremonyExtras("new", prf) },
        sealAndStore: suspend () -> Boolean = { true },
    ): PasskeyKeysStoreEnd = storeKeysForAddedPasskey(prfOutput = creationPrf, prfEnabled = prfEnabled) { registrationPrf ->
        turnOnPasskeyKeys(
            credentialIdB64 = "new",
            storedPhrase = storedPhrase,
            typedPhrase = null,
            identitySeed = seed,
            registrationPrfOutput = registrationPrf,
            seedOf = { seed.copyOf() },
            evaluatePrf = {
                ports.ceremonies += 1
                ceremony()
            },
            sealAndStore = { phrase, output ->
                val ok = sealAndStore()
                ports.stored = phrase to output
                ok
            },
        )
    }

    @Test
    fun `the creation's secret stores the keys with no further prompt`() = runTest {
        val ports = Ports()
        val end = addAndStore(ports)
        assertEquals(PasskeyKeysStoreEnd.Stored, end)
        assertEquals(0, ports.ceremonies)
        assertEquals("phrase", ports.stored!!.first)
        assertArrayEquals(prf, ports.stored!!.second)
        assertEquals("settings.passkeys.added_restores", addedPasskeyText(end, "Pixel").key)
        assertFalse(addedPasskeyIsError(end))
    }

    @Test
    fun `without a secret from the creation one more prompt stores the keys`() = runTest {
        for (enabled in listOf(true, null)) {
            val ports = Ports()
            assertEquals(PasskeyKeysStoreEnd.Stored, addAndStore(ports, creationPrf = null, prfEnabled = enabled))
            assertEquals(1, ports.ceremonies)
        }
    }

    @Test
    fun `a provider that said it cannot evaluate PRF logs in only, with no prompt`() = runTest {
        val ports = Ports()
        val end = addAndStore(ports, creationPrf = null, prfEnabled = false)
        assertEquals(PasskeyKeysStoreEnd.NoPrf, end)
        assertEquals(0, ports.ceremonies)
        assertNull(ports.stored)
        assertEquals("settings.passkeys.added_no_keys", addedPasskeyText(end, "Pixel").key)
        assertFalse(addedPasskeyIsError(end))
    }

    @Test
    fun `a second prompt that gives no secret logs in only`() = runTest {
        val ports = Ports()
        val end = addAndStore(ports, creationPrf = null, ceremony = { PasskeyCeremonyExtras("new", null) })
        assertEquals(PasskeyKeysStoreEnd.NoPrf, end)
        assertNull(ports.stored)
    }

    @Test
    fun `no phrase on this device adds the passkey as log in only and asks nothing`() = runTest {
        val ports = Ports()
        val end = addAndStore(ports, creationPrf = null, storedPhrase = null)
        assertEquals(PasskeyKeysStoreEnd.PhraseNeeded, end)
        assertEquals(0, ports.ceremonies)
        assertNull(ports.stored)
        val text = addedPasskeyText(end, "Pixel")
        assertEquals("settings.passkeys.added", text.key)
        assertEquals("Pixel", text.texts["name"])
        assertFalse(addedPasskeyIsError(end))
    }

    @Test
    fun `a dismissed second prompt says the passkey logs in only`() = runTest {
        val ports = Ports()
        val end = addAndStore(ports, creationPrf = null, ceremony = { throw WebauthnUserCancelledError() })
        assertEquals(PasskeyKeysStoreEnd.Cancelled, end)
        assertNull(ports.stored)
        assertEquals("settings.passkeys.keys_choice.cancelled", addedPasskeyText(end, "Pixel").key)
    }

    @Test
    fun `a different passkey answering stores nothing`() = runTest {
        val ports = Ports()
        val end = addAndStore(ports, creationPrf = null, ceremony = { PasskeyCeremonyExtras("other", prf) })
        assertEquals(PasskeyKeysStoreEnd.OtherPasskey, end)
        assertNull(ports.stored)
        assertEquals("settings.passkeys.keys_choice.error.other_passkey", addedPasskeyText(end, "Pixel").key)
        assertTrue(addedPasskeyIsError(end))
    }

    @Test
    fun `a server that keeps no keys for passkeys is unavailable`() = runTest {
        val refused = addAndStore(Ports(), sealAndStore = { false })
        assertEquals(PasskeyKeysStoreEnd.Unavailable, refused)
        val thrown = addAndStore(Ports(), sealAndStore = { throw AuthHttpError(503, "/vault/passkey", "") })
        assertEquals(PasskeyKeysStoreEnd.Unavailable, thrown)
        assertEquals("settings.passkeys.keys_choice.unavailable", addedPasskeyText(thrown, "Pixel").key)
    }

    @Test
    fun `offline and other failures after creation say the passkey logs in only for now`() = runTest {
        val offline = addAndStore(
            Ports(),
            sealAndStore = { throw AuthNetworkError("/vault/passkey", IOException("down")) },
        )
        assertEquals(PasskeyKeysStoreEnd.Network, offline)
        assertEquals("settings.passkeys.added_keys_failed", addedPasskeyText(offline, "Pixel").key)
        assertEquals(
            PasskeyKeysStoreEnd.Network,
            addAndStore(Ports(), creationPrf = null, ceremony = { throw IOException("down") }),
        )
        var told: Throwable? = null
        val failed = storeKeysForAddedPasskey(prf, null, onFailure = { told = it }) {
            throw AuthHttpError(500, "/vault/passkey", "")
        }
        assertEquals(PasskeyKeysStoreEnd.Failed, failed)
        assertTrue(told is AuthHttpError)
        assertEquals("settings.passkeys.added_keys_failed", addedPasskeyText(failed, "Pixel").key)
        assertTrue(addedPasskeyIsError(offline))
        assertTrue(addedPasskeyIsError(failed))
        assertTrue(addedPasskeyIsError(PasskeyKeysStoreEnd.Unavailable))
    }

    @Test
    fun `cancelling the caller propagates`() {
        assertThrows(CancellationException::class.java) {
            kotlinx.coroutines.runBlocking {
                runPasskeyKeysStore { throw CancellationException("gone") }
            }
        }
    }

    // ---- the row ------------------------------------------------------------

    @Test
    fun `a row offers the repair only when it holds no keys and is not known to lack PRF`() {
        assertFalse(passkeyOffersRestoreKeys("a", withKeys = listOf("a"), knownNoPrf = emptySet()))
        assertTrue(passkeyOffersRestoreKeys("b", withKeys = listOf("a"), knownNoPrf = emptySet()))
        assertTrue(passkeyOffersRestoreKeys("b", withKeys = null, knownNoPrf = emptySet()))
        assertFalse(passkeyOffersRestoreKeys("b", withKeys = listOf("a"), knownNoPrf = setOf("b")))
    }

    @Test
    fun `the repair says what happened, and nothing when the phrase input opens or the sheet was dismissed`() {
        assertEquals("settings.passkeys.keys_choice.done", restoreKeysText(PasskeyKeysStoreEnd.Stored)?.key)
        assertEquals("settings.passkeys.keys_choice.no_prf", restoreKeysText(PasskeyKeysStoreEnd.NoPrf)?.key)
        assertNull(restoreKeysText(PasskeyKeysStoreEnd.PhraseNeeded))
        assertNull(restoreKeysText(PasskeyKeysStoreEnd.Cancelled))
        assertEquals(
            "settings.passkeys.keys_choice.error.phrase_mismatch",
            restoreKeysText(PasskeyKeysStoreEnd.PhraseMismatch)?.key,
        )
        assertEquals("restore_identity.error.invalid_phrase", restoreKeysText(PasskeyKeysStoreEnd.PhraseInvalid)?.key)
        // The row names "this passkey", not "the passkey you just added".
        assertEquals(
            "settings.passkeys.keys.error.other_passkey",
            restoreKeysText(PasskeyKeysStoreEnd.OtherPasskey)?.key,
        )
        assertEquals("settings.passkeys.error.network", restoreKeysText(PasskeyKeysStoreEnd.Network)?.key)
        assertEquals("settings.passkeys.error.generic", restoreKeysText(PasskeyKeysStoreEnd.Failed)?.key)
    }
}
