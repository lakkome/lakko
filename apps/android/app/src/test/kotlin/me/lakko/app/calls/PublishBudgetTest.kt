// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.webrtc.RtpParameters
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.concurrent.thread

/**
 * The SFU's ceiling on this device's camera senders (`PublishBudget.kt`;
 * ADR-0068, amendment of 2026-10-11): the frame, the one mapping from the
 * frame to a sender's ceiling, and its application to a sender, on the one
 * serial path every write to that sender's parameters takes
 * (`SenderParameters.kt`). The test list follows the web client's
 * `publish-budget.test.ts`.
 */
class PublishBudgetTest {

    private val main = CameraSenderKind.MAIN
    private val middle = CameraSenderKind.MIDDLE
    private val light = CameraSenderKind.LIGHT

    // ---- the frame -----------------------------------------------------------

    @Test
    fun `the frame's two values are the two ceilings`() {
        assertEquals(PublishBudget(2500, 500), readPublishBudget(2500, 500))
        assertEquals(PublishBudget(1, MAX_PUBLISH_BUDGET_KBPS), readPublishBudget(1, MAX_PUBLISH_BUDGET_KBPS.toLong()))
    }

    @Test
    fun `a ceiling of zero or above the protocol's bound refuses the frame`() {
        assertNull(readPublishBudget(0, 500))
        assertNull(readPublishBudget(900, 0))
        assertNull(readPublishBudget(MAX_PUBLISH_BUDGET_KBPS + 1L, 500))
        assertNull(readPublishBudget(900, 4_000_000_000L))
        assertNull(readPublishBudget(-1, 500))
    }

    @Test
    fun `a refused frame that says it is a publish-budget one is recognised`() {
        assertTrue(claimsPublishBudget("""{"kind":"publish-budget","main_kbps":0}"""))
        assertTrue(claimsPublishBudget("""{ "main_kbps": 5, "kind" : "publish-budget" }"""))
        assertFalse(claimsPublishBudget("""{"kind":"video-needed","main":true,"middle":true}"""))
        assertFalse(claimsPublishBudget("not json"))
        assertFalse(claimsPublishBudget("null"))
        assertFalse(claimsPublishBudget("\"publish-budget\""))
    }

    // ---- the mapping ---------------------------------------------------------

    @Test
    fun `nothing is written before the first frame, so each sender keeps what it was created with`() {
        for (kind in CAMERA_SENDER_KINDS) assertNull(senderCeiling(kind, null))
        // What they were created with: the main one no ceiling, the middle
        // one 500 kbit/s, the light one 150.
        val plan = cameraSenderPlan(3) { true }.associateBy { it.kind }
        assertNull(plan.getValue(main).maxBitrateBps)
        assertEquals(500_000, plan.getValue(middle).maxBitrateBps)
        assertEquals(150_000, plan.getValue(light).maxBitrateBps)
        assertNull(encodingCeiling(listOf(cameraMainEncoding())))
        assertEquals(500_000, encodingCeiling(listOf(cameraCopyEncoding(CameraCopyKind.MIDDLE))))
        assertEquals(150_000, encodingCeiling(listOf(cameraCopyEncoding(CameraCopyKind.LIGHT))))
    }

    @Test
    fun `the main and the middle sender get the frame's figure, in bits a second`() {
        val budget = PublishBudget(mainKbps = 1200, middleKbps = 483)
        assertEquals(1_200_000, senderCeiling(main, budget))
        assertEquals(483_000, senderCeiling(middle, budget))
    }

    @Test
    fun `the light sender is never written`() {
        assertNull(senderCeiling(light, PublishBudget(200, 100)))
        assertEquals(listOf(main, middle), BUDGETED_SENDER_KINDS)
    }

    @Test
    fun `the server's figure is applied as it stands, with no client-side clamp or share`() {
        assertEquals(
            MAX_PUBLISH_BUDGET_KBPS * 1000,
            senderCeiling(middle, PublishBudget(1, MAX_PUBLISH_BUDGET_KBPS)),
        )
        assertEquals(1000, senderCeiling(main, PublishBudget(1, 1)))
    }

    // ---- one read-change-set -------------------------------------------------

