// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0060 section 6 (amended 2026-10-29) — an add of a person found by
 * search: which held grants sign a committer delegation, and which bindings
 * let a sweep write the Welcome proofless into an open mailbox (only this
 * device's own record for the add, or a kept envelope with NO delegation).
 */
class OpenWelcomeTest {

    private val group = "ab".repeat(32)
    private val other = "cd".repeat(32)
    private val eve = "e1".repeat(32)
    private val evePrev = "e0".repeat(32)
    private val bob = "b0".repeat(32)

    private fun hex(b: ByteArray) = reachHex(b)

    @Test
    fun `a delegation is signed with a contact, group, channel or fof key, never member, reply or link`() {
        val held = listOf(
            HeldGrantRef(ReachKind.MEMBER, group),
            HeldGrantRef(ReachKind.FOF, ""),
            HeldGrantRef(ReachKind.REPLY, "11"),
            HeldGrantRef(ReachKind.CHANNEL, ""),
            HeldGrantRef(ReachKind.LINK, "22"),
            HeldGrantRef(ReachKind.GROUP, ""),
            HeldGrantRef(ReachKind.CONTACT, ""),
        )
        assertEquals(
            listOf(ReachKind.CONTACT, ReachKind.GROUP, ReachKind.CHANNEL, ReachKind.FOF),
            delegationGrantOrder(held).map { it.kind },
        )
        // A found fof grant alone signs.
        assertEquals(
            listOf(HeldGrantRef(ReachKind.FOF, "")),
            delegationGrantOrder(listOf(HeldGrantRef(ReachKind.FOF, ""), HeldGrantRef(ReachKind.MEMBER, group))),
        )
    }

    @Test
    fun `this device's own record opens the proofless path for that group and person only`() {
        val records = withOpenBinding(emptyList(), group, eve, "aa11")
        assertEquals(listOf("aa11"), openWelcomeBindings(records, emptyList(), group, listOf(eve)).map(::hex))
        assertTrue(openWelcomeBindings(records, emptyList(), other, listOf(eve)).isEmpty())
        assertTrue(openWelcomeBindings(records, emptyList(), group, listOf(bob)).isEmpty())
        // Filed under a key the person has since moved from: found through
        // their proven chain.
        val old = withOpenBinding(emptyList(), group, evePrev, "aa00")
        assertEquals(listOf("aa00"), openWelcomeBindings(old, emptyList(), group, listOf(eve, evePrev)).map(::hex))
    }

    @Test
    fun `a newer record for the same add replaces the earlier one`() {
        val first = withOpenBinding(emptyList(), group, eve, "aa11")
        val second = withOpenBinding(first, group.uppercase(), eve.uppercase(), "AA22")
        assertEquals(1, second.size)
        assertEquals("aa22", second.single().bindingHex)
        assertEquals(2, withOpenBinding(second, other, eve, "aa33").size)
    }

    @Test
    fun `only a kept envelope with no delegation opens the proofless path`() {
        val none = KeptDelegationView(eve, reachUnhex("bb01")!!, delegationCount = 0)
        val signed = KeptDelegationView(eve, reachUnhex("bb02")!!, delegationCount = 3)
        assertEquals(listOf("bb01"), openWelcomeBindings(emptyList(), listOf(signed, none), group, listOf(eve)).map(::hex))
        assertTrue(openWelcomeBindings(emptyList(), listOf(signed), group, listOf(eve)).isEmpty())
        val forBob = KeptDelegationView(bob, reachUnhex("bb03")!!, delegationCount = 0)
        assertTrue(openWelcomeBindings(emptyList(), listOf(forBob), group, listOf(eve)).isEmpty())
    }

    @Test
    fun `the own record is tried before a kept envelope, and a binding is offered once`() {
        val records = withOpenBinding(emptyList(), group, eve, "cc01")
        val kept = listOf(
            KeptDelegationView(eve, reachUnhex("cc02")!!, 0),
            KeptDelegationView(eve, reachUnhex("cc01")!!, 0),
        )
        assertEquals(listOf("cc01", "cc02"), openWelcomeBindings(records, kept, group, listOf(eve)).map(::hex))
    }
}
