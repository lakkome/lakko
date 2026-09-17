// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The outbound wake policy: which call-signaling frames ring a closed phone.
 *
 * ## Why the fixtures are built from field NUMBERS
 *
 * The real composer is `uniffi.lakko_ffi_uniffi.composeCallSignaling`, and it
 * cannot run here — this is the plain-JVM `test` source set, where
 * `liblakko_ffi_uniffi.so` does not exist (the same host-vs-instrumented split
 * `MediaStateMappingTest` and `GroupCallWireFixtures` document). So the rule
 * under test reads a tag table, and a test written against hand-copied tag
 * bytes would agree with a hand-copied table for the wrong reason: both would
 * keep passing after a wire change that moved the field numbers.
 *
 * These fixtures therefore start from the `oneof body` field numbers in
 * `schemas/mls-app/call_signaling_v1.proto` — the schema is the source of
 * truth, per the repo's "schemas/ is the single source of truth" rule — and
 * derive the tag varint the way protobuf does, `(field shl 3) or 2`. If field
 * 13 stops being `end`, [`bodyFieldNumbers`] is what a reader must change, and
 * changing it is what breaks these tests.
 *
 * `groupLeave` at field 16 is the case worth having: its tag is 130, a TWO-byte
 * varint, which is exactly how a first-byte-only sniff silently misses it.
 */
class CallSignalingWakeTest {

    /**
     * `CallSignalingPayload.body` field numbers, verbatim from
     * `schemas/mls-app/call_signaling_v1.proto` lines 56-62.
     */
    private val bodyFieldNumbers: Map<CallSignalingBodyKind, Int> = mapOf(
        CallSignalingBodyKind.OFFER to 10,
        CallSignalingBodyKind.ANSWER to 11,
        CallSignalingBodyKind.CANDIDATE to 12,
        CallSignalingBodyKind.END to 13,
        CallSignalingBodyKind.GROUP_START to 14,
        CallSignalingBodyKind.GROUP_JOIN to 15,
        CallSignalingBodyKind.GROUP_LEAVE to 16,
    )

    private val callId: ByteArray = ByteArray(16) { (0xA0 + it).toByte() }

    /**
     * A whole `CallSignalingPayload` carrying [kind]'s body, encoded the way
     * prost does: the oneof tag, the body length, then the body's own fields in
     * number order. Every body in the schema declares `bytes call_id = 1`, so
     * every frame opens `0x0A 0x10` inside its body — which is the header the
     * group-tag arm checks for.
     */
    private fun frame(kind: CallSignalingBodyKind, trailing: ByteArray = ByteArray(0)): ByteArray {
        val field = requireNotNull(bodyFieldNumbers[kind])
        val body = byteArrayOf(0x0A, callId.size.toByte()) + callId + trailing
        return varint(((field.toLong() shl 3) or 2L)) + varint(body.size.toLong()) + body
    }

    private fun varint(value: Long): ByteArray {
        val out = ArrayList<Byte>()
        var v = value
        while (true) {
            if (v and 0x7FL.inv() == 0L) {
                out.add(v.toByte())
                return out.toByteArray()
            }
            out.add(((v and 0x7F) or 0x80).toByte())
            v = v ushr 7
        }
    }

    // -----------------------------------------------------------------
    // The table itself, pinned against the schema's field numbers
    // -----------------------------------------------------------------

    @Test
    fun `every body field number in the schema maps back to its own kind`() {
        for ((kind, _) in bodyFieldNumbers) {
            assertEquals("field ${bodyFieldNumbers[kind]}", kind, callSignalingBodyKind(frame(kind)))
        }
    }

    @Test
    fun `the derived tags are the bytes the table hard-codes`() {
        // Belt and braces: the arithmetic above is only a pin if it produces
        // the tags the shipped wire actually carries. These are the values
        // `apps/web/src/calls/call-signaling-wake.ts` and the service-worker
        // mirror independently encode, so a mismatch here means one of the
        // three drifted.
        assertEquals(0x52, frame(CallSignalingBodyKind.OFFER)[0].toInt() and 0xFF)
        assertEquals(0x5A, frame(CallSignalingBodyKind.ANSWER)[0].toInt() and 0xFF)
        assertEquals(0x62, frame(CallSignalingBodyKind.CANDIDATE)[0].toInt() and 0xFF)
        assertEquals(0x6A, frame(CallSignalingBodyKind.END)[0].toInt() and 0xFF)
        assertEquals(0x72, frame(CallSignalingBodyKind.GROUP_START)[0].toInt() and 0xFF)
        assertEquals(0x7A, frame(CallSignalingBodyKind.GROUP_JOIN)[0].toInt() and 0xFF)
        val leave = frame(CallSignalingBodyKind.GROUP_LEAVE)
        assertEquals(0x82, leave[0].toInt() and 0xFF)
        assertEquals(0x01, leave[1].toInt() and 0xFF)
    }

