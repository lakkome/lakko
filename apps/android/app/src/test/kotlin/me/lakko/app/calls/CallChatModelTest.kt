// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * What the call screen draws of a group call's chat (`docs/ux/calls.md`
 * section 4.1.4, `CallChatModel.kt`): the unread count, the preview, own
 * messages, the composer's text rule and the panel's shape.
 */
class CallChatModelTest {
    init {
        EnglishForTests.install()
    }

    private var now = 1_000L
    private fun model() = CallChatModel { now }

    /** The core's rule, stood in for: 1..=8 bytes, not blank, no control
     *  character but LF and TAB. */
    private val rule = object : CallChatTextRule {
        override val maxBytes = 8

        override fun check(text: String): String? = when {
            text.isEmpty() -> "empty"
            text.isBlank() -> "blank"
            text.toByteArray().size > maxBytes -> "tooLong"
            text.any { it.isISOControl() && it != '\n' && it != '\t' } -> "controlCharacter"
            else -> null
        }

        override fun clamp(text: String): String {
            var out = text
            while (out.toByteArray().size > maxBytes) out = out.dropLast(1)
            return out
        }
    }

    @Test
    fun `a message from someone else counts and previews while the panel is closed`() {
        val m = model()
        m.addReceived("p1", 1uL, "hello", "Anna")
        assertEquals(1, m.unread.value)
        assertEquals("hello", m.preview.value?.text)
        assertEquals("Anna", m.preview.value?.senderName)
        assertNull(m.announcement.value)
    }

    @Test
    fun `opening the panel clears the count and the preview`() {
        val m = model()
        m.addReceived("p1", 1uL, "a", "Anna")
        m.addReceived("p1", 2uL, "b", "Anna")
        m.openPanel()
        assertEquals(0, m.unread.value)
        assertNull(m.preview.value)
        assertTrue(m.panelOpen.value)
    }

    @Test
    fun `while the panel is open a message is announced, never counted or previewed`() {
        val m = model()
        m.openPanel()
        m.addReceived("p1", 1uL, "hi", "Anna")
        assertEquals(0, m.unread.value)
        assertNull(m.preview.value)
        assertEquals("p1:1", m.announcement.value?.messageId)
    }

    @Test
    fun `own messages are never counted or previewed`() {
        val m = model()
        m.addOwn(1uL, "mine", sent = true)
        assertEquals(0, m.unread.value)
        assertNull(m.preview.value)
        assertTrue(m.messages.value.single().own)
    }

    @Test
    fun `a second message replaces the preview and an old timer does not take it down`() {
        val m = model()
        m.addReceived("p1", 1uL, "first", "Anna")
        val first = m.preview.value!!.serial
        m.addReceived("p2", 1uL, "second", "Ben")
        val second = m.preview.value!!
        assertEquals("second", second.text)
        assertTrue(second.serial > first)
        // The first preview's 4 seconds end: the second stays.
        m.expirePreview(first)
        assertEquals("second", m.preview.value?.text)
        m.expirePreview(second.serial)
        assertNull(m.preview.value)
        // The count is untouched by the preview going.
        assertEquals(2, m.unread.value)
    }

    @Test
    fun `the badge reads 1 to 9 then 9+`() {
        assertNull(callChatBadgeText(0))
        assertEquals("1", callChatBadgeText(1))
        assertEquals("9", callChatBadgeText(9))
        assertEquals("9+", callChatBadgeText(10))
        assertEquals("9+", callChatBadgeText(250))
    }

    @Test
    fun `the circle's spoken name follows the panel and the count`() {
        assertEquals("Open chat", callChatSpokenLabel(open = false, unread = 0).en)
        assertEquals("Close chat", callChatSpokenLabel(open = true, unread = 3).en)
        assertEquals("Open chat, 3 unread", callChatSpokenLabel(open = false, unread = 3).en)
    }

    @Test
    fun `a message is shown once`() {
        val m = model()
        m.addReceived("p1", 1uL, "x", "Anna")
        m.addReceived("p1", 1uL, "x", "Anna")
        m.addOwn(1uL, "y", sent = true)
        m.addOwn(1uL, "y", sent = true)
        assertEquals(2, m.messages.value.size)
        assertEquals(1, m.unread.value)
    }

    @Test
    fun `a message not sent is marked and send again updates the same bubble`() {
        val m = model()
        m.addOwn(1uL, "hello", sent = false)
        assertEquals(CallChatDelivery.NOT_SENT, m.messages.value.single().delivery)
        m.setOwnDelivery(1uL, sent = true)
        assertEquals(CallChatDelivery.SENT, m.messages.value.single().delivery)
        assertEquals(1, m.messages.value.size)
    }

