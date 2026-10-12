// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0065 R.6 / R.7 cases 1 and 3 - the ORDER of claim-then-build, driven
 * through a recording port (mirror of the web `continuity-runtime` cases).
 * The property under test is the sequence: nothing is delivered before the
 * claim is decided, and a lost claim delivers nothing at all.
 */
class ContinuityRuntimeTest {
    private val old = "aa".repeat(4)
    private val succ = "bb".repeat(4)
    private val other = "cc".repeat(4)

    private class Port(
        var own: String? = "me",
        val superseded: Map<String, String> = mapOf("old" to "ACCT"),
        val facts: MutableMap<String, ContinuityFacts> = HashMap(),
        var state: StoredClaims = NO_CLAIMS,
        var cursor: Long? = null,
        var devices: Boolean = true,
        var postFails: Boolean = false,
        var deliverOk: Boolean = true,
        val successorId: String,
    ) : ContinuityPort {
        val calls = ArrayList<String>()

        override fun ownPersonKeyB64() = own
        override fun supersededKeys() = superseded
        override fun currentKeyOf(accountIdB64: String) = "new"
        override fun facts(groupIdHex: String) = facts[groupIdHex]
        override fun claimState(oldGroupIdHex: String) = state
        override fun cursorOf(oldGroupIdHex: String) = cursor
        override suspend fun newIdentityHasDevices(accountIdB64: String) = devices
        override suspend fun prepareSuccessor(oldGroupIdHex: String, newIdentityAccountIdB64: String): String {
            calls += "prepare"
            return successorId
        }
        override suspend fun postClaim(oldGroupIdHex: String, successorGroupIdHex: String): Long {
            calls += "post"
            if (postFails) throw IllegalStateException("offline")
            state = state.copy(own = OwnClaim(successorGroupIdHex, 10))
            return 10
        }
        override suspend fun abandonOwnClaim(oldGroupIdHex: String) { calls += "abandon" }
        override suspend fun discardSuccessor(successorGroupIdHex: String) { calls += "discard:$successorGroupIdHex" }
        override suspend fun settle(oldGroupIdHex: String, winner: SuccessorClaim) {
            calls += "settle:${groupHexOfB64(winner.successorGroupIdB64)}"
            state = state.copy(accepted = winner, own = null)
        }
        override suspend fun recordLineage(oldGroupIdHex: String, successorGroupIdHex: String) { calls += "lineage:$successorGroupIdHex" }
        override suspend fun deliverSuccessor(oldGroupIdHex: String, successorGroupIdHex: String): Boolean {
            calls += "deliver"
            return deliverOk
        }
        override suspend fun carryName(oldGroupIdHex: String, successorGroupIdHex: String) { calls += "name" }
        override suspend fun addPerson(successorGroupIdHex: String, personKeyB64: String): Boolean {
            calls += "add:$personKeyB64"
            return true
        }
        override fun recheckDue(successorGroupIdHex: String) = true
        override suspend fun setAdmins(successorGroupIdHex: String, adminKeysB64: List<String>) { calls += "admins:$adminKeysB64" }
        override suspend fun transferOwnership(successorGroupIdHex: String, newOwnerKeyB64: String, stayAdmin: Boolean) {
            calls += "transfer:$newOwnerKeyB64:$stayAdmin"
        }
        override suspend fun resendOwnMessages(oldGroupIdHex: String, successorGroupIdHex: String, accountIdB64: String) {
            calls += "resend"
        }
    }

    private fun chat() = ContinuityFacts(ConversationKind.Chat, listOf("me", "old"), null, emptyList(), true)

    private fun port() = Port(successorId = succ).also { it.facts[old] = chat() }

    @Test
    fun `nothing to continue`() = runTest {
        assertEquals(ContinuityStep.NOTHING, continueConversation(Port(successorId = succ), old))
        val repairable = port().also {
            it.facts[old] = ContinuityFacts(ConversationKind.Group, listOf("me", "old"), "me", emptyList(), true)
        }
        assertEquals(ContinuityStep.NOTHING, continueConversation(repairable, old))
        assertTrue(repairable.calls.isEmpty())
    }

    @Test
    fun `steps 1 and 2 - prepare, then claim, and nothing is delivered`() = runTest {
        val p = port()
        assertEquals(ContinuityStep.CLAIMED, continueConversation(p, old))
        assertEquals(listOf("prepare", "post"), p.calls)
    }

    @Test
    fun `no published device yet - nothing is created`() = runTest {
        val p = port().also { it.devices = false }
        assertEquals(ContinuityStep.WAITING_FOR_DEVICES, continueConversation(p, old))
        assertTrue(p.calls.isEmpty())
    }

    @Test
    fun `a failed claim drops the local successor`() = runTest {
        val p = port().also { it.postFails = true }
        assertEquals(ContinuityStep.FAILED, continueConversation(p, old))
        assertEquals(listOf("prepare", "post", "abandon", "discard:$succ"), p.calls)
    }

    @Test
    fun `step 3 waits for the journal before anything is delivered`() = runTest {
        val p = port().also { it.state = StoredClaims(own = OwnClaim(succ, 10)); it.cursor = 9 }
        assertEquals(ContinuityStep.WAITING_FOR_JOURNAL, continueConversation(p, old))
        assertTrue(p.calls.isEmpty())
    }

