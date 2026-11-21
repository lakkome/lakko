// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiHeldReach
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachPurpose
import uniffi.lakko_ffi_uniffi.FfiReachWritePlan

/**
 * G-53 — the client half of the core's reach plan: one plan per mailbox, the
 * search's found grant passed as `found`, what the core returns mapped back to
 * the held grants, superseded ones deleted. The ORDER is the core's
 * (`lakko-contacts::reach_plan`); these tests drive a stand-in planner that
 * records what it was asked (the host JVM loads no native library).
 */
class ReachPlanTest {

    private class Call(
        val held: List<FfiHeldReach>,
        val purpose: FfiReachPurpose,
        val target: ByteArray?,
        val shared: List<ByteArray>,
        val found: UInt?,
    )

    /** Returns the candidates [answer] gives for each call, recording it. */
    private class Recording(
        private val answer: (List<FfiHeldReach>) -> FfiReachWritePlan,
    ) : ReachPlanner {
        val calls = mutableListOf<Call>()
        override fun plan(
            held: List<FfiHeldReach>,
            purpose: FfiReachPurpose,
            targetScope: ByteArray?,
            sharedScopes: List<ByteArray>,
            found: UInt?,
        ): FfiReachWritePlan {
            calls += Call(held, purpose, targetScope, sharedScopes, found)
            return answer(held)
        }
    }

    private fun h(kind: ReachKind, scope: String, version: Int, mailbox: String, wire: String) =
        HeldForPlan(kind, scope, version.toUInt(), mailbox, wire, payload = wire)

    @Test
    fun `one plan per mailbox, candidates in the core's order, each mailbox in turn`() {
        val held = listOf(
            h(ReachKind.GROUP, "aa", 2, "m1", "g2"),
            h(ReachKind.CONTACT, "", 1, "m2", "c1"),
            h(ReachKind.FOF, "", 3, "m1", "f3"),
        )
        // The stand-in reverses its input: whatever the core says goes.
        val planner = Recording { input ->
            FfiReachWritePlan(input.map { it.index }.reversed(), emptyList())
        }
        val plan = planHeldWrites(held, FfiReachPurpose.WELCOME, null, emptyList(), null, planner)
        assertEquals(listOf("f3", "g2", "c1"), plan.candidates.map { it.payload })
        assertEquals(2, planner.calls.size)
        // Indexes are local to one call.
        assertEquals(listOf(0u, 1u), planner.calls[0].held.map { it.index })
        assertEquals(listOf(FfiReachClass.GROUP, FfiReachClass.FOF), planner.calls[0].held.map { it.`class` })
        assertEquals(listOf(0u), planner.calls[1].held.map { it.index })
    }

    @Test
    fun `the found grant is named by its index, and only when it is held`() {
        val held = listOf(
            h(ReachKind.CONTACT, "", 1, "m1", "c1"),
            h(ReachKind.FOF, "", 3, "m1", "f3"),
        )
        val planner = Recording { FfiReachWritePlan(emptyList(), emptyList()) }
        planHeldWrites(held, FfiReachPurpose.DELEGATION, "ab", emptyList(), "F3", planner)
        planHeldWrites(held, FfiReachPurpose.DELEGATION, "ab", emptyList(), "gone", planner)
        assertEquals(1u, planner.calls[0].found)
        assertNull(planner.calls[1].found)
        assertEquals(FfiReachPurpose.DELEGATION, planner.calls[0].purpose)
        assertArrayEquals(byteArrayOf(0xab.toByte()), planner.calls[0].target)
    }

    @Test
    fun `shared scopes are passed once each, as bytes`() {
        val planner = Recording { FfiReachWritePlan(emptyList(), emptyList()) }
        planHeldWrites(
            listOf(h(ReachKind.MEMBER, "aa", 1, "m1", "w")),
            FfiReachPurpose.WELCOME,
            null,
            listOf("AA", "aa", "bb"),
            null,
            planner,
        )
        assertEquals(listOf("aa", "bb"), planner.calls.single().shared.map(::reachHex))
        assertNull(planner.calls.single().target)
    }

