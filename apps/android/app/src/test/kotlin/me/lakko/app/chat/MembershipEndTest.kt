// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException
import uniffi.lakko_ffi_uniffi.FolderConversationKind
import uniffi.lakko_ffi_uniffi.FolderConversationRef
import uniffi.lakko_ffi_uniffi.FolderMemberState

/**
 * ADR-0087 section 9 - the end of a membership: the trigger, the epoch check,
 * the owed record and its order, the held Welcome, the handle replacement.
 *
 * The host JVM cannot load the native core, so [MembershipEndPorts] stands in
 * for the repository, and what these pin is the order and the rules. The
 * walk over a live group is the maintainer's, on running clients.
 */
class MembershipEndTest {

    private val own = setOf("own-account-key")

    // ---- the trigger ----------------------------------------------------

    @Test
    fun `a retired own leaf keeps its history, the account is still in the roster`() {
        assertFalse(
            membershipFullyEnded(
                isActive = false,
                isGroupKind = true,
                rosterAccountKeys = listOf("someone", "own-account-key"),
                ownAccountKeys = own,
            ),
        )
    }

    @Test
    fun `no leaf of this account left in a frozen roster ends the membership`() {
        assertTrue(membershipFullyEnded(false, true, listOf("someone", "else"), own))
    }

    @Test
    fun `isActive alone never ends it`() {
        assertFalse(membershipFullyEnded(true, true, listOf("someone"), own))
    }

    @Test
    fun `a torn read is not an end`() {
        assertFalse(membershipFullyEnded(null, true, listOf("someone"), own))
        assertFalse(membershipFullyEnded(false, true, null, own))
        assertFalse("no own key read", membershipFullyEnded(false, true, listOf("someone"), emptySet()))
    }

    @Test
    fun `a one-to-one never reads as ended here, no commit can take a person out`() {
        assertFalse(membershipFullyEnded(false, false, listOf("someone"), own))
    }

    @Test
    fun `an own predecessor key in the roster counts as this account`() {
        val keys = setOf("current", "predecessor")
        assertFalse(membershipFullyEnded(false, true, listOf("predecessor"), keys))
    }

    // ---- the epoch check before acting on a heard register ---------------

    @Test
    fun `a group held at or below the register's epoch ended`() {
        assertEquals(HeardEndVerdict.ACT, heardEndVerdict(true, true, 10u, 10u))
        assertEquals(HeardEndVerdict.ACT, heardEndVerdict(true, true, 9u, 10u))
    }

    @Test
    fun `a group held active above the register's epoch came back, nothing stops`() {
        assertEquals(HeardEndVerdict.ANSWER_JOINED, heardEndVerdict(true, true, 11u, 10u))
    }

    @Test
    fun `an inactive group below the register is wiped, above it it is another end`() {
        assertEquals(HeardEndVerdict.ACT, heardEndVerdict(true, false, 10u, 12u))
        assertEquals(HeardEndVerdict.NOTHING, heardEndVerdict(true, false, 13u, 12u))
    }

    @Test
    fun `a group not held, or not read, has nothing to act on`() {
        assertEquals(HeardEndVerdict.NOTHING, heardEndVerdict(false, null, null, 10u))
        assertEquals(HeardEndVerdict.NOTHING, heardEndVerdict(true, null, 5u, 10u))
        assertEquals(HeardEndVerdict.NOTHING, heardEndVerdict(true, true, null, 10u))
    }

    // ---- the owed record -------------------------------------------------

    private class MemStore : MembershipEndStore {
        val rows = LinkedHashMap<String, OwedEnd>()
        var refuseSaves = false

        override fun load(accountIdB64: String): List<OwedEnd> = rows.values.toList()

        override fun save(accountIdB64: String, row: OwedEnd): Boolean {
            if (refuseSaves) return false
            rows[row.idHex] = row
            return true
        }

        override fun remove(accountIdB64: String, idHex: String) {
            rows.remove(idHex)
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.clear()
        }
    }

