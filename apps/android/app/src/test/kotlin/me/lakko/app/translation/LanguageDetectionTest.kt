// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.DetectedLanguage
import uniffi.lakko_ffi_uniffi.LanguageCandidates
import uniffi.lakko_ffi_uniffi.TranslateOffer
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import uniffi.lakko_ffi_uniffi.TranslationSourceResolution

/**
 * ADR-0082 note of 2026-11-15 on Android: the language of a message comes from
 * the shared core. The host JVM has no native library, so the core detector is
 * replaced by a fake port and the shared rules' confidence line (700) is
 * passed in, as `TranslationCenter` passes `translationDetectionConfidentPermille()`.
 */
class LanguageDetectionTest {
    private val confident = 700

    private fun lang(tag: String, permille: Int) = DetectedLanguage(tag, permille.toUShort())

    /** A detector port answering from a table, counting the calls. */
    private class FakeDetector(val answers: Map<String, DetectedLanguage?>) : LanguageDetector {
        val calls = mutableListOf<String>()

        override suspend fun detect(text: String): DetectedLanguage? {
            calls += text
            return answers[text]
        }
    }

    // ----- detection drives the offer and the bar ----------------------------

    @Test
    fun `a detected message feeds its language to the offer rule`() = runTest {
        val detections = MessageDetections()
        val detector = FakeDetector(mapOf("Hoe gaat het met je vandaag" to lang("nl", 930)))
        detections.detect("c/7", "Hoe gaat het met je vandaag", detector)

        val known = SourceRules.known(detections.detections.value["c/7"]?.detected, chosen = null, confident)
        val input = SourceRules.offerInput(
            enabled = true,
            text = "Hoe gaat het met je vandaag",
            known = known,
            engineForKnown = TranslationEngineState.READY,
            anyEngine = true,
        )
        assertEquals(lang("nl", 930), input.detected)
        assertEquals(TranslationEngineState.READY, input.engineForDetected)
        assertTrue(input.hasText)
    }

    @Test
    fun `without a detection the offer rule is told nothing was found`() {
        val input = SourceRules.offerInput(
            enabled = true,
            text = "ok",
            known = SourceRules.known(null, null, confident),
            engineForKnown = null,
            anyEngine = true,
        )
        assertNull(input.detected)
        assertEquals(TranslationEngineState.UNAVAILABLE, input.engineForDetected)
    }

    @Test
    fun `detections of recent messages are what the conversation bar reads`() = runTest {
        val detections = MessageDetections()
        val detector = FakeDetector(
            mapOf("one" to lang("es", 900), "two" to lang("es", 880), "three" to null),
        )
        detections.detect("c/1", "one", detector)
        detections.detect("c/2", "two", detector)
        detections.detect("c/3", "three", detector)
        val recent = listOf("c/1", "c/2", "c/3").mapNotNull { detections.detections.value[it]?.detected }
        assertEquals(listOf(lang("es", 900), lang("es", 880)), recent)
    }

    // ----- one detection per message version ---------------------------------

    @Test
    fun `a message is detected once per version, again after an edit`() = runTest {
        val detections = MessageDetections()
        val detector = FakeDetector(mapOf("hallo daar vriend" to lang("nl", 900), "hej med dig min ven" to lang("da", 900)))
        detections.detect("c/5", "hallo daar vriend", detector)
        detections.detect("c/5", "hallo daar vriend", detector)
        assertEquals(listOf("hallo daar vriend"), detector.calls)

        detections.detect("c/5", "hej med dig min ven", detector)
        assertEquals(2, detector.calls.size)
        assertEquals(lang("da", 900), detections.detections.value["c/5"]?.detected)
    }

    @Test
    fun `an edit to text the detector cannot read drops the old language`() = runTest {
        val detections = MessageDetections()
        val detector = FakeDetector(mapOf("hallo daar vriend" to lang("nl", 900), "123" to null))
        detections.detect("c/5", "hallo daar vriend", detector)
        detections.detect("c/5", "123", detector)
        assertNull(detections.detections.value["c/5"])
    }

    @Test
    fun `a late answer for an older version is dropped`() = runTest {
        val detections = MessageDetections()
        val gate = CompletableDeferred<Unit>()
        val slow = object : LanguageDetector {
            override suspend fun detect(text: String): DetectedLanguage? {
                if (text == "old") gate.await()
                return if (text == "old") lang("nl", 900) else lang("de", 900)
            }
        }
        val first = launch { detections.detect("c/9", "old", slow) }
        testScheduler.runCurrent()
        detections.detect("c/9", "new", slow)
        gate.complete(Unit)
        first.join()
        assertEquals(lang("de", 900), detections.detections.value["c/9"]?.detected)
    }