    /**
     * A sender as libwebrtc's is: every read hands out a fresh copy of what
     * it holds, and a set replaces what it holds with what it is handed.
     */
    private class FakeSender(first: RtpParameters.Encoding? = RtpParameters.Encoding(null, true, null)) :
        CeilingSender, ActivitySender {
        var held: List<RtpParameters.Encoding> = listOfNotNull(first)
        var refuse = false
        var throwOnRead = false

        /** The ceiling each accepted or refused set carried, in order. */
        val asked = ArrayList<Int?>()

        fun read(): List<RtpParameters.Encoding> {
            if (throwOnRead) throw IllegalStateException("closed")
            return held.map(::copy)
        }

        fun set(encodings: List<RtpParameters.Encoding>): Boolean {
            asked += encodings.firstOrNull()?.maxBitrateBps
            if (refuse) return false
            held = encodings.map(::copy)
            return true
        }

        val first: RtpParameters.Encoding? get() = held.firstOrNull()

        override fun setEncodingCeiling(maxBitrateBps: Int): CeilingOutcome = serialParameterWrite(this) {
            val encodings = read()
            applyEncodingCeiling(encodings, maxBitrateBps) { set(encodings) }
        }

        override fun encodingActive(): Boolean? = read().firstOrNull()?.active

        override fun setEncodingActive(active: Boolean): Boolean = serialParameterWrite(this) {
            val encodings = read()
            val e = encodings.firstOrNull() ?: return@serialParameterWrite false
            e.active = active
            set(encodings)
        }

        companion object {
            fun copy(e: RtpParameters.Encoding): RtpParameters.Encoding =
                RtpParameters.Encoding(e.rid, e.active, e.scaleResolutionDownBy).apply {
                    maxBitrateBps = e.maxBitrateBps
                    scalabilityMode = e.scalabilityMode
                }
        }
    }

    @Test
    fun `nothing is set when the encoding already holds the ceiling`() {
        val encodings = listOf(cameraCopyEncoding(CameraCopyKind.MIDDLE))
        var sets = 0
        assertEquals(CeilingOutcome.APPLIED, applyEncodingCeiling(encodings, 500_000) { sets += 1; true })
        assertEquals(0, sets)
    }

    @Test
    fun `only the ceiling changes, the rest of the encoding is kept`() {
        val encodings = listOf(cameraCopyEncoding(CameraCopyKind.MIDDLE, active = false))
        assertEquals(CeilingOutcome.APPLIED, applyEncodingCeiling(encodings, 483_000) { true })
        val e = encodings.single()
        assertEquals(483_000, e.maxBitrateBps)
        assertEquals(CAMERA_MIDDLE_SCALE_DOWN, e.scaleResolutionDownBy)
        assertFalse(e.active)
    }

    @Test
    fun `a sender that had no ceiling gets one`() {
        val encodings = listOf(cameraMainEncoding())
        assertEquals(CeilingOutcome.APPLIED, applyEncodingCeiling(encodings, 1_200_000) { true })
        assertEquals(1_200_000, encodings.single().maxBitrateBps)
        assertEquals(CAMERA_SCALABILITY_MODE, encodings.single().scalabilityMode)
    }

    @Test
    fun `a sender with no encoding, a refusal and a throw are reported, never thrown`() {
        assertEquals(CeilingOutcome.NO_ENCODING, applyEncodingCeiling(emptyList(), 1000) { true })
        assertEquals(CeilingOutcome.REFUSED, applyEncodingCeiling(listOf(cameraMainEncoding()), 1000) { false })
        assertEquals(
            CeilingOutcome.REFUSED,
            applyEncodingCeiling(listOf(cameraMainEncoding()), 1000) { throw IllegalStateException("closed") },
        )
    }

    @Test
    fun `what an encoding holds is read, and nothing as null`() {
        assertEquals(150_000, encodingCeiling(listOf(cameraCopyEncoding(CameraCopyKind.LIGHT))))
        assertNull(encodingCeiling(listOf(cameraMainEncoding())))
        assertNull(encodingCeiling(emptyList()))
    }

    // ---- keeping a sender under the ceiling last asked -----------------------

    private val warnings = ArrayList<String>()
    private fun ceiling(name: String = "main") = SenderCeiling(name) { warnings += it }

