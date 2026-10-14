// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Which camera senders encode and send (`SenderActivity.kt`, ADR-0070
 * "senders nobody watches"): the decision from the SFU's `video-needed`
 * word, the people count and the camera, and its application to a sender's
 * encoding. The test list follows the web client's `sender-activity.test.ts`.
 */
class SenderActivityTest {

    private val main = CameraSenderKind.MAIN
    private val middle = CameraSenderKind.MIDDLE
    private val light = CameraSenderKind.LIGHT

    private fun input(
        cameraOn: Boolean = true,
        exists: Boolean = true,
        middleWanted: Boolean = true,
        needed: VideoNeeded? = null,
    ) = SenderActivityInput(cameraOn, exists, middleWanted, needed)

    // ---- the SFU's word ------------------------------------------------------

    @Test
    fun `before any frame the SFU has said nothing, and never says anything of the light sender`() {
        for (kind in CAMERA_SENDER_KINDS) assertNull(neededWord(kind, null))
        val needed = VideoNeeded(main = false, middle = true)
        assertEquals(false, neededWord(main, needed))
        assertEquals(true, neededWord(middle, needed))
        assertNull(neededWord(light, needed))
    }

    // ---- the decision --------------------------------------------------------

    @Test
    fun `before the first frame every existing sender of a camera that is on is active`() {
        for (kind in CAMERA_SENDER_KINDS) assertTrue(senderActive(kind, input()))
    }

    @Test
    fun `the SFU's false switches the main and the middle sender off, never the light one`() {
        val nobody = VideoNeeded(main = false, middle = false)
        assertFalse(senderActive(main, input(needed = nobody)))
        assertFalse(senderActive(middle, input(needed = nobody)))
        assertTrue(senderActive(light, input(needed = nobody)))

        val mainOnly = VideoNeeded(main = true, middle = false)
        assertTrue(senderActive(main, input(needed = mainOnly)))
        assertFalse(senderActive(middle, input(needed = mainOnly)))
    }

    @Test
    fun `the middle sender also needs the people count, whatever the SFU says`() {
        val everything = VideoNeeded(main = true, middle = true)
        assertFalse(senderActive(middle, input(middleWanted = false, needed = everything)))
        assertFalse(senderActive(middle, input(middleWanted = false)))
        // The count is read for the middle sender only.
        assertTrue(senderActive(main, input(middleWanted = false, needed = everything)))
        assertTrue(senderActive(light, input(middleWanted = false, needed = everything)))
    }

    @Test
    fun `no sender is active with the camera off or without the sender`() {
        for (kind in CAMERA_SENDER_KINDS) {
            assertFalse(senderActive(kind, input(cameraOn = false)))
            assertFalse(senderActive(kind, input(exists = false)))
        }
    }

    @Test
    fun `the decision for every sender at once`() {
        val active = sendersActive(
            cameraOn = true,
            middleWanted = true,
            needed = VideoNeeded(main = false, middle = true),
            exists = { it != light },
        )
        assertEquals(mapOf(main to false, middle to true, light to false), active)
    }

    // ---- the log lines ---------------------------------------------------------

    @Test
    fun `a sender's tail names the SFU's word and whether it is active`() {
        assertEquals(" needed=yes active=yes", describeSenderActivity(SenderActivityNote(true, true)))
        assertEquals(" needed=no active=no", describeSenderActivity(SenderActivityNote(false, false)))
        assertEquals(" needed=- active=yes", describeSenderActivity(SenderActivityNote(null, true)))
    }

    @Test
    fun `a video-needed frame is one line when it changed what is active, and none otherwise`() {
        val all = mapOf(main to true, middle to true, light to true)
        val needed = VideoNeeded(main = false, middle = true)
        assertEquals(
            "call video needed main=no middle=yes active main=no middle=yes light=yes",
            describeVideoNeededChange(needed, all, all + (main to false)),
        )
        assertNull(describeVideoNeededChange(needed, all, all))
    }

    // ---- applying it to a sender -------------------------------------------------

    /** A sender whose one encoding's flag is [active]; refuses what [refuse] names. */
    private class FakeSender(var active: Boolean? = true) : ActivitySender {
        var refuse: (Boolean) -> Boolean = { false }
        var throwOnSet = false
        val sets = ArrayList<Boolean>()

        override fun encodingActive(): Boolean? = active