    @Test
    fun `one grant listed twice is one grant`() {
        val held = listOf(
            h(ReachKind.GROUP, "aa", 1, "m1", "w"),
            // The same grant filed under the person's predecessor key.
            h(ReachKind.GROUP, "aa", 1, "m1", "w"),
        )
        val planner = Recording { input -> FfiReachWritePlan(input.map { it.index }, emptyList()) }
        val plan = planHeldWrites(held, FfiReachPurpose.WELCOME, null, emptyList(), null, planner)
        assertEquals(1, planner.calls.single().held.size)
        assertEquals(listOf("w"), plan.candidates.map { it.payload })
    }

    @Test
    fun `superseded grants come back for deletion and a refused list plans nothing`() {
        val held = listOf(
            h(ReachKind.GROUP, "aa", 1, "m1", "old"),
            h(ReachKind.GROUP, "bb", 2, "m1", "new"),
            h(ReachKind.CONTACT, "", 1, "m2", "c"),
        )
        val planner = ReachPlanner { input, _, _, _, _ ->
            if (input.size == 1) throw IllegalStateException("duplicateIndex")
            FfiReachWritePlan(listOf(1u), listOf(0u))
        }
        val plan = planHeldWrites(held, FfiReachPurpose.WELCOME, null, emptyList(), null, planner)
        assertEquals(listOf("new"), plan.candidates.map { it.payload })
        assertEquals(listOf("old"), plan.superseded.map { it.payload })
    }

    @Test
    fun `a scope that is not hex is left out of the plan`() {
        val planner = Recording { input -> FfiReachWritePlan(input.map { it.index }, emptyList()) }
        val plan = planHeldWrites(
            listOf(h(ReachKind.GROUP, "zz", 1, "m1", "bad"), h(ReachKind.CONTACT, "", 1, "m1", "c")),
            FfiReachPurpose.WELCOME,
            null,
            emptyList(),
            null,
            planner,
        )
        assertEquals(listOf("c"), plan.candidates.map { it.payload })
        assertEquals(0, planner.calls.single().held.single().scope.size)
    }

    @Test
    fun `nothing held plans nothing and asks nothing`() {
        val planner = Recording { FfiReachWritePlan(emptyList(), emptyList()) }
        val plan = planHeldWrites(emptyList<HeldForPlan<String>>(), FfiReachPurpose.WELCOME, null, emptyList(), null, planner)
        assertTrue(plan.candidates.isEmpty() && plan.superseded.isEmpty())
        assertTrue(planner.calls.isEmpty())
    }

    // --- the found grant of an add ------------------------------------------

    @Test
    fun `the found grant is recorded per group and person, the newest replacing`() {
        var records = withFoundGrant(emptyList(), "AB", "E1", "W1")
        records = withFoundGrant(records, "ab", "e1", "w2")
        records = withFoundGrant(records, "cd", "e1", "w3")
        assertEquals("w2", foundGrantFor(records, "ab", listOf("e1")))
        assertEquals("w3", foundGrantFor(records, "CD", listOf("e1")))
        assertNull(foundGrantFor(records, "ef", listOf("e1")))
        // Filed under a key the person has since moved from: found through
        // the proven chain, current key first.
        assertEquals("w2", foundGrantFor(records, "ab", listOf("e2", "e1")))
    }

    @Test
    fun `found records are bounded, the oldest going first`() {
        var records = emptyList<StoredFoundGrant>()
        for (i in 0..MAX_FOUND_GRANTS) records = withFoundGrant(records, "g$i", "p", "w$i")
        assertEquals(MAX_FOUND_GRANTS, records.size)
        assertNull(foundGrantFor(records, "g0", listOf("p")))
        assertEquals("w$MAX_FOUND_GRANTS", foundGrantFor(records, "g$MAX_FOUND_GRANTS", listOf("p")))
    }

    // --- groups shared now ---------------------------------------------------

