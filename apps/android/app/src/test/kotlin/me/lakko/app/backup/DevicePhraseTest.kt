// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * Every device that holds the account's keys also holds the recovery
 * phrase: every landing path writes the phrase store. The store itself is
 * Keystore-backed and does not run on the host JVM, so the write is driven
 * through an injected writer, and the "every path calls it" rule is checked
 * against the repository's source, as the web client does.
 */
class DevicePhraseTest {

    @Test
    fun `a stored phrase reaches the writer as its bytes, which are then zeroed`() {
        var seen: ByteArray? = null
        var copy: String? = null
        val ok = DevicePhrase.storeWith(
            phrase = "abandon ability able",
            path = DevicePhrase.LandingPath.Signup,
            write = { bytes ->
                seen = bytes
                copy = String(bytes, Charsets.UTF_8)
            },
        )
        assertTrue(ok)
        assertEquals("abandon ability able", copy)
        assertTrue("the buffer handed to the store is wiped", seen!!.all { it == 0.toByte() })
    }

    @Test
    fun `a write that happened lifts the three-word suspension, a failed one does not`() {
        var lifted = 0
        assertTrue(
            DevicePhrase.storeWith(
                phrase = "abandon ability able",
                path = DevicePhrase.LandingPath.ProvidePhrase,
                write = { },
                onStored = { lifted += 1 },
            ),
        )
        assertEquals(1, lifted)
        assertFalse(
            DevicePhrase.storeWith(
                phrase = "abandon ability able",
                path = DevicePhrase.LandingPath.ProvidePhrase,
                write = { throw IllegalStateException("keystore gone") },
                onStored = { lifted += 1 },
                log = { _, _ -> },
            ),
        )
        assertEquals(1, lifted)
    }

    @Test
    fun `a failed write is logged with the path and never the phrase, and the landing continues`() {
        val logged = ArrayList<String>()
        val ok = DevicePhrase.storeWith(
            phrase = "secret words here",
            path = DevicePhrase.LandingPath.DeviceRestore,
            write = { throw IllegalStateException("keystore gone") },
            log = { message, _ -> logged += message },
        )
        assertFalse(ok)
        assertEquals(1, logged.size)
        assertTrue(logged[0].contains("device-restore"))
        assertFalse(logged[0].contains("secret"))
    }

    @Test
    fun `the entropy of the stored phrase is sixteen bytes or nothing`() {
        val entropy = ByteArray(PHRASE_ENTROPY_LENGTH) { 5 }
        assertArrayEquals(entropy, entropyOfStoredPhrase("twelve words") { entropy.copyOf() })
        // No stored phrase: no recovery row is published.
        assertNull(entropyOfStoredPhrase(null) { throw AssertionError("must not be asked") })
        // A phrase the core cannot parse.
        assertNull(entropyOfStoredPhrase("garbage") { throw IllegalArgumentException("not a phrase") })
        // Any other length is refused and wiped.
        val wrong = ByteArray(32) { 5 }
        assertNull(entropyOfStoredPhrase("old phrase") { wrong })
        assertTrue(wrong.all { it == 0.toByte() })
    }

    @Test
    fun `every landing path that gives the device keys writes the phrase store`() {
        val source = repositorySource()
        val called = Regex("""DevicePhrase\.LandingPath\.(\w+)""").findAll(source).map { it.groupValues[1] }.toSet()
        for (path in DevicePhrase.LandingPath.entries) {
            assertTrue("no DevicePhrase.store call names ${path.name}", path.name in called)
        }
        // And no call names a path that is not listed.
        val listed = DevicePhrase.LandingPath.entries.map { it.name }.toSet()
        assertEquals(emptySet<String>(), called - listed)
        // One store call per path.
        val stores = Regex("""DevicePhrase\s*\.\s*store\(""").findAll(source).count()
        assertEquals(DevicePhrase.LandingPath.entries.size, stores)
    }

    @Test
    fun `the landing paths are the seven ways a device comes to hold keys`() {
        assertEquals(
            listOf(
                "signup",
                "signup-guest",
                "recover",
                "provide-phrase",
                "device-pairing",
                "device-restore",
                "fresh-identity",
            ),
            DevicePhrase.LandingPath.entries.map { it.tag },
        )
    }

    private fun repositorySource(): String {
        val relative = "src/main/kotlin/me/lakko/app/auth/AuthRepository.kt"
        val file = sequenceOf(File(relative), File("app/$relative"), File("apps/android/app/$relative"))
            .firstOrNull { it.isFile }
            ?: error("AuthRepository.kt not found from ${File(".").absolutePath}")
        return file.readText()
    }
}
