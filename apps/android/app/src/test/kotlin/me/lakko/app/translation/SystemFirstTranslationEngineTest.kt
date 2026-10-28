// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.TranslationEngineState

/** ADR-0082 amended 2026-10-27: the system service first, ML Kit only without one. */
class SystemFirstTranslationEngineTest {
    private class Fake(
        val name: String,
        var hasAny: Boolean,
        val packs: List<PackInfo> = emptyList(),
    ) : TranslationEngine {
        val calls = mutableListOf<String>()
        var removed: String? = null

        override suspend fun available(): Boolean = hasAny

        override suspend fun state(source: String, target: String): TranslationEngineState {
            calls += "state"
            return if (hasAny) TranslationEngineState.READY else TranslationEngineState.UNAVAILABLE
        }

        override suspend fun sources(target: String): List<String> = listOf(name)

        override suspend fun translate(text: String, source: String, target: String): String {
            calls += "translate"
            return "$name:$text"
        }

        override suspend fun download(source: String, target: String, progress: (Int?) -> Unit): PackOutcome =
            PackOutcome.DONE

        override suspend fun installedPacks(): List<PackInfo> = packs

        override suspend fun removePack(language: String): Boolean {
            removed = language
            return true
        }
    }

    private fun pack(lang: String, system: Boolean) =
        PackInfo(lang, null, forTranslation = true, forTranscription = false, managedBySystem = system)

    @Test
    fun `a device with a system service never reaches ML Kit`() = runTest {
        val system = Fake("system", hasAny = true)
        val mlkit = Fake("mlkit", hasAny = true)
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals("system:hola", engine.translate("hola", "es", "en"))
        assertEquals(listOf("system"), engine.sources("en"))
        assertTrue(mlkit.calls.isEmpty())
    }

    @Test
    fun `a device without a system service uses ML Kit`() = runTest {
        val system = Fake("system", hasAny = false)
        val mlkit = Fake("mlkit", hasAny = true)
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals("mlkit:hola", engine.translate("hola", "es", "en"))
        assertTrue(system.calls.isEmpty())
    }

    @Test
    fun `below API 31 there is no system engine and ML Kit answers`() = runTest {
        val mlkit = Fake("mlkit", hasAny = true)
        val engine = SystemFirstTranslationEngine(null, mlkit)
        assertEquals(TranslationEngineState.READY, engine.state("es", "en"))
    }

    @Test
    fun `packs of both are listed and only Lakko's own are removed`() = runTest {
        val system = Fake("system", hasAny = true, packs = listOf(pack("de", system = true)))
        val mlkit = Fake("mlkit", hasAny = true, packs = listOf(pack("es", system = false)))
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertEquals(listOf("de", "es"), engine.installedPacks().map { it.language })
        assertTrue(engine.removePack("es"))
        assertEquals("es", mlkit.removed)
        assertEquals(null, system.removed)
    }

    @Test
    fun `with anti-censorship on and no system service nothing is offered`() = runTest {
        val system = Fake("system", hasAny = false)
        val mlkit = Fake("mlkit", hasAny = false)
        val engine = SystemFirstTranslationEngine(system, mlkit)
        assertFalse(engine.available())
    }
}
