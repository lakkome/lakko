// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import kotlinx.coroutines.test.runTest
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenQuotaExhausted
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0099 section 2, UX section 2.2 / 2.3 — the report sheet's rules and every send state. */
class ReportTest {

    private val author = "QVVUSE9SLUFDQ09VTlQtS0VZLTMyLUJZVEVTLSEhISE="
    private val evidence = MessageEvidence(AttestationVerdict.VERIFIED, "cA==", "aw==", null)

    private fun subject(also: ReportAlso = ReportAlso.Block(author), pickAuthor: String? = author) = ReportSubject(
        target = ReportTargetRef.Account(author),
        name = "Anna",
        conversationKind = ReportConversationKind.ONE_TO_ONE,
        pickGroupIdHex = "ab",
        pickAuthorKeyB64 = pickAuthor,
        initialMessages = listOf(ReportableMessage(7, "ab", evidence)),
        other = emptyList(),
        also = also,
    )

    // ---------------------------------------------------------------- the sheet

    @Test
    fun `send needs a reason and takes no second tap`() {
        val s0 = ReportSheetState.initial(subject())
        assertFalse(s0.canSend)
        val s1 = s0.withReason(1u)
        assertTrue(s1.canSend)
        assertFalse(s1.copy(phase = ReportPhase.SENDING).canSend)
        assertTrue(s1.copy(phase = ReportPhase.FAILED).canSend)
    }

    @Test
    fun `also block is on by default and leaving is off`() {
        assertTrue(ReportSheetState.initial(subject()).also)
        assertFalse(ReportSheetState.initial(subject(also = ReportAlso.Leave("ab"))).also)
        assertFalse(ReportSheetState.initial(subject(also = ReportAlso.Unsubscribe("cd"))).also)
    }

    @Test
    fun `the comment is cut at 500 characters and picks at 20 messages`() {
        val long = "a".repeat(REPORT_COMMENT_LIMIT + 30)
        assertEquals(REPORT_COMMENT_LIMIT, ReportSheetState().withComment(long).comment.length)
        // A character outside the BMP counts as one.
        val emoji = "😀".repeat(REPORT_COMMENT_LIMIT + 1)
        assertEquals(REPORT_COMMENT_LIMIT, ReportSheetState().withComment(emoji).comment.codePointCount(0, REPORT_COMMENT_LIMIT * 2))
        val many = (1L..30L).map { ReportableMessage(it, "ab", evidence) }
        assertEquals(REPORT_MESSAGE_LIMIT, ReportSheetState().withPicked(many).picked.size)
    }

    @Test
    fun `the included line counts, reports a gone message and says when nothing is included`() {
        assertEquals(IncludedLine("report.included_none", 0, 0), includedLine(emptyList(), 0))
        assertEquals(IncludedLine("report.included", 1, 1), includedLine(listOf(ReportableMessage(1, "ab", evidence)), 0))
        assertEquals(
            IncludedLine("report.included_partial", 2, 3),
            includedLine(
                listOf(ReportableMessage(1, "ab", evidence), ReportableMessage(2, "ab", null), ReportableMessage(3, "ab", evidence)),
                0,
            ),
        )
        assertEquals(IncludedLine("report.included", 1, 1), includedLine(emptyList(), 1))
    }

    @Test
    fun `a person target picks only that person's messages and never own ones`() {
        val other = "T1RIRVItQUNDT1VOVC1LRVktMzItQllURVMtISEhISE="
        assertTrue(pickable(subject(), author, isOwn = false))
        assertFalse(pickable(subject(), other, isOwn = false))
        assertFalse(pickable(subject(), author, isOwn = true))
        assertTrue(pickable(subject(pickAuthor = null), other, isOwn = false))
        assertFalse(pickable(subject(pickAuthor = null), null, isOwn = false))
    }

    @Test
    fun `a moderator list older than the highest seen is refused`() {
        assertTrue(moderatorListAccepted(3u, null))
        assertTrue(moderatorListAccepted(3u, 3u))
        assertTrue(moderatorListAccepted(4u, 3u))
        assertFalse(moderatorListAccepted(2u, 3u))
    }