    @Test
    fun `a group counts only while this account is active in it and the person is in its roster`() {
        val bob = byteArrayOf(1)
        val bobPrev = byteArrayOf(2)
        val carol = byteArrayOf(3)
        val resolve: (ByteArray) -> ByteArray = { if (it.contentEquals(bobPrev)) bob else it }
        val groups = listOf(
            "AA" to listOf(carol, bob),
            "bb" to listOf(carol),
            // This account left: not shared, whoever is in it.
            "cc" to null,
            // The person's leaf still names a proven predecessor.
            "dd" to listOf(bobPrev),
        )
        assertEquals(listOf("aa", "dd"), sharedScopesWith(groups, bob, resolve))
        assertTrue(sharedScopesWith(groups, byteArrayOf(9), resolve).isEmpty())
    }

    // --- deleting what a higher version replaced -----------------------------

    @Test
    fun `a superseded grant is deleted and the list is kept when none matched`() {
        val held = listOf(
            StoredHeld("e1", "GROUP", "aa", "11", previousGrantHex = "10"),
            StoredHeld("e1", "GROUP", "bb", "22"),
            StoredHeld("f1", "GROUP", "aa", "33"),
        )
        val next = withoutSupersededGrants(held, listOf("e1"), listOf("GROUP" to "22", "GROUP" to "10"))
        assertEquals(
            listOf(StoredHeld("e1", "GROUP", "aa", "11"), StoredHeld("f1", "GROUP", "aa", "33")),
            next,
        )
        assertSame(held, withoutSupersededGrants(held, listOf("e1"), listOf("GROUP" to "99")))
        // Another person's grant is never touched.
        assertSame(held, withoutSupersededGrants(held, listOf("e1"), listOf("GROUP" to "33")))
    }

    // --- follow-up: proofless writes and the committer envelope ------------

    @Test
    fun `the plan counts the grants it handed to the core`() {
        val planner = Recording { FfiReachWritePlan(emptyList(), emptyList()) }
        val plan = planHeldWrites(
            listOf(h(ReachKind.GROUP, "aa", 1, "m1", "g"), h(ReachKind.GROUP, "aa", 1, "m1", "g"), h(ReachKind.FOF, "", 1, "m2", "f")),
            FfiReachPurpose.WELCOME,
            null,
            emptyList(),
            null,
            planner,
        )
        assertEquals(2, plan.considered)
        assertTrue(plan.candidates.isEmpty())
    }

    @Test
    fun `a stale grant with no shared group stops the proofless write, nothing held does not`() {
        // Nothing held: the open mailbox may take it, as before.
        assertTrue(prooflessAfterPlan(considered = 0, candidates = 0))
        // Held, none usable (a group grant with no group shared now): no
        // proofless write, so a contact request takes a shared group or waits.
        assertFalse(prooflessAfterPlan(considered = 2, candidates = 0))
        // A candidate was offered: proofless only after none yielded, as before.
        assertTrue(prooflessAfterPlan(considered = 2, candidates = 1))
    }

    @Test
    fun `a person found in Everyone gets the open envelope whatever grant is held`() {
        assertEquals(CommitterEnvelopeKind.OPEN, committerEnvelopeKind(foundOpen = true, signingGrantHeld = true))
        assertEquals(CommitterEnvelopeKind.OPEN, committerEnvelopeKind(foundOpen = true, signingGrantHeld = false))
        assertEquals(CommitterEnvelopeKind.SIGNED, committerEnvelopeKind(foundOpen = false, signingGrantHeld = true))
        assertEquals(CommitterEnvelopeKind.NONE, committerEnvelopeKind(foundOpen = false, signingGrantHeld = false))
    }

    @Test
    fun `the latest search of an add replaces what an earlier one recorded`() {
        val found = withFoundGrant(withFoundGrant(emptyList(), "ab", "e1", "w"), "cd", "e1", "w2")
        assertEquals(listOf(StoredFoundGrant("cd", "e1", "w2")), withoutFoundGrant(found, "AB", "E1"))
        val open = withOpenBinding(withOpenBinding(emptyList(), "ab", "e1", "b1"), "ab", "f1", "b2")
        assertEquals(listOf(StoredOpenBinding("ab", "f1", "b2")), withoutOpenBinding(open, "ab", "e1"))
    }
}
