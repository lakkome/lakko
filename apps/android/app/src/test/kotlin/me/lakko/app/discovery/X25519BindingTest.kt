// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import java.io.File
import java.util.Base64
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Security review 2026-11-21 — nothing is sealed to an account X25519 key the
 * directory serves unless the account's signature key signed it
 * (`verifyX25519Binding`), and every identity upload carries the binding.
 *
 * The host JVM has no native core, so the verifier is a fake here; the core's
 * own tests pin the real signature.
 */
class X25519BindingTest {
    private val enc = Base64.getEncoder()
    private val ed = ByteArray(32) { 1 }
    private val x = ByteArray(32) { 2 }
    private val binding = ByteArray(64) { 3 }

    private fun bundle(bindingB64: String?) = IdentityFetchResponse(
        signature_pubkey = enc.encodeToString(ed),
        x25519_pubkey = enc.encodeToString(x),
        x25519_binding = bindingB64,
    )

    @Test
    fun `a binding that verifies yields the served X25519 key, checked over the exact bytes`() {
        var seen: Triple<ByteArray, ByteArray, ByteArray>? = null
        val key = bundle(enc.encodeToString(binding)).boundX25519Key { b, e, k ->
            seen = Triple(b, e, k)
            true
        }
        assertArrayEquals(x, key)
        assertArrayEquals(binding, seen!!.first)
        assertArrayEquals(ed, seen!!.second)
        assertArrayEquals(x, seen!!.third)
    }

    @Test
    fun `an absent, empty or unverified binding yields nothing to seal to`() {
        var calls = 0
        val yes = X25519BindingVerifier { _, _, _ -> calls++; true }
        assertNull("absent", bundle(null).boundX25519Key(yes))
        assertNull("empty", bundle("").boundX25519Key(yes))
        assertEquals("never asked without a binding", 0, calls)
        assertNull("does not verify", bundle(enc.encodeToString(binding)).boundX25519Key { _, _, _ -> false })
        assertNull("the verifier fails", bundle(enc.encodeToString(binding)).boundX25519Key { _, _, _ -> error("native") })
    }

    @Test
    fun `malformed bytes or wrong lengths are refused before the verifier`() {
        var calls = 0
        val yes = X25519BindingVerifier { _, _, _ -> calls++; true }
        assertNull(bundle("not base64!").boundX25519Key(yes))
        assertNull(bundle(enc.encodeToString(ByteArray(63))).boundX25519Key(yes))
        assertNull(
            X25519Bindings.boundX25519Key(enc.encodeToString(ByteArray(31)), enc.encodeToString(x), enc.encodeToString(binding), yes),
        )
        assertNull(
            X25519Bindings.boundX25519Key(enc.encodeToString(ed), enc.encodeToString(ByteArray(33)), enc.encodeToString(binding), yes),
        )
        assertEquals(0, calls)
        assertEquals(64, X25519Bindings.BINDING_LEN)
    }

    // ---- where the rule applies --------------------------------------------

    private fun source(path: String) = File("src/main/kotlin/me/lakko/app/$path").readText()

    @Test
    fun `no seal site reads a fetched X25519 key without its binding`() {
        // Every file that turns a directory bundle into an X25519 key to seal
        // to goes through the binding check.
        val sites = listOf(
            "contacts/ContactsClient.kt",
            "chat/ChatRepository.kt",
            "search/PeopleSearchService.kt",
            "channels/ChannelAdmissionSeedShare.kt",
        )
        for (site in sites) {
            val text = source(site)
            assertFalse("$site decodes x25519_pubkey directly", Regex("""[Dd]ecode\([^)]*\.x25519_pubkey""").containsMatchIn(text))
            assertTrue("$site checks the binding", text.contains("boundX25519Key"))
        }
        // The other readers of a fetched bundle only compare signature keys.
        val all = File("src/main/kotlin/me/lakko/app").walkTopDown().filter { it.extension == "kt" }
        for (file in all) {
            val text = file.readText()
            val reads = Regex("""\b\w+\.x25519_pubkey\b""").findAll(text).map { it.value }.toList()
            for (read in reads) {
                if (file.name == "X25519Binding.kt" || file.name == "Wire.kt") continue
                // The own publish check compares this device's own key.
                if (file.name == "AuthRepository.kt") continue
                assertTrue(
                    "${file.name} reads $read outside the binding check",
                    text.contains("boundX25519Key") && text.contains("x25519_binding"),
                )
            }
        }
    }

    @Test
    fun `every identity upload signs the binding with the identity it publishes`() {
        val auth = source("auth/AuthRepository.kt")
        val uploads = Regex("""discovery\.uploadIdentityBundle\(""").findAll(auth).count()
        val bindings = Regex("""x25519Binding = uniffi\.lakko_ffi_uniffi\.signX25519Binding\(identity\)""").findAll(auth).count()
        assertTrue(uploads > 0)
        assertEquals(uploads, bindings)
    }
}
