// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/**
 * What a passkey does about the account's keys: asked for them on a device
 * that has none, and given them when the person turns that on. The vault,
 * the core and the ceremony are fakes.
 */
class PasskeyKeysTest {

    private val prf = ByteArray(32) { 1 }
    private val blob = ByteArray(44) { 2 }

    // ---- asking a passkey for the keys ------------------------------------

    @Test
    fun `no prf output means no keys and the vault is not asked`() = runTest {
        var asked = false
        val outcome = keysFromPasskey(
            prfOutput = null,
            fetchBlob = {
                asked = true
                blob
            },
            open = { _, _ -> "phrase" },
            providePhrase = { throw AssertionError("nothing to provide") },
        )
        assertEquals(PasskeyKeysOutcome.NoKeys, outcome)
        assertFalse(asked)
    }

    @Test
    fun `nothing stored for the passkey means no keys`() = runTest {
        val outcome = keysFromPasskey(
            prfOutput = prf,
            fetchBlob = { null },
            open = { _, _ -> throw AssertionError("nothing to open") },
            providePhrase = { throw AssertionError("nothing to provide") },
        )
        assertEquals(PasskeyKeysOutcome.NoKeys, outcome)
    }

    @Test
    fun `a blob that does not open is unusable and nothing is provided`() = runTest {
        var provided = false
        val outcome = keysFromPasskey(
            prfOutput = prf,
            fetchBlob = { blob },
            open = { _, _ -> throw PasskeyVaultUnusable() },
            providePhrase = { provided = true },
        )
        assertEquals(PasskeyKeysOutcome.Unusable, outcome)
        assertFalse(provided)
    }

    @Test
    fun `a phrase that is not the account's is unusable`() = runTest {
        val outcome = keysFromPasskey(
            prfOutput = prf,
            fetchBlob = { blob },
            open = { _, _ -> "someone else's phrase" },
            providePhrase = { throw PasskeyPhraseNotThisAccount() },
        )
        assertEquals(PasskeyKeysOutcome.Unusable, outcome)
    }

    @Test
    fun `a blob that opens into the account's phrase restores the keys`() = runTest {
        val seen = ArrayList<String>()
        val outcome = keysFromPasskey(
            prfOutput = prf,
            fetchBlob = { blob },
            open = { output, sealed ->
                assertArrayEquals(prf, output)
                assertArrayEquals(blob, sealed)
                "the phrase"
            },
            providePhrase = { seen += it },
        )
        assertEquals(PasskeyKeysOutcome.Restored, outcome)
        assertEquals(listOf("the phrase"), seen)
    }

    @Test
    fun `an unreachable vault or directory is neither no keys nor unusable`() {
        assertThrows(IOException::class.java) {
            kotlinx.coroutines.runBlocking {
                keysFromPasskey(
                    prfOutput = prf,
                    fetchBlob = { throw IOException("offline") },
                    open = { _, _ -> "p" },
                    providePhrase = {},
                )
            }
        }
        assertThrows(IllegalStateException::class.java) {
            kotlinx.coroutines.runBlocking {
                keysFromPasskey(
                    prfOutput = prf,
                    fetchBlob = { blob },
                    open = { _, _ -> "p" },
                    providePhrase = { throw IllegalStateException("directory unreachable") },
                )
            }
        }
    }

    // ---- turning it on -----------------------------------------------------

    private val identitySeed = ByteArray(64) { 4 }

    private class Calls {
        var ceremonies = 0
        var stored: Pair<String, ByteArray>? = null
    }

    private suspend fun turnOn(
        calls: Calls,
        storedPhrase: String? = "stored phrase",
        typedPhrase: String? = null,
        registrationPrf: ByteArray? = prf,
        seedOf: (String) -> ByteArray = { identitySeed.copyOf() },
        ceremony: PasskeyCeremonyExtras = PasskeyCeremonyExtras("cred", prf),
        serverStores: Boolean = true,
    ): PasskeyKeysChoiceResult = turnOnPasskeyKeys(
        credentialIdB64 = "cred",
        storedPhrase = storedPhrase,
        typedPhrase = typedPhrase,
        identitySeed = identitySeed,
        registrationPrfOutput = registrationPrf,
        seedOf = seedOf,
        evaluatePrf = {
            calls.ceremonies += 1
            ceremony
        },
        sealAndStore = { phrase, output ->
            calls.stored = phrase to output
            serverStores
        },
    )

    @Test
    fun `the stored phrase and the registration's secret are sealed with no further prompt`() = runTest {
        val calls = Calls()
        assertEquals(PasskeyKeysChoiceResult.Stored, turnOn(calls))
        assertEquals(0, calls.ceremonies)
        assertEquals("stored phrase", calls.stored!!.first)
        assertArrayEquals(prf, calls.stored!!.second)
    }

    @Test
    fun `a device without the phrase asks for it and stores nothing`() = runTest {
        val calls = Calls()
        assertEquals(PasskeyKeysChoiceResult.PhraseNeeded, turnOn(calls, storedPhrase = null))
        assertEquals(PasskeyKeysChoiceResult.PhraseNeeded, turnOn(calls, storedPhrase = null, typedPhrase = "  "))
        assertEquals(null, calls.stored)
        assertEquals(0, calls.ceremonies)
    }

    @Test
    fun `a typed phrase must parse and give this device's identity seed`() = runTest {
        val calls = Calls()
        assertEquals(
            PasskeyKeysChoiceResult.PhraseInvalid,
            turnOn(
                calls,
                storedPhrase = null,
                typedPhrase = "nonsense",
                seedOf = { throw IllegalArgumentException("bad") },
            ),
        )
        assertEquals(
            PasskeyKeysChoiceResult.PhraseMismatch,
            turnOn(
                calls,
                storedPhrase = null,
                typedPhrase = "another account",
                seedOf = { ByteArray(64) { 9 } },
            ),
        )
        assertEquals(null, calls.stored)
        assertEquals(
            PasskeyKeysChoiceResult.Stored,
            turnOn(calls, storedPhrase = null, typedPhrase = " typed phrase "),
        )
        assertEquals("typed phrase", calls.stored!!.first)
    }

    @Test
    fun `without a secret from the registration the passkey is asked once more`() = runTest {
        val calls = Calls()
        assertEquals(PasskeyKeysChoiceResult.Stored, turnOn(calls, registrationPrf = null))
        assertEquals(1, calls.ceremonies)
    }

    @Test
    fun `another passkey answering is refused and nothing is stored`() = runTest {
        val calls = Calls()
        val result = turnOn(calls, registrationPrf = null, ceremony = PasskeyCeremonyExtras("other", prf))
        assertEquals(PasskeyKeysChoiceResult.OtherPasskey, result)
        assertEquals(null, calls.stored)
    }

    @Test
    fun `a passkey that gives no secret signs in only`() = runTest {
        val calls = Calls()
        val result = turnOn(calls, registrationPrf = null, ceremony = PasskeyCeremonyExtras("cred", null))
        assertEquals(PasskeyKeysChoiceResult.NoPrf, result)
        assertEquals(null, calls.stored)
    }

    @Test
    fun `a server that keeps no keys for passkeys is unavailable`() = runTest {
        val calls = Calls()
        assertEquals(PasskeyKeysChoiceResult.Unavailable, turnOn(calls, serverStores = false))
        assertTrue(calls.stored != null)
    }
}
