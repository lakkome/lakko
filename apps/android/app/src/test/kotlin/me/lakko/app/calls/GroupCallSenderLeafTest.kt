// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiCallEndReason
import uniffi.lakko_ffi_uniffi.FfiCallMediaFlags
import uniffi.lakko_ffi_uniffi.FfiCallSignalingBody

/**
 * A group-control body may only speak for its own sender (web parity, no wire
 * change): `GroupCallStart.initiator_leaf` and the join / leave `leaf_index`
 * are sender-written fields, so a body whose claimed leaf differs from the MLS
 * sender leaf of the row that carried it never reaches the ledger.
 */
class GroupCallSenderLeafTest {

    private val callId = ByteArray(16) { 9 }
    private val hex = callIdHexOf(callId)
    private val media = FfiCallMediaFlags(audio = true, video = false)

    @Test
    fun `a start is accepted only from the leaf it names`() {
        val body = FfiCallSignalingBody.GroupStart(callId, 4u, 1_000uL, media)
        assertEquals(
            GroupCallSignal.Start(hex, 4u, 1_000L, audio = true, video = false),
            groupCallSignalForRow(body, senderLeaf = 4u),
        )
        assertNull(groupCallSignalForRow(body, senderLeaf = 5u))
        assertNull(groupCallSignalForRow(body, senderLeaf = null))
    }

    @Test
    fun `a join or leave on another member's behalf is dropped`() {
        val join = FfiCallSignalingBody.GroupJoin(callId, 2u, 5uL)
        val leave = FfiCallSignalingBody.GroupLeave(callId, 2u, FfiCallEndReason.HUNG_UP)
        assertEquals(GroupCallSignal.Join(hex, 2u), groupCallSignalForRow(join, 2u))
        assertEquals(GroupCallSignal.Leave(hex, 2u), groupCallSignalForRow(leave, 2u))
        assertNull(groupCallSignalForRow(join, 7u))
        assertNull(groupCallSignalForRow(leave, 7u))
        assertNull(groupCallSignalForRow(join, null))
    }

    @Test
    fun `an end names no leaf and is not gated`() {
        val end = FfiCallSignalingBody.End(callId, FfiCallEndReason.HUNG_UP)
        assertEquals(GroupCallSignal.End(hex), groupCallSignalForRow(end, senderLeaf = 1u))
        assertEquals(GroupCallSignal.End(hex), groupCallSignalForRow(end, senderLeaf = null))
    }
}