    /** Records every step; [failAt] stands in for a crash at that step. */
    private class Ports(
        var publish: EndPublish = EndPublish.PUBLISHED,
        var mls: MlsDelete = MlsDelete.DELETED,
    ) : MembershipEndPorts {
        val steps = ArrayList<String>()
        var failAt: String? = null
        var persisted = true

        private fun step(name: String) {
            if (failAt == name) throw IllegalStateException("crash at $name")
            steps += name
        }

        override suspend fun publishEnded(row: OwedEnd): EndPublish {
            step("publish")
            return publish
        }

        override suspend fun stop(row: OwedEnd) = step("stop")

        override suspend fun wipeStores(row: OwedEnd) = step("wipe")

        override suspend fun deleteMls(row: OwedEnd): MlsDelete {
            step("delete")
            return mls
        }

        override suspend fun persistDurably(): Boolean {
            step("persist")
            return persisted
        }

        override suspend fun dropRegistry(row: OwedEnd) = step("drop")

        override suspend fun cameBack(row: OwedEnd) = step("cameBack")
    }

    private fun applied(left: Boolean = false) = appliedEnd("aa", "Team", left, 7u, nowMs = 0L)

    private fun run(row: OwedEnd, store: MemStore, ports: Ports): EndRun =
        runBlocking { runMembershipEnd("acct", row, store, ports) }

    @Test
    fun `the steps run in the order the spec gives, the registry last`() {
        val store = MemStore()
        val ports = Ports()
        assertEquals(EndRun.DONE, run(applied(), store, ports))
        assertEquals(listOf("publish", "stop", "wipe", "delete", "persist", "drop"), ports.steps)
    }

    @Test
    fun `a finished end keeps no row, the notice is never stored`() {
        // UX 9 (2026-11-06): the notice is shown live and not kept for later.
        val store = MemStore()
        run(applied(), store, Ports())
        assertTrue(store.rows.isEmpty())
        assertTrue("nothing is ending any more", endingIdsOf(store.load("acct")).isEmpty())
    }

    @Test
    fun `a person who left gets no notice and no row stays`() {
        val store = MemStore()
        store.save("acct", applied(left = true))
        assertFalse(store.rows.getValue("aa").noticeOwed)
        run(applied(left = true), store, Ports())
        assertTrue(store.rows.isEmpty())
    }

    @Test
    fun `a device that heard the register does not publish it and owes the notice`() {
        val store = MemStore()
        val ports = Ports()
        assertTrue(heardEnd("aa", "Team", 7u).noticeOwed)
        assertEquals(EndRun.DONE, run(heardEnd("aa", "Team", 7u), store, ports))
        assertEquals(listOf("stop", "wipe", "delete", "persist", "drop"), ports.steps)
        assertTrue(store.rows.isEmpty())
    }

    @Test
    fun `a chosen delete is the wipe alone, no register, no notice`() {
        val store = MemStore()
        val ports = Ports()
        assertEquals(EndRun.DONE, run(chosenEnd("aa", 0u), store, ports))
        assertEquals(listOf("wipe", "delete", "persist", "drop"), ports.steps)
        assertTrue(store.rows.isEmpty())
    }

    @Test
    fun `a closed notice stays closed while the steps finish`() {
        val store = MemStore()
        val row = heardEnd("aa", "Team", 7u).copy(noticeClosed = true)
        assertFalse(row.noticeOwed)
        run(row, store, Ports())
        assertTrue(store.rows.isEmpty())
    }

    @Test
    fun `nothing is wiped while the synced state is not open, and the run resumes`() {
        val store = MemStore()
        val ports = Ports(publish = EndPublish.NOT_OPEN)
        val row = applied()
        store.save("acct", row)
        assertEquals(EndRun.WAITING, run(row, store, ports))
        assertEquals(listOf("publish"), ports.steps)
        assertEquals(EndStage.RECORDED, store.rows.getValue("aa").stage)
        ports.publish = EndPublish.PUBLISHED
        assertEquals(EndRun.DONE, run(store.rows.getValue("aa"), store, ports))
        assertEquals(listOf("publish", "publish", "stop", "wipe", "delete", "persist", "drop"), ports.steps)
    }