    @Test
    fun `the hand-encoded group fixtures the receive path uses are named too`() {
        assertEquals(CallSignalingBodyKind.GROUP_START, callSignalingBodyKind(GroupCallWireFixtures.groupStart()))
        assertEquals(CallSignalingBodyKind.GROUP_JOIN, callSignalingBodyKind(GroupCallWireFixtures.groupJoin()))
        assertEquals(CallSignalingBodyKind.GROUP_LEAVE, callSignalingBodyKind(GroupCallWireFixtures.groupLeave()))
    }

    @Test
    fun `anything that is not call signaling has no kind`() {
        assertNull(callSignalingBodyKind(ByteArray(0)))
        // A chat envelope is JSON.
        assertNull(callSignalingBodyKind("""{"v":1,"text":"hi"}""".toByteArray(Charsets.UTF_8)))
        // Every `lakko/...` service envelope starts with `l`.
        assertNull(callSignalingBodyKind("lakko/profile/v1".toByteArray(Charsets.UTF_8)))
        assertNull(callSignalingBodyKind("lakko/committer-beacon/v1".toByteArray(Charsets.UTF_8)))
        // 0x72 and 0x7A are `r` and `z`: plain text opening with either must
        // not be swallowed as a group frame.
        assertNull(callSignalingBodyKind("rabbit".toByteArray(Charsets.UTF_8)))
        assertNull(callSignalingBodyKind("zebra".toByteArray(Charsets.UTF_8)))
        // A lone 0x82 without its continuation byte is not a group-leave tag.
        assertNull(callSignalingBodyKind(byteArrayOf(0x82.toByte())))
        assertNull(callSignalingBodyKind(byteArrayOf(0x82.toByte(), 0x02)))
    }

    // -----------------------------------------------------------------
    // The decision
    // -----------------------------------------------------------------

    @Test
    fun `an offer wakes — it is the ring itself`() {
        assertTrue(shouldWakeForCallSignaling(frame(CallSignalingBodyKind.OFFER)))
        // A video offer is the same body with more fields set after call_id.
        val withSdp = frame(CallSignalingBodyKind.OFFER, trailing = byteArrayOf(0x12, 0x03) + "v=0".toByteArray())
        assertTrue(shouldWakeForCallSignaling(withSdp))
    }

    @Test
    fun `an end wakes — so a cancelled call stops ringing a closed app`() {
        assertTrue(shouldWakeForCallSignaling(frame(CallSignalingBodyKind.END)))
        // Every end reason is the same body; the reason is a later field.
        for (reason in 0..5) {
            val trailing = if (reason == 0) ByteArray(0) else byteArrayOf(0x10, reason.toByte())
            assertTrue(shouldWakeForCallSignaling(frame(CallSignalingBodyKind.END, trailing)))
        }
    }

    @Test
    fun `answer and candidate stay silent — the peer is already awake`() {
        assertFalse(shouldWakeForCallSignaling(frame(CallSignalingBodyKind.ANSWER)))
        assertFalse(shouldWakeForCallSignaling(frame(CallSignalingBodyKind.CANDIDATE)))
    }

    @Test
    fun `the group-call control frames stay silent`() {
        assertFalse(shouldWakeForCallSignaling(frame(CallSignalingBodyKind.GROUP_START)))
        assertFalse(shouldWakeForCallSignaling(frame(CallSignalingBodyKind.GROUP_JOIN)))
        assertFalse(shouldWakeForCallSignaling(frame(CallSignalingBodyKind.GROUP_LEAVE)))
        // And the fixtures the group-call path actually publishes.
        assertFalse(shouldWakeForCallSignaling(GroupCallWireFixtures.groupStart()))
        assertFalse(shouldWakeForCallSignaling(GroupCallWireFixtures.groupJoin()))
        assertFalse(shouldWakeForCallSignaling(GroupCallWireFixtures.groupLeave()))
    }

    @Test
    fun `exactly two of the seven bodies wake`() {
        val waking = bodyFieldNumbers.keys.filter { shouldWakeForCallSignaling(frame(it)) }.toSet()
        assertEquals(setOf(CallSignalingBodyKind.OFFER, CallSignalingBodyKind.END), waking)
    }

    @Test
    fun `bytes that are not call signaling fail toward silence`() {
        assertFalse(shouldWakeForCallSignaling(ByteArray(0)))
        assertFalse(shouldWakeForCallSignaling("""{"v":1,"text":"hi"}""".toByteArray(Charsets.UTF_8)))
        assertFalse(shouldWakeForCallSignaling("lakko/group-name/v1".toByteArray(Charsets.UTF_8)))
    }
}