    @Test
    fun `the control shows a message with the number the session gave it`() {
        val m = model()
        var nextSeq = 0uL
        var socketUp = false
        val resent = mutableListOf<ULong>()
        val control = callChatControl(
            model = m,
            sendOut = { _, done -> done(CallChatSendResult(++nextSeq, socketUp)) },
            resendOut = { seq, done ->
                resent += seq
                done(socketUp)
            },
        )
        control.send("one")
        assertEquals(CallChatDelivery.NOT_SENT, m.messages.value.single().delivery)
        socketUp = true
        control.resend(1uL)
        assertEquals(listOf(1uL), resent)
        assertEquals(CallChatDelivery.SENT, m.messages.value.single().delivery)
        control.send("two")
        assertEquals(listOf("self:1", "self:2"), m.messages.value.map { it.id })
    }

    @Test
    fun `the end of the call clears every message and closes the panel`() {
        val m = model()
        m.openPanel()
        m.addReceived("p1", 1uL, "a", "Anna")
        m.addOwn(1uL, "b", sent = true)
        m.closePanel()
        m.addReceived("p1", 2uL, "c", "Anna")
        m.clear()
        assertTrue(m.messages.value.isEmpty())
        assertFalse(m.panelOpen.value)
        assertEquals(0, m.unread.value)
        assertNull(m.preview.value)
        assertNull(m.announcement.value)
    }

    @Test
    fun `the call state clears the chat it held when the call ends`() {
        val ui = CallUiState(scope = kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.Dispatchers.Unconfined))
        val m = model()
        val control = callChatControl(m, { _, _ -> }, { _, _ -> })
        ui.setCallChat(CallChatHandle(m, control))
        m.openPanel()
        m.addReceived("p1", 1uL, "a", "Anna")
        // System back closes the open panel first.
        assertTrue(ui.closeCallChatPanel())
        assertFalse(ui.closeCallChatPanel())
        ui.setCallChat(null)
        assertTrue(m.messages.value.isEmpty())
        assertNull(ui.callChat.value)
    }

    @Test
    fun `the sender's name shows above the first of a run from one other person`() {
        val m = model()
        m.addReceived("p1", 1uL, "a", "Anna")
        m.addReceived("p1", 2uL, "b", "Anna")
        m.addOwn(1uL, "c", sent = true)
        m.addReceived("p1", 3uL, "d", "Anna")
        m.addReceived("p2", 1uL, "e", "Ben")
        val list = m.messages.value
        assertEquals(listOf(true, false, false, true, true), list.indices.map { callChatShowsSender(list, it) })
    }

    @Test
    fun `the name over a bubble is the tile's, then the one it had when it arrived`() {
        val msg = CallChatMessage("p1:1", own = false, peerId = "p1", seq = 1uL, text = "x", atMs = 0, senderName = "Old")
        assertEquals("New", callChatSenderName(msg, "New"))
        // The sender left the call: their tile is gone.
        assertEquals("Old", callChatSenderName(msg, null))
        assertEquals("Old", callChatSenderName(msg, "  "))
    }

    @Test
    fun `the composer makes line ends LF and stops at the limit with the too-long line`() {
        assertEquals(CallChatInput("a\nb\nc", tooLong = false), callChatInput("a\r\nb\rc", rule))
        // Cut: the field takes no more and says so.
        assertEquals(CallChatInput("12345678", tooLong = true), callChatInput("123456789", rule))
        // Exactly at the limit: no more fits.
        assertTrue(callChatInput("12345678", rule).tooLong)
        assertFalse(callChatInput("1234", rule).tooLong)
    }

    @Test
    fun `send is enabled only for text the rule accepts`() {
        assertFalse(callChatCanSend("", rule))
        assertFalse(callChatCanSend("  \n ", rule))
        assertFalse(callChatCanSend("a\u0007", rule))
        assertTrue(callChatCanSend("hi\nyou", rule))
    }

    @Test
    fun `the panel is a column at 900 dp and wider`() {
        assertFalse(callChatIsColumn(411f))
        assertFalse(callChatIsColumn(899f))
        assertTrue(callChatIsColumn(900f))
        assertTrue(callChatIsColumn(1280f))
    }

    @Test
    fun `the preview shows only with the panel closed, the bar up and nothing over the call`() {
        val p = CallChatPreview(1, "p1:1", "p1", "Anna", "hi")
        assertTrue(callChatPreviewShown(p, panelOpen = false, controlsVisible = true, overlayOpen = false))
        assertFalse(callChatPreviewShown(null, panelOpen = false, controlsVisible = true, overlayOpen = false))
        assertFalse(callChatPreviewShown(p, panelOpen = true, controlsVisible = true, overlayOpen = false))
        assertFalse(callChatPreviewShown(p, panelOpen = false, controlsVisible = false, overlayOpen = false))
        assertFalse(callChatPreviewShown(p, panelOpen = false, controlsVisible = true, overlayOpen = true))
    }
}