    @Test
    fun `nothing to tell never stops the wipe`() {
        val store = MemStore()
        val ports = Ports(publish = EndPublish.NOTHING_TO_TELL)
        assertEquals(EndRun.DONE, run(applied(), store, ports))
        assertTrue("wipe" in ports.steps && "drop" in ports.steps)
    }

    @Test
    fun `a crash at any step resumes from the owed record and finishes`() {
        for (crash in listOf("publish", "stop", "wipe", "delete", "persist", "drop")) {
            val store = MemStore()
            val ports = Ports()
            val row = applied()
            store.save("acct", row)
            ports.failAt = crash
            assertEquals(crash, EndRun.RETRY, run(row, store, ports))
            assertFalse("the registry goes last: $crash", crash != "drop" && "drop" in ports.steps)
            // The process died: what is left is the record on disk.
            val resumed = store.rows.getValue("aa")
            ports.failAt = null
            ports.steps.clear()
            assertEquals(crash, EndRun.DONE, run(resumed, store, ports))
            assertEquals(crash, "drop", ports.steps.last())
            assertTrue(crash, store.rows.isEmpty())
        }
    }

    @Test
    fun `a crash after a step does not run the steps before it again`() {
        val store = MemStore()
        val ports = Ports()
        val row = applied()
        store.save("acct", row)
        ports.failAt = "wipe"
        run(row, store, ports)
        ports.failAt = null
        ports.steps.clear()
        run(store.rows.getValue("aa"), store, ports)
        assertEquals(listOf("wipe", "delete", "persist", "drop"), ports.steps)
    }

    @Test
    fun `the record does not move past a step it could not save`() {
        val store = MemStore()
        val ports = Ports()
        store.refuseSaves = true
        assertEquals(EndRun.RETRY, run(applied(), store, ports))
        assertEquals(listOf("publish"), ports.steps)
    }

    @Test
    fun `a snapshot that did not land keeps the known-groups row`() {
        val store = MemStore()
        val ports = Ports()
        ports.persisted = false
        assertEquals(EndRun.RETRY, run(applied(), store, ports))
        assertFalse("drop" in ports.steps)
        assertEquals(EndStage.STOPPED, store.rows.getValue("aa").stage)
    }

    @Test
    fun `a refused delete keeps the group and tries again`() {
        val store = MemStore()
        val ports = Ports(mls = MlsDelete.FAILED)
        assertEquals(EndRun.RETRY, run(applied(), store, ports))
        assertFalse("persist" in ports.steps)
    }

    @Test
    fun `the core refusing at the last step means this device came back`() {
        val store = MemStore()
        val ports = Ports(mls = MlsDelete.STILL_ACTIVE)
        assertEquals(EndRun.ABANDONED, run(heardEnd("aa", "Team", 7u), store, ports))
        assertEquals("cameBack", ports.steps.last())
        assertFalse("drop" in ports.steps)
        assertTrue(store.rows.isEmpty())
    }

    @Test
    fun `an end still running is on its way out, a notice row is not`() {
        val running = applied()
        val done = heardEnd("bb", "", 1u).copy(stage = EndStage.NOTICE_ONLY)
        assertEquals(setOf("aa"), endingIdsOf(listOf(running, done)))
    }

    @Test
    fun `the epoch survives the row as a full u64`() {
        val row = appliedEnd("AA", "", false, ULong.MAX_VALUE)
        assertEquals("aa", row.idHex)
        assertEquals(ULong.MAX_VALUE, row.epochValue)
    }

    // ---- the notice ------------------------------------------------------

    @Test
    fun `one removal reads by its name, several by the newest and a count`() {
        assertNull(endNoticeText(emptyList(), "Group"))
        val one = endNoticeText(listOf(MembershipEndNotice("aa", "Team", 1)), "Group")
        assertEquals("chat.ended.notice", one?.key)
        assertEquals("Team", one?.texts?.get("name"))
        val many = endNoticeText(
            listOf(
                MembershipEndNotice("aa", "Team", 1),
                MembershipEndNotice("bb", "", 3),
                MembershipEndNotice("cc", "News", 2),
                MembershipEndNotice("aa", "Team", 1),
            ),
            "Group",
        )
        assertEquals("chat.ended.notice_more", many?.key)
        // The newest, its blank name read by the catalog word, and two more.
        assertEquals("Group", many?.texts?.get("name"))
        assertEquals(2uL, many?.counts?.get("n"))
    }