    @Test
    fun `the ceiling is set, and again when it changes`() {
        val c = ceiling()
        val s = FakeSender(cameraMainEncoding())
        c.bind(s)
        assertEquals(CeilingOutcome.APPLIED, c.want(1_200_000))
        assertEquals(1_200_000, s.first?.maxBitrateBps)
        c.want(683_000)
        assertEquals(683_000, s.first?.maxBitrateBps)
        assertEquals(listOf<Int?>(1_200_000, 683_000), s.asked)
        // Asking again for what it holds costs no set.
        c.want(683_000)
        assertEquals(2, s.asked.size)
        assertEquals(CAMERA_SCALABILITY_MODE, s.first?.scalabilityMode)
        assertTrue(warnings.isEmpty())
    }

    @Test
    fun `nothing is written before the first word, so the plan's own ceiling stands`() {
        val c = ceiling("middle")
        val s = FakeSender(cameraCopyEncoding(CameraCopyKind.MIDDLE))
        c.bind(s)
        assertNull(c.want(null))
        assertTrue(s.asked.isEmpty())
        assertEquals(500_000, s.first?.maxBitrateBps)
        assertNull(c.lastWanted)
    }

    @Test
    fun `the last word wins`() {
        val c = ceiling()
        val s = FakeSender()
        c.bind(s)
        c.want(2_000_000)
        c.want(1_000_000)
        c.want(700_000)
        assertEquals(700_000, s.first?.maxBitrateBps)
        assertEquals(700_000, c.lastWanted)
    }

    @Test
    fun `a refused write is asked again at every later look, and said once`() {
        val c = ceiling()
        val s = FakeSender()
        s.refuse = true
        c.bind(s)
        assertEquals(CeilingOutcome.REFUSED, c.want(900_000))
        assertNull(s.first?.maxBitrateBps)
        assertEquals(1, warnings.size)
        assertTrue(warnings.single().contains("main camera sender's bitrate ceiling could not be set"))
        // The next tick asks again; still refused, not said again.
        c.want(900_000)
        assertEquals(2, s.asked.size)
        assertEquals(1, warnings.size)
        // libwebrtc gives in.
        s.refuse = false
        assertEquals(CeilingOutcome.APPLIED, c.want(900_000))
        assertEquals(900_000, s.first?.maxBitrateBps)
    }

    @Test
    fun `a sender that throws is a refusal, not a crash`() {
        val c = ceiling()
        val s = FakeSender()
        s.throwOnRead = true
        c.bind(s)
        assertEquals(CeilingOutcome.REFUSED, c.want(900_000))
        assertEquals(1, warnings.size)
    }

    @Test
    fun `the ceiling is put back when asked again after libwebrtc lost it`() {
        val c = ceiling()
        val s = FakeSender()
        c.bind(s)
        c.want(900_000)
        // A renegotiation or a replaced track reset the encoding.
        s.held = listOf(RtpParameters.Encoding(null, true, null))
        c.want(900_000)
        assertEquals(900_000, s.first?.maxBitrateBps)
    }

    @Test
    fun `a sender created again gets the ceiling in force, not the one it was created with`() {
        val c = ceiling("middle")
        val first = FakeSender(cameraCopyEncoding(CameraCopyKind.MIDDLE))
        c.bind(first)
        c.want(300_000)
        assertEquals(300_000, first.first?.maxBitrateBps)
        // The middle sender is removed and added again with the plan's ceiling.
        val second = FakeSender(cameraCopyEncoding(CameraCopyKind.MIDDLE))
        c.bind(second)
        c.want(c.lastWanted)
        assertEquals(300_000, second.first?.maxBitrateBps)
        assertEquals(1, first.asked.size)
        assertTrue(warnings.isEmpty())
    }

    @Test
    fun `a new sender may say its own warning`() {
        val c = ceiling()
        val first = FakeSender().apply { refuse = true }
        c.bind(first)
        c.want(900_000)
        val second = FakeSender().apply { refuse = true }
        c.bind(second)
        c.want(900_000)
        assertEquals(2, warnings.size)
    }

    @Test
    fun `nothing happens without a sender, and the last word is applied once there is one`() {
        val c = ceiling()
        assertNull(c.want(800_000))
        assertFalse(c.bound)
        val s = FakeSender()
        c.bind(s)
        c.want(c.lastWanted)
        assertEquals(800_000, s.first?.maxBitrateBps)
    }

    @Test
    fun `everything is forgotten at the end of the call`() {
        val c = ceiling()
        val s = FakeSender()
        c.bind(s)
        c.want(900_000)
        c.reset()
        assertFalse(c.bound)
        assertNull(c.lastWanted)
        // The next call's sender is not written before that call's first frame.
        val next = FakeSender()
        c.bind(next)
        c.want(null)
        assertTrue(next.asked.isEmpty())
    }