    @Test
    fun `reason keys follow the catalog`() {
        assertEquals("report.reason.child_safety", reasonKey("child-safety"))
        assertEquals("report.reason.spam", reasonKey("spam"))
        assertNull(reasonKey(null))
    }

    @Test
    fun `a sent report is first in the list and replaces its own receipt`() {
        val a = SentReport("a", "Anna", 1, 10)
        val b = SentReport("b", "Bo", 2, 20)
        assertEquals(listOf(b, a), withSentReport(listOf(a), b))
        assertEquals(listOf(a), withSentReport(listOf(a), a))
    }

    @Test
    fun `the root key is read from base64url and refused at any other length`() {
        val key = ByteArray(32) { it.toByte() }
        val encoded = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(key)
        assertTrue(key.contentEquals(pinnedModerationRootKey(encoded)))
        assertNull(pinnedModerationRootKey(""))
        assertNull(pinnedModerationRootKey("AAAA"))
    }

    // ---------------------------------------------------------------- the send

    private class FakePorts(
        var root: ByteArray? = ByteArray(32),
        var list: ModeratorListFetch = ModeratorListFetch.Ok(byteArrayOf(1)),
        var version: ULong = 5u,
        var highest: ULong? = null,
        val posts: MutableList<ReportPost> = mutableListOf(ReportPost.Created("r".repeat(22))),
        var token: () -> String = { "tok" },
    ) : ReportPorts {
        val tokens = mutableListOf<String>()
        var recorded: ULong? = null
        var sealed = 0

        override fun rootKey(): ByteArray? = root
        override suspend fun fetchModeratorList(): ModeratorListFetch = list
        override fun moderatorListVersion(list: ByteArray, rootKey: ByteArray): ULong = version
        override fun highestListVersion(): ULong? = highest
        override fun recordListVersion(version: ULong) {
            recorded = version
        }

        override suspend fun takeToken(): String = token().also { tokens += it }
        override suspend fun seal(list: ByteArray, rootKey: ByteArray): ByteArray {
            sealed += 1
            return byteArrayOf(9)
        }

        override suspend fun post(report: ByteArray, tokenHeaderValue: String): ReportPost =
            posts.removeAt(0)
    }

    @Test
    fun `a report is sent and the list version kept`() = runTest {
        val ports = FakePorts()
        assertEquals(ReportOutcome.Sent("r".repeat(22)), sendReport(ports))
        assertEquals(5uL, ports.recorded)
        assertEquals(1, ports.tokens.size)
    }

    @Test
    fun `no pinned root, no list or an older list is unavailable and nothing is sealed`() = runTest {
        for (ports in listOf(
            FakePorts(root = null),
            FakePorts(list = ModeratorListFetch.Unavailable),
            FakePorts(version = 2u, highest = 3u),
        )) {
            assertEquals(ReportOutcome.Unavailable, sendReport(ports))
            assertEquals(0, ports.sealed)
        }
    }

    @Test
    fun `a list that does not verify is unavailable`() = runTest {
        val ports = object : ReportPorts by FakePorts() {
            override fun moderatorListVersion(list: ByteArray, rootKey: ByteArray): ULong = error("bad signature")
        }
        assertEquals(ReportOutcome.Unavailable, sendReport(ports))
    }

    @Test
    fun `no network is a failure the sheet can retry`() = runTest {
        assertEquals(ReportOutcome.Failed, sendReport(FakePorts(list = ModeratorListFetch.Network)))
        assertEquals(ReportOutcome.Failed, sendReport(FakePorts(posts = mutableListOf(ReportPost.Network))))
        assertEquals(ReportOutcome.Failed, sendReport(FakePorts(posts = mutableListOf(ReportPost.Refused(400, "bad-report")))))
    }

    @Test
    fun `the daily token allowance and the service rate limit are the limit state`() = runTest {
        val quota = FakePorts(token = { throw AnonTokenQuotaExhausted(AnonTokenContext.REPORT, 60) })
        assertEquals(ReportOutcome.Limited, sendReport(quota))
        assertEquals(ReportOutcome.Limited, sendReport(FakePorts(posts = mutableListOf(ReportPost.Refused(429, "rate-limited")))))
    }

