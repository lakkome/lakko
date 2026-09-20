// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The pure decisions of the one call slot and of the hosted group and link
 * calls (`CallSlot.kt`, `GroupCallHostRules.kt`, `LinkCallHostRules.kt`). The
 * wording is pinned because it is shared with the web client.
 */
class CallSlotRulesTest {

    // ---- CallSlot -----------------------------------------------------------

    @Test
    fun `the slot is free only when no kind of call holds it`() {
        assertNull(callSlotOccupant(oneToOneInCall = false, groupCallActive = false, linkCallActive = false))
        assertEquals(CallKind.ONE_TO_ONE, callSlotOccupant(true, false, false))
        assertEquals(CallKind.GROUP, callSlotOccupant(false, true, false))
        assertEquals(CallKind.LINK, callSlotOccupant(false, false, true))
    }

    @Test
    fun `an inconsistent host still reports one deterministic occupant`() {
        assertEquals(CallKind.ONE_TO_ONE, callSlotOccupant(true, true, true))
        assertEquals(CallKind.GROUP, callSlotOccupant(false, true, true))
    }

    @Test
    fun `the busy message names the running call and what to do`() {
        assertEquals(
            "You are already in a call. End it before you start another one.",
            callBusyMessage(CallKind.ONE_TO_ONE, CallVerb.START),
        )
        assertEquals(
            "You are already in a group call. End it before you join another one.",
            callBusyMessage(CallKind.GROUP, CallVerb.JOIN),
        )
        assertEquals(
            "You are already in a call from a link. End it before you start another one.",
            callBusyMessage(CallKind.LINK, CallVerb.START),
        )
    }

    // ---- Group call: leave ------------------------------------------------------

    @Test
    fun `a joined device that was the last one in the call publishes Leave and End`() {
        val plan = planGroupCallLeave(
            reason = GroupLeaveReason.HUNG_UP,
            sessionFailed = false,
            joinPublished = true,
            initiator = false,
            recordOtherFreshMembers = emptySet(),
        )
        assertEquals(GroupCallLeavePlan(GroupLeaveReason.HUNG_UP, publishLeave = true, publishEnd = true), plan)
    }

    @Test
    fun `leaving a call others are still in publishes only the Leave`() {
        val plan = planGroupCallLeave(GroupLeaveReason.HUNG_UP, false, true, true, setOf("bob"))
        assertTrue(plan.publishLeave)
        assertEquals(false, plan.publishEnd)
    }

    @Test
    fun `a joiner that never joined ends nothing, even when the record is empty`() {
        val plan = planGroupCallLeave(GroupLeaveReason.ERROR, false, false, false, emptySet())
        assertEquals(GroupCallLeavePlan(GroupLeaveReason.ERROR, publishLeave = false, publishEnd = false), plan)
    }

    @Test
    fun `an initiator whose connection failed before joining ends the call it announced`() {
        val plan = planGroupCallLeave(GroupLeaveReason.HUNG_UP, true, false, true, null)
        assertEquals(GroupCallLeavePlan(GroupLeaveReason.ERROR, publishLeave = false, publishEnd = true), plan)
    }

    // ---- Group call: epoch change ---------------------------------------------

    private fun epochAction(
        call: String? = "g1",
        settledGroup: String = "g1",
        joined: Boolean = true,
        active: Boolean? = true,
        settled: Boolean = true,
        keyed: ULong? = 4u,
        current: ULong? = 5u,
    ) = decideGroupCallEpochChange(call, settledGroup, joined, active, settled, keyed, current)

    @Test
    fun `a settled epoch advance in the call's group rekeys`() {
        assertEquals(GroupCallEpochAction.REKEY, epochAction())
        assertEquals(GroupCallEpochAction.REKEY, epochAction(keyed = null))
    }

    @Test
    fun `nothing to do for another group, no call, or an unchanged epoch`() {
        assertEquals(GroupCallEpochAction.NONE, epochAction(settledGroup = "g2"))
        assertEquals(GroupCallEpochAction.NONE, epochAction(call = null))
        assertEquals(GroupCallEpochAction.NONE, epochAction(keyed = 5u))
    }

    @Test
    fun `an unsettled or unreadable epoch keeps the current key`() {
        assertEquals(GroupCallEpochAction.NONE, epochAction(settled = false))
        assertEquals(GroupCallEpochAction.NONE, epochAction(current = null))
    }

