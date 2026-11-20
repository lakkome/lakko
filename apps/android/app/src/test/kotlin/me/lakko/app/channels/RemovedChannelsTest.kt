// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.runTest
import me.lakko.app.feed.PersistedChannelPost
import me.lakko.app.moderation.AccountStanding
import me.lakko.app.moderation.ModerationLogPins
import me.lakko.app.moderation.StandingAnswer
import me.lakko.app.moderation.SuspensionMarks
import me.lakko.app.moderation.SuspensionStandings
import me.lakko.app.moderation.VerifiedStanding
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.lakko_ffi_uniffi.InfoSection

/**
 * ADR-0089 step 5 — a channel Lakko's moderators removed (`410
 * channel-removed`): it is kept, its held posts stay readable, nothing that
 * writes is offered, reads stop to one per period, the line says "removed"
 * only when the moderation log proves it, and a normal answer restores it.
 */
class RemovedChannelsTest {

    private val channel = "0123456789abcdef0123456789abcdef"
    private var now = 1_000_000L
    private var answer: StandingAnswer = StandingAnswer.UNKNOWN
    private val asked = mutableListOf<String>()

    @Before
    fun setUp() {
        RemovedChannels.clear()
        RemovedChannels.nowMs = { now }
        RemovedChannels.standingOf = { id ->
            asked += id
            answer
        }
        RemovedChannels.launcher = { block -> runBlocking { block() } }
    }

    @After
    fun tearDown() {
        ChannelUnsubscribe.tellOtherDevices = savedTell
        ChannelUnsubscribe.serverClient = savedClient
        ChannelUnsubscribe.serverSign = savedSign
        ChannelUnsubscribe.serverHeaderName = savedHeader
        RemovedChannels.clear()
        RemovedChannels.nowMs = System::currentTimeMillis
        RemovedChannels.standingOf = { SuspensionMarks.channelAnswer(it) }
        SuspensionMarks.overrideChannelsForTests(null)
    }

    private val savedTell = ChannelUnsubscribe.tellOtherDevices
    private val savedClient = ChannelUnsubscribe.serverClient
    private val savedSign = ChannelUnsubscribe.serverSign
    private val savedHeader = ChannelUnsubscribe.serverHeaderName

    private fun removedError() = ChannelsApiException(410, RemovedChannels.CODE, "removed by moderators")

    @Test
    fun `a removed answer is recognised, and is not an end of the channel`() {
        val e = removedError()
        assertTrue(RemovedChannels.isRemovedError(e))
        assertTrue(RemovedChannels.isRemovedError(RuntimeException("wrapped", e)))
        assertFalse(ChannelRemovalWatch.isEnd(e))
        assertFalse(ChannelRemovalWatch.isDeleted(e))
        val deleted = ChannelsApiException(410, "channel-deleted", "x")
        assertFalse(RemovedChannels.isRemovedError(deleted))
        assertTrue(ChannelRemovalWatch.isDeleted(deleted))
    }

    @Test
    fun `an unverified removal shows the neutral line, never the removal`() {
        answer = StandingAnswer.UNKNOWN
        RemovedChannels.noteRemoved(channel)
        val r = RemovedChannels.state.value[channel]
        assertTrue(r!!.checked)
        assertFalse(r.verified)
        assertEquals("channels.unavailable", channelRemovedLineKey(r))
        assertEquals(listOf(channel), asked)
    }

    @Test
    fun `a log that says not removed still shows only the neutral line`() {
        answer = StandingAnswer(AccountStanding.NOT_SUSPENDED, now + 60_000)
        RemovedChannels.noteRemoved(channel.uppercase())
        assertEquals("channels.unavailable", channelRemovedLineKey(RemovedChannels.state.value[channel]))
    }

