// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import kotlinx.serialization.json.Json
import me.lakko.app.chat.MessageLogPayload
import me.lakko.app.chat.MessageLogStore
import me.lakko.app.chat.PersistedAttachment
import me.lakko.app.chat.PersistedGroup
import me.lakko.app.chat.PersistedMessage
import me.lakko.app.chat.from
import me.lakko.app.chat.toAttachmentRef
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ThumbRef
import java.io.File
import java.nio.file.Files

class MediaPersistenceAndCacheTest {

    private val tmp: File = Files.createTempDirectory("lakko-media-test").toFile()

    @After
    fun cleanUp() {
        tmp.deleteRecursively()
    }

    private fun voiceNoteRef() = AttachmentRef(
        blobId = ByteArray(32) { 1 },
        key = ByteArray(32) { 2 },
        nonce = ByteArray(12) { 3 },
        mime = "video/mp4",
        size = 1234u,
        name = "video-note-1.mp4",
        kind = KIND_VIDEO_NOTE,
        durationMs = 42_000u,
        waveform = byteArrayOf(0, 64, 127, -1),
        width = 480u,
        height = 480u,
        thumb = ThumbRef(
            blobId = ByteArray(32) { 4 },
            key = ByteArray(32) { 5 },
            nonce = ByteArray(12) { 6 },
            mime = "image/jpeg",
            size = 9_000u,
            width = 240u,
            height = 240u,
        ),
    )

    @Test
    fun persistedAttachmentRoundTripsEveryMediaField() {
        val original = voiceNoteRef()
        val back = PersistedAttachment.from(original).toAttachmentRef()!!
        assertArrayEquals(original.blobId, back.blobId)
        assertArrayEquals(original.key, back.key)
        assertArrayEquals(original.nonce, back.nonce)
        assertEquals(original.mime, back.mime)
        assertEquals(original.size, back.size)
        assertEquals(original.name, back.name)
        assertEquals(original.kind, back.kind)
        assertEquals(original.durationMs, back.durationMs)
        assertArrayEquals(original.waveform, back.waveform)
        assertEquals(original.width, back.width)
        assertEquals(original.height, back.height)
        val t = back.thumb!!
        assertArrayEquals(original.thumb!!.blobId, t.blobId)
        assertArrayEquals(original.thumb!!.key, t.key)
        assertArrayEquals(original.thumb!!.nonce, t.nonce)
        assertEquals("image/jpeg", t.mime)
        assertEquals(9_000uL, t.size)
        assertEquals(240u, t.width)
    }

    @Test
    fun roundTripSurvivesTheJsonEnvelopeTheStoreWrites() {
        val payload = MessageLogPayload(
            groups = mapOf(
                "g" to PersistedGroup(
                    cursor = 7,
                    messages = listOf(
                        PersistedMessage(
                            messageId = 7,
                            direction = "incoming",
                            text = "",
                            attachments = listOf(PersistedAttachment.from(voiceNoteRef())),
                            createdAt = "2026-09-13T00:00:00Z",
                        ),
                    ),
                ),
            ),
        )
        val text = Json.encodeToString(MessageLogPayload.serializer(), payload)
        val decoded = MessageLogStore.decodeLogPayload(text)!!
        val ref = decoded.groups.getValue("g").messages.single().attachments.single().toAttachmentRef()!!
        assertEquals(KIND_VIDEO_NOTE, ref.kind)
        assertEquals(42_000u, ref.durationMs)
        assertNotNull(ref.thumb)
    }

    @Test
    fun aRowWrittenBeforeTheMediaFieldsDecodesAsAPlainFile() {
        val legacy = """{"version":1,"groups":{"g":{"cursor":1,"messages":[{"messageId":1,"direction":"incoming","text":"",
            "attachments":[{"blobIdB64":"AQ==","keyB64":"Ag==","nonceB64":"Aw==","mime":"image/png","size":3,"name":"a.png"}],
            "createdAt":"x"}]}}}""".replace("\n", "")
        val ref = MessageLogStore.decodeLogPayload(legacy)!!
            .groups.getValue("g").messages.single().attachments.single().toAttachmentRef()!!
        assertNull(ref.kind)
        assertNull(ref.durationMs)
        assertNull(ref.waveform)
        assertNull(ref.thumb)
        assertArrayEquals(byteArrayOf(1), ref.blobId)
    }