    @Test
    fun `a group this device left ends the call locally, settled or not`() {
        assertEquals(GroupCallEpochAction.END_LOCALLY, epochAction(active = false))
        assertEquals(GroupCallEpochAction.END_LOCALLY, epochAction(active = false, settled = false))
        assertEquals(GroupCallEpochAction.END_LOCALLY, epochAction(joined = false))
        assertEquals(GroupCallEpochAction.REKEY, epochAction(active = null))
    }

    // ---- Group call: start ----------------------------------------------------

    @Test
    fun `a group call starts only on a free slot`() {
        assertEquals(GroupCallStartDecision.Start, decideGroupCallStart(null, null, "g1", null))
        assertEquals(
            GroupCallStartDecision.Busy(callBusyMessage(CallKind.ONE_TO_ONE, CallVerb.START)),
            decideGroupCallStart(CallKind.ONE_TO_ONE, null, "g1", null),
        )
        assertEquals(
            GroupCallStartDecision.Busy(callBusyMessage(CallKind.LINK, CallVerb.JOIN)),
            decideGroupCallStart(CallKind.LINK, null, "g1", "c1"),
        )
    }

    @Test
    fun `returning to the chat of the live group call is not a second call`() {
        val live = "g1" to "c1"
        assertEquals(GroupCallStartDecision.AlreadyInThisCall, decideGroupCallStart(CallKind.GROUP, live, "g1", null))
        assertEquals(GroupCallStartDecision.AlreadyInThisCall, decideGroupCallStart(CallKind.GROUP, live, "g1", "c1"))
        assertEquals(
            GroupCallStartDecision.Busy(callBusyMessage(CallKind.GROUP, CallVerb.JOIN)),
            decideGroupCallStart(CallKind.GROUP, live, "g1", "c2"),
        )
        assertEquals(
            GroupCallStartDecision.Busy(callBusyMessage(CallKind.GROUP, CallVerb.START)),
            decideGroupCallStart(CallKind.GROUP, live, "g2", null),
        )
    }

    // ---- Group call: ending by itself when everyone else has left --------------

    @Test
    fun `the alone-end timer arms only when a remote was seen and the roster is now empty`() {
        assertEquals(GroupCallAloneAction.ARM_END, groupCallAloneAction(remoteCount = 0, sawRemote = true, armed = false))
    }

    @Test
    fun `a call that never had a remote never arms the alone-end timer`() {
        // The initiator waiting for the first joiner: nobody left.
        assertEquals(GroupCallAloneAction.NONE, groupCallAloneAction(remoteCount = 0, sawRemote = false, armed = false))
        assertEquals(GroupCallAloneAction.NONE, groupCallAloneAction(remoteCount = 1, sawRemote = false, armed = false))
    }

    @Test
    fun `someone coming back while armed disarms the alone-end timer`() {
        assertEquals(GroupCallAloneAction.DISARM, groupCallAloneAction(remoteCount = 1, sawRemote = true, armed = true))
        assertEquals(GroupCallAloneAction.DISARM, groupCallAloneAction(remoteCount = 3, sawRemote = true, armed = true))
    }

    @Test
    fun `nothing to do when already armed and still alone, or when others are present and nothing is armed`() {
        assertEquals(GroupCallAloneAction.NONE, groupCallAloneAction(remoteCount = 0, sawRemote = true, armed = true))
        assertEquals(GroupCallAloneAction.NONE, groupCallAloneAction(remoteCount = 2, sawRemote = true, armed = false))
    }

    @Test
    fun `the alone-end window is the one shared with iOS and web`() {
        assertEquals(5_000L, GROUP_CALL_ALONE_END_MS)
    }

    // ---- Link call --------------------------------------------------------------

    @Test
    fun `an owner is live from founding and a joiner waits for its Welcome`() {
        val ownerLive = reduceLinkCallPhase(LinkCallPhase.STARTING, LinkCallEvent.Joined)
        assertEquals(LinkCallPhase.LIVE, ownerLive)
        assertEquals(LinkCallPhase.LIVE, reduceLinkCallPhase(ownerLive, LinkCallEvent.SignalingOpen(owner = true)))

        val waiting = reduceLinkCallPhase(LinkCallPhase.STARTING, LinkCallEvent.SignalingOpen(owner = false))
        assertEquals(LinkCallPhase.WAITING, waiting)
        assertEquals(LinkCallPhase.LIVE, reduceLinkCallPhase(waiting, LinkCallEvent.Joined))
    }

    @Test
    fun `a Welcome that arrived before signaling-open reported keeps the call live`() {
        val live = reduceLinkCallPhase(LinkCallPhase.STARTING, LinkCallEvent.Joined)
        assertEquals(LinkCallPhase.LIVE, reduceLinkCallPhase(live, LinkCallEvent.SignalingOpen(owner = false)))
    }

