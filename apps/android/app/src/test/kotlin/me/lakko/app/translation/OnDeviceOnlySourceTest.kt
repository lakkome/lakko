// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0082 section 2 — the fail-closed test of the Android client.
 *
 * Translation and transcription happen on this device or not at all. The
 * Android API makes the remote engine the easy one: `createSpeechRecognizer`
 * returns the default recognition service, which its own documentation says
 * "is likely to stream audio to remote servers". So this test refuses that
 * call anywhere in the app's sources, and pins that the transcription engine
 * asks `isOnDeviceRecognitionAvailable` and builds its recognizer with
 * `createOnDeviceSpeechRecognizer`. It also refuses the deprecated
 * `TranslationManager.createTranslator` (only `createOnDeviceTranslator` is
 * allowed), any ML Kit natural-language SDK, and any engine reached from the
 * push / background decrypt path (section 7).
 *
 * Source reading, like `NoSystemShareSheetTest`: no host test can observe
 * which recognition service the platform would bind.
 */
class OnDeviceOnlySourceTest {
    private val root: File by lazy {
        listOf(File("src/main/kotlin"), File("app/src/main/kotlin")).first { it.isDirectory }
    }

    private val sources: List<File> by lazy {
        root.walkTopDown().filter { it.isFile && it.extension == "kt" }.toList()
    }

    /** The lines of [file] that are code, not commentary. */
    private fun codeLines(file: File): List<String> =
        file.readLines().filterNot { line ->
            val trimmed = line.trim()
            trimmed.startsWith("//") || trimmed.startsWith("*") || trimmed.startsWith("/*")
        }

    private fun offenders(predicate: (String) -> Boolean): List<String> =
        sources.filter { f -> codeLines(f).any(predicate) }.map { it.path }

    @Test
    fun `no source asks for the default speech recognizer`() {
        // `createOnDeviceSpeechRecognizer(` contains `SpeechRecognizer(` but not
        // `createSpeechRecognizer(`; the word boundary is the `create` prefix.
        val bad = Regex("""(?<![A-Za-z])createSpeechRecognizer\s*\(""")
        assertEquals(emptyList<String>(), offenders { bad.containsMatchIn(it) })
    }

    @Test
    fun `no source asks for a translator that is not the on-device one`() {
        val bad = Regex("""\.createTranslator\s*\(""")
        assertEquals(emptyList<String>(), offenders { bad.containsMatchIn(it) })
    }

    @Test
    fun `ML Kit Translate is reached only through the fallback engine, and language-id never`() {
        // ADR-0082 amended 2026-10-27: ML Kit Translate is the fallback for a
        // device with no system translation service, and only that file may
        // touch it. ML Kit Language Identification reports the identified
        // languages and is not linked at all.
        val translateUsers = offenders { "com.google.mlkit.nl.translate" in it }
        assertEquals(listOf("MlKitTranslationEngine.kt"), translateUsers.map { File(it).name })
        assertEquals(emptyList<String>(), offenders { "com.google.mlkit.nl.languageid" in it })
        val buildFiles = listOf(File("build.gradle.kts"), File("app/build.gradle.kts"), File("../gradle/libs.versions.toml"), File("gradle/libs.versions.toml"))
            .filter { it.isFile }
        assertTrue(buildFiles.isNotEmpty())
        for (f in buildFiles) {
            val text = f.readText()
            assertTrue(f.path, "mlkit:language-id" !in text && "name = \"language-id\"" !in text)
        }
    }

    @Test
    fun `no source reaches the platform text classifier's language detection`() {
        // ADR-0082 section 1: `TextClassifier` may run in an OEM process. The
        // language of a message comes from the shared core (CoreLanguageDetector).
        val bad = Regex("""TextClassificationManager|TextClassifier|(?<![A-Za-z])TextLanguage\b|\.detectLanguage\s*\(""")
        assertEquals(emptyList<String>(), offenders { bad.containsMatchIn(it) })
    }

    @Test
    fun `a pack is offered with anti-censorship on`() {
        // ADR-0082 amendment of 2026-11-19: a pack download, and ML Kit's
        // report of the languages, is ordinary platform traffic; nothing in
        // translation decides on anti-censorship, so the pack dialog and the
        // ML Kit fallback behave the same with it on. TranslationCenter needs
        // the native core, which the host JVM does not load, so this reads
        // the sources: no translation file consults the switch, and the pack
        // dialog has a single shape whose confirm button downloads.
        val translation = sources.filter { f ->
            f.parentFile.name == "translation" &&
                (f.parentFile.parentFile.name == "app" || f.parentFile.parentFile.name == "ui")
        }
        assertTrue(translation.any { it.name == "TranslationCenter.kt" })
        assertTrue(translation.any { it.name == "TranslationViews.kt" })
        val consulting = translation.filter { f -> codeLines(f).any { "AntiCensorship" in it || "antiCensorship" in it } }
        assertEquals(emptyList<String>(), consulting.map { it.name })
        val views = codeLines(translation.single { it.name == "TranslationViews.kt" }).joinToString("\n")
        val dialog = views.substringAfter("fun TranslationDialogs(").substringBefore("picker?.let")
        assertTrue("onClick = TranslationCenter::acceptPack" in dialog)
        assertTrue("blocked" !in dialog)
    }

    @Test
    fun `the transcription engine checks on-device availability and uses the on-device factory`() {
        val engine = sources.single { it.name == "OnDeviceSpeechTranscriber.kt" }
        val code = codeLines(engine).joinToString("\n")
        assertTrue("SpeechRecognizer.isOnDeviceRecognitionAvailable(" in code)
        assertTrue("SpeechRecognizer.createOnDeviceSpeechRecognizer(" in code)
        // The recognizer reads the message's audio, not the microphone.
        assertTrue("RecognizerIntent.EXTRA_AUDIO_SOURCE" in code)
    }

    @Test
    fun `nothing in the background paths translates or transcribes`() {
        val background = sources.filter { f ->
            val p = f.invariantSeparatorsPath
            "/push/" in p || f.name.contains("BackgroundDecrypt") || f.name.contains("MessagingService")
        }
        assertTrue(background.isNotEmpty())
        val bad = background.filter { f ->
            codeLines(f).any { "TranslationCenter" in it || "me.lakko.app.translation" in it || "SpeechRecognizer" in it }
        }
        assertEquals(emptyList<String>(), bad.map { it.path })
    }

    @Test
    fun `there are sources to read`() {
        assertTrue(sources.size > 100)
    }
}
