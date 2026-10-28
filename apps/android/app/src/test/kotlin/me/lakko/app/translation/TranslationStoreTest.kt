// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.chat.nextLastOpId
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0082 section 5 — the local store of translations and transcripts, and
 * the pure helpers of the transcription engine. The shared rules
 * (`storedResultIsCurrent` and the rest) are native and are tested in
 * `lakko-ui-rules`; the host JVM does not load the core.
 */
class TranslationStoreTest {
    private val key = TranslationStoreState.messageKey("ab01", 42)

    @Test
    fun `a state round-trips through its encoding`() {
        val s = TranslationStoreState()
            .withTranslation(key, StoredTranslation(42, "es", "en", "hello"))
            .withTranscript(key, StoredTranscript(42, "es", "hola"))
            .withChoices("ab01", ConversationChoices(autoTranslate = true, dismissedBar = listOf("fr")))
        assertEquals(s, TranslationStoreState.decode(TranslationStoreState.encode(s)))
    }

    @Test
    fun `an unreadable file is an empty store`() {
        assertEquals(TranslationStoreState(), TranslationStoreState.decode("not json"))
        assertEquals(TranslationStoreState(), TranslationStoreState.decode(null))
    }

    @Test
    fun `a deleted message takes its results with it, and only its own`() {
        val other = TranslationStoreState.messageKey("ab01", 43)
        val s = TranslationStoreState()
            .withTranslation(key, StoredTranslation(42, "es", "en", "hello"))
            .withTranscript(key, StoredTranscript(42, "es", "hola"))
            .withTranslation(other, StoredTranslation(43, "es", "en", "bye"))
            .withoutMessage(key)
        assertFalse(s.translations.containsKey(key))
        assertFalse(s.transcripts.containsKey(key))
        assertTrue(s.translations.containsKey(other))
        assertEquals(1, s.resultCount)
    }

    @Test
    fun `delete all removes results and keeps the conversation choices`() {
        val s = TranslationStoreState()
            .withTranslation(key, StoredTranslation(42, "es", "en", "hello"))
            .withChoices("ab01", ConversationChoices(autoTranslate = true))
            .withoutResults()
        assertEquals(0, s.resultCount)
        assertTrue(s.conversations.getValue("ab01").autoTranslate)
    }

    @Test
    fun `default choices are not stored`() {
        val s = TranslationStoreState()
            .withChoices("ab01", ConversationChoices(autoTranslate = true))
            .withChoices("ab01", ConversationChoices())
        assertTrue(s.conversations.isEmpty())
    }

    private fun msg(id: Long, last: Long = 0) =
        ChatMessage(messageId = id, direction = Direction.Incoming, text = "x", createdAt = "", lastOpId = last)

    @Test
    fun `an edit always raises the op id, and a replayed op does not`() {
        val m = msg(10)
        assertEquals(10, m.effectiveOpId)
        // An incoming edit carries its own DS id.
        assertEquals(15, nextLastOpId(m, 15))
        // The same op applied again leaves it where it is.
        assertEquals(15, nextLastOpId(msg(10, 15), 15))
        // An own edit whose publish returned no id still rises.
        assertEquals(11, nextLastOpId(m, 0))
        assertEquals(16, nextLastOpId(msg(10, 15), 0))
    }

    @Test
    fun `segments join with single spaces and blanks drop out`() {
        assertEquals("one two", joinSegments(listOf(" one ", "", "two")))
        assertEquals("", joinSegments(emptyList()))
    }

    @Test
    fun `stereo PCM becomes the channel mean`() {
        // Two frames: (100, 300) and (-200, -400).
        val bytes = byteArrayOf(100, 0, 44, 1, 56, -1, 112, -2)
        val mono = pcm16ToMono(bytes, 2)
        assertArrayEquals(shortArrayOf(200, -300), mono)
    }

    @Test
    fun `resampling to the same rate keeps every sample`() {
        val r = LinearResampler(16_000)
        val out = r.push(shortArrayOf(1, 2, 3, 4), 16_000)
        assertEquals(8, out.size)
    }

    @Test
    fun `resampling halves the sample count from 32 kHz`() {
        val r = LinearResampler(16_000)
        val out = r.push(ShortArray(320) { 7 }, 32_000)
        assertEquals(160 * 2, out.size)
    }
}
