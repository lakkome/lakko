// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.CallCurrentScreenShare
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody
import uniffi.lakko_ffi_uniffi.VerifiedCallScreenShare

/**
 * Receiving a screen share (ADR-0083): the client's wiring around the shared
 * fold, the stream ids a share arrives under, and the layout and
 * announcement rules the call screen draws from.
 *
 * The fold itself is `lakko-calls::screen_share` and the host JVM cannot load
 * the core, so [ModelFold] stands in for it, following the rule of ADR-0083
 * section 2 closely enough for the wiring to be visible (the rule itself is
 * tested in the core).
 */
class ScreenShareReceiveTest {

    private val anna = ByteArray(32) { 0x0a }
    private val boris = ByteArray(32) { 0x0b }
    private val own = ByteArray(32) { 0x01 }

    /** The ADR-0083 section 2 fold, as plainly as it can be written. */
    private class ModelFold(private val ownKey: ByteArray) : ScreenShareFold {
        private data class Id(val seq: ULong, val key: String)
        private val live = LinkedHashMap<Id, VerifiedCallScreenShare>()
        private val stopped = HashSet<Id>()
        var closed = false

        private fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it.toInt() and 0xff) }

        override fun observe(state: VerifiedCallScreenShare): Boolean {
            val before = current()
            val id = Id(state.seq, hex(state.signerKey))
            if (state.sharing) {
                if (id !in stopped) live[id] = state
            } else {
                stopped += id
                live.remove(id)
            }
            return before != current()
        }

        override fun participantLeft(signerKey: ByteArray): Boolean {
            val before = current()
            live.keys.filter { it.key == hex(signerKey) }.forEach { stopped += it; live.remove(it) }
            return before != current()
        }

        override fun current(): CallCurrentScreenShare? {
            val top = live.entries.maxWithOrNull(compareBy({ it.key.seq }, { it.key.key }))?.value ?: return null
            return CallCurrentScreenShare(
                seq = top.seq,
                signerKey = top.signerKey,
                peerId = top.peerId,
                withSound = top.withSound,
                accountKey = top.accountKey,
                isOwn = top.signerKey.contentEquals(ownKey),
            )
        }

        override fun close() {
            closed = true
        }
    }

    private fun state(seq: Int, key: ByteArray, peer: String, sharing: Boolean = true) =
        VerifiedCallScreenShare(
            seq = seq.toULong(),
            signerKey = key,
            peerId = peer,
            sharing = sharing,
            withSound = false,
            accountKey = ByteArray(32),
        )

    private fun receiver(): Pair<ScreenShareReceiver, MutableList<ScreenShareView?>> {
        val seen = mutableListOf<ScreenShareView?>()
        return ScreenShareReceiver(ModelFold(own)) { seen += it } to seen
    }

    // ---- The fold wiring ------------------------------------------------------

    @Test
    fun `a state that did not verify changes nothing`() {
        val (r, seen) = receiver()
        assertFalse(r.receive(null))
        assertNull(r.current)
        assertTrue(seen.isEmpty())
    }

    @Test
    fun `a verified share becomes the current share`() {
        val (r, seen) = receiver()
        assertTrue(r.receive(state(1, anna, "pa")))
        assertEquals("pa", r.current?.peerId)
        assertEquals(1, seen.size)
        // The same state again is no change on screen.
        assertFalse(r.receive(state(1, anna, "pa")))
        assertEquals(1, seen.size)
    }

    @Test
    fun `a newer share replaces the previous one`() {
        val (r, seen) = receiver()
        r.receive(state(1, anna, "pa"))
        assertTrue(r.receive(state(2, boris, "pb")))
        assertEquals("pb", r.current?.peerId)
        // The superseded sharer's stop is no change: the current share stays.
        assertFalse(r.receive(state(1, anna, "pa", sharing = false)))
        assertEquals("pb", r.current?.peerId)
        assertEquals(listOf("pa", "pb"), seen.map { it?.peerId })
    }

    @Test
    fun `a stop ends the share`() {
        val (r, seen) = receiver()
        r.receive(state(3, anna, "pa"))
        assertTrue(r.receive(state(3, anna, "pa", sharing = false)))
        assertNull(r.current)
        assertNull(seen.last())
    }

    @Test
    fun `the sharer leaving ends the share`() {
        val (r, _) = receiver()
        r.receive(state(1, anna, "pa"))
        assertFalse("someone else leaving changes nothing", r.participantLeft(boris))
        assertTrue(r.participantLeft(anna))
        assertNull(r.current)
    }

    @Test
    fun `closing clears the share and takes nothing after`() {
        val fold = ModelFold(own)
        val seen = mutableListOf<ScreenShareView?>()
        val r = ScreenShareReceiver(fold) { seen += it }
        r.receive(state(1, anna, "pa"))
        r.close()
        assertTrue(fold.closed)
        assertNull(r.current)
        assertNull(seen.last())
        assertFalse(r.receive(state(2, boris, "pb")))
    }

    // ---- Stream ids -----------------------------------------------------------

    @Test
    fun `a stream id ending in the screen suffix is that peer's share`() {
        val id = splitStreamId("abc$SCREEN_STREAM_SUFFIX")
        assertEquals("abc", id.peerId)
        assertEquals(RemoteStreamRole.SCREEN, id.role)
        assertTrue(isScreenStreamId("abc~screen"))
        // A one-to-one peer names its own stream; only the suffix counts.
        assertTrue(isScreenStreamId("e3b0c4~screen"))
    }

    @Test
    fun `other stream ids are not a share`() {
        assertFalse(isScreenStreamId("abc"))
        assertFalse(isScreenStreamId("abc~light"))
        assertFalse(isScreenStreamId(null))
        // A bare suffix names no peer.
        assertEquals(RemoteStreamRole.MAIN, splitStreamId("~screen").role)
    }

    @Test
    fun `a share's picture goes to the share map, not to the camera`() {
        val ui = CallUiState(CoroutineScope(Dispatchers.Unconfined))
        ui.setParticipants(listOf(SfuCallUi.remoteParticipant("Anna", "pa")))
        val track = org.webrtc.VideoTrack(7L)
        applyRemoteVideoTrack(ui, track, "pa~screen", emptyMap())
        assertEquals(track, ui.remoteScreenVideoTracks.value["peer-pa"])
        assertTrue(ui.remoteVideoTracks.value.isEmpty())
        assertFalse("a share never turns the camera on", ui.participants.value.single().camOn)
        applyRemoteVideoTrack(ui, null, "pa~screen", emptyMap())
        assertTrue(ui.remoteScreenVideoTracks.value.isEmpty())
    }

    @Test
    fun `a share is not aligned with its sharer's voice`() {
        assertNull(avReceiverOf("pa~screen", audio = false))
        assertNull(avReceiverOf("pa~screen", audio = true))
    }

    // ---- The tile and the stage -----------------------------------------------

    private fun person(id: String, local: Boolean = false) = CallUiState.Participant(
        id = id,
        displayName = if (local) "" else id.removePrefix("peer-"),
        isLocal = local,
        micOn = true,
        camOn = true,
        quality = CallUiState.QualityTier.Good,
        speaking = false,
    )

    private fun view(peer: String, seq: Int = 1, own: Boolean = false) =
        ScreenShareView(seq = seq.toULong(), signerKeyHex = peer, peerId = peer, withSound = false, isOwn = own)

    @Test
    fun `a share adds its own tile beside its sharer's`() {
        val people = listOf(person("local", local = true), person("peer-pa"), person("peer-pb"))
        val tiles = withShareTile(people, view("pb"))
        assertEquals(4, tiles.size)
        val share = tiles.single { it.isScreen }
        assertEquals("screen-pb", share.id)
        assertEquals("pb", share.displayName)
        assertEquals("peer-pb", sharerParticipantId(share.id))
        // The sharer's camera tile is still there.
        assertTrue(tiles.any { it.id == "peer-pb" && !it.isScreen })
    }

    @Test
    fun `no share tile without a share, for a sharer not yet in the roster, or for an own share`() {
        val people = listOf(person("local", local = true), person("peer-pa"))
        assertEquals(people, withShareTile(people, null))
        assertEquals(people, withShareTile(people, view("pz")))
        assertEquals(people, withShareTile(people, view("pa", own = true)))
    }

    @Test
    fun `the stage rule is told which tile is the share`() {
        val rules = FakeGroupCallRules(stage = { candidates, _ ->
            candidates.indexOfFirst { it.isScreen }.takeIf { it >= 0 }?.toUInt()
        })
        val people = listOf(person("local", local = true), person("peer-pa"))
        val tiles = withShareTile(people, view("pa"))
        val stage = stagePerson(rules, tiles, pinnedId = null, speech = StageSpeech(activeSpeakerId = "peer-pa"))
        assertEquals("screen-pa", stage?.id)
        val asked = rules.stageQuestions.single().first
        assertEquals(listOf(false, true, false), asked.map { it.isScreen })
        // The share's tile never speaks; its sharer's camera tile does.
        assertEquals(listOf(false, false, true), asked.map { it.speaking })
    }

    @Test
    fun `a pin is passed to the stage rule with a share live`() {
        val rules = FakeGroupCallRules(stage = { _, pinned -> pinned })
        val tiles = withShareTile(listOf(person("local", local = true), person("peer-pa"), person("peer-pb")), view("pa"))
        val stage = stagePerson(rules, tiles, pinnedId = "peer-pb", speech = StageSpeech.None)
        assertEquals("peer-pb", stage?.id)
        assertTrue(shareChipShown("screen-pa", "peer-pb", stage?.id))
        assertFalse(shareChipShown("screen-pa", null, "screen-pa"))
        assertFalse(shareChipShown(null, "peer-pb", "peer-pb"))
    }

    // ---- Layout and announcements ---------------------------------------------

    @Test
    fun `a first share switches to Focus and the end of the last restores the layout`() {
        var layout = shareLayoutTransition(null, view("pa"), GroupLayoutKind.GRID, null)
        assertEquals(GroupLayoutKind.FOCUS, layout.choice)
        // A replacement changes nothing.
        layout = shareLayoutTransition(view("pa"), view("pb", seq = 2), layout.choice, layout.saved)
        assertEquals(GroupLayoutKind.FOCUS, layout.choice)
        layout = shareLayoutTransition(view("pb", seq = 2), null, layout.choice, layout.saved)
        assertEquals(GroupLayoutKind.GRID, layout.choice)
        assertNull(layout.saved)
        // No choice made before the share: none after it.
        val fromDefault = shareLayoutTransition(null, view("pa"), null, null)
        assertNull(shareLayoutTransition(view("pa"), null, fromDefault.choice, fromDefault.saved).choice)
    }

    @Test
    fun `the call state follows the share`() {
        val ui = CallUiState(CoroutineScope(Dispatchers.Unconfined))
        ui.setLayoutChoice(GroupLayoutKind.GRID)
        ui.setScreenShare(view("pa"))
        assertEquals(GroupLayoutKind.FOCUS, ui.layoutChoice.value)
        assertEquals("call.screen_share.started", ui.shareAnnouncement.value?.announcement?.key)
        ui.pin("screen-pa")
        // Replaced: the pin on the old share goes, the layout stays.
        ui.setScreenShare(view("pb", seq = 2))
        assertNull(ui.pinnedId.value)
        assertEquals(GroupLayoutKind.FOCUS, ui.layoutChoice.value)
        assertEquals("pb", ui.shareAnnouncement.value?.announcement?.peerId)
        ui.setScreenShare(null)
        assertEquals(GroupLayoutKind.GRID, ui.layoutChoice.value)
        val last = ui.shareAnnouncement.value
        assertNotNull(last)
        assertEquals("call.screen_share.stopped", last!!.announcement.key)
        assertEquals(3L, last.serial)
    }

    @Test
    fun `announcements name the change and nothing else`() {
        assertEquals("call.screen_share.started", screenShareAnnouncement(null, view("pa"))?.key)
        assertEquals("call.screen_share.stopped", screenShareAnnouncement(view("pa"), null)?.key)
        assertEquals("pa", screenShareAnnouncement(view("pa"), null)?.peerId)
        assertNull(screenShareAnnouncement(view("pa"), view("pa")))
        assertNull(screenShareAnnouncement(null, null))
    }

    // ---- Zoom -----------------------------------------------------------------

    @Test
    fun `zoom stays between whole and the maximum and never leaves the box`() {
        var z = ShareZoom.Whole.step(2f, 1000f, -1000f, 400f, 200f)
        assertEquals(2f, z.scale)
        assertEquals(200f, z.offsetX) // (2 - 1) / 2 of 400
        assertEquals(-100f, z.offsetY)
        z = z.step(100f, 0f, 0f, 400f, 200f)
        assertEquals(MAX_SHARE_ZOOM, z.scale)
        // Back to whole: no offset.
        assertEquals(ShareZoom.Whole, z.step(0.01f, 50f, 50f, 400f, 200f))
        assertTrue(ShareZoom.Whole.step(Float.NaN, Float.NaN, 0f, 400f, 200f).isWhole)
    }

    // ---- One-to-one routing ---------------------------------------------------

    @Test
    fun `a raw one-to-one share body is routed to the call and never applied there`() {
        val body = FfiCallSignalingBody.ScreenShare(ByteArray(16) { 1 }, ByteArray(8))
        assertEquals(CallSignalingRoute.ONE_TO_ONE_ONLY, callSignalingRoute(body))
        assertEquals("screen-share", callSignalingBodyKindName(body))
        assertNull(groupCallSignalOf(body))
        val route = routeOneToOneSignaling(
            body = body,
            groupIdHex = "aa",
            rowCreatedAtMs = 0L,
            nowMs = 0L,
            occupant = OneToOneSlotKey("aa", callIdHexOf(body.callId)),
            endedCallIds = emptySet(),
            otherCallBusy = false,
        )
        assertEquals(OneToOneRoute.DROP, route)
    }
}