    @Test
    fun `the banner's name falls back to the catalog word`() {
        val unnamed = endNoticeLine("", "Group")
        assertEquals("chat.ended.notice", unnamed.key)
        assertEquals("Group", unnamed.texts["name"])
        assertEquals("Team", endNoticeLine("Team", "Group").texts["name"])
    }

    @Test
    fun `the dialog holds the draft verbatim, blank is no draft`() {
        val notice = MembershipEndNotice("aa", "Team")
        assertEquals("  half a sentence ", endDialogFor(notice, "  half a sentence ").draft)
        assertNull(endDialogFor(notice, "   ").draft)
    }

    // ---- added again -----------------------------------------------------

    @Test
    fun `a Welcome with no record held is an ordinary join`() {
        assertEquals(JoinReplacement.NONE, joinReplacement(false, false, null))
    }

    @Test
    fun `a retired leaf that comes back keeps its history in a new handle`() {
        assertEquals(JoinReplacement.KEEP_HISTORY, joinReplacement(true, false, true))
    }

    @Test
    fun `a person who was out starts empty`() {
        assertEquals(JoinReplacement.FRESH, joinReplacement(true, false, false))
        assertEquals("an end still owed", JoinReplacement.FRESH, joinReplacement(true, true, true))
    }

    @Test
    fun `an old handle that cannot be read throws nothing away`() {
        assertEquals(JoinReplacement.KEEP_HISTORY, joinReplacement(true, false, null))
    }

    @Test
    fun `the teardown runs again for the handle a Welcome brought back`() {
        val latch = EndedTeardownLatch()
        val ended = MembershipState.Ended(left = false)
        assertEquals(ENDED_TEARDOWN_STEPS, latch.claim(ended))
        assertTrue(latch.claim(ended).isEmpty())
        latch.reset()
        assertEquals(ENDED_TEARDOWN_STEPS, latch.claim(ended))
    }

    // ---- the held Welcome ------------------------------------------------

    @Test
    fun `the refusal names the group through the wrapping`() {
        val refusal = FfiException.MembershipGroupStillActive("00ff")
        assertEquals("00ff", groupStillActiveHex(refusal))
        assertEquals("00ff", groupStillActiveHex(ChatError("joinFromWelcome failed", refusal)))
    }

    @Test
    fun `any other failure is not that refusal`() {
        assertNull(groupStillActiveHex(IllegalStateException("no")))
        assertNull(groupStillActiveHex(null))
        assertNull("a message that is no hex id", groupStillActiveHex(FfiException.MembershipGroupStillActive("oops")))
    }

    @Test
    fun `a read that did not reach the end decides nothing`() {
        assertEquals(HeldWelcomeVerdict.KEEP, heldWelcomeVerdict(false, true))
        assertEquals(HeldWelcomeVerdict.KEEP, heldWelcomeVerdict(false, false))
        assertEquals(HeldWelcomeVerdict.KEEP, heldWelcomeVerdict(false, null))
    }

    @Test
    fun `the journal drained and the group ended, or gone, retries the join`() {
        assertEquals(HeldWelcomeVerdict.RETRY, heldWelcomeVerdict(true, false))
        assertEquals(HeldWelcomeVerdict.RETRY, heldWelcomeVerdict(true, null))
    }

    @Test
    fun `the journal drained and the group still active is a duplicate or a forgery`() {
        assertEquals(HeldWelcomeVerdict.DROP, heldWelcomeVerdict(true, true))
    }

    @Test
    fun `identical bytes are held once and a group holds a few at most`() {
        val first = HeldWelcome("aa", "w0")
        assertEquals(listOf(first), withHeldWelcome(listOf(first), first))
        var rows = emptyList<HeldWelcome>()
        for (i in 0..MAX_HELD_WELCOMES_PER_GROUP) rows = withHeldWelcome(rows, HeldWelcome("aa", "w$i"))
        rows = withHeldWelcome(rows, HeldWelcome("bb", "other"))
        assertEquals(MAX_HELD_WELCOMES_PER_GROUP, rows.count { it.groupIdHex == "aa" })
        assertFalse("the oldest goes", rows.contains(first))
        assertEquals(1, rows.count { it.groupIdHex == "bb" })
    }