        override fun setEncodingActive(active: Boolean): Boolean {
            sets += active
            if (throwOnSet) throw IllegalStateException("disposed")
            if (refuse(active)) return false
            this.active = active
            return true
        }
    }

    @Test
    fun `the flag is set only when the encoding says otherwise`() {
        val sender = FakeSender(active = true)
        assertEquals(ActivityOutcome.APPLIED, applyEncodingActive(sender, true))
        assertTrue(sender.sets.isEmpty())
        assertEquals(ActivityOutcome.APPLIED, applyEncodingActive(sender, false))
        assertEquals(listOf(false), sender.sets)
        assertEquals(false, sender.active)
    }

    @Test
    fun `a sender with no encoding, a refusal and a throw are all answered, never thrown`() {
        assertEquals(ActivityOutcome.NO_ENCODING, applyEncodingActive(FakeSender(active = null), true))
        assertEquals(
            ActivityOutcome.REFUSED,
            applyEncodingActive(FakeSender().apply { refuse = { true } }, false),
        )
        assertEquals(
            ActivityOutcome.REFUSED,
            applyEncodingActive(FakeSender().apply { throwOnSet = true }, false),
        )
    }

    @Test
    fun `needed to active on a bound sender, and the last word wins`() {
        val sender = FakeSender()
        val activity = SenderActivity("main")
        // Nothing is bound yet: what is asked is remembered, nothing is set.
        assertNull(activity.want(false))
        assertFalse(activity.bound)
        activity.bind(sender)
        assertTrue(activity.bound)
        activity.want(false)
        activity.want(true)
        activity.want(false)
        assertEquals(listOf(false, true, false), sender.sets)
        assertEquals(false, sender.active)
        assertEquals(false, activity.lastWanted)
    }

    @Test
    fun `asking again puts back a flag the platform reset`() {
        val sender = FakeSender()
        val activity = SenderActivity("middle").apply { bind(sender) }
        activity.want(false)
        // A renegotiation switched the encoding back on.
        sender.active = true
        activity.want(false)
        assertEquals(listOf(false, false), sender.sets)
        assertEquals(false, sender.active)
    }

    @Test
    fun `a refused switch-off is not asked again, and the sender goes on sending`() {
        val warnings = ArrayList<String>()
        val sender = FakeSender().apply { refuse = { !it } }
        val activity = SenderActivity("main") { warnings += it }.apply { bind(sender) }
        assertEquals(ActivityOutcome.REFUSED, activity.want(false))
        activity.want(false)
        activity.want(false)
        assertEquals(listOf(false), sender.sets)
        assertEquals(true, sender.active)
        assertEquals(
            listOf("camera sender activity: the main camera sender could not be stopped, it goes on sending"),
            warnings,
        )
        // Something else is wanted, then off again: it is asked once more.
        activity.want(true)
        activity.want(false)
        assertEquals(listOf(false, false), sender.sets)
    }

    @Test
    fun `a refused switch-on is asked again at every later ask, and said once`() {
        val warnings = ArrayList<String>()
        val sender = FakeSender(active = false).apply { refuse = { it } }
        val activity = SenderActivity("middle") { warnings += it }.apply { bind(sender) }
        activity.want(true)
        activity.want(true)
        assertEquals(listOf(true, true), sender.sets)
        assertEquals(1, warnings.size)
        assertTrue(warnings.single().endsWith("could not be started, asking again"))
        // The platform gives in.
        sender.refuse = { false }
        assertEquals(ActivityOutcome.APPLIED, activity.want(true))
        assertEquals(true, sender.active)
    }

    @Test
    fun `a different sender starts from nothing`() {
        val warnings = ArrayList<String>()
        val first = FakeSender().apply { refuse = { !it } }
        val activity = SenderActivity("light") { warnings += it }.apply { bind(first) }
        activity.want(false)
        val second = FakeSender().apply { refuse = { !it } }
        activity.bind(second)
        // The refusal of the first sender is not held against the second,
        // and its warning may be said.
        activity.want(false)
        assertEquals(listOf(false), second.sets)
        assertEquals(2, warnings.size)
        // Binding the same sender again changes nothing.
        activity.bind(second)
        activity.want(false)
        assertEquals(listOf(false), second.sets)

        activity.reset()
        assertFalse(activity.bound)
        assertNull(activity.lastWanted)
    }
}
