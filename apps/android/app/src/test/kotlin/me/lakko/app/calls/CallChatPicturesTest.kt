// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runCurrent
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Pictures pasted into the chat of a group call (`docs/ux/calls.md` section
 * 4.1.4.1): the tray, preparing a paste, what Send sends and in which order,
 * the bubble's size, and the uploads deleted when the call ends. The core,
 * the file store and the bitmap decoder are stood in for, so no native
 * library is needed.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class CallChatPicturesTest {
    private val rule = object : CallChatTextRule {
        override fun check(text: String): String? = when {
            text.isBlank() -> "empty"
            text.contains('\u0007') -> "control"
            else -> null
        }

        override fun clamp(text: String): String = text
        override val maxBytes: Int = 800
    }

    private fun picture(w: Int = 1200, h: Int = 800, fill: Byte = 1) =
        PreparedCallChatPicture(ByteArray(10) { fill }, w, h)

    // -- The tray ------------------------------------------------------------

    @Test
    fun `a pasted picture goes into the tray and a second one replaces it`() {
        val first = picture(fill = 1)
        val second = picture(fill = 2)
        var tray = callChatTrayAfterPaste(CallChatTray(), CallChatPaste.Picture(first))
        assertSame(first, tray.picture)
        tray = callChatTrayAfterPaste(tray, CallChatPaste.Picture(second))
        assertSame(second, tray.picture)
        assertNull(tray.notice)
    }

    @Test
    fun `a paste that is not a picture leaves the tray as it was and says so`() {
        val held = picture()
        val tray = callChatTrayAfterPaste(CallChatTray(picture = held), CallChatPaste.NotAPicture)
        assertSame(held, tray.picture)
        assertEquals(CallChatTrayNotice.PICTURE_ONLY, tray.notice)
        val tooLarge = callChatTrayAfterPaste(CallChatTray(), CallChatPaste.TooLarge)
        assertNull(tooLarge.picture)
        assertEquals(CallChatTrayNotice.TOO_LARGE, tooLarge.notice)
        // A picture after that clears the line.
        assertNull(callChatTrayAfterPaste(tray, CallChatPaste.Picture(picture())).notice)
    }

    @Test
    fun `the cross empties the tray`() {
        assertEquals(CallChatTray(), callChatTrayRemoved(CallChatTray(picture = picture())))
    }

    @Test
    fun `a paste is prepared at the conversation limit and judged by what decodes`() {
        var askedSide = 0
        val ok = prepareCallChatPicture(
            bytes = byteArrayOf(1, 2, 3),
            maxLongestSide = 2560,
            maxBytes = 100,
            reencode = { _, side -> askedSide = side; ByteArray(50) },
            boundsOf = { CallChatPictureSize(2560, 1440) },
        )
        assertEquals(2560, askedSide)
        val prepared = (ok as CallChatPaste.Picture).picture
        assertEquals(2560, prepared.width)
        assertEquals(1440, prepared.height)
        assertEquals(50, prepared.jpeg.size)

        val notDecoded = prepareCallChatPicture(byteArrayOf(1), 2560, 100, { _, _ -> null }, { null })
        assertSame(CallChatPaste.NotAPicture, notDecoded)
        val empty = prepareCallChatPicture(ByteArray(0), 2560, 100, { _, _ -> ByteArray(1) }, { CallChatPictureSize(1, 1) })
        assertSame(CallChatPaste.NotAPicture, empty)
        val tooLarge = prepareCallChatPicture(byteArrayOf(1), 2560, 100, { _, _ -> ByteArray(101) }, { CallChatPictureSize(10, 10) })
        assertSame(CallChatPaste.TooLarge, tooLarge)
    }

    // -- What Send sends -----------------------------------------------------

    @Test
    fun `send sends the picture first and the text after it`() {
        val p = picture()
        val plan = callChatSendPlan("hello", p, rule)!!
        assertSame(p, plan.picture)
        assertEquals("hello", plan.text)
        // A picture alone, and white space with it, is the picture only.
        assertNull(callChatSendPlan("   ", p, rule)!!.text)
        // Text the rule refuses is not sent with the picture.
        assertNull(callChatSendPlan("bad\u0007", p, rule)!!.text)
        // No picture and no text: Send is disabled.
        assertNull(callChatSendPlan("  ", null, rule))
        assertFalse(callChatCanSendWith("", null, rule))
        assertTrue(callChatCanSendWith("", p, rule))
        assertTrue(callChatCanSendWith("hi", null, rule))
    }

    // -- The bubble's size ---------------------------------------------------

    @Test
    fun `a picture bubble fits 240 by 240 keeping its shape, at least 80 on the short side`() {
        assertEquals(CallChatBox(240, 180), callChatPictureBox(4000, 3000))
        assertEquals(CallChatBox(180, 240), callChatPictureBox(3000, 4000))
        assertEquals(CallChatBox(240, 240), callChatPictureBox(2560, 2560))
        // Never enlarged beyond its shape's need, but the short side is 80.
        assertEquals(CallChatBox(160, 80), callChatPictureBox(100, 50))
        assertEquals(CallChatBox(80, 80), callChatPictureBox(40, 40))
        // A very long picture: 240 by 80, cropped.
        assertEquals(CallChatBox(240, 80), callChatPictureBox(2560, 10))
        assertEquals(CallChatBox(80, 240), callChatPictureBox(10, 2560))
        // A size that cannot be read.
        assertEquals(CallChatBox(240, 240), callChatPictureBox(0, 10))
    }

    @Test
    fun `the tray thumbnail is 64 high and its width follows the shape`() {
        assertEquals(96, callChatTrayThumbWidth(1500, 1000))
        assertEquals(64, callChatTrayThumbWidth(500, 500))
        assertEquals(192, callChatTrayThumbWidth(10_000, 100))
        assertEquals(32, callChatTrayThumbWidth(100, 10_000))
    }

    // -- The sender ----------------------------------------------------------

    /** The session's chat, recorded: what was numbered and what went out. */
    private class FakeWire : CallChatWire {
        var next = 0uL
        val texts = HashMap<ULong, String?>()
        val attached = HashMap<ULong, CallChatPictureRef>()
        val transmitted = mutableListOf<ULong>()
        var socketUp = true

        override suspend fun reserve(text: String?): ULong {
            next += 1uL
            texts[next] = text
            return next
        }

        override suspend fun attachPicture(seq: ULong, picture: CallChatPictureRef) {
            attached[seq] = picture
        }

        override suspend fun transmit(seq: ULong): Boolean {
            // A picture with nothing attached has nothing to send.
            if (texts[seq] == null && attached[seq] == null) return false
            if (!socketUp) return false
            transmitted += seq
            return true
        }
    }

    private class Harness {
        val scope = TestScope(UnconfinedTestDispatcher())
        val model = CallChatModel { 0L }
        val wire = FakeWire()
        var putOk = true
        var putGate: CompletableDeferred<Unit>? = null
        var encrypted = 0
        val puts = mutableListOf<ByteArray>()
        val deleted = mutableListOf<List<ByteArray>>()
        var downloadResult: ByteArray? = byteArrayOf(9, 9)
        val sender = CallChatSender(
            scope = scope,
            model = model,
            wire = wire,
            deps = CallChatPictureDeps(
                encrypt = { jpeg ->
                    encrypted += 1
                    EncryptedCallChatPicture(
                        blobId = ByteArray(32) { encrypted.toByte() },
                        key = ByteArray(32),
                        nonce = ByteArray(12),
                        ciphertext = jpeg,
                    )
                },
                put = { p ->
                    putGate?.await()
                    puts += p.blobId
                    putOk
                },
                download = { downloadResult },
                deleteUploaded = { deleted += it },
            ),
        )
    }

    @Test
    fun `a picture and its text go out picture first, numbered in one sequence`() {
        val h = Harness()
        h.sender.send("before")
        h.sender.sendPicture(picture(), "caption")
        h.scope.runCurrent()
        assertEquals(listOf(1uL, 2uL, 3uL), h.wire.transmitted)
        assertNull(h.wire.texts[2uL])
        assertEquals("caption", h.wire.texts[3uL])
        val attached = h.wire.attached.getValue(2uL)
        assertEquals(1200.toUShort(), attached.width)
        assertEquals(800.toUShort(), attached.height)
        assertEquals(10u, attached.size)
        val messages = h.model.messages.value
        assertEquals(listOf("self:1", "self:2", "self:3"), messages.map { it.id })
        assertTrue(messages.all { it.delivery == CallChatDelivery.SENT })
        assertEquals(CallChatPictureSize(1200, 800), messages[1].picture)
    }

    @Test
    fun `while the picture uploads it and its text show as sending, the text waits`() {
        val h = Harness()
        h.putGate = CompletableDeferred()
        h.sender.sendPicture(picture(), "caption")
        h.scope.runCurrent()
        val during = h.model.messages.value
        assertEquals(listOf(CallChatDelivery.SENDING, CallChatDelivery.SENDING), during.map { it.delivery })
        assertTrue(h.wire.transmitted.isEmpty())
        h.putGate!!.complete(Unit)
        h.scope.runCurrent()
        assertEquals(listOf(1uL, 2uL), h.wire.transmitted)
    }

    @Test
    fun `a failed upload is not sent and send again uploads the same ciphertext`() {
        val h = Harness()
        h.putOk = false
        h.sender.sendPicture(picture(), "caption")
        h.scope.runCurrent()
        val failed = h.model.messages.value
        assertEquals(CallChatDelivery.NOT_SENT, failed[0].delivery)
        // The text still goes, after the picture's attempt.
        assertEquals(CallChatDelivery.SENT, failed[1].delivery)
        assertEquals(listOf(2uL), h.wire.transmitted)
        h.putOk = true
        h.sender.resend(1uL)
        h.scope.runCurrent()
        assertEquals(1, h.encrypted)
        assertEquals(2, h.puts.size)
        assertArrayEquals(h.puts[0], h.puts[1])
        assertEquals(CallChatDelivery.SENT, h.model.ownDelivery(1uL))
        assertEquals(listOf(2uL, 1uL), h.wire.transmitted)
    }

    @Test
    fun `an uploaded picture whose message did not go is sent again without a new upload`() {
        val h = Harness()
        h.wire.socketUp = false
        h.sender.sendPicture(picture(), null)
        h.scope.runCurrent()
        assertEquals(CallChatDelivery.NOT_SENT, h.model.ownDelivery(1uL))
        h.wire.socketUp = true
        h.sender.resend(1uL)
        h.scope.runCurrent()
        assertEquals(1, h.puts.size)
        assertEquals(CallChatDelivery.SENT, h.model.ownDelivery(1uL))
    }

    @Test
    fun `every picture uploaded in the call is deleted once when the call ends`() {
        val h = Harness()
        h.sender.sendPicture(picture(), null)
        h.sender.sendPicture(picture(), null)
        h.scope.runCurrent()
        h.sender.endCall()
        h.sender.endCall()
        assertEquals(1, h.deleted.size)
        assertEquals(2, h.deleted.single().size)
        assertArrayEquals(h.puts[0], h.deleted.single()[0])
        assertArrayEquals(h.puts[1], h.deleted.single()[1])
        // Nothing more is sent after the end.
        h.sender.sendPicture(picture(), null)
        h.scope.runCurrent()
        assertEquals(2, h.puts.size)
    }

    @Test
    fun `a picture still uploading when the call ends is deleted too`() {
        val h = Harness()
        h.putGate = CompletableDeferred()
        h.sender.sendPicture(picture(), null)
        h.scope.runCurrent()
        h.sender.endCall()
        h.scope.runCurrent()
        // The blob was recorded before its upload began, so the end deletes
        // it; the stopped upload asks for its delete again (the owed delete
        // keeps one).
        val blob = ByteArray(32) { 1 }
        assertTrue(h.deleted.isNotEmpty())
        assertTrue(h.deleted.all { batch -> batch.size == 1 && batch[0].contentEquals(blob) })
        assertTrue(h.puts.isEmpty())
        assertTrue(h.wire.transmitted.isEmpty())
    }

    @Test
    fun `a received picture shows at once and then the downloaded bytes, or not available`() {
        val h = Harness()
        val ref = CallChatPictureRef(ByteArray(32), ByteArray(32), ByteArray(12), 10u, 640.toUShort(), 480.toUShort())
        h.sender.onReceivedPicture("p1", 1uL, ref, "Anna")
        h.scope.runCurrent()
        val shown = h.model.messages.value.single()
        assertEquals(CallChatPictureSize(640, 480), shown.picture)
        val ready = h.model.pictures.value.getValue(shown.id) as CallChatPictureState.Ready
        assertArrayEquals(byteArrayOf(9, 9), ready.bytes)
        // Counted and previewed as a picture.
        assertEquals(1, h.model.unread.value)
        assertTrue(h.model.preview.value!!.isPicture)

        h.downloadResult = null
        h.sender.onReceivedPicture("p1", 2uL, ref, "Anna")
        h.scope.runCurrent()
        assertSame(CallChatPictureState.Unavailable, h.model.pictures.value.getValue("p1:2"))
        // The same picture is shown once.
        h.sender.onReceivedPicture("p1", 2uL, ref, "Anna")
        assertEquals(2, h.model.messages.value.size)
    }

    @Test
    fun `the end of the call clears the pictures with the messages`() {
        val m = CallChatModel { 0L }
        m.addOwnPicture(1uL, CallChatPictureSize(10, 10), byteArrayOf(1))
        m.clear()
        assertTrue(m.pictures.value.isEmpty())
        assertTrue(m.messages.value.isEmpty())
    }

    // -- The channel ---------------------------------------------------------

    @Test
    fun `the channel numbers pictures and texts in one sequence and sends a picture once attached`() {
        val sent = mutableListOf<String>()
        val shownPictures = mutableListOf<Pair<String, ULong>>()
        val channel = CallChatChannel(
            CallChatChannelDeps(
                compose = { self, seq, text -> "$self|$seq|t|$text".toByteArray() },
                verify = { from, _, bytes ->
                    val parts = String(bytes).split("|")
                    if (parts[2] == "p") {
                        VerifiedCallChatView(parts[1].toULong(), "", CallChatPictureRef(ByteArray(32), ByteArray(32), ByteArray(12), 1u, 1u, 1u))
                    } else {
                        VerifiedCallChatView(parts[1].toULong(), parts[3])
                    }.takeIf { parts[0] == from }
                },
                sendSealed = { sent += String(it); true },
                selfPeerId = { "me" },
                onMessage = { _, _, _ -> },
                composePicture = { self, seq, _ -> "$self|$seq|p".toByteArray() },
                onPicture = { peer, seq, _ -> shownPictures += peer to seq },
            ),
        )
        val pictureSeq = channel.reserve(null)
        assertFalse(channel.resend(pictureSeq))
        assertEquals(2uL, channel.send("hi").seq)
        channel.attachPicture(pictureSeq, CallChatPictureRef(ByteArray(32), ByteArray(32), ByteArray(12), 1u, 1u, 1u))
        assertTrue(channel.resend(pictureSeq))
        assertEquals(listOf("me|2|t|hi", "me|1|p"), sent)

        channel.onPeerIdentified("p1", byteArrayOf())
        channel.onReceived("p1", "p1|4|p".toByteArray())
        channel.onReceived("p1", "p1|4|p".toByteArray())
        assertEquals(listOf("p1" to 4uL), shownPictures)
    }
}
