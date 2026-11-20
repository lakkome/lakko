// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.ds.MailboxMessage
import me.lakko.app.push.GroupPushRouting
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0087 phase A on Android — the host-side rules: reaction ops and their
 * order, the group-setting classifier and receive verdict, the knock version
 * bump, the sealed-knock drain, the `:` query, and the store row codecs. The
 * Rust rules themselves (fold, verdicts, chips) are asserted in Rust.
 */
class Adr0087PhaseATest {

    private val a = "A".repeat(43) + "="
    private val b = "B".repeat(43) + "="

    @Test
    fun `reaction ops keep DS order and a re-delivered op changes nothing`() {
        var ops = emptyList<ReactionOpRecord>()
        ops = appendReactionOp(ops, ReactionOpRecord(b, "x", true, 9))
        ops = appendReactionOp(ops, ReactionOpRecord(a, "y", true, 4))
        ops = appendReactionOp(ops, ReactionOpRecord(a, "y", true, 4))
        assertEquals(listOf(4L, 9L), ops.map { it.at })
        assertEquals(10L, localOpId(ops))
        assertEquals(1L, localOpId(emptyList()))
    }

    @Test
    fun `own emoji and kinds are read from what is held`() {
        val held = listOf(Reaction(a, "1", 5), Reaction(b, "2", 3), Reaction(a, "2", 7))
        assertEquals(listOf("1", "2"), ownReactionEmoji(held, a))
        assertEquals(emptyList<String>(), ownReactionEmoji(held, null))
        assertEquals(listOf("2", "1"), reactionKinds(held))
        assertTrue(othersReactedTo(held, a))
        assertFalse(othersReactedTo(held.filter { it.reactor == a }, a))
    }

    @Test
    fun `a fold that cannot run keeps what was held`() {
        val fold = ReactionFold { null }
        assertNull(fold.fold(listOf(ReactionOpRecord(a, "x", true, 1))))
    }

    @Test
    fun `group-setting envelopes are classified by their domain`() {
        for (kind in GroupSettingKind.entries) {
            assertEquals(kind, groupSettingKindOf(kind.prefix + byteArrayOf(1, 2, 3)))
        }
        assertNull(groupSettingKindOf("{\"v\":1}".toByteArray()))
        assertNull(groupSettingKindOf("lakko/group-photo/v1".toByteArray()))
        assertNull(groupSettingKindOf(ByteArray(0)))
    }

    @Test
    fun `only the owner or an admin sets the photo or the reactions, newer wins`() {
        val owner = ByteArray(32) { 1 }
        val admin = ByteArray(32) { 2 }
        val member = ByteArray(32) { 3 }
        val table = GroupPushRouting.OwnerLookup.Known(owner, listOf(admin))
        assertEquals(GroupNameVerdict.ACCEPT_OWNER, ownerOrAdminVerdict(owner, table, false, false) { false })
        assertEquals(GroupNameVerdict.ACCEPT_ADMIN, ownerOrAdminVerdict(admin, table, true, false) { true })
        assertEquals(GroupNameVerdict.REJECT_STALE, ownerOrAdminVerdict(admin, table, true, false) { false })
        assertEquals(GroupNameVerdict.REJECT_NOT_AUTHORIZED, ownerOrAdminVerdict(member, table, false, false) { true })
        assertEquals(GroupNameVerdict.ACCEPT_IDENTICAL, ownerOrAdminVerdict(member, table, true, true) { false })
        assertEquals(
            GroupNameVerdict.REJECT_UNVERIFIABLE,
            ownerOrAdminVerdict(null, table, false, false) { true },
        )
        assertEquals(
            GroupNameVerdict.REJECT_UNVERIFIABLE,
            ownerOrAdminVerdict(owner, GroupPushRouting.OwnerLookup.Unavailable, false, false) { true },
        )
    }

    @Test
    fun `removing an admin bumps the knock version, adding one does not`() {
        assertEquals(0.toUShort(), nextKnockVersion(null, setOf("aa"), 0u))
        assertEquals(0.toUShort(), nextKnockVersion(setOf("aa"), setOf("aa", "bb"), 0u))
        assertEquals(1.toUShort(), nextKnockVersion(setOf("aa", "bb"), setOf("aa"), 0u))
        assertEquals(3.toUShort(), nextKnockVersion(setOf("aa"), emptySet(), 2u))
    }

    @Test
    fun `a full queue stops the knock cursor before the row`() {
        val rows = (1L..4L).map { MailboxMessage(id = it, payload = byteArrayOf(it.toByte()), createdAt = "") }
        val drained = drainSealedKnockRows(rows, after = 0) { bytes ->
            when (bytes[0].toInt()) {
                1 -> SealedKnockStep.KEEP to "one"
                2 -> SealedKnockStep.DROP to null
                3 -> SealedKnockStep.STOP to null
                else -> SealedKnockStep.KEEP to "four"
            }
        }
        assertEquals(listOf("one"), drained.accepted.map { it.second })
        assertEquals(2L, drained.cursor)
        val thrown = drainSealedKnockRows<String>(rows.take(1), after = 0) { _ -> throw IllegalStateException() }
        assertEquals(1L, thrown.cursor)
        assertTrue(thrown.accepted.isEmpty())
    }