    @Test
    fun `blank text is never sent to the detector`() = runTest {
        val detections = MessageDetections()
        val detector = FakeDetector(emptyMap())
        detections.detect("c/1", "   ", detector)
        assertTrue(detector.calls.isEmpty())
    }

    // ----- the tap: translate from any guess, the picker only without one ----

    @Test
    fun `a confident detection is the source`() {
        assertEquals(lang("nl", 930), SourceRules.known(lang("nl", 930), chosen = null, confident))
        // and it beats a language chosen earlier in the conversation
        assertEquals("nl", SourceRules.known(lang("nl", 930), chosen = "es", confident)?.tag)
    }

    @Test
    fun `an unsure guess is translated from at once`() {
        val known = SourceRules.known(lang("nl", 410), chosen = null, confident)
        assertEquals("nl", known?.tag)
    }

    @Test
    fun `an unsure Chinese guess is still a source`() {
        // The core never reports `zh` as confident; a tap still translates from it.
        assertEquals("zh", SourceRules.known(lang("zh", 600), chosen = null, confident)?.tag)
    }

    @Test
    fun `an unsure guess beats the language picked for the conversation`() {
        // Found live 2026-11-15: a remembered Dutch pick read a Ukrainian
        // message (uk at 670, under the confident line) from Dutch.
        val known = SourceRules.known(lang("uk", 670), chosen = "nl", confident)
        assertEquals("uk", known?.tag)
    }

    @Test
    fun `an unusable guess yields to the language picked for the conversation`() {
        // The guess is the language translated into: the conversation's
        // language is used, not the picker.
        val known = SourceRules.known(lang("en", 300), chosen = "nl", confident, guessUnusable = true)
        assertEquals("nl", known?.tag)
    }

    @Test
    fun `an unusable guess with no conversation pick stays marked for the picker`() {
        val known = SourceRules.known(lang("en", 300), chosen = null, confident, guessUnusable = true)
        assertEquals("en", known?.tag)
    }

    @Test
    fun `with no guess and no choice the tap opens the picker`() {
        assertNull(SourceRules.known(null, chosen = null, confident))
    }

    @Test
    fun `with no guess the conversation's picked language is used`() {
        assertEquals("es", SourceRules.known(null, chosen = "es", confident)?.tag)
        assertEquals("nl", SourceRules.known(null, chosen = "nl", confident)?.tag)
    }

    @Test
    fun `the core detector never fails its caller`() = runTest {
        // Host JVM: the library is absent; the detector says "could not tell"
        // instead of failing the caller.
        val answer = CoreLanguageDetector.detect("Hoe gaat het met je vandaag")
        assertTrue(answer == null || answer.tag == "nl")
    }

    // ----- the language line up front (UX 3.2) ---------------------------------

    @Test
    fun `the language line asks the menu row's rule with the engine set to can translate`() {
        val input = SourceRules.languageLineInput(enabled = true, text = "hola", detected = lang("es", 410))
        assertEquals(TranslationEngineState.READY, input.engineForDetected)
        assertTrue(input.anyEngine)
        assertEquals(lang("es", 410), input.detected)
        assertTrue(input.hasText)
        // Empty text and the setting are the rule's to refuse.
        assertFalse(SourceRules.languageLineInput(true, "  ", detected = lang("es", 900)).hasText)
        assertFalse(SourceRules.languageLineInput(false, "hola", detected = lang("es", 900)).enabled)
    }

    private fun resolution(
        line: String?,
        canTranslate: Boolean,
        source: String? = line,
        guess: String? = null,
        permille: Int = 200,
    ) = TranslationSourceResolution(line, permille.toUShort(), canTranslate, source, guess)

    @Test
    fun `the language line is not shown when the rule offers nothing`() {
        // Own, no text, setting off, a reading language or an unsure guess
        // that is one: the shared offer rule answers NONE.
        assertNull(SourceRules.languageLine(TranslateOffer.NONE, resolution("en", true)))
        // The source rule named no language: no line.
        assertNull(SourceRules.languageLine(TranslateOffer.TRANSLATE, resolution(null, false, guess = "tk")))
        assertNull(SourceRules.languageLine(TranslateOffer.TRANSLATE, null))
    }

    @Test
    fun `the language line names what the source rule names`() {
        assertEquals(
            TranslateLine.Detected("es", canTranslate = true),
            SourceRules.languageLine(TranslateOffer.TRANSLATE, resolution("es", true)),
        )
        // A confident language the device cannot translate from: the name alone.
        assertEquals(
            TranslateLine.Detected("tk", canTranslate = false),
            SourceRules.languageLine(TranslateOffer.TRANSLATE_ASK_LANGUAGE, resolution("tk", false, permille = 950)),
        )
    }