    @Test
    fun `a removal the log proves shows the removal line`() {
        answer = StandingAnswer(AccountStanding.SUSPENDED, now + 30 * 60_000)
        RemovedChannels.noteRemoved(channel)
        val r = RemovedChannels.state.value[channel]!!
        assertTrue(r.verified)
        assertEquals("suspended.channel_removed", channelRemovedLineKey(r))
        assertEquals(30 * 60_000L, r.periodMs)
    }

    @Test
    fun `a failing log check is the neutral line`() {
        RemovedChannels.standingOf = { error("network") }
        RemovedChannels.noteRemoved(channel)
        assertEquals("channels.unavailable", channelRemovedLineKey(RemovedChannels.state.value[channel]))
    }

    @Test
    fun `a channel not removed has no line and reads freely`() {
        assertNull(channelRemovedLineKey(null))
        repeat(3) { assertTrue(RemovedChannels.mayAsk(channel)) }
    }

    @Test
    fun `reads of a removed channel stop to one per period`() {
        answer = StandingAnswer(AccountStanding.SUSPENDED, now + 20 * 60_000)
        RemovedChannels.noteRemoved(channel)
        // The first period runs from the 410 itself.
        assertFalse(RemovedChannels.mayAsk(channel))
        now += RemovedChannels.FALLBACK_PERIOD_MS
        assertTrue(RemovedChannels.mayAsk(channel))
        assertFalse(RemovedChannels.mayAsk(channel))
        now += 20 * 60_000 - 1
        assertFalse(RemovedChannels.mayAsk(channel))
        now += 1
        assertTrue(RemovedChannels.mayAsk(channel))
    }

    @Test
    fun `the period is the verified reuse time, bounded, else the back off`() {
        val n = 5_000_000L
        assertEquals(RemovedChannels.FALLBACK_PERIOD_MS, RemovedChannels.periodOf(StandingAnswer.UNKNOWN, n))
        assertEquals(
            RemovedChannels.FALLBACK_PERIOD_MS,
            RemovedChannels.periodOf(StandingAnswer(AccountStanding.NOT_SUSPENDED, n + 999_999), n),
        )
        assertEquals(
            RemovedChannels.MIN_PERIOD_MS,
            RemovedChannels.periodOf(StandingAnswer(AccountStanding.SUSPENDED, n + 10), n),
        )
        assertEquals(
            RemovedChannels.MAX_PERIOD_MS,
            RemovedChannels.periodOf(StandingAnswer(AccountStanding.SUSPENDED, n + 100L * 3_600_000L), n),
        )
    }

    @Test
    fun `a normal answer restores the channel with no trace`() {
        answer = StandingAnswer(AccountStanding.SUSPENDED, now + 60_000)
        RemovedChannels.noteRemoved(channel)
        assertTrue(RemovedChannels.isRemoved(channel))
        RemovedChannels.noteAnswered(channel.uppercase())
        assertFalse(RemovedChannels.isRemoved(channel))
        assertNull(channelRemovedLineKey(RemovedChannels.state.value[channel]))
        assertTrue(RemovedChannels.mayAsk(channel))
    }

    @Test
    fun `the info withholds everything that writes or reads the channel`() {
        val all = listOf(
            InfoSection.HANDLE,
            InfoSection.CHANNEL_DESCRIPTION,
            InfoSection.RENAME_GROUP,
            InfoSection.INVITE_PEOPLE,
            InfoSection.INVITE_LINK,
            InfoSection.SUBSCRIBERS,
            InfoSection.ADMINS,
            InfoSection.REACTIONS,
            InfoSection.GROUP_TYPE,
            InfoSection.MEMBERS_CAN_FIND_ME,
            InfoSection.REPORT,
            InfoSection.UNSUBSCRIBE,
            InfoSection.DELETE_GROUP,
        )
        assertEquals(all, removedChannelSections(all, removed = false))
        assertEquals(
            listOf(
                InfoSection.HANDLE,
                InfoSection.CHANNEL_DESCRIPTION,
                InfoSection.MEMBERS_CAN_FIND_ME,
                InfoSection.UNSUBSCRIBE,
            ),
            removedChannelSections(all, removed = true),
        )
    }