    @Test
    fun `the winner of a one-to-one delivers and re-sends`() = runTest {
        val p = port().also { it.state = StoredClaims(own = OwnClaim(succ, 10)); it.cursor = 10 }
        assertEquals(ContinuityStep.BUILT, continueConversation(p, old))
        assertEquals(listOf("settle:$succ", "lineage:$succ", "deliver", "resend"), p.calls)
    }

    @Test
    fun `a lost claim discards and adopts - no Welcome ever goes out`() = runTest {
        val earlier = SuccessorClaim(4, groupB64OfHex(other), "me", false, emptyList())
        val p = port().also {
            it.state = StoredClaims(accepted = earlier, own = OwnClaim(succ, 10))
            it.cursor = 10
        }
        assertEquals(ContinuityStep.LOST, continueConversation(p, old))
        assertEquals(listOf("discard:$succ", "settle:$other", "lineage:$other"), p.calls)
        assertFalse("deliver" in p.calls)
    }

    @Test
    fun `an own claim whose publish never reported an id is posted again`() = runTest {
        val p = port().also { it.state = StoredClaims(own = OwnClaim(succ, null)) }
        assertEquals(ContinuityStep.CLAIMED, continueConversation(p, old))
        assertEquals(listOf("post"), p.calls)
    }

    @Test
    fun `another account's accepted claim is adopted`() = runTest {
        val p = port().also {
            it.state = StoredClaims(accepted = SuccessorClaim(3, groupB64OfHex(other), "someone", false, emptyList()))
        }
        assertEquals(ContinuityStep.ADOPTED, continueConversation(p, old))
        assertEquals(listOf("lineage:$other"), p.calls)
    }

    @Test
    fun `a delivery that fails is retried at the next pass`() = runTest {
        val p = port().also {
            it.state = StoredClaims(own = OwnClaim(succ, 10)); it.cursor = 10; it.deliverOk = false
        }
        assertEquals(ContinuityStep.FAILED, continueConversation(p, old))
        assertFalse("resend" in p.calls)
    }

    private fun ownedGroup(admins: List<String>) =
        ContinuityFacts(ConversationKind.Group, listOf("me", "old", "b", "c"), "old", admins, true)

    @Test
    fun `a plain member does not found where another admin exists`() = runTest {
        val p = port().also { it.facts[old] = ownedGroup(listOf("b")) }
        assertEquals(ContinuityStep.NOT_ELIGIBLE, continueConversation(p, old))
        assertTrue(p.calls.isEmpty())
    }

    @Test
    fun `the winner of an owned group adds the people, restores the admins, then the owner`() = runTest {
        val p = port().also {
            it.facts[old] = ownedGroup(listOf("me", "b"))
            it.state = StoredClaims(own = OwnClaim(succ, 10)); it.cursor = 10
            // The new identity's leaf is not in this device's view yet: no
            // role is written for a key no leaf holds, and nothing is
            // transferred to it.
            it.facts[succ] = ContinuityFacts(ConversationKind.Group, listOf("me"), "me", emptyList(), true)
        }
        assertEquals(ContinuityStep.BUILT, continueConversation(p, old))
        assertEquals(
            listOf("settle:$succ", "lineage:$succ", "deliver", "name", "add:b", "add:c"),
            p.calls,
        )
        // Next pass: the people are in; one role change, then the transfer.
        p.calls.clear()
        p.facts[succ] = ContinuityFacts(ConversationKind.Group, listOf("me", "new", "b", "c"), "me", emptyList(), true)
        assertEquals(ContinuityStep.BUILT, continueConversation(p, old))
        // The register is write-once: recording it again changes nothing.
        assertEquals(listOf("lineage:$succ", "lineage:$succ", "deliver", "name", "admins:[b]"), p.calls)
        p.calls.clear()
        p.facts[succ] = ContinuityFacts(ConversationKind.Group, listOf("me", "new", "b", "c"), "me", listOf("b"), true)
        assertEquals(ContinuityStep.BUILT, continueConversation(p, old))
        assertEquals(listOf("lineage:$succ", "lineage:$succ", "deliver", "name", "transfer:new:true"), p.calls)
    }

    @Test
    fun `a moved admin that did not found re-checks completeness, a member does not`() = runTest {
        val accepted = SuccessorClaim(3, groupB64OfHex(succ), "b", true, listOf("b", "me"))
        val p = port().also {
            it.facts[old] = ownedGroup(listOf("b", "me"))
            it.state = StoredClaims(accepted = accepted)
            it.facts[succ] = ContinuityFacts(ConversationKind.Group, listOf("me", "b", "new"), "b", listOf("me"), true)
        }
        assertEquals(ContinuityStep.RECHECKED, continueConversation(p, old))
        assertEquals(listOf("lineage:$succ", "add:c"), p.calls)
        p.calls.clear()
        p.facts[succ] = ContinuityFacts(ConversationKind.Group, listOf("me", "b", "new"), "b", emptyList(), true)
        assertEquals(ContinuityStep.ADOPTED, continueConversation(p, old))
        assertEquals(listOf("lineage:$succ"), p.calls)
        assertFalse(mayRecheckCompleteness(null, "me"))
    }

    @Test
    fun `group ids convert between hex and base64`() {
        assertEquals(old, groupHexOfB64(groupB64OfHex(old)))
        assertEquals("", groupB64OfHex("zz"))
        assertEquals("", groupHexOfB64("***"))
    }
}