    @Test
    fun `the specific report after a teardown replaces ended`() {
        val ended = reduceLinkCallPhase(LinkCallPhase.WAITING, LinkCallEvent.Ended)
        assertEquals(LinkCallPhase.ENDED, ended)
        assertEquals(LinkCallPhase.DEAD, reduceLinkCallPhase(ended, LinkCallEvent.NoCommitter))
        assertEquals(LinkCallPhase.ERROR, reduceLinkCallPhase(ended, LinkCallEvent.Error))
        // Dead and error are final.
        assertEquals(LinkCallPhase.DEAD, reduceLinkCallPhase(LinkCallPhase.DEAD, LinkCallEvent.Joined))
        assertEquals(LinkCallPhase.ERROR, reduceLinkCallPhase(LinkCallPhase.ERROR, LinkCallEvent.Ended))
        // A live call is never declared dead by a late timeout.
        assertEquals(LinkCallPhase.LIVE, reduceLinkCallPhase(LinkCallPhase.LIVE, LinkCallEvent.NoCommitter))
    }

    @Test
    fun `only starting, waiting and live hold the slot`() {
        assertEquals(
            setOf(LinkCallPhase.STARTING, LinkCallPhase.WAITING, LinkCallPhase.LIVE),
            LinkCallPhase.values().filter(::linkCallOccupiesSlot).toSet(),
        )
    }

    @Test
    fun `a link call starts only on a free slot, and never twice`() {
        assertEquals(LinkCallStartDecision.Start, decideLinkCallStart(null, null, "c1", CallVerb.JOIN))
        assertEquals(LinkCallStartDecision.AlreadyInThisCall, decideLinkCallStart(CallKind.LINK, "c1", "c1", CallVerb.JOIN))
        assertEquals(
            LinkCallStartDecision.Busy(callBusyMessage(CallKind.LINK, CallVerb.JOIN)),
            decideLinkCallStart(CallKind.LINK, "c1", "c2", CallVerb.JOIN),
        )
        assertEquals(
            LinkCallStartDecision.Busy(callBusyMessage(CallKind.GROUP, CallVerb.START)),
            decideLinkCallStart(CallKind.GROUP, null, "c1", CallVerb.START),
        )
    }

    @Test
    fun `the link-call status line appears only away from the landing and before live or after the end`() {
        assertNull(linkCallNotice(null, landingShowingIt = false, errorMessage = null))
        assertNull(linkCallNotice(LinkCallPhase.WAITING, landingShowingIt = true, errorMessage = null))
        assertNull(linkCallNotice(LinkCallPhase.LIVE, landingShowingIt = false, errorMessage = null))
        assertEquals(
            LinkCallNotice(
                "Waiting for the owner to admit you…",
                listOf(LinkCallNoticeAction.OPEN, LinkCallNoticeAction.CANCEL),
            ),
            linkCallNotice(LinkCallPhase.WAITING, false, null),
        )
        assertEquals(
            listOf(LinkCallNoticeAction.OPEN, LinkCallNoticeAction.CANCEL),
            linkCallNotice(LinkCallPhase.STARTING, false, null)?.actions,
        )
        assertEquals(
            LinkCallNotice("This call has ended or isn't active.", listOf(LinkCallNoticeAction.DISMISS)),
            linkCallNotice(LinkCallPhase.DEAD, false, null),
        )
        assertEquals("The call failed: boom", linkCallNotice(LinkCallPhase.ERROR, false, "boom")?.text)
        assertEquals("The call failed.", linkCallNotice(LinkCallPhase.ERROR, false, null)?.text)
    }

    // ---- The conversation row ------------------------------------------------

    @Test
    fun `the row is written only for a call whose media flowed`() {
        assertNull(groupCallTranscriptEntry(initiator = true, video = false, connectedAtMs = null, endedAtMs = 5_000L))
        assertEquals(
            CallTranscriptEntry(CallOutcome.OUTGOING, video = true, durationSecs = 60L),
            groupCallTranscriptEntry(initiator = true, video = true, connectedAtMs = 1_000L, endedAtMs = 61_500L),
        )
        assertEquals(
            CallTranscriptEntry(CallOutcome.INCOMING, video = false, durationSecs = 0L),
            groupCallTranscriptEntry(initiator = false, video = false, connectedAtMs = 1_000L, endedAtMs = 1_000L),
        )
    }
}