    @Test
    fun `the owner of a removed channel can take it out of the list, but not delete it`() {
        val owner = listOf(
            InfoSection.HANDLE,
            InfoSection.CHANNEL_DESCRIPTION,
            InfoSection.RENAME_GROUP,
            InfoSection.INVITE_LINK,
            InfoSection.ADMINS,
            InfoSection.GROUP_TYPE,
            InfoSection.DELETE_GROUP,
        )
        val removed = removedChannelSections(owner, removed = true)
        assertTrue(InfoSection.UNSUBSCRIBE in removed)
        assertFalse(InfoSection.DELETE_GROUP in removed)
        assertEquals(1, removed.count { it == InfoSection.UNSUBSCRIBE })
        assertFalse(InfoSection.UNSUBSCRIBE in removedChannelSections(owner, removed = false))
    }

    @Test
    fun `the posts kept on the device stay readable`() {
        val t = timelineFromPostLog(
            listOf(
                PersistedChannelPost(cursor = 7, serverTsMs = 700, text = "second", wireB64 = "AQID"),
                PersistedChannelPost(cursor = 3, serverTsMs = 300, text = "first"),
            ),
        )
        assertEquals(listOf(3L, 7L), t.posts.map { it.cursor })
        assertEquals(listOf("first", "second"), t.posts.map { it.text })
        assertTrue(t.posts.none { it.unavailable })
        assertArrayEquals(byteArrayOf(1, 2, 3), t.posts[1].wire)
    }

    private fun client(status: HttpStatusCode, body: String): ChannelsClient =
        ChannelsClient(
            baseUrl = "http://test.invalid",
            engine = MockEngine {
                respond(content = body, status = status, headers = headersOf("Content-Type", "application/json"))
            },
        )

    @Test
    fun `a 410 channel-removed marks the channel and ends nothing`() = runTest {
        var deleted: String? = null
        ChannelsClient.onChannelDeleted = { deleted = it }
        try {
            client(HttpStatusCode.Gone, """{"code":"channel-removed","detail":"removed","log_index":12}""").use { c ->
                val e = try {
                    c.fetchPosts(channel, 0u)
                    null
                } catch (e: ChannelsApiException) {
                    e
                }
                assertTrue(RemovedChannels.isRemovedError(e))
            }
            assertTrue(RemovedChannels.isRemoved(channel))
            assertNull(deleted)
        } finally {
            ChannelsClient.onChannelDeleted = null
        }
    }

    @Test
    fun `a normal answer from the service restores a removed channel`() = runTest {
        RemovedChannels.noteRemoved(channel)
        assertTrue(RemovedChannels.isRemoved(channel))
        client(HttpStatusCode.OK, """{"posts":[],"next_after":0}""").use { c ->
            c.fetchPosts(channel, 0u)
        }
        assertFalse(RemovedChannels.isRemoved(channel))
    }

    @Test
    fun `the channel standing asks the shared service with the 16-byte id`() = runTest {
        val seen = mutableListOf<ByteArray>()
        SuspensionMarks.overrideChannelsForTests(
            SuspensionStandings(
                pins = ModerationLogPins(ByteArray(32) { 7 }, emptyList(), 1),
                prefixOf = { "%02x%02x".format(it[0].toInt() and 0xff, it[1].toInt() and 0xff) },
                fetch = { "answer" },
                verify = { _, id, _, _ ->
                    seen += id
                    VerifiedStanding(suspended = true, reuseUntilMs = Long.MAX_VALUE, logIndex = 3uL)
                },
                scope = this,
                prove = { _, _, _, _ -> true },
            ),
        )
        val a = SuspensionMarks.channelAnswer(channel)
        assertEquals(AccountStanding.SUSPENDED, a.standing)
        assertEquals(Long.MAX_VALUE, a.reuseUntilMs)
        assertArrayEquals(ChannelsComposer.channelIdFromHex(channel), seen.first())
        assertEquals(StandingAnswer.UNKNOWN, SuspensionMarks.channelAnswer("not-a-channel"))
    }