    @Test
    fun `a 503 from the service is unavailable`() = runTest {
        val ports = FakePorts(posts = mutableListOf(ReportPost.Refused(503, "reporting-unavailable")))
        assertEquals(ReportOutcome.Unavailable, sendReport(ports))
    }

    @Test
    fun `a spent token is replaced once`() = runTest {
        var n = 0
        val ports = FakePorts(
            posts = mutableListOf(ReportPost.Refused(401, "anon-token-spent"), ReportPost.Created("x".repeat(22))),
            token = { "t${n++}" },
        )
        assertEquals(ReportOutcome.Sent("x".repeat(22)), sendReport(ports))
        assertEquals(listOf("t0", "t1"), ports.tokens)
    }

    @Test
    fun `the sheet asks on opening whether reporting is available and seals nothing`() = runTest {
        val ok = FakePorts()
        assertEquals(ReportAvailability.AVAILABLE, checkReportAvailability(ok))
        assertEquals(0, ok.sealed)
        assertTrue(ok.tokens.isEmpty())
        assertEquals(5uL, ok.recorded)
        for (ports in listOf(
            FakePorts(root = null),
            FakePorts(list = ModeratorListFetch.Unavailable),
            FakePorts(version = 2u, highest = 3u),
        )) {
            assertEquals(ReportAvailability.UNAVAILABLE, checkReportAvailability(ports))
        }
        val unverified = object : ReportPorts by FakePorts() {
            override fun moderatorListVersion(list: ByteArray, rootKey: ByteArray): ULong = error("bad signature")
        }
        assertEquals(ReportAvailability.UNAVAILABLE, checkReportAvailability(unverified))
        assertEquals(ReportAvailability.UNKNOWN, checkReportAvailability(FakePorts(list = ModeratorListFetch.Network)))
    }

    @Test
    fun `send stays off once reporting is not available, whatever is edited`() {
        val s = ReportSheetState.initial(subject()).withReason(1u).copy(phase = ReportPhase.UNAVAILABLE)
        assertFalse(s.canSend)
        assertEquals(ReportPhase.UNAVAILABLE, s.withReason(2u).phase)
        assertEquals(ReportPhase.UNAVAILABLE, s.withComment("more").phase)
        assertFalse(s.withReason(2u).canSend)
        // Other refusals still clear on an edit.
        assertEquals(ReportPhase.EDITING, s.copy(phase = ReportPhase.LIMITED).withReason(2u).phase)
        assertTrue(s.copy(phase = ReportPhase.FAILED).withReason(2u).canSend)
    }

    @Test
    fun `the block still applies at the limit, leaving only after a send`() {
        val block = ReportAlso.Block(author)
        val leave = ReportAlso.Leave("ab")
        assertTrue(alsoApplies(ReportOutcome.Sent("r"), leave))
        assertTrue(alsoApplies(ReportOutcome.Limited, block))
        assertTrue(alsoApplies(ReportOutcome.Unavailable, block))
        assertFalse(alsoApplies(ReportOutcome.Limited, leave))
        assertFalse(alsoApplies(ReportOutcome.Failed, block))
        assertNull(phaseAfter(ReportOutcome.Sent("r")))
        assertEquals(ReportPhase.LIMITED, phaseAfter(ReportOutcome.Limited))
        assertEquals(ReportPhase.UNAVAILABLE, phaseAfter(ReportOutcome.Unavailable))
        assertEquals(ReportPhase.FAILED, phaseAfter(ReportOutcome.Failed))
    }

    @Test
    fun `subjects carry the right target, picking and also`() {
        val msg = ReportableMessage(1, "ab", evidence)
        val m = messageReportSubject(author, "Anna", "ab", oneToOne = false, message = msg)
        assertEquals(ReportConversationKind.GROUP, m.conversationKind)
        assertEquals(listOf(msg), m.initialMessages)
        assertEquals(ReportAlso.Block(author), m.also)
        val g = groupReportSubject("club", "Club", "ab")
        assertEquals(ReportTargetKind.GROUP, g.targetKind)
        assertNull(g.pickAuthorKeyB64)
        assertEquals(ReportAlso.Leave("ab"), g.also)
        val c = channelReportSubject("00", "News", ReportConversationKind.DISCOVER, subscribed = false)
        assertEquals(ReportAlso.None, c.also)
        val r = contactRequestReportSubject(author, "Anna", byteArrayOf(1))
        assertEquals(ReportConversationKind.CONTACT_REQUEST, r.conversationKind)
        assertEquals(1, r.other.size)
        assertNull(r.pickGroupIdHex)
    }

