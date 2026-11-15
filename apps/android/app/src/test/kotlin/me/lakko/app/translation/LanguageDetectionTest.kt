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
import uniffi.lakko_ffi_uniffi.TranslationEngineState

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

        val known = SourceRules.known(detections.detections.value["c/7"], chosen = null, confident)
        val input = SourceRules.offerInput(
            enabled = true,
            text = "Hoe gaat het met je vandaag",
            isOwn = false,
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
            isOwn = false,
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
        val recent = listOf("c/1", "c/2", "c/3").mapNotNull { detections.detections.value[it] }
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
        assertEquals(lang("da", 900), detections.detections.value["c/5"])
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
        assertEquals(lang("de", 900), detections.detections.value["c/9"])
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
    fun `a language picked for the conversation beats an unsure guess`() {
        val known = SourceRules.known(lang("nl", 410), chosen = "af", confident)
        assertEquals("af", known?.tag)
    }

    @Test
    fun `with no guess and no choice the tap opens the picker`() {
        assertNull(SourceRules.known(null, chosen = null, confident))
    }

    @Test
    fun `with no guess the conversation's picked language is used`() {
        assertEquals("es", SourceRules.known(null, chosen = "es", confident)?.tag)
    }

    @Test
    fun `the core detector never fails its caller`() = runTest {
        // Host JVM: the library is absent; the detector says "could not tell"
        // instead of failing the caller.
        val answer = CoreLanguageDetector.detect("Hoe gaat het met je vandaag")
        assertTrue(answer == null || answer.tag == "nl")
    }

    // ----- an unusable guess opens the picker with the guess marked ----------

    @Test
    fun `only an unsure detection with no conversation pick is a guess`() {
        assertTrue(SourceRules.isUnsureGuess(lang("nl", 410), chosen = null, confident))
        assertFalse(SourceRules.isUnsureGuess(lang("nl", 930), chosen = null, confident))
        assertFalse(SourceRules.isUnsureGuess(lang("nl", 410), chosen = "af", confident))
        assertFalse(SourceRules.isUnsureGuess(null, chosen = null, confident))
    }

    @Test
    fun `a guess the engine can translate from is translated from`() {
        assertEquals(TapAction.Translate("nl"), SourceRules.tap("nl", isGuess = true, sameAsTarget = false, engine = null))
        assertEquals(
            TapAction.Translate("nl"),
            SourceRules.tap("nl", isGuess = true, sameAsTarget = false, engine = TranslationEngineState.READY),
        )
        assertEquals(
            TapAction.Translate("nl"),
            SourceRules.tap("nl", isGuess = true, sameAsTarget = false, engine = TranslationEngineState.NEEDS_PACK),
        )
    }

    @Test
    fun `a guess that is the target language opens the picker with the guess marked`() {
        assertEquals(TapAction.Picker("en"), SourceRules.tap("en", isGuess = true, sameAsTarget = true, engine = null))
    }

    @Test
    fun `a guess the engine cannot translate from opens the picker with the guess marked`() {
        assertEquals(
            TapAction.Picker("tl"),
            SourceRules.tap("tl", isGuess = true, sameAsTarget = false, engine = TranslationEngineState.UNAVAILABLE),
        )
    }

    @Test
    fun `a confident or picked source keeps its lines`() {
        // Not a guess: "already in English" and "Can't translate from" stay.
        assertEquals(TapAction.AlreadyIn("en"), SourceRules.tap("en", isGuess = false, sameAsTarget = true, engine = null))
        assertEquals(
            TapAction.Translate("tl"),
            SourceRules.tap("tl", isGuess = false, sameAsTarget = false, engine = TranslationEngineState.UNAVAILABLE),
        )
    }

    @Test
    fun `no source opens the picker with nothing marked`() {
        assertEquals(TapAction.Picker(null), SourceRules.tap(null, isGuess = false, sameAsTarget = false, engine = null))
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