    // ---- the wipes that are rules ----------------------------------------

    private fun ref(byte: Int) = FolderConversationRef(FolderConversationKind.GROUP, ByteArray(32) { byte.toByte() })

    private fun folder(
        included: List<FolderConversationRef> = emptyList(),
        excluded: List<FolderConversationRef> = emptyList(),
        pinned: List<FolderConversationRef> = emptyList(),
    ) = ChatFolder(ByteArray(16), "F", 0u, 0u, included, excluded, pinned)

    @Test
    fun `a conversation leaves every folder by a none register and its pin goes`() {
        var stamp = 0uL
        val gone = ref(1)
        val stays = ref(2)
        val changes = conversationRemovalChanges(
            listOf(folder(included = listOf(gone, stays), pinned = listOf(stays, gone)), folder(excluded = listOf(gone))),
            gone,
        ) { ++stamp }
        checkNotNull(changes)
        assertEquals(2, changes.members.size)
        assertTrue(changes.members.all { it.state == FolderMemberState.NONE })
        assertEquals(1, changes.pins.size)
        assertEquals(1, changes.pins.single().pinned.size)
    }

    @Test
    fun `a folder that holds nothing of it writes nothing`() {
        assertNull(conversationRemovalChanges(listOf(folder(included = listOf(ref(2)))), ref(1)) { 1uL })
    }

    @Test
    fun `one journal's own echoes go, another's stay`() {
        val echoes = OwnEchoRegistry()
        echoes.remember("aa", 1)
        echoes.remember("bb", 1)
        echoes.forgetGroup("aa")
        assertFalse(echoes.consume("aa", 1))
        assertTrue(echoes.consume("bb", 1))
    }

    @Test
    fun `one group's watermarks go, another's stay`() {
        val store = InMemoryReadWatermarkStore()
        store.merge("acct", "aa", mapOf("k" to 4L))
        store.merge("acct", "bb", mapOf("k" to 4L))
        store.deleteGroup("acct", "AA")
        assertTrue(store.load("acct", "aa").isEmpty())
        assertEquals(1, store.load("acct", "bb").size)
    }

    @Test
    fun `an unread count goes with its group`() {
        val unread = LocalUnreadCounter()
        unread.onIncomingApplied("aa", listOf(1L, 2L))
        unread.onIncomingApplied("bb", listOf(1L))
        unread.forget("aa")
        assertEquals(0, unread.snapshot("aa").count)
        assertEquals(1, unread.snapshot("bb").count)
    }

    @Test
    fun `unsent text goes with its conversation`() {
        val carry = ComposerCarry()
        carry.note("AA", "typed")
        carry.forget("aa")
        assertNull(carry.textOf("aa"))
    }

    @Test
    fun `no continuity fact of the group stays`() {
        val row = ContinuityRow()
            .withMoved("aa", 1L, oneToOne = true)
            .withResendCursor("aa", 5L)
            .withPushRetired("aa")
            .withAwaitedSwaps("aa", mapOf("p" to 1L))
            .withMoved("bb", 2L)
        val after = row.withoutGroup("aa").withResent("cc").let { it.copy(resent = it.resent - "cc") }
        assertEquals(setOf("bb"), after.moved.keys)
        assertTrue(after.movedOneToOne.isEmpty())
        assertTrue(after.resendCursor.isEmpty())
        assertTrue(after.pushRetired.isEmpty())
        assertTrue(after.swapping.isEmpty())
    }

    @Test
    fun anOpenConversationWaitsForItsNoticeWhileTheEndIsOwed() {
        assertFalse(conversationIsGone(sawConversation = true, held = false, endOwed = true))
        assertTrue(conversationIsGone(sawConversation = true, held = false, endOwed = false))
        assertFalse(conversationIsGone(sawConversation = true, held = true, endOwed = false))
        assertFalse(conversationIsGone(sawConversation = false, held = false, endOwed = false))
    }