    // ---------------------------------------------------------------- channel posts

    private fun b64url(bytes: ByteArray): String = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    @Test
    fun `a channel-info report picks from the stored posts, newest first, with the stored wire or the one in memory`() {
        val logged = listOf(
            me.lakko.app.feed.PersistedChannelPost(1, 10, "old", wireB64 = b64url(byteArrayOf(1))),
            me.lakko.app.feed.PersistedChannelPost(3, 30, "new", wireB64 = null),
            me.lakko.app.feed.PersistedChannelPost(2, 20, "mid", wireB64 = null),
        )
        val candidates = channelPostCandidates(logged, inMemoryWireB64 = mapOf(3L to b64url(byteArrayOf(3))))
        assertEquals(listOf(3L, 2L, 1L), candidates.map { it.cursor })
        assertEquals(listOf("new", "mid", "old"), candidates.map { it.text })
        assertTrue(byteArrayOf(3).contentEquals(candidates[0].wire))
        assertNull(candidates[1].wire)
        assertTrue(byteArrayOf(1).contentEquals(candidates[2].wire))
    }

    @Test
    fun `picked posts become channel-post evidence and a post without its wire is left out`() {
        val channelHex = "00112233445566778899aabbccddeeff"
        val posts = listOf(
            ReportablePost(5, 50, "a", byteArrayOf(5, 5)),
            ReportablePost(4, 40, "b", null),
        )
        val items = evidenceItems(emptyList(), emptyList(), posts, channelHex)
        assertEquals(1, items.size)
        val item = items.single() as uniffi.lakko_ffi_uniffi.FfiEvidenceItem.ChannelPost
        assertEquals(16, item.channelId.size)
        assertTrue(byteArrayOf(5, 5).contentEquals(item.post))
        assertEquals(IncludedLine("report.included_partial", 1, 2), includedLine(emptyList(), 0, posts))
        // At most 20 posts are kept on a pick.
        val many = (1L..30L).map { ReportablePost(it, it, "p", byteArrayOf(1)) }
        assertEquals(REPORT_MESSAGE_LIMIT, ReportSheetState().withPickedPosts(many).pickedPosts.size)
    }

    @Test
    fun `channel info offers Add messages over the channel's posts, other entry points do not`() {
        val info = channelReportSubject("00", "News", ReportConversationKind.CHANNEL, pickPosts = true)
        assertEquals("00", info.pickChannelIdHex)
        assertNull(channelReportSubject("00", "News", ReportConversationKind.DISCOVER).pickChannelIdHex)
    }

    @Test
    fun `a synced text post keeps its wire in the log`() = runTest {
        val deps = object : me.lakko.app.feed.ChannelSyncDeps {
            override suspend fun fetchPage(channelIdHex: String, after: Long, limit: Int) =
                me.lakko.app.feed.ChannelPostPage(
                    if (after == 0L) listOf(me.lakko.app.feed.ChannelPostWire(1, 1_000, "AQI")) else emptyList(),
                )

            override suspend fun decode(channelIdHex: String, payloadB64: String) =
                me.lakko.app.feed.PostOutcome.Text("hi")
        }
        val slot = me.lakko.app.feed.syncChannelSlot("00", null, deps, nowMs = 1_000)
        assertEquals("AQI", slot.posts.single().wireB64)
    }

    @Test
    fun `report statuses and receipt ids parse strictly`() {
        assertEquals(ReportStatus.ACTION, ReportStatus.fromWire("action"))
        assertNull(ReportStatus.fromWire("unknown"))
        assertTrue(ModerationClient.RECEIPT_ID.matches("AbCdEfGhIjKlMnOpQrStUv"))
        assertFalse(ModerationClient.RECEIPT_ID.matches("../etc"))
        assertEquals("abc", jsonField("""{"receipt_id":"abc"}""", "receipt_id"))
        assertNull(jsonField("not json", "error"))
    }
}
