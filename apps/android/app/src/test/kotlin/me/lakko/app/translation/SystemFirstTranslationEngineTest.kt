// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.TranslationEngineState

/**
 * ADR-0082 notes of 2026-10-27 and 2026-11-15: per language pair, the system
 * service where it has the pack, ML Kit for every other pair.
 */
class SystemFirstTranslationEngineTest {
    private class Fake(
        val name: String,
        val states: Map<String, TranslationEngineState> = emptyMap(),
        val languages: List<String> = emptyList(),
        val packs: List<PackInfo> = emptyList(),
        val downloadOutcome: PackOutcome = PackOutcome.DONE,
        val hasAny: Boolean = states.values.any { it != TranslationEngineState.UNAVAILABLE },
    ) : TranslationEngine {
        val calls = mutableListOf<String>()
        var removed: String? = null

        override suspend fun available(): Boolean = hasAny

        override suspend fun state(source: String, target: String): TranslationEngineState =
            states["$source>$target"] ?: TranslationEngineState.UNAVAILABLE

        override suspend fun sources(target: String): List<String> = languages

        override suspend fun translate(text: String, source: String, target: String): String {
            calls += "translate $source>$target"
            return "$name:$text"
        }

        override suspend fun download(source: String, target: String, progress: (Int?) -> Unit): PackOutcome {
            calls += "download $source>$target"
            return downloadOutcome
        }

        override suspend fun installedPacks(): List<PackInfo> = packs

        override suspend fun removePack(language: String): Boolean {
            removed = language
            return true
        }
    }

    private val ready = TranslationEngineState.READY
    private val needsPack = TranslationEngineState.NEEDS_PACK

    private fun pack(lang: String, system: Boolean) =
        PackInfo(lang, null, forTranslation = true, forTranscription = false, managedBySystem = system)

    @Test
    fun `a pair the system has on the device is the system's`() = runTest {
        val system = Fake("system", mapOf("es>en" to ready))
        val mlkit = Fake("mlkit", mapOf("es>en" to ready))
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals(ready, engine.state("es", "en"))
        assertEquals("system:hola", engine.translate("hola", "es", "en"))
        assertEquals(PackOutcome.DONE, engine.download("es", "en") {})
        assertTrue(mlkit.calls.isEmpty())
    }

    @Test
    fun `a pair the system only offers to download goes to ML Kit`() = runTest {
        // Seen on a Pixel 9: the system said NEEDS_PACK for es>en and da>en
        // and never fetched the pack.
        val system = Fake("system", mapOf("es>en" to needsPack))
        val mlkit = Fake("mlkit", mapOf("es>en" to ready))
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals(ready, engine.state("es", "en"))
        assertEquals("mlkit:hola", engine.translate("hola", "es", "en"))
        assertTrue(system.calls.isEmpty())
    }

    @Test
    fun `the pack of such a pair is ML Kit's to download`() = runTest {
        val system = Fake("system", mapOf("da>en" to needsPack))
        val mlkit = Fake("mlkit", mapOf("da>en" to needsPack))
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals(needsPack, engine.state("da", "en"))
        assertEquals(PackOutcome.DONE, engine.download("da", "en") {})
        assertEquals(listOf("download da>en"), mlkit.calls)
        assertTrue(system.calls.isEmpty())
    }

    @Test
    fun `a pair the system lacks altogether goes to ML Kit`() = runTest {
        val system = Fake("system", mapOf("es>en" to ready))
        val mlkit = Fake("mlkit", mapOf("uk>en" to needsPack))
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals(needsPack, engine.state("uk", "en"))
    }

    @Test
    fun `with anti-censorship on a pair the system cannot supply is unavailable`() = runTest {
        // The real ML Kit engine answers nothing while anti-censorship is on.
        val system = Fake("system", mapOf("es>en" to needsPack, "de>en" to ready))
        val engine = SystemFirstTranslationEngine(system, MlKitTranslationEngine { true })
        assertEquals(TranslationEngineState.UNAVAILABLE, engine.state("es", "en"))
        assertEquals(PackOutcome.FAILED, engine.download("es", "en") {})
        // A pack already on the device keeps working.
        assertEquals(ready, engine.state("de", "en"))
    }

    @Test
    fun `below API 31 there is no system engine and ML Kit answers`() = runTest {
        val mlkit = Fake("mlkit", mapOf("es>en" to ready))
        val engine = SystemFirstTranslationEngine(null, mlkit)
        assertEquals(ready, engine.state("es", "en"))
    }

    @Test
    fun `the source list is ML Kit's and what the system has on the device`() = runTest {
        val system = Fake(
            "system",
            mapOf("de>en" to ready, "sw>en" to needsPack),
            languages = listOf("de", "sw"),
        )
        val mlkit = Fake("mlkit", mapOf("es>en" to needsPack), languages = listOf("es", "uk"))
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals(listOf("es", "uk", "de"), engine.sources("en"))
    }

    @Test
    fun `packs of both are listed and only Lakko's own are removed`() = runTest {
        val system = Fake("system", mapOf("de>en" to ready), packs = listOf(pack("de", system = true)))
        val mlkit = Fake("mlkit", mapOf("es>en" to ready), packs = listOf(pack("es", system = false)))
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals(listOf("de", "es"), engine.installedPacks().map { it.language })
        assertEquals(listOf(true, false), engine.installedPacks().map { it.managedBySystem })
        assertTrue(engine.removePack("es"))
        assertEquals("es", mlkit.removed)
        assertEquals(null, system.removed)
    }

    @Test
    fun `with anti-censorship on and no system service nothing is offered`() = runTest {
        val engine = SystemFirstTranslationEngine(Fake("system"), Fake("mlkit"))
        assertFalse(engine.available())
    }

    @Test
    fun `a scheduled pack download does not leave the line on Downloading`() {
        // Nothing watches a download the platform only scheduled.
        assertEquals(AfterPack.BACK_TO_START, afterPack(PackOutcome.SCHEDULED))
        assertEquals(AfterPack.CONTINUE, afterPack(PackOutcome.DONE))
        assertEquals(AfterPack.FAILED, afterPack(PackOutcome.FAILED))
    }
}
