// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import android.content.Context
import kotlinx.coroutines.test.runTest
import me.lakko.app.account.AccountStoreGate
import me.lakko.app.channels.ChannelHandle
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import uniffi.lakko_ffi_uniffi.ChannelNotifyBatch
import uniffi.lakko_ffi_uniffi.ChannelNotifyContext
import uniffi.lakko_ffi_uniffi.ChannelPostCandidate
import uniffi.lakko_ffi_uniffi.ChannelReadKind

/**
 * `docs/ux/channel-notifications.md` — the wiring around the shared rule: the
 * inputs a read hands it, what is shown from its answer, the per-device
 * "last notified post" cursor, and the background read. The rule itself is
 * tested in Rust (`lakko_ui_rules::channel_notify`); the host JVM has no
 * native library, so a spy stands in for it here.
 */
@RunWith(RobolectricTestRunner::class)
class ChannelNotifyTest {
    private val account = "AAECAwQFBgcICQoLDA0ODw=="
    private val channel = "0123456789abcdef0123456789abcdef"
    private val other = "fedcba9876543210fedcba9876543210"
    private val now = 1_800_000_000_000L

    private lateinit var context: Context

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
        AccountStoreGate.resetForTest()
    }

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private fun post(cursor: Long, text: String = "p$cursor") = PersistedChannelPost(cursor, now - 1_000, text)

    /** A rule that records what it was asked and answers [notify]. */
    private class SpyRule(private val notify: List<Int> = emptyList(), private val last: Long = 0L) : ChannelNotifyRule {
        val contexts = mutableListOf<ChannelNotifyContext>()
        val posts = mutableListOf<List<ChannelPostCandidate>>()

        override fun postsToNotify(context: ChannelNotifyContext, posts: List<ChannelPostCandidate>): ChannelNotifyBatch {
            contexts += context
            this.posts += posts
            val stored = if (last != 0L) last else posts.maxOfOrNull { it.cursor.toLong() } ?: context.lastNotified.toLong()
            return ChannelNotifyBatch(notify.map { it.toUInt() }, stored.toULong())
        }
    }

    private fun facts(
        muted: Boolean = false,
        open: Boolean = false,
        own: Boolean = false,
        lastNotified: Long = 0L,
    ) = ChannelNotifyFacts(muted, open, own, lastNotified, now)

    // ---- read kind -----------------------------------------------------------

    @Test
    fun aReadWithASlotBeforeIsOngoingAndOneWithoutIsFirstOrHistoryByTheMarker() {
        assertEquals(ChannelReadKind.ONGOING, channelReadKind(ChannelPostSlot(cursor = 3), null))
        assertEquals(ChannelReadKind.ONGOING, channelReadKind(ChannelPostSlot(cursor = 3), 9L))
        assertEquals(ChannelReadKind.FIRST_AFTER_SUBSCRIBE, channelReadKind(null, null))
        assertEquals(ChannelReadKind.HISTORY, channelReadKind(null, 4L))
    }

    // ---- decision wiring -----------------------------------------------------

    @Test
    fun theRuleIsAskedAboutTheNewPostsAndOneNonPostRecordAtTheNewCursor() {
        val rule = SpyRule()
        val before = ChannelPostSlot(cursor = 5, posts = listOf(post(4), post(5)))
        // Posts 6 and 8 opened; 7 and 9 were records that are no posts.
        val after = ChannelPostSlot(cursor = 9, posts = listOf(post(4), post(5), post(6), post(8)))
        decideChannelRead(rule, ChannelRead(channel, before, after, readMarker = 5L), facts(own = true, lastNotified = 5L))

        val asked = rule.posts.single()
        assertEquals(listOf(6UL, 8UL, 9UL), asked.map { it.cursor })
        assertEquals(listOf(true, true, false), asked.map { it.isPost })
        assertTrue(asked.all { it.own })
        assertEquals(listOf((now - 1_000).toULong(), (now - 1_000).toULong(), 0UL), asked.map { it.serverTsMs })
        val ctx = rule.contexts.single()
        assertEquals(ChannelReadKind.ONGOING, ctx.readKind)
        assertEquals(5UL, ctx.readMarker)
        assertEquals(5UL, ctx.lastNotified)
        assertEquals(now.toULong(), ctx.nowMs)
    }

    @Test
    fun noNonPostRecordIsAddedWhenTheNewCursorIsAPost() {
        val rule = SpyRule()
        val before = ChannelPostSlot(cursor = 5, posts = listOf(post(5)))
        val after = ChannelPostSlot(cursor = 6, posts = listOf(post(5), post(6)))
        decideChannelRead(rule, ChannelRead(channel, before, after, null), facts())
        assertEquals(listOf(6UL), rule.posts.single().map { it.cursor })
    }

    @Test
    fun mutedAndOpenReachTheRuleAndAFirstReadHasNoMarker() {
        val rule = SpyRule()
        val after = ChannelPostSlot(cursor = 2, posts = listOf(post(1), post(2)))
        decideChannelRead(rule, ChannelRead(channel, null, after, null), facts(muted = true, open = true))
        val ctx = rule.contexts.single()
        assertTrue(ctx.muted)
        assertTrue(ctx.openAndVisible)
        assertEquals(0UL, ctx.readMarker)
        assertEquals(ChannelReadKind.FIRST_AFTER_SUBSCRIBE, ctx.readKind)
        assertEquals(listOf(1UL, 2UL), rule.posts.single().map { it.cursor })
    }

    @Test
    fun theLastIndexTheRuleNamesIsTheOneShownAndItsCursorIsStored() {
        val rule = SpyRule(notify = listOf(0, 1), last = 9L)
        val before = ChannelPostSlot(cursor = 5)
        val after = ChannelPostSlot(cursor = 9, posts = listOf(post(6, "first"), post(8, "second")))
        val d = decideChannelRead(rule, ChannelRead(channel, before, after, 5L), facts())
        assertEquals("second", d.post?.text)
        assertEquals(9L, d.lastNotified)
    }

    @Test
    fun nothingShownStillStoresTheCursorTheRuleGave() {
        val rule = SpyRule(notify = emptyList(), last = 12L)
        val d = decideChannelRead(
            rule,
            ChannelRead(channel, ChannelPostSlot(cursor = 10), ChannelPostSlot(cursor = 12, posts = listOf(post(11))), 10L),
            facts(muted = true),
        )
        assertNull(d.post)
        assertEquals(12L, d.lastNotified)
    }

    // ---- notifier ------------------------------------------------------------

    private class Shown : (ChannelNotice) -> Unit {
        val notices = mutableListOf<ChannelNotice>()
        override fun invoke(n: ChannelNotice) {
            notices += n
        }
    }

    private fun factsOf(muted: Set<String> = emptySet(), own: Set<String> = emptySet()) =
        object : ChannelPostNotifier.Facts {
            override val held: Set<String> = setOf(channel, other)
            override fun muted(channelIdHex: String) = channelIdHex in muted
            override fun openAndVisible(channelIdHex: String) = false
            override fun own(channelIdHex: String) = channelIdHex in own
            override fun title(channelIdHex: String) = "Title $channelIdHex"
            override fun body(channelIdHex: String, post: PersistedChannelPost) = "Body ${post.text}"
        }

    @Test
    fun theNotifierShowsOneNoticePerChannelTaggedByTheChannelAndStoresItsCursor() = runTest {
        val store = ChannelNotifiedStore.on(context)
        val shown = Shown()
        val rule = SpyRule(notify = listOf(0, 1))
        val notifier = ChannelPostNotifier(store, rule, { now }, shown)
        notifier.onReads(
            account,
            listOf(
                ChannelRead(channel, ChannelPostSlot(cursor = 1), ChannelPostSlot(cursor = 3, posts = listOf(post(2), post(3, "newest"))), 1L),
            ),
            factsOf(),
        )
        val notice = shown.notices.single()
        assertEquals("channel:$channel", notice.tag)
        assertEquals(channel, notice.channelIdHex)
        assertEquals("Title $channel", notice.title)
        assertEquals("Body newest", notice.body)
        assertEquals(3L, store.read(account)[channel])
    }

    @Test
    fun theNextReadIsJudgedFromTheStoredCursorAndTheFactsOfThatChannel() = runTest {
        val store = ChannelNotifiedStore.on(context)
        store.write(account, channel, 7L, setOf(channel))
        val rule = SpyRule()
        val notifier = ChannelPostNotifier(store, rule, { now }) {}
        notifier.onReads(
            account,
            listOf(ChannelRead(channel, ChannelPostSlot(cursor = 7), ChannelPostSlot(cursor = 8, posts = listOf(post(8))), 7L)),
            factsOf(muted = setOf(channel), own = setOf(channel)),
        )
        val ctx = rule.contexts.single()
        assertEquals(7UL, ctx.lastNotified)
        assertTrue(ctx.muted)
        assertTrue(rule.posts.single().single().own)
    }

    // ---- store ---------------------------------------------------------------

    @Test
    fun theStoredCursorNeverMovesBackAndChannelsNoLongerHeldLeave() {
        val store = ChannelNotifiedStore.on(context)
        store.write(account, channel, 9L, setOf(channel, other))
        store.write(account, other, 4L, setOf(channel, other))
        store.write(account, channel, 3L, setOf(channel, other))
        assertEquals(mapOf(channel to 9L, other to 4L), store.read(account))
        store.write(account, channel, 10L, setOf(channel))
        assertEquals(mapOf(channel to 10L), store.read(account))
        // Another account's row is its own.
        assertTrue(store.read("BBECAwQFBgcICQoLDA0ODw==").isEmpty())
    }

    @Test
    fun aMalformedChannelIdIsNotStored() {
        val store = ChannelNotifiedStore.on(context)
        store.write(account, "not-a-channel", 3L, setOf("not-a-channel"))
        assertTrue(store.read(account).isEmpty())
    }

    @Test
    fun aSealedAccountTakesNoWriteAndForgetRemovesTheRow() {
        val store = ChannelNotifiedStore.on(context)
        store.write(account, channel, 2L, setOf(channel))
        AccountStoreGate.seal(account)
        store.write(account, channel, 5L, setOf(channel))
        assertEquals(2L, store.read(account)[channel])
        ChannelNotifiedStore.forgetAccount(context, account)
        assertTrue(store.read(account).isEmpty())
    }

    @Test
    fun theAccountWipeHasAStepThatRemovesTheRow() {
        val store = ChannelNotifiedStore.on(context)
        store.write(account, channel, 3L, setOf(channel))
        val step = me.lakko.app.account.AccountLocalWipe.steps.single { it.name == "channel-notified" }
        me.lakko.app.account.AccountLocalWipe.runSteps(context, account, listOf(step))
        assertTrue(store.read(account).isEmpty())
    }

    // ---- background read -----------------------------------------------------

    @Test
    fun theBackgroundReadOpensFollowedChannelsOnlyAndSkipsThosePublishedTo() {
        val held = listOf(
            ChannelHandle(id = channel, localName = "A", localDescription = "", addedAtMs = 1UL),
            ChannelHandle(id = other, localName = "B", localDescription = "", addedAtMs = 1UL),
            ChannelHandle(id = "11".repeat(16), localName = "C", localDescription = "", addedAtMs = 1UL),
        )
        val seeds = mapOf(
            channel to mapOf(2u to ByteArray(32), 5u to ByteArray(32)),
            other to mapOf(1u to ByteArray(32)),
        )
        val readable = subscriberReadables(held, seeds, publisherIds = setOf(other))
        assertEquals(listOf(channel), readable.map { it.idHex })
        assertEquals(5L, readable.single().newestSeedEpoch)
    }

    @Test
    fun theBackgroundReadReportsOnlyChannelsThatMovedAndWritesNoLog() = runTest {
        val readable = listOf(
            ChannelReadable(channel, mapOf(1u to ByteArray(32)), null, 1L),
            ChannelReadable(other, mapOf(1u to ByteArray(32)), null, 1L),
        )
        val log = ChannelPostLog(channels = mapOf(channel to ChannelPostSlot(cursor = 2, posts = listOf(post(2)))))
        val reads = readChannelsOnce(
            channels = readable,
            log = log,
            readMarkers = mapOf(channel to 2L),
            nowMs = now,
            deps = { r ->
                object : ChannelSyncDeps {
                    override suspend fun fetchPage(channelIdHex: String, after: Long, limit: Int): ChannelPostPage =
                        if (channelIdHex == channel && after < 3) {
                            ChannelPostPage(listOf(ChannelPostWire(3, now - 1_000, "p3")))
                        } else {
                            ChannelPostPage(emptyList())
                        }

                    override suspend fun decode(channelIdHex: String, payloadB64: String): PostOutcome =
                        PostOutcome.Text(payloadB64)
                }
            },
        )
        val read = reads.single()
        assertEquals(channel, read.channelIdHex)
        assertEquals(2L, read.slotBefore?.cursor)
        assertEquals(3L, read.slotAfter.cursor)
        assertEquals(2L, read.readMarker)
        assertFalse(reads.any { it.channelIdHex == other })
    }
}