    @Test
    fun `the info of a removed channel has no Report`() {
        val subscriber = listOf(InfoSection.HANDLE, InfoSection.SUBSCRIBERS, InfoSection.REPORT, InfoSection.UNSUBSCRIBE)
        assertEquals(
            listOf(InfoSection.HANDLE, InfoSection.UNSUBSCRIBE),
            removedChannelSections(subscriber, removed = true),
        )
        assertTrue(InfoSection.REPORT in removedChannelSections(subscriber, removed = false))
    }

    @Test
    fun `an unsubscribe answered 410 is done, not a failure, and marks nothing`() = runTest {
        client(HttpStatusCode.Gone, """{"code":"channel-removed","detail":"removed","log_index":3}""").use { c ->
            c.unsubscribe(channel, "0a".repeat(32), "x-sig", "v")
        }
        assertFalse(RemovedChannels.isRemoved(channel))
    }

    private fun stateWithChannel(): ChannelsUiState = ChannelsUiState().also {
        it.addChannel(ChannelHandle(id = channel, localName = "News", localDescription = "", addedAtMs = 1uL))
        it.setSubscriberKeys(channel, ChannelSubscriberKeys(secret = ByteArray(32) { 5 }, publicKey = ByteArray(32) { 0x0a }))
    }

    @Test
    fun `a subscriber's unsubscribe of a removed channel completes on this device and tells the other devices`() {
        val told = kotlinx.coroutines.CompletableDeferred<Boolean>()
        val sent = kotlinx.coroutines.CompletableDeferred<String>()
        ChannelUnsubscribe.tellOtherDevices = { _, tell -> told.complete(tell) }
        ChannelUnsubscribe.serverSign = { _, _, _ -> "sig" }
        ChannelUnsubscribe.serverHeaderName = { "x-lakko-unsubscribe-sig" }
        ChannelUnsubscribe.serverClient = { base ->
            ChannelsClient(
                baseUrl = base,
                engine = MockEngine { request ->
                    sent.complete(request.method.value)
                    respond(
                        content = """{"code":"channel-removed","detail":"removed"}""",
                        status = HttpStatusCode.Gone,
                        headers = headersOf("Content-Type", "application/json"),
                    )
                },
            )
        }
        val state = stateWithChannel()
        RemovedChannels.noteRemoved(channel)

        ChannelUnsubscribe.leave(state, channel, "http://test.invalid")

        assertTrue(state.channels.value.none { it.id == channel })
        assertFalse(state.subscriberKeysByChannel.value.containsKey(channel))
        assertFalse(RemovedChannels.isRemoved(channel))
        runBlocking {
            kotlinx.coroutines.withTimeout(5_000) {
                assertTrue(told.await())
                assertEquals("DELETE", sent.await())
            }
        }
    }

    @Test
    fun `the owner's leave of a removed channel ends it on every device without the service`() {
        val told = kotlinx.coroutines.CompletableDeferred<Boolean>()
        var asked = false
        ChannelUnsubscribe.tellOtherDevices = { _, tell -> told.complete(tell) }
        ChannelUnsubscribe.serverClient = { asked = true; error("no service call expected") }
        val state = stateWithChannel()
        RemovedChannels.noteRemoved(channel)

        ChannelUnsubscribe.leaveRemoved(state, channel)

        assertTrue(state.channels.value.none { it.id == channel })
        assertFalse(RemovedChannels.isRemoved(channel))
        runBlocking { kotlinx.coroutines.withTimeout(5_000) { assertTrue(told.await()) } }
        runBlocking { kotlinx.coroutines.delay(100) }
        assertFalse(asked)
    }
}