    @Test
    fun aMalformedOptionalFieldIsDroppedAndTheAttachmentSurvives() {
        val bad = PersistedAttachment.from(voiceNoteRef()).copy(waveformB64 = "***not base64***")
        val ref = bad.toAttachmentRef()!!
        assertNull(ref.waveform)
        assertEquals(KIND_VIDEO_NOTE, ref.kind)
        // A required field that does not decode drops the attachment.
        assertNull(bad.copy(blobIdB64 = "***").toAttachmentRef())
    }

    @Test
    fun cacheWritesUnderSanitizedNamesInsideItsSession() {
        val cache = ChatMediaCache(tmp)
        val f = cache.put("aabb", "../../shared_prefs/evil.xml", "fallback", byteArrayOf(9))!!
        assertEquals("evil.xml", f.name)
        assertTrue(f.canonicalPath.startsWith(File(tmp, ChatMediaCache.ROOT_DIR).canonicalPath))
        assertArrayEquals(byteArrayOf(9), f.readBytes())
        assertEquals(f, cache.get("aabb"))
        // Two messages with the same sender-chosen name do not collide.
        val g = cache.put("ccdd", "evil.xml", "fallback", byteArrayOf(8))!!
        assertFalse(f == g)
        assertArrayEquals(byteArrayOf(9), f.readBytes())
    }

    @Test
    fun aKeyCanNeverBecomeAPath() {
        assertEquals("______etc", ChatMediaCache.entryDirName("../../etc"))
        assertEquals("entry", ChatMediaCache.entryDirName(""))
        val cache = ChatMediaCache(tmp)
        val f = cache.put("../../outside", "x", "fallback", byteArrayOf(1))!!
        assertTrue(f.canonicalPath.startsWith(File(tmp, ChatMediaCache.ROOT_DIR).canonicalPath))
    }

    @Test
    fun disposeDeletesEveryDecryptedFileAndRefusesLateWrites() {
        val cache = ChatMediaCache(tmp)
        val f = cache.put("k1", "voice.m4a", "fallback", byteArrayOf(1, 2, 3))!!
        val take = cache.newEntryFile("take", "rec.mp4")!!.apply { writeBytes(byteArrayOf(4)) }
        cache.dispose()
        assertFalse(f.exists())
        assertFalse(take.exists())
        assertTrue(cache.isDisposed)
        // A download that resolves after the chat closed keeps nothing.
        assertNull(cache.put("k2", "late.m4a", "fallback", byteArrayOf(5)))
        val orphan = File(tmp, "orphan.bin").apply { writeBytes(byteArrayOf(6)) }
        assertNull(cache.adopt("k3", orphan))
        assertFalse(orphan.exists())
        assertEquals(0, cache.size)
    }

    @Test
    fun replacingAndRemovingDeleteTheOldFile() {
        val cache = ChatMediaCache(tmp)
        val first = cache.put("k", "a.gif", "f", byteArrayOf(1))!!
        val second = cache.put("k", "b.gif", "f", byteArrayOf(2))!!
        assertFalse(first.exists())
        cache.remove("k")
        assertFalse(second.exists())
        assertNull(cache.get("k"))
    }

    @Test
    fun sweepRemovesSessionsLeftByADeadProcessButNotTheLiveOne() {
        val dead = ChatMediaCache(tmp)
        val leftover = dead.put("x", "x.m4a", "f", byteArrayOf(1))!!
        val live = ChatMediaCache(tmp)
        val kept = live.put("y", "y.m4a", "f", byteArrayOf(2))!!
        ChatMediaCache.sweepStale(tmp, keep = live)
        assertFalse(leftover.exists())
        assertTrue(kept.exists())
    }

    @Test
    fun playedMarksAreBoundedAndSurviveEncoding() {
        val set = PlayedMediaSet(max = 3)
        assertTrue(set.markPlayed("a"))
        assertFalse(set.markPlayed("a"))
        set.markPlayed("b")
        set.markPlayed("c")
        set.markPlayed("d")
        assertFalse(set.isPlayed("a"))
        assertEquals(listOf("b", "c", "d"), set.snapshot())
        val restored = PlayedMediaSet(max = 3).apply { seed(set.encode() + "\nnot a key!\n\n") }
        assertEquals(listOf("b", "c", "d"), restored.snapshot())
    }
}