    @Test
    fun `the hola case shows no line and translates from Spanish`() {
        // Seen on a Pixel: "hola, qué tal?" an unsure Catalan with es fr it
        // close, no Catalan on the device. The engine answers become the
        // rule's translatable list; the rule itself (resolveTranslationSource)
        // is asserted in Rust with this case, since the host JVM has no core.
        val states = mapOf(
            "ca" to TranslationEngineState.UNAVAILABLE,
            "es" to TranslationEngineState.READY,
            "fr" to TranslationEngineState.NEEDS_PACK,
            "it" to TranslationEngineState.READY,
        )
        assertEquals(listOf("es", "fr", "it"), SourceRules.translatable(listOf("ca", "es", "fr", "it")) { states[it] })
        // The rule's answer for that input since walk 7 (2026-11-20): an
        // unsure guess names nothing up front; Translate still starts from
        // Spanish.
        val r = resolution(null, false, source = "es", permille = 180)
        assertNull(SourceRules.languageLine(TranslateOffer.TRANSLATE_ASK_LANGUAGE, r))
        assertEquals(TapAction.Translate("es"), SourceRules.tapSource(r))
    }

    @Test
    fun `no source opens the picker with the guess marked`() {
        assertEquals(TapAction.Picker("tk"), SourceRules.tapSource(resolution(null, false, source = null, guess = "tk")))
    }

    @Test
    fun `a detection carries its close candidates and a plain detector none`() = runTest {
        val near = LanguageCandidates(lang("ca", 180), listOf("es", "fr"))
        val ranked = object : LanguageDetector {
            override suspend fun detect(text: String): DetectedLanguage? = near.detected

            override suspend fun detectCandidates(text: String): LanguageCandidates? = near
        }
        val detections = MessageDetections()
        detections.detect("c/1", "hola, qué tal?", ranked)
        assertEquals(near, detections.detections.value["c/1"])
        // Seen on a Pixel: Catalan first, then the Spanish the tap uses.
        assertEquals(listOf("ca", "es", "fr"), SourceRules.candidateTags(detections.detections.value["c/1"]))
        // A detector with no ranked list: the detection alone.
        val plain = object : LanguageDetector {
            override suspend fun detect(text: String): DetectedLanguage? = lang("tk", 200)
        }
        assertEquals(LanguageCandidates(lang("tk", 200), emptyList()), plain.detectCandidates("goedendag"))
        assertEquals(emptyList<String>(), SourceRules.candidateTags(null))
    }

    @Test
    fun `no language line until the engine has answered`() {
        val states = mapOf("es" to TranslationEngineState.READY)
        assertNull(SourceRules.translatable(listOf("ca", "es")) { states[it] })
    }

    @Test
    fun `a shown translation's line replaces the language line`() {
        val upFront = TranslateLine.Detected("es", true)
        assertEquals(upFront, me.lakko.app.ui.chat.bubbleCaptionLine("hola", null, upFront))
        val shown = TranslateLine.Shown("es", "hello")
        assertEquals(shown, me.lakko.app.ui.chat.bubbleCaptionLine("hola", shown, upFront))
        assertNull(me.lakko.app.ui.chat.bubbleCaptionLine("", null, upFront))
    }

    // ----- tags as the engines name them -------------------------------------

    @Test
    fun `engine tags map onto the core's Norwegian and Tagalog`() {
        assertEquals("nb", PlatformLanguageTags.fromPlatform("no"))
        assertEquals("nb-NO", PlatformLanguageTags.fromPlatform("no-NO"))
        assertEquals("nb", PlatformLanguageTags.fromPlatform("NO"))
        assertEquals("tl", PlatformLanguageTags.fromPlatform("fil"))
        assertEquals("tl-PH", PlatformLanguageTags.fromPlatform("fil-PH"))
        assertEquals("tl", PlatformLanguageTags.fromPlatform("tl"))
        assertEquals("nb", PlatformLanguageTags.fromPlatform("nb"))
        assertEquals("nl", PlatformLanguageTags.fromPlatform("nl"))
        assertEquals("pt-BR", PlatformLanguageTags.fromPlatform("pt-BR"))
        assertEquals("nn", PlatformLanguageTags.fromPlatform("nn"))
    }

    @Test
    fun `core tags map onto ML Kit's codes`() {
        assertEquals("no", PlatformLanguageTags.toMlKit("nb"))
        assertEquals("no", PlatformLanguageTags.toMlKit("nb-NO"))
        assertEquals("tl", PlatformLanguageTags.toMlKit("tl"))
        assertEquals("tl", PlatformLanguageTags.toMlKit("fil"))
        assertEquals("nl", PlatformLanguageTags.toMlKit("nl"))
        assertEquals("zh", PlatformLanguageTags.toMlKit("zh-Hans"))
        assertEquals("fa", PlatformLanguageTags.toMlKit("fa"))
    }
}