    @Test
    fun `the colon query starts a word and needs two letters`() {
        assertEquals(ColonQuery(6, "sm"), colonQueryAt("hello :sm", 9))
        assertEquals(ColonQuery(0, "heart"), colonQueryAt(":heart", 6))
        assertNull(colonQueryAt("hello :s", 8))
        assertNull(colonQueryAt("a:sm", 4))
        assertNull(colonQueryAt("hello :sm ", 10))
        assertNull(colonQueryAt(":sm", 3, collapsed = false))
        val q = colonQueryAt("hi :sm there", 6)!!
        assertEquals("hi 😀 there" to 5, applyColonPick("hi :sm there", 6, q, "😀"))
    }

    @Test
    fun `the store rows round-trip`() {
        val photo = StoredGroupPhoto(null, 42u)
        assertEquals(photo, decodeGroupPhoto(encodeGroupPhoto(photo)))
        val some = StoredGroupReactions(AllowedReactions.Only(listOf("x", "y")), 7u)
        assertEquals(some, decodeGroupReactions(encodeGroupReactions(some)))
        val off = StoredGroupReactions(AllowedReactions.Off, 8u)
        assertEquals(off, decodeGroupReactions(encodeGroupReactions(off)))
        assertNull(decodeGroupReactions("{\"mode\":\"only\",\"emoji\":[],\"updatedAtMs\":\"1\"}"))
        val handle = StoredGroupHandle("hiking_club", true, 9u, ByteArray(169) { 5 }, ByteArray(64) { 6 })
        assertEquals(handle, decodeGroupHandle(encodeGroupHandle(handle)))
        assertNull(decodeGroupPhoto("not json"))
    }

    @Test
    fun `handle input is canonical and size bands follow ADR-0059`() {
        assertEquals("hiking_club", canonicalHandleInput("  @Hiking_Club "))
        assertEquals(listOf(0, 1, 2, 3, 4, 5), listOf(1, 10, 100, 1_000, 10_000, 100_000).map(::sizeBandOf))
    }

    @Test
    fun `create needs a well-formed name and a public group needs a name`() {
        assertTrue(me.lakko.app.ui.chat.canCreateGroupWithHandle(false, "", true, false))
        assertFalse(me.lakko.app.ui.chat.canCreateGroupWithHandle(false, "", true, true))
        assertTrue(me.lakko.app.ui.chat.canCreateGroupWithHandle(false, "Hike", true, true))
        assertFalse(me.lakko.app.ui.chat.canCreateGroupWithHandle(false, "Hike", false, false))
        assertFalse(me.lakko.app.ui.chat.canCreateGroupWithHandle(true, "Hike", true, false))
    }

    @Test
    fun `unseen reactions survive a round trip of their row`() {
        val raw = encodeUnseen(mapOf("aa" to setOf(3L, 1L), "bb" to emptySet()))
        assertEquals("aa:1,3", raw)
        assertEquals(setOf(1L, 3L), decodeUnseen(raw)["aa"])
        assertTrue(decodeUnseen("junk;x:;:1").isEmpty())
    }

    @Test
    fun `a sent request waits a day before Ask again`() {
        // Amended 2026-11-12 (UX 6.2): one day, was seven.
        assertEquals(24L * 60 * 60 * 1000, ASK_AGAIN_AFTER_MS)
        val r = SentRequest(idHex = "ab", name = "Hike", handle = "hike", sentAtMs = 0, knockKeyB64 = "")
        assertTrue(sentRequestWaiting(r, ASK_AGAIN_AFTER_MS - 1))
        assertFalse(sentRequestWaiting(r, ASK_AGAIN_AFTER_MS))
        assertFalse(mayAskAgain(r, 1))
        assertTrue(mayAskAgain(null, 1))
        assertEquals(listOf(r), decodeSentRequests(encodeSentRequests(listOf(r))))
        assertTrue(decodeSentRequests("not json").isEmpty())
    }

    @Test
    fun `a link request round-trips with what Ask again needs and no knock key`() {
        val r = SentRequest(
            idHex = "cd",
            name = "",
            handle = "",
            sentAtMs = 5,
            capabilityB64 = "Y2Fw",
            blobIdB64 = "YmxvYg==",
        )
        assertTrue(r.fromLink)
        assertFalse(SentRequest(idHex = "ab", name = "Hike", handle = "hike", sentAtMs = 0, knockKeyB64 = "a2V5").fromLink)
        assertEquals(listOf(r), decodeSentRequests(encodeSentRequests(listOf(r))))
        // A row written before the link fields existed still reads.
        val old = decodeSentRequests("""{"r":[{"idHex":"ab","name":"Hike","handle":"hike","sentAtMs":3,"knockKeyB64":"a2V5"}]}""")
        assertEquals(1, old.size)
        assertFalse(old[0].fromLink)
    }

