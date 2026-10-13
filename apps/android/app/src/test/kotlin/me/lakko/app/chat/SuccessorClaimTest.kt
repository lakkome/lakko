// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0073 R.6 - claim-then-build, row by row (mirror of the web
 *  `successor-claim` and `group-closed-store` tables). */
class SuccessorClaimTest {
    private val superseded = setOf("old")

    private fun claim(id: Long, sender: String, admin: Boolean = false, admins: List<String> = emptyList(), s: String = "S$id") =
        SuccessorClaim(id, s, sender, admin, admins)

    @Test
    fun `what makes a group unrepairable`() {
        assertEquals("old", unrepairableBy(ConversationKind.Chat, listOf("me", "old"), "me", null, superseded))
        assertNull(unrepairableBy(ConversationKind.Chat, listOf("me", "peer"), "me", null, superseded))
        // The own key is never the unrepairable one.
        assertNull(unrepairableBy(ConversationKind.Chat, listOf("old", "peer"), "old", null, superseded))
        assertEquals("old", unrepairableBy(ConversationKind.Group, listOf("me", "old"), "me", "old", superseded))
        assertNull(unrepairableBy(ConversationKind.Group, listOf("me", "old"), "me", "me", superseded))
        assertNull(unrepairableBy(ConversationKind.Group, listOf("me", "old"), "me", null, superseded))
    }

    @Test
    fun `who is eligible`() {
        fun e(kind: ConversationKind, sender: String, admin: Boolean, admins: List<String>, surviving: String? = "me") =
            claimEligible(kind, sender, admin, admins, "old", superseded + "old2", surviving)
        assertFalse(e(ConversationKind.Chat, "old", false, emptyList()))
        assertFalse(e(ConversationKind.Group, "old2", true, listOf("old2")))
        assertTrue(e(ConversationKind.Chat, "me", false, emptyList()))
        assertFalse(e(ConversationKind.Chat, "stranger", false, emptyList()))
        assertTrue(e(ConversationKind.Group, "adm", true, listOf("adm")))
        assertFalse(e(ConversationKind.Group, "member", false, listOf("adm")))
        // The table names no admin but the superseded owner: any member.
        assertTrue(e(ConversationKind.Group, "member", false, emptyList()))
        assertTrue(e(ConversationKind.Group, "member", false, listOf("old2")))
    }

    private fun verdict(c: SuccessorClaim, held: SuccessorClaim? = null, unrepairable: String? = "old", kind: ConversationKind = ConversationKind.Chat) =
        claimVerdict(kind, "G", c, held, unrepairable, superseded, "me")

    @Test
    fun `the receive rule`() {
        assertEquals(ClaimVerdict.DROP_MALFORMED, verdict(claim(1, "me", s = "")))
        assertEquals(ClaimVerdict.DROP_MALFORMED, verdict(claim(1, "me", s = "G")))
        // A removed device that still holds the old keys: never, not even pending.
        assertEquals(ClaimVerdict.DROP_INELIGIBLE, verdict(claim(1, "old"), unrepairable = null))
        assertEquals(ClaimVerdict.KEEP_PENDING, verdict(claim(1, "me"), unrepairable = null))
        assertEquals(ClaimVerdict.DROP_INELIGIBLE, verdict(claim(1, "stranger")))
        assertEquals(ClaimVerdict.ACCEPT, verdict(claim(5, "me")))
        assertEquals(ClaimVerdict.IGNORE_LATER, verdict(claim(6, "me"), held = claim(5, "me")))
        assertEquals(ClaimVerdict.IGNORE_LATER, verdict(claim(5, "me"), held = claim(5, "me")))
        // Judged late: an earlier row replaces the held one.
        assertEquals(ClaimVerdict.ACCEPT, verdict(claim(4, "me"), held = claim(5, "me")))
        assertEquals(
            ClaimVerdict.DROP_INELIGIBLE,
            verdict(claim(1, "member", admins = listOf("adm")), kind = ConversationKind.Group),
        )
    }

    @Test
    fun `the first eligible claim in journal order wins`() {
        val claims = listOf(claim(9, "me"), claim(2, "old"), claim(4, "stranger"), claim(7, "me"))
        assertEquals(7L, firstEligibleClaim(ConversationKind.Chat, "G", claims, "old", superseded, "me")?.messageId)
        assertNull(firstEligibleClaim(ConversationKind.Chat, "G", claims, null, superseded, "me"))
        assertEquals(ClaimOutcome.Won, claimOutcome(7, null))
        assertEquals(ClaimOutcome.Won, claimOutcome(7, claim(7, "me")))
        assertEquals(ClaimOutcome.Won, claimOutcome(7, claim(8, "me")))
        assertEquals(ClaimOutcome.Lost(claim(3, "me")), claimOutcome(7, claim(3, "me")))
    }

    @Test
    fun `whether this device founds`() {
        fun f(
            unrepairable: String? = "old",
            held: SuccessorClaim? = null,
            posted: Boolean = false,
            ended: Boolean = false,
            own: String? = "me",
            devices: Boolean = true,
            kind: ConversationKind = ConversationKind.Chat,
            admin: Boolean = false,
            admins: List<String> = emptyList(),
        ) = shouldFoundSuccessor(kind, unrepairable, held, posted, ended, own, admin, admins, superseded, devices)
        assertTrue(f())
        assertFalse(f(unrepairable = null))
        assertFalse(f(held = claim(1, "me")))
        assertFalse(f(posted = true))
        assertFalse(f(ended = true))
        assertFalse(f(own = null))
        assertFalse(f(devices = false))
        assertFalse(f(kind = ConversationKind.Group, admins = listOf("adm")))
        assertTrue(f(kind = ConversationKind.Group, admin = true, admins = listOf("me")))
    }

    @Test
    fun `the claim state keeps a bounded, ordered pending list and round-trips`() {
        var state = NO_CLAIMS
        for (id in listOf(5L, 3L, 5L, 9L)) state = withPendingClaim(state, claim(id, "me"))
        assertEquals(listOf(3L, 5L, 9L), state.pending.map { it.messageId })
        for (id in 100L..130L) state = withPendingClaim(state, claim(id, "me"))
        assertEquals(MAX_PENDING_CLAIMS, state.pending.size)
        val accepted = withAcceptedClaim(state.copy(own = OwnClaim("aa", 4)), claim(3, "me", admin = true, admins = listOf("me")))
        assertTrue(accepted.pending.isEmpty())
        assertEquals(OwnClaim("aa", 4), accepted.own)
        val full = accepted.copy(prepared = PreparedSuccessor("aa", "V2VsY29tZQ==", listOf("k1"), false))
        assertEquals(full, decodeStoredClaims(encodeStoredClaims(full)))
        assertNull(decodeStoredClaims("{"))
        assertNull(decodeStoredClaims("""{"a":{"m":-1,"s":"x","k":"y","a":true}}"""))
        assertEquals(NO_CLAIMS, decodeStoredClaims("{}"))
        val backend = InMemorySuccessorClaimBackend()
        backend.save("acct", "AB", full)
        assertEquals(full, backend.load("acct", "ab"))
        assertEquals(mapOf("ab" to full), backend.loadAll("acct"))
        backend.remove("acct", "ab")
        assertNull(backend.load("acct", "ab"))
    }
}
