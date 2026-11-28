// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.io.File
import java.io.IOException
import java.nio.file.Files
import javax.crypto.spec.SecretKeySpec
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Before
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.ThumbRef

/**
 * `docs/ux/connection-state.md` section 7.3 — the outbox.
 *
 * Every test runs the REAL store ([FileOutboxStore] over a temporary
 * directory, every file sealed with the real [AesGcmOutboxCipher] code; only
 * its key is a software AES key instead of the Android Keystore's, which a
 * host JVM does not have). A "restart" is a second [OutboxQueue] over the same
 * directory after the first one's scope was cancelled, as a process death
 * cancels it.
 *
 * Faked: the repository's rows (a map of local ids to acks, [FakeRows]), the
 * publish ([deliver]: the network is a switch) and storage ([FakeUploader]).
 * The repository's own wiring of these seams (`ChatRepository`) is not run
 * here: it needs the native core and a live session.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class OutboxTest {

    private lateinit var root: File
    private lateinit var store: FileOutboxStore

    @Before
    fun setUp() {
        root = Files.createTempDirectory("outbox-test").toFile()
        store = FileOutboxStore(root, AesGcmOutboxCipher { SecretKeySpec(ByteArray(32) { 7 }, "AES") })
    }

    @After
    fun tearDown() {
        root.deleteRecursively()
    }

    /** The conversation's rows, as `GroupRecord` holds them. */
    private class FakeRows : OutboxRows {
        val acks = LinkedHashMap<Long, Ack>()
        val uploaded = HashMap<Long, AttachmentRef>()
        val uploadedAll = HashMap<Long, List<AttachmentRef>>()
        val removed = ArrayList<Long>()
        val shownTexts = ArrayList<String>()
        private var next = -1L

        fun newRow(): Long {
            val id = next--
            acks[id] = Ack.PENDING
            return id
        }

        override fun isHeld(groupIdHex: String): Boolean = groupIdHex == GROUP

        override fun show(entry: OutboxEntry, ack: Ack, posterBytes: ByteArray?): Long {
            val id = next--
            acks[id] = ack
            shownTexts += entry.text.ifEmpty { entry.media?.kind ?: "" }
            return id
        }

        override fun setAck(groupIdHex: String, localId: Long, ack: Ack, refusal: me.lakko.app.media.UploadRefusal?) {
            if (localId in acks) acks[localId] = ack
        }

        override fun setUploaded(groupIdHex: String, localId: Long, refs: List<AttachmentRef>) {
            uploaded[localId] = refs.last()
            uploadedAll[localId] = refs
        }

        override fun remove(groupIdHex: String, localId: Long) {
            acks.remove(localId)
            removed += localId
        }
    }

    private class FakeUploader(var online: () -> Boolean) : OutboxUploader {
        val uploads = ArrayList<ByteArray>()

        override suspend fun fetchSource(url: String): Pair<ByteArray, String> = ByteArray(8) to "image/gif"

        override suspend fun uploadThumb(bytes: ByteArray, mime: String, width: Int, height: Int): ThumbRef {
            if (!online()) throw IOException("no route")
            return ThumbRef(ByteArray(32) { 1 }, ByteArray(32) { 2 }, ByteArray(12) { 3 }, mime, bytes.size.toULong(), null, null)
        }

        override suspend fun uploadMain(
            bytes: ByteArray,
            mime: String,
            name: String,
            meta: AttachmentMediaMeta?,
            onProgress: (Float) -> Unit,
        ): AttachmentRef {
            if (!online()) throw IOException("no route")
            uploads += bytes
            onProgress(1f)
            return AttachmentRef(
                blobId = ByteArray(32) { 9 },
                key = ByteArray(32) { 8 },
                nonce = ByteArray(12) { 7 },
                mime = mime,
                size = bytes.size.toULong(),
                name = name,
                kind = meta?.kind,
                width = meta?.width,
                height = meta?.height,
                thumb = meta?.thumb,
            )
        }
    }

    /** One session: a queue, its rows, its network. */
    private inner class Session(
        val scope: CoroutineScope,
        val online: MutableStateFlow<Boolean>,
    ) {
        val rows = FakeRows()
        val uploader = FakeUploader { online.value }
        /** What was published, in order: (text or media kind, attachment count). */
        val published = ArrayList<String>()
        /** Overrides the publish (refusals). */
        var refuse: ((OutboxEntry) -> DeliverOutcome?)? = null
        /** Called inside the publish, before it decides (the write-first check). */
        var onDeliver: (OutboxEntry) -> Unit = {}
        val deletedBlobs = ArrayList<ByteArray>()
        /** What each publish carried, by text. */
        val publishedAttachments = HashMap<String, List<PersistedAttachment>>()
        /** Publishes tried (the upload of a media message comes before). */
        var attempts = 0

        val queue = OutboxQueue(
            scope = scope,
            backend = store,
            rows = rows,
            deliver = { entry, _ ->
                attempts += 1
                onDeliver(entry)
                val override = refuse?.invoke(entry)
                if (override != null) {
                    override
                } else {
                    if (!online.value) throw IOException("no route")
                    published += (entry.media?.kind ?: entry.text) + "/" + entry.attachments.size
                    publishedAttachments[entry.text] = entry.attachments
                    rows.acks[entry.localId] = Ack.SENT
                    DeliverOutcome.Sent
                }
            },
            uploader = uploader,
            isOnline = { online.value },
            awaitOnline = { online.first { it } },
            recheckDelayMs = 0L,
            deleteBlobs = { deletedBlobs += it },
        )

        fun addText(text: String): Long {
            val id = rows.newRow()
            queue.add(queue.newEntry(GROUP, id).copy(text = text))
            return id
        }

        fun addVoice(bytes: ByteArray): Long {
            val id = rows.newRow()
            queue.add(
                queue.newEntry(GROUP, id).copy(
                    media = OutboxMedia(
                        kind = me.lakko.app.media.KIND_VOICE,
                        mime = "audio/mp4",
                        name = "voice.m4a",
                        durationMs = 4_000,
                        fileId = java.util.UUID.randomUUID().toString(),
                    ),
                ),
                file = bytes,
            )
            return id
        }

        /** A new message as the repository sends it: queued behind waiting
         *  ones, else sent now. */
        suspend fun send(id: Long) {
            if (!queue.queueBehindIfWaiting(GROUP, id)) queue.sendNow(GROUP, id)
        }
    }

    private fun filesDir(): File? = root.listFiles()?.firstOrNull()?.let { File(it, "files") }

    private fun storedFiles(): List<File> = filesDir()?.listFiles()?.toList().orEmpty()

    @Test
    fun the_entry_is_written_before_the_first_attempt() = runTest {
        val s = Session(detachedScope(), MutableStateFlow(true))
        s.queue.restore(ACCOUNT)
        var storedAtDeliver: List<OutboxEntry>? = null
        s.onDeliver = { storedAtDeliver = store.load(ACCOUNT) }
        val id = s.addText("hello")
        s.send(id)
        val seen = storedAtDeliver ?: error("never delivered")
        assertEquals(1, seen.size)
        assertEquals("hello", seen[0].text)
        assertEquals(OutboxState.WAITING, seen[0].state)
        // Published: the entry is gone from the store.
        assertTrue(store.load(ACCOUNT).isEmpty())
        assertEquals(listOf("hello/0"), s.published)
    }

    @Test
    fun waiting_text_and_media_survive_a_restart_and_go_in_order_on_online() = runTest {
        val offline = MutableStateFlow(false)
        val scope1 = detachedScope()
        val first = Session(scope1, offline)
        first.queue.restore(ACCOUNT)
        val voice = ByteArray(2048) { (it % 251).toByte() }
        val mediaId = first.addVoice(voice)
        first.send(mediaId)
        val textId = first.addText("after the voice note")
        first.send(textId)
        advanceUntilIdle()
        assertEquals(Ack.WAITING, first.rows.acks[mediaId])
        assertEquals(Ack.WAITING, first.rows.acks[textId])
        // Stored: two waiting entries, the media one with its prepared file,
        // sealed (the bytes do not appear in clear on disk).
        val stored = store.load(ACCOUNT)
        assertEquals(listOf(OutboxState.WAITING, OutboxState.WAITING), stored.map { it.state })
        assertEquals(listOf(me.lakko.app.media.KIND_VOICE, null), stored.map { it.media?.kind })
        assertEquals(1, storedFiles().size)
        val raw = storedFiles().single().readBytes()
        assertFalse(String(raw, Charsets.ISO_8859_1).contains(String(voice.copyOfRange(0, 64), Charsets.ISO_8859_1)))
        assertFalse(String(File(root.listFiles()!!.single(), "entries.bin").readBytes(), Charsets.ISO_8859_1).contains("after the voice note"))

        // The process ends.
        scope1.cancel()

        // A new session over the same store, still offline: both rows are
        // back, at the end, in written order, waiting.
        val online = MutableStateFlow(false)
        val second = Session(detachedScope(), online)
        second.queue.restore(ACCOUNT)
        advanceUntilIdle()
        assertEquals(listOf(me.lakko.app.media.KIND_VOICE, "after the voice note"), second.rows.shownTexts)
        assertEquals(listOf(Ack.WAITING, Ack.WAITING), second.rows.acks.values.toList())
        assertTrue(second.published.isEmpty())

        // The servers answer: the voice note is uploaded from its stored
        // file and sent, then the text.
        online.value = true
        advanceUntilIdle()
        assertEquals(listOf("${me.lakko.app.media.KIND_VOICE}/1", "after the voice note/0"), second.published)
        assertTrue(second.uploader.uploads.single().contentEquals(voice))
        assertTrue(store.load(ACCOUNT).isEmpty())
        assertTrue(storedFiles().isEmpty())
    }

    @Test
    fun a_tray_photo_whose_upload_got_no_answer_is_sent_waits_survives_a_restart_and_goes_in_order() = runTest {
        // docs/ux/media-messages.md section 11 (2026-11-24): Send with one
        // tray photo already uploaded and one whose upload got no answer.
        val offline = MutableStateFlow(false)
        val scope1 = detachedScope()
        val first = Session(scope1, offline)
        first.queue.restore(ACCOUNT)
        val readyRef = AttachmentRef(
            blobId = ByteArray(32) { 1 },
            key = ByteArray(32) { 1 },
            nonce = ByteArray(12) { 1 },
            mime = "image/jpeg",
            size = 10u,
            name = "a.jpg",
        )
        val photo = ByteArray(700) { (it % 13).toByte() }
        val thumb = ByteArray(40) { 3 }
        val (file, files) = PendingAttachment(
            index = 1,
            bytes = photo,
            mime = "image/jpeg",
            name = "b.jpg",
            width = 1200,
            height = 900,
            thumbBytes = thumb,
            thumbWidth = 320,
            thumbHeight = 240,
        ).toOutbox()
        val photoMsg = first.rows.newRow()
        first.queue.add(
            first.queue.newEntry(GROUP, photoMsg).copy(
                text = "two photos",
                attachments = listOf(PersistedAttachment.from(readyRef)),
                files = listOf(file),
            ),
            files = files,
        )
        first.send(photoMsg)
        val textId = first.addText("after the photos")
        first.send(textId)
        advanceUntilIdle()
        assertEquals(Ack.WAITING, first.rows.acks[photoMsg])
        assertEquals(Ack.WAITING, first.rows.acks[textId])
        assertEquals(2, storedFiles().size)
        scope1.cancel()

        val online = MutableStateFlow(false)
        val second = Session(detachedScope(), online)
        second.queue.restore(ACCOUNT)
        advanceUntilIdle()
        assertEquals(listOf("two photos", "after the photos"), second.rows.shownTexts)
        assertTrue(second.published.isEmpty())
        online.value = true
        advanceUntilIdle()
        // The photo message first, with both photos in the tray's order (the
        // ready one, then the one uploaded now), then the text.
        assertEquals(listOf("two photos/2", "after the photos/0"), second.published)
        val sent = second.publishedAttachments.getValue("two photos")
        assertEquals("a.jpg", sent[0].name)
        assertEquals("b.jpg", sent[1].name)
        assertEquals(1200u, sent[1].width)
        assertNotNull(sent[1].thumb)
        assertTrue(second.uploader.uploads.single().contentEquals(photo))
        assertTrue(store.load(ACCOUNT).isEmpty())
        assertTrue(storedFiles().isEmpty())
    }

    @Test
    fun a_text_written_after_a_waiting_media_message_goes_after_it() = runTest {
        val online = MutableStateFlow(false)
        val s = Session(detachedScope(), online)
        s.queue.restore(ACCOUNT)
        val mediaId = s.addVoice(ByteArray(100) { 1 })
        s.send(mediaId)
        assertEquals(Ack.WAITING, s.rows.acks[mediaId])
        // A text written now joins the queue behind the voice note.
        val textId = s.addText("second")
        assertTrue(s.queue.queueBehindIfWaiting(GROUP, textId))
        assertEquals(Ack.WAITING, s.rows.acks[textId])
        online.value = true
        advanceUntilIdle()
        assertEquals(listOf("${me.lakko.app.media.KIND_VOICE}/1", "second/0"), s.published)
    }

    @Test
    fun a_refused_entry_stays_not_sent_and_survives_a_restart() = runTest {
        val online = MutableStateFlow(true)
        val scope1 = detachedScope()
        val first = Session(scope1, online)
        first.queue.restore(ACCOUNT)
        val refusal = IllegalStateException("conversation is behind")
        first.refuse = { DeliverOutcome.Refused(refusal) }
        val id = first.addText("refused")
        try {
            first.send(id)
            fail("the refusal is thrown to the caller")
        } catch (e: IllegalStateException) {
            assertEquals(refusal, e)
        }
        assertEquals(Ack.FAILED, first.rows.acks[id])
        assertEquals(listOf(OutboxState.FAILED), store.load(ACCOUNT).map { it.state })
        scope1.cancel()

        val second = Session(detachedScope(), online)
        second.queue.restore(ACCOUNT)
        advanceUntilIdle()
        // Shown "Not sent", and nothing sends it by itself.
        assertEquals(listOf(Ack.FAILED), second.rows.acks.values.toList())
        second.queue.requestAllWaiting()
        advanceUntilIdle()
        assertTrue(second.published.isEmpty())
        // The person's Retry sends it.
        val restoredId = second.rows.acks.keys.single()
        assertTrue(second.queue.retry(GROUP, restoredId))
        advanceUntilIdle()
        assertEquals(listOf("refused/0"), second.published)
        assertTrue(store.load(ACCOUNT).isEmpty())
    }

    @Test
    fun delete_removes_the_entry_and_its_file() = runTest {
        val s = Session(detachedScope(), MutableStateFlow(false))
        s.queue.restore(ACCOUNT)
        val id = s.addVoice(ByteArray(500) { 4 })
        s.send(id)
        assertEquals(1, storedFiles().size)
        assertEquals(1, store.load(ACCOUNT).size)
        assertTrue(s.queue.delete(GROUP, id))
        assertTrue(store.load(ACCOUNT).isEmpty())
        assertTrue(storedFiles().isEmpty())
        assertEquals(listOf(id), s.rows.removed)
        assertFalse(s.queue.has(GROUP, id))
    }

    @Test
    fun delete_of_an_uploaded_but_unsent_message_deletes_its_blob() = runTest {
        val online = MutableStateFlow(true)
        val s = Session(detachedScope(), online)
        s.queue.restore(ACCOUNT)
        // The upload lands, the publish is refused.
        s.refuse = { DeliverOutcome.Refused(null) }
        val id = s.addVoice(ByteArray(300) { 5 })
        s.send(id)
        assertEquals(Ack.FAILED, s.rows.acks[id])
        assertNotNull(s.rows.uploaded[id])
        // The reference exists: the prepared file is gone already.
        assertTrue(storedFiles().isEmpty())
        assertTrue(s.queue.delete(GROUP, id))
        assertEquals(1, s.deletedBlobs.size)
        assertTrue(store.load(ACCOUNT).isEmpty())
    }

    @Test
    fun the_account_wipe_clears_the_outbox_and_its_files() = runTest {
        val scope1 = detachedScope()
        val s = Session(scope1, MutableStateFlow(false))
        s.queue.restore(ACCOUNT)
        s.send(s.addVoice(ByteArray(64) { 6 }))
        s.send(s.addText("waiting"))
        assertEquals(2, store.load(ACCOUNT).size)
        scope1.cancel()
        s.queue.clear()
        store.deleteAccount(ACCOUNT)
        assertTrue(store.load(ACCOUNT).isEmpty())
        assertTrue(root.listFiles().isNullOrEmpty())
        val next = Session(detachedScope(), MutableStateFlow(true))
        next.queue.restore(ACCOUNT)
        advanceUntilIdle()
        assertTrue(next.rows.acks.isEmpty())
    }

    @Test
    fun the_count_of_automatic_sends_is_stored_and_bounds_them_after_a_restart() = runTest {
        val online = MutableStateFlow(false)
        val scope1 = detachedScope()
        val first = Session(scope1, online)
        first.queue.restore(ACCOUNT)
        val id = first.addText("flaky")
        first.send(id)
        // Online is reported, but the publish still gets no answer (and the
        // state reads Waiting again, so this session's pass stops there).
        first.refuse = {
            online.value = false
            throw IOException("dropped")
        }
        online.value = true
        advanceUntilIdle()
        assertEquals(1, store.load(ACCOUNT).single().autoSends)
        assertEquals(Ack.WAITING, first.rows.acks[id])
        scope1.cancel()

        val second = Session(detachedScope(), MutableStateFlow(true))
        second.refuse = { throw IOException("dropped") }
        second.queue.restore(ACCOUNT)
        advanceUntilIdle()
        // Four more automatic sends: five in all, then "Not sent".
        assertEquals(MAX_AUTO_RESENDS - 1, second.attempts)
        second.queue.requestAllWaiting()
        advanceUntilIdle()
        assertEquals(MAX_AUTO_RESENDS - 1, second.attempts)
        val entry = store.load(ACCOUNT).single()
        assertEquals(OutboxState.FAILED, entry.state)
        assertEquals(MAX_AUTO_RESENDS, entry.autoSends)
        assertEquals(listOf(Ack.FAILED), second.rows.acks.values.toList())
    }

    @Test
    fun an_entry_of_a_conversation_no_longer_held_is_dropped_with_its_file() = runTest {
        val scope1 = detachedScope()
        val s = Session(scope1, MutableStateFlow(false))
        s.queue.restore(ACCOUNT)
        val id = s.rows.newRow()
        s.queue.add(
            s.queue.newEntry(OTHER_GROUP, id).copy(
                media = OutboxMedia(kind = "voice", mime = "audio/mp4", name = "v.m4a", fileId = "f00d"),
            ),
            file = ByteArray(10),
        )
        assertEquals(1, storedFiles().size)
        scope1.cancel()
        val next = Session(detachedScope(), MutableStateFlow(true))
        next.queue.restore(ACCOUNT)
        assertTrue(next.rows.acks.isEmpty())
        assertTrue(store.load(ACCOUNT).isEmpty())
        assertTrue(storedFiles().isEmpty())
    }

    @Test
    fun a_store_that_cannot_be_written_never_blocks_the_send() = runTest {
        val broken = object : OutboxBackend {
            override fun load(accountIdB64: String): List<OutboxEntry> = emptyList()
            override fun save(accountIdB64: String, entries: List<OutboxEntry>): Boolean = false
            override fun writeFile(accountIdB64: String, fileId: String, bytes: ByteArray): Boolean = false
            override fun readFile(accountIdB64: String, fileId: String): ByteArray? = null
            override fun deleteFile(accountIdB64: String, fileId: String) = Unit
            override fun deleteAccount(accountIdB64: String) = Unit
        }
        val rows = FakeRows()
        val published = ArrayList<Int>()
        val uploader = FakeUploader { true }
        val queue = OutboxQueue(
            scope = detachedScope(),
            backend = broken,
            rows = rows,
            deliver = { entry, _ ->
                published += entry.attachments.size
                DeliverOutcome.Sent
            },
            uploader = uploader,
            isOnline = { true },
            awaitOnline = {},
            recheckDelayMs = 0L,
        )
        queue.restore(ACCOUNT)
        val id = rows.newRow()
        val bytes = ByteArray(40) { 2 }
        queue.add(
            queue.newEntry(GROUP, id).copy(
                media = OutboxMedia(kind = "voice", mime = "audio/mp4", name = "v.m4a", fileId = "beef"),
            ),
            file = bytes,
        )
        queue.sendNow(GROUP, id)
        assertEquals(listOf(1), published)
        assertTrue(uploader.uploads.single().contentEquals(bytes))
        assertNull(queue.entriesNow().firstOrNull())
    }

    @Test
    fun waiting_sends_into_a_closed_one_to_one_turn_not_sent_and_never_go() = runTest {
        // UX 16.11 (alignment 2026-11-27): the person of this one-to-one
        // started over while two messages waited for the network.
        val online = MutableStateFlow(false)
        val s = Session(detachedScope(), online)
        s.queue.restore(ACCOUNT)
        val a = s.addText("one")
        s.send(a)
        val b = s.addText("two")
        s.send(b)
        advanceUntilIdle()
        assertEquals(Ack.WAITING, s.rows.acks[a])
        assertEquals(2, s.queue.failWaiting(GROUP))
        assertEquals(Ack.FAILED, s.rows.acks[a])
        assertEquals(Ack.FAILED, s.rows.acks[b])
        assertEquals(listOf(OutboxState.FAILED, OutboxState.FAILED), store.load(ACCOUNT).map { it.state })
        // The network comes back: nothing waits, nothing is sent.
        online.value = true
        advanceUntilIdle()
        assertTrue(s.published.isEmpty())
        // Nothing left to turn.
        assertEquals(0, s.queue.failWaiting(GROUP))
    }

    private companion object {
        const val ACCOUNT = "b3V0Ym94LXRlc3QtYWNjdA"
        const val GROUP = "aa11"
        const val OTHER_GROUP = "bb22"
    }
}

/** A scope [advanceUntilIdle] drives (its dispatcher) that the test's own job
 *  does not wait for. */
private fun TestScope.detachedScope(): CoroutineScope =
    CoroutineScope(coroutineContext.minusKey(kotlinx.coroutines.Job) + SupervisorJob())
