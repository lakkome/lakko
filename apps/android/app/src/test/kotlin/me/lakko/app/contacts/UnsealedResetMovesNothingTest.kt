// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.auth.ResetContinuity
import me.lakko.app.auth.acceptsChannelReturns
import me.lakko.app.channels.ReturnCard
import me.lakko.app.channels.ReturnChannel
import me.lakko.app.channels.ReturnOwnRole
import me.lakko.app.channels.channelReturnsFor
import me.lakko.app.channels.unsentReturns
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0101 sections 10 and 11, walk of 2026-11-28: after an UNSEALED reset
 * nothing of the old relationship moves to the account's keys (a server alone
 * can forge an unsealed reset). Only what a SEALED reset retired moves: a
 * channel return, a successor, a re-admission, a history page, a returned
 * profile all read [movableSupersededKeysOf] and [acceptedResetOfKey].
 */
class UnsealedResetMovesNothingTest {

    private val acct = "ACCT"
    private val k0 = "K0"
    private val k1 = "K1"
    private val k2 = "K2"
    private val k3 = "K3"

    /** A sealed reset k0 -> k1, accepted here. */
    private fun sealedRow(): ResetLedgerRow = ResetLedgerRow(
        acceptedKeyGeneration = 1,
        previousKeysB64 = listOf(k0),
        supersededKeysB64 = listOf(k0),
        resetSignedAtMs = 1_000,
    )

    private val startedOver = StartedOver(oldKeyB64 = k1, newKeyB64 = k2, keyGeneration = 2, signedAtMs = 2_000)

    private fun channel(): ReturnChannel = ReturnChannel(
        idHex = "c1",
        ownRole = ReturnOwnRole.OWNER,
        cards = listOf(ReturnCard(identityPkB64 = k0, subscriberPkB64 = "S0")),
        ownerKeyB64 = "ME",
        publisherKeysB64 = emptyList(),
    )

    @Test
    fun `a sealed reset still produces the return`() {
        val rows = mapOf(acct to sealedRow())
        assertEquals(mapOf(k0 to acct), movableSupersededKeysOf(rows))
        val reset = acceptedResetOfKey(k1, rows) { if (it == acct) k1 else null }
        assertNotNull(reset)
        val planned = channelReturnsFor(listOf(channel()), movableSupersededKeysOf(rows), setOf("ME"))
        assertEquals(1, planned.size)
        assertEquals(1, unsentReturns(planned, emptyMap()) { reset?.keyGeneration }.size)
    }

    @Test
    fun `an unsealed reset seen since moves nothing, before and after the new account is accepted`() {
        // Seen, not accepted: the pin is still k1.
        val open = withStartedOver(sealedRow(), startedOver)!!
        val rows = mapOf(acct to open)
        assertTrue(movableSupersededKeysOf(rows).isEmpty())
        assertNull(acceptedResetOfKey(k1, rows) { k1 })
        assertTrue(channelReturnsFor(listOf(channel()), movableSupersededKeysOf(rows), setOf("ME")).isEmpty())
        // The keys stay superseded for what is refused and dropped.
        assertEquals(mapOf(k0 to acct), supersededKeysOf(rows))

        // Accepted as a new account: the pin is k2, the old relationship
        // stays ended.
        val (adopted, _) = adoptStartedOver(open)!!
        val after = mapOf(acct to adopted)
        assertTrue(movableSupersededKeysOf(after).isEmpty())
        assertNull(acceptedResetOfKey(k2, after) { k2 })
        assertTrue(adopted.endedRelationshipKeysB64.containsAll(listOf(k0, k1)))
    }

    @Test
    fun `a later sealed reset of the new account moves only its own keys`() {
        val (adopted, _) = adoptStartedOver(withStartedOver(sealedRow(), startedOver)!!)!!
        val plan = acceptResetPlan(
            pin = Pin(pinnedSignaturePubkeyB64 = k2, rotationMarker = 0L),
            row = adopted,
            provenPredecessorsB64 = emptyList(),
            reset = AcceptedReset(acct, heldKeyB64 = k2, newKeyB64 = k3, keyGeneration = 3, signedAtMs = 3_000, nowMs = 3_000),
        ) as AcceptResetPlan.Accepted
        val rows = mapOf(acct to plan.row)
        assertEquals(mapOf(k2 to acct), movableSupersededKeysOf(rows))
        assertNotNull(acceptedResetOfKey(k3, rows) { k3 })
        // The card under k0 (the ended relationship) gets no return.
        assertTrue(channelReturnsFor(listOf(channel()), movableSupersededKeysOf(rows), setOf("ME")).isEmpty())
    }

    @Test
    fun `no verdict and a refused reset move nothing`() {
        // Nothing accepted: no row, or a row with no reset signed.
        assertTrue(movableSupersededKeysOf(emptyMap()).isEmpty())
        assertNull(acceptedResetOfKey(k1, emptyMap()) { k1 })
        val baseline = mapOf(acct to ResetLedgerRow(acceptedKeyGeneration = 1))
        assertTrue(movableSupersededKeysOf(baseline).isEmpty())
        assertNull(acceptedResetOfKey(k1, baseline) { k1 })
        assertTrue(channelReturnsFor(listOf(channel()), movableSupersededKeysOf(baseline), setOf("ME")).isEmpty())
    }

    @Test
    fun `a device whose own reset was unsealed takes no channel return`() {
        val unsealed = ResetContinuity(expiresAtMs = 10, keyGeneration = "2", sealed = false)
        assertFalse(acceptsChannelReturns(unsealed))
        assertTrue(acceptsChannelReturns(unsealed.copy(sealed = true)))
        assertTrue(acceptsChannelReturns(null))
        // A record written before the field reads as sealed.
        val older = kotlinx.serialization.json.Json { ignoreUnknownKeys = true }
            .decodeFromString(ResetContinuity.serializer(), "{\"expiresAtMs\":10,\"keyGeneration\":\"2\"}")
        assertTrue(older.sealed)
    }
}