    @Test
    fun `one rule decides the wait for the list, the invite page and the publisher card`() {
        assertTrue(requestStillWaiting(sentAtMs = 100, nowMs = 100 + ASK_AGAIN_AFTER_MS - 1))
        assertFalse(requestStillWaiting(sentAtMs = 100, nowMs = 100 + ASK_AGAIN_AFTER_MS))
        assertEquals(ASK_AGAIN_AFTER_MS, me.lakko.app.ui.channels.PUBLISHER_ASK_AGAIN_AFTER_MS)
    }

    @Test
    fun `nothing found only when the one list is empty and nothing is asked or explained`() {
        val f: (Int, Int, Boolean, Int, Boolean, Boolean) -> Boolean = { a, b, c, d, e, g ->
            me.lakko.app.search.searchShowsNothingFound(a, b, c, d, e, g)
        }
        assertTrue(f(0, 0, false, 0, false, false))
        assertFalse(f(1, 0, false, 0, false, false))
        assertFalse(f(0, 1, false, 0, false, false))
        assertFalse(f(0, 0, true, 0, false, false))
        assertFalse(f(0, 0, false, 1, false, false))
        assertFalse(f(0, 0, false, 0, true, false))
        assertFalse(f(0, 0, false, 0, false, true))
        // The end line never says "not found"; the empty list does.
        assertNull(me.lakko.app.search.searchEndLine(me.lakko.app.search.PeopleSearchState.NOT_FOUND))
        assertNull(me.lakko.app.search.searchEndLine(me.lakko.app.search.PeopleSearchState.SEARCHING))
        assertEquals(me.lakko.app.search.SEARCH_FAILED, me.lakko.app.search.searchEndLine(me.lakko.app.search.PeopleSearchState.FAILED))
        assertEquals(
            me.lakko.app.search.SEARCH_OUT_OF_SEARCHES,
            me.lakko.app.search.searchEndLine(me.lakko.app.search.PeopleSearchState.ALLOWANCE_USED),
        )
    }

    @Test
    fun `a group or channel the person is in matches its at-name and counts as a membership`() {
        assertTrue(conversationRowMatchesQuery("@hiking", "Trails", "", "hiking_club"))
        assertTrue(conversationRowMatchesQuery("HIKING", "Trails", "", "hiking_club"))
        assertFalse(conversationRowMatchesQuery("@", "Trails", "", "hiking_club"))
        assertFalse(conversationRowMatchesQuery("@sail", "Trails", "", ""))
        val group = ConversationListRow(
            id = "AA", kind = ConversationRowKind.GROUP, title = "g", preview = "",
            time = null, lastActivityMs = null, headId = "BB",
        )
        val channel = ConversationListRow(
            id = "cc", kind = ConversationRowKind.CHANNEL, title = "c", preview = "",
            time = null, lastActivityMs = null,
        )
        val ended = group.copy(id = "dd", headId = "dd", membershipEnded = true)
        val chat = group.copy(id = "ee", headId = "ee", kind = ConversationRowKind.CHAT)
        assertEquals(setOf("aa", "bb", "cc"), searchMemberIds(listOf(group, channel, ended, chat)))
    }

    @Test
    fun `a join request line names the link and the time`() {
        val nameOf = { k: String -> if (k == a) "Anna" else "?" }
        val whenOf = { _: Long -> "2 hours ago" }
        val linked = joinRequestLine(KnockOrigin(5, a), null, nameOf, whenOf)!!
        assertEquals("chat.members.join.invited_by", linked.key)
        assertEquals(mapOf("name" to "Anna", "when" to "2 hours ago"), linked.texts)
        val searched = joinRequestLine(KnockOrigin(5, null), null, nameOf, whenOf)!!
        assertEquals("chat.members.join.asked", searched.key)
        val old = joinRequestLine(null, a, nameOf, whenOf)!!
        assertEquals("chat.members.join.invited_by_name", old.key)
        assertNull(joinRequestLine(null, null, nameOf, whenOf))
        assertEquals(KnockOrigin(5, a), decodeKnockOrigin(encodeKnockOrigin(KnockOrigin(5, a))))
        assertEquals(KnockOrigin(null, null), decodeKnockOrigin(encodeKnockOrigin(KnockOrigin(null, null))))
        assertNull(decodeKnockOrigin("garbage"))
    }

    @Test
    fun `the knock position only moves forward`() {
        assertEquals(12L, knockCursorToSave(10, 12))
        assertNull(knockCursorToSave(12, 12))
        assertNull(knockCursorToSave(12, 5))
    }
}