    // ---- the ceiling and the other writes to one sender's parameters ---------

    @Test
    fun `the ceiling and the active flag do not undo each other`() {
        val s = FakeSender(cameraMainEncoding())
        val activity = SenderActivity("main")
        val c = ceiling()
        activity.bind(s)
        c.bind(s)
        activity.want(false)
        c.want(900_000)
        assertEquals(false, s.first?.active)
        assertEquals(900_000, s.first?.maxBitrateBps)
        activity.want(true)
        assertEquals(true, s.first?.active)
        assertEquals(900_000, s.first?.maxBitrateBps)
        c.want(400_000)
        assertEquals(true, s.first?.active)
        assertEquals(400_000, s.first?.maxBitrateBps)
        assertEquals(CAMERA_SCALABILITY_MODE, s.first?.scalabilityMode)
    }

    @Test
    fun `writes of one sender never overlap, whatever threads they come from`() {
        val sender = Any()
        val inside = AtomicInteger(0)
        val overlaps = AtomicInteger(0)
        val start = CountDownLatch(1)
        val threads = (0 until 8).map {
            thread {
                start.await()
                repeat(200) {
                    serialParameterWrite(sender) {
                        if (inside.incrementAndGet() != 1) overlaps.incrementAndGet()
                        Thread.yield()
                        inside.decrementAndGet()
                    }
                }
            }
        }
        start.countDown()
        threads.forEach { it.join(10_000) }
        assertEquals(0, overlaps.get())
    }

    @Test
    fun `two writers of different fields on two threads both land`() {
        // Each read-change-set reads inside the path, so neither sets an
        // encoding read before the other's write.
        repeat(50) {
            val s = FakeSender(cameraMainEncoding())
            val start = CountDownLatch(1)
            val a = thread {
                start.await()
                s.setEncodingActive(false)
            }
            val b = thread {
                start.await()
                s.setEncodingCeiling(900_000)
            }
            start.countDown()
            a.join(10_000)
            b.join(10_000)
            assertEquals(false, s.first?.active)
            assertEquals(900_000, s.first?.maxBitrateBps)
        }
    }

    @Test
    fun `one sender's write does not hold another sender's back, and a write may contain another`() {
        val one = Any()
        val other = Any()
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val holder = thread {
            serialParameterWrite(one) {
                entered.countDown()
                release.await(10, TimeUnit.SECONDS)
            }
        }
        assertTrue(entered.await(10, TimeUnit.SECONDS))
        assertEquals("other", serialParameterWrite(other) { "other" })
        release.countDown()
        holder.join(10_000)
        assertEquals(2, serialParameterWrite(one) { serialParameterWrite(one) { 2 } })
    }

    @Test
    fun `a failed write does not hold the rest back`() {
        val sender = Any()
        val order = ArrayList<String>()
        try {
            serialParameterWrite(sender) {
                order += "first"
                throw IllegalStateException("refused")
            }
        } catch (expected: IllegalStateException) {
            order += "thrown"
        }
        assertEquals(2, serialParameterWrite(sender) { order += "second"; 2 })
        assertEquals(listOf("first", "thrown", "second"), order)
    }

    // ---- the diagnostics -----------------------------------------------------

    @Test
    fun `one line says the budget changed, and nothing when a frame repeats it`() {
        assertEquals(
            "call publish budget main=1922 middle=500",
            describePublishBudgetChange(null, PublishBudget(1922, 500)),
        )
        assertEquals(
            "call publish budget main=200 middle=483",
            describePublishBudgetChange(PublishBudget(1922, 500), PublishBudget(200, 483)),
        )
        assertNull(describePublishBudgetChange(PublishBudget(200, 483), PublishBudget(200, 483)))
    }

    @Test
    fun `a send video line gains its ceiling tail in kbit a second`() {
        assertEquals(" ceiling=1200", describeSenderCeiling(SenderCeilingNote(1_200_000)))
        assertEquals(" ceiling=150", describeSenderCeiling(SenderCeilingNote(150_999)))
        assertEquals(" ceiling=-", describeSenderCeiling(SenderCeilingNote(null)))
        assertEquals("", describeSenderCeiling(null))
    }
}