    @Test
    fun `an end under way is not active, whatever the read says`() {
        // The wipe deletes the group and the read answers null, which means
        // active: the composer came back behind the dialog.
        assertEquals(false, activeBehindEnd(isActive = null, ending = true))
        assertEquals(false, activeBehindEnd(isActive = true, ending = true))
        assertNull(activeBehindEnd(isActive = null, ending = false))
        assertEquals(true, activeBehindEnd(isActive = true, ending = false))
    }

    @Test
    fun `the title stays behind the dialog after the wipe took the name`() {
        assertEquals("Hikers", titleBehindEnd(live = "Group", held = "Hikers", ending = true))
        assertEquals("Renamed", titleBehindEnd(live = "Renamed", held = "Hikers", ending = false))
        // Nothing held: the live title, never nothing.
        assertEquals("Group", titleBehindEnd(live = "Group", held = null, ending = true))
    }

    @Test
    fun `a title read during the wipe is never the one held`() {
        assertTrue(holdsTitle(threadKnown = true, ending = false))
        assertFalse(holdsTitle(threadKnown = true, ending = true))
        assertFalse(holdsTitle(threadKnown = false, ending = false))
    }

    @Test
    fun `a removed person is never told the conversation is being set up`() {
        assertFalse(showsSettingUpLine(threadKnown = false, ending = true))
        assertTrue(showsSettingUpLine(threadKnown = false, ending = false))
        assertFalse(showsSettingUpLine(threadKnown = true, ending = false))
    }

    @Test
    fun `a group of one has no call controls and they come with a second person`() {
        assertFalse(showsCallControls(isGroup = true, people = 1))
        assertTrue(showsCallControls(isGroup = true, people = 2))
        assertTrue(showsCallControls(isGroup = true, people = 7))
        // A roster not read yet does not flash them on a group of one.
        assertFalse(showsCallControls(isGroup = true, people = null))
    }

    @Test
    fun `a one-to-one keeps its call controls whatever the roster reads`() {
        assertTrue(showsCallControls(isGroup = false, people = 2))
        assertTrue(showsCallControls(isGroup = false, people = 1))
        assertTrue(showsCallControls(isGroup = false, people = null))
    }

    @Test
    fun `an end shows no row saying the messages are still here`() {
        val ended = MembershipState.Ended(left = false)
        assertNull(terminalRowBehindEnd(ended, ending = true))
        // A retired leaf keeps its history and its row.
        assertEquals(terminalRow(ended), terminalRowBehindEnd(ended, ending = false))
    }

    @Test
    fun `the info screen behind the dialog keeps nothing that acts and no frozen line`() {
        val behind = infoBehindEnd(ending = true, membershipEnded = true)
        assertFalse(behind.exitBlock)
        assertFalse(behind.frozenRosterLine)
        assertFalse(behind.membersCanFindMe)
    }

    @Test
    fun `a retired leaf keeps its frozen line and its exit, a live group its switch`() {
        val retired = infoBehindEnd(ending = false, membershipEnded = true)
        assertTrue(retired.exitBlock)
        assertTrue(retired.frozenRosterLine)
        val live = infoBehindEnd(ending = false, membershipEnded = false)
        assertTrue(live.exitBlock)
        assertFalse(live.frozenRosterLine)
        assertTrue(live.membersCanFindMe)
    }

    @Test
    fun `a removal noticed while the notice is up gives the combined text, in either order`() {
        // UX 9 (2026-11-06): one notice at a time, never a pile of banners.
        val older = MembershipEndNotice("aa", "QA Phase A Walk", 1_000L)
        val newer = MembershipEndNotice("bb", "QA Base Alone", 2_000L)
        val combined = endNoticeText(listOf(newer, older), "Group")
        assertEquals(combined, endNoticeText(listOf(older, newer), "Group"))
        assertEquals("chat.ended.notice_more", combined?.key)
        assertEquals("QA Base Alone", combined?.texts?.get("name"))
        assertEquals(1uL, combined?.counts?.get("n"))
    }
}
