// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/**
 * A wake whose message the running app already applied.
 *
 * Seen on a Pixel (2026-11-22): a group message arrived while Lakko was in the
 * background with the group's chat open; the notification read "Lakko" /
 * "New message" and nothing was logged. The open chat's long poll and the
 * wake's background read fetch the same rows, a row the app applied cannot be
 * decrypted a second time, and the read fell back to the generic banner
 * without saying why. These tests pin the preview taken from the app's stored
 * log, the reason every generic banner now carries, and the rule that pauses
 * the chat screen's poll in the background.
 */
class AppAppliedWakeTest {

    private val groupHex = "c".repeat(64)
    private val accountId = "acct-applied=="
    private val ownPersonKey = "own-person=="
    private val peerKey = "peer-person=="

    @Before
    fun englishCatalog() {
        me.lakko.app.l10n.EnglishForTests.install()
    }

    private class Group(
        private val decode: (Int) -> ProcessedRow? = { null },
        private val throwOn: Set<Int> = emptySet(),
        private val kind: me.lakko.app.chat.ConversationKind = me.lakko.app.chat.ConversationKind.Group,
    ) : ThrowawayGroup {
        override fun process(payload: ByteArray): ProcessedRow? {
            val tag = payload[0].toInt() and 0xFF
            // The key of a row the app already applied is gone: the same
            // refusal a wrong-epoch row gets, never the own-message one.
            if (tag in throwOn) throw RuntimeException("process_message: SecretReuseError")
            return decode(tag)
        }

        override fun facts(): WakeGroupFacts =
            WakeGroupFacts(
                kind = kind,
                rosterPersonKeysB64 = listOf("own-person==", "peer-person=="),
                ownerKeyB64 = null,
                ownPersonKeyB64 = "own-person==",
            )

        override fun close() = Unit
    }

    /** [stored] answers per read: the n-th read of an id returns
     *  `stored(id, n)` (n starts at 1), so a log written mid-wake is
     *  modelled. */
    private inner class Deps(
        private val group: Group?,
        private val rows: List<FetchedRow> = emptyList(),
        private val stored: (Long, Int) -> StoredWakeMessage? = { _, _ -> null },
        private val names: Map<String, String> = mapOf(peerKey to "Mira"),
        private val snapshot: ByteArray? = ByteArray(8) { 2 },
        private val seed: ByteArray? = ByteArray(64) { 1 },
        private val route: Boolean = true,
        private val fetchThrows: Boolean = false,
        /** Thrown by the first [fetchFailures] reads, when set. */
        private val fetchFailure: Throwable? = null,
        private val fetchFailures: Int = 0,
        private val blocked: Set<String> = emptySet(),
        private val groupName: String? = "Call walk",
    ) : PushDecryptDeps {
        val reads = HashMap<Long, Int>()
        var fetches = 0

        override fun lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?): String? =
            pushGidHex.takeIf { route && accountIdB64 == accountId }

        override fun loadIdentitySeed(accountIdB64: String): ByteArray? = seed?.copyOf()

        override fun loadSnapshot(accountIdB64: String): ByteArray? = snapshot

        override fun openThrowawayGroup(seed: ByteArray, snapshot: ByteArray, groupIdBytes: ByteArray): ThrowawayGroup? =
            group

        override suspend fun dsFetch(
            groupIdBytes: ByteArray,
            afterId: Long,
            signer: me.lakko.app.ds.DsJournalSigner?,
        ): List<FetchedRow> {
            fetches += 1
            if (fetchThrows) throw RuntimeException("ds refused")
            if (fetchFailure != null && fetches <= fetchFailures) throw fetchFailure
            return rows
        }

        override fun startCursor(accountIdB64: String, groupHex: String): Long? = null

        override fun heldRow(accountIdB64: String, groupHex: String, messageId: Long): me.lakko.app.chat.HeldRow? =
            stored(messageId, 1)?.let {
                me.lakko.app.chat.HeldRow(
                    if (it.incoming) me.lakko.app.chat.HeldRowDirection.INCOMING else me.lakko.app.chat.HeldRowDirection.OUTGOING,
                    it.authorKeyB64,
                )
            }

        override fun storedMessage(accountIdB64: String, groupHex: String, messageId: Long): StoredWakeMessage? {
            val n = (reads[messageId] ?: 0) + 1
            reads[messageId] = n
            return stored(messageId, n)
        }

        override fun personName(accountIdB64: String, personKeyB64: String): String? = names[personKeyB64]

        override fun groupName(accountIdB64: String, groupHex: String): String? = groupName

        override fun isBlocked(accountIdB64: String, senderAccountB64: String?): Boolean = senderAccountB64 in blocked
    }

    private fun row(id: Long, tag: Int) = FetchedRow(id, byteArrayOf(tag.toByte()))

    private fun incoming(text: String, author: String? = peerKey) =
        StoredWakeMessage(incoming = true, authorKeyB64 = author, text = text, attachments = emptyList(), deleted = false)

    private suspend fun wake(deps: Deps, pointer: Long? = 20): PushWakeOutcome =
        decryptPushWake(
            PushWake(groupHex, pointer = pointer, accountIdB64 = accountId),
            deps,
            nowMs = 0L,
            ringWindowMs = 0L,
        )

    @Test
    fun `a message the app already applied is shown from the app's stored log`() = runTest {
        val deps = Deps(
            group = Group(throwOn = setOf(1)),
            rows = listOf(row(20, 1)),
            stored = { id, _ -> if (id == 20L) incoming("push test from iphone") else null },
        )
        val outcome = wake(deps)

        assertNull(outcome.preview)
        assertEquals("Call walk", outcome.storedPreview?.title)
        assertEquals("Mira: push test from iphone", outcome.storedPreview?.body)
        assertNull(outcome.fallbackReason)
        assertEquals(0, outcome.unreadableRows)
        // Off screen it is the notification; on screen the app shows it.
        assertSame(outcome.storedPreview, outcome.shownPreview(appOnScreen = false))
        assertNull(outcome.shownPreview(appOnScreen = true))
        assertFalse(outcome.genericBannerWanted(appOnScreen = false))
        assertFalse(outcome.genericBannerWanted(appOnScreen = true))
        assertEquals(
            "wake shows the stored message: the app had already read it",
            wakeBannerLogLine(outcome, outcome.shownPreview(appOnScreen = false)),
        )
    }

    @Test
    fun `a one-to-one stored message is titled by the sender`() = runTest {
        val deps = Deps(
            group = Group(throwOn = setOf(1), kind = me.lakko.app.chat.ConversationKind.Chat),
            rows = listOf(row(20, 1)),
            stored = { _, _ -> incoming("hi") },
        )
        val preview = wake(deps).storedPreview

        assertEquals("Mira", preview?.title)
        assertEquals("hi", preview?.body)
    }

    @Test
    fun `the app's log is read again when the app had not written it yet`() = runTest {
        // The app writes its MLS snapshot just before its message log: the
        // first read falls between the two.
        val deps = Deps(
            group = Group(throwOn = setOf(1)),
            rows = listOf(row(20, 1)),
            stored = { _, n -> if (n >= 2) incoming("late write") else null },
        )
        val outcome = wake(deps)

        assertEquals("Mira: late write", outcome.storedPreview?.body)
        assertEquals(2, deps.reads[20L])
    }

    @Test
    fun `this device's own row in the log still posts nothing`() = runTest {
        val own = StoredWakeMessage(incoming = false, authorKeyB64 = ownPersonKey, text = "mine", attachments = emptyList(), deleted = false)
        val deps = Deps(group = Group(throwOn = setOf(1)), rows = listOf(row(20, 1)), stored = { _, _ -> own })
        val outcome = wake(deps)

        assertNull(outcome.storedPreview)
        assertFalse(outcome.genericBannerWanted(appOnScreen = false))
        // Read once: the row is in the log, so there is nothing to wait for.
        assertEquals(1, deps.reads[20L])
    }

    @Test
    fun `a row neither decrypted nor stored is the generic banner, with its reason`() = runTest {
        val deps = Deps(group = Group(throwOn = setOf(1)), rows = listOf(row(20, 1)))
        val outcome = wake(deps)

        assertNull(outcome.storedPreview)
        assertEquals(1, outcome.unreadableRows)
        assertEquals(WakeFallbackReason.UNREADABLE_ROWS, outcome.fallbackReason)
        assertTrue(outcome.genericBannerWanted(appOnScreen = false))
        assertEquals(
            "wake shows the generic banner: a row did not decrypt and the app does not hold it",
            wakeBannerLogLine(outcome, outcome.shownPreview(appOnScreen = false)),
        )
    }

    @Test
    fun `an older row that fails is not taken for the wake's message`() = runTest {
        // Row 18 sits before the pointer: an earlier message the app showed
        // long ago. Only the wake's own rows are read from the log.
        val deps = Deps(
            group = Group(throwOn = setOf(1)),
            rows = listOf(row(18, 1)),
            stored = { id, _ -> if (id == 18L) incoming("old") else null },
        )
        val outcome = wake(deps)

        assertNull(outcome.storedPreview)
        assertTrue(deps.reads.isEmpty())
    }

    @Test
    fun `a blocked sender's stored message notifies nothing`() = runTest {
        val deps = Deps(
            group = Group(throwOn = setOf(1)),
            rows = listOf(row(20, 1)),
            stored = { _, _ -> incoming("spam") },
            blocked = setOf(peerKey),
        )
        assertNull(wake(deps).storedPreview)
    }

    @Test
    fun `a deleted stored message is not shown`() = runTest {
        val deleted = incoming("").copy(deleted = true)
        val deps = Deps(group = Group(throwOn = setOf(1)), rows = listOf(row(20, 1)), stored = { _, _ -> deleted })
        assertNull(wake(deps).storedPreview)
    }

    @Test
    fun `a stored message from a sender with no name is the generic banner, logged as such`() = runTest {
        val deps = Deps(
            group = Group(throwOn = setOf(1)),
            rows = listOf(row(20, 1)),
            stored = { _, _ -> incoming("hello") },
            names = emptyMap(),
        )
        val outcome = wake(deps)

        assertEquals(WakeFallbackReason.UNNAMED_SENDER, outcome.fallbackReason)
        assertEquals(
            "wake shows the generic banner: the sender has no name on this device",
            wakeBannerLogLine(outcome, outcome.shownPreview(appOnScreen = false)),
        )
    }

    @Test
    fun `a snapshot that cannot be used still shows the stored message`() = runTest {
        // The app was writing its snapshot, or it is past what this read
        // could open: the log holds the wake's message.
        val deps = Deps(group = null, snapshot = null, stored = { id, _ -> if (id == 20L) incoming("from the log") else null })
        val outcome = wake(deps)

        assertFalse(outcome.readRan)
        assertEquals("Lakko", outcome.storedPreview?.title)
        assertEquals("Mira: from the log", outcome.storedPreview?.body)
        assertFalse(outcome.genericBannerWanted(appOnScreen = false))
        assertFalse(outcome.genericBannerWanted(appOnScreen = true))
    }

    @Test
    fun `every early return names its reason`() = runTest {
        assertEquals(
            WakeFallbackReason.NO_POINTER,
            wake(Deps(group = Group()), pointer = null).fallbackReason,
        )
        assertEquals(WakeFallbackReason.NO_ROUTE, wake(Deps(group = Group(), route = false)).fallbackReason)
        assertEquals(WakeFallbackReason.NO_SEED, wake(Deps(group = Group(), seed = null)).fallbackReason)
        assertEquals(WakeFallbackReason.NO_SNAPSHOT, wake(Deps(group = Group(), snapshot = null)).fallbackReason)
        assertEquals(WakeFallbackReason.GROUP_NOT_OPENED, wake(Deps(group = null)).fallbackReason)
        assertEquals(WakeFallbackReason.FETCH_FAILED, wake(Deps(group = Group(), fetchThrows = true)).fallbackReason)
        val empty = wake(Deps(group = Group(), fetchThrows = true))
        assertTrue(empty.genericBannerWanted(appOnScreen = false))
        assertEquals(
            "wake shows the generic banner: the journal read failed (RuntimeException)",
            wakeBannerLogLine(empty, empty.shownPreview(appOnScreen = false)),
        )
    }

    @Test
    fun `a decrypted message keeps its preview and logs nothing`() = runTest {
        val deps = Deps(
            group = Group(decode = { ProcessedRow(ProcessedKind.CHAT, "hello", 0, senderName = "Mira", senderAccountB64 = peerKey) }),
            rows = listOf(row(20, 1)),
        )
        val outcome = wake(deps)

        assertEquals("Mira: hello", outcome.preview?.body)
        assertNull(outcome.fallbackReason)
        assertNull(wakeBannerLogLine(outcome, outcome.shownPreview(appOnScreen = false)))
        assertTrue(deps.reads.isEmpty())
    }

    @Test
    fun `the timeout and a thrown read have their own lines`() {
        val timeout = PushWakeOutcome.emptyBecause(WakeFallbackReason.TIMEOUT)
        assertEquals("wake shows the generic banner: the background read timed out", wakeBannerLogLine(timeout, null))
        val failed = PushWakeOutcome.emptyBecause(WakeFallbackReason.READ_FAILED)
        assertEquals("wake shows the generic banner: the background read failed", wakeBannerLogLine(failed, null))
    }

    @Test
    fun `no reason line carries anything but plain words`() {
        for (reason in WakeFallbackReason.entries) {
            assertTrue(reason.logText, reason.logText.all { it == ' ' || it == '\'' || it.isLetter() })
        }
    }

    @Test
    fun `the stored log row maps direction, author, text and attachments`() {
        val log = me.lakko.app.chat.MessageLogPayload(
            groups = mapOf(
                groupHex to me.lakko.app.chat.PersistedGroup(
                    cursor = 20,
                    messages = listOf(
                        me.lakko.app.chat.PersistedMessage(
                            messageId = 20,
                            direction = "incoming",
                            text = "hi",
                            attachments = emptyList(),
                            createdAt = "2026-11-22T15:51:50Z",
                            authorKeyB64 = peerKey,
                        ),
                        me.lakko.app.chat.PersistedMessage(
                            messageId = 21,
                            direction = "outgoing",
                            text = "mine",
                            attachments = emptyList(),
                            createdAt = "2026-11-22T15:51:51Z",
                            authorKeyB64 = ownPersonKey,
                        ),
                    ),
                ),
            ),
        )

        val incoming = storedWakeMessageIn(log, groupHex.uppercase(), 20)
        assertEquals(true, incoming?.incoming)
        assertEquals(peerKey, incoming?.authorKeyB64)
        assertEquals("hi", incoming?.text)
        assertEquals(false, storedWakeMessageIn(log, groupHex, 21)?.incoming)
        assertNull(storedWakeMessageIn(log, groupHex, 22))
        assertNull(storedWakeMessageIn(null, groupHex, 20))
    }

    @Test
    fun `a network failure of the journal read is tried once more`() = runTest {
        val deps = Deps(
            group = Group(decode = { ProcessedRow(ProcessedKind.CHAT, "third push test", 0, senderName = "G55 Two", senderAccountB64 = peerKey) }),
            rows = listOf(row(20, 1)),
            fetchFailure = me.lakko.app.ds.DsNetworkError("/ds/group/x/messages", java.net.SocketException("Software caused connection abort")),
            fetchFailures = 1,
        )
        val outcome = wake(deps)

        assertEquals(2, deps.fetches)
        assertEquals("G55 Two: third push test", outcome.preview?.body)
        assertNull(outcome.fallbackReason)
    }

    @Test
    fun `a network failure twice is the generic banner naming the failure class`() = runTest {
        val deps = Deps(
            group = Group(),
            rows = listOf(row(20, 1)),
            fetchFailure = me.lakko.app.ds.DsNetworkError("/ds/group/x/messages", javax.net.ssl.SSLException("reset")),
            fetchFailures = 2,
        )
        val outcome = wake(deps)

        assertEquals(2, deps.fetches)
        assertEquals(WakeFallbackReason.FETCH_FAILED, outcome.fallbackReason)
        assertEquals(
            "wake shows the generic banner: the journal read failed (network SSLException)",
            wakeBannerLogLine(outcome, outcome.shownPreview(appOnScreen = false)),
        )
    }

    @Test
    fun `a refused proof is not retried and its status is logged`() = runTest {
        val deps = Deps(
            group = Group(),
            rows = listOf(row(20, 1)),
            fetchFailure = me.lakko.app.ds.DsProofRefusedError("/ds/group/x/messages", clockSkewed = false, body = ""),
            fetchFailures = 5,
        )
        val outcome = wake(deps)

        assertEquals(1, deps.fetches)
        assertEquals(
            "wake shows the generic banner: the journal read failed (http 401, proof refused)",
            wakeBannerLogLine(outcome, outcome.shownPreview(appOnScreen = false)),
        )
    }

    @Test
    fun `the failure detail carries a status or a class, never a message text`() {
        assertEquals("http 404", fetchFailureDetail(me.lakko.app.ds.DsHttpError(404, "/ds/group/abc/messages", "body")))
        assertEquals(
            "http 401, proof refused, device clock off",
            fetchFailureDetail(me.lakko.app.ds.DsProofRefusedError("/ds/group/x/messages", clockSkewed = true, body = "")),
        )
        assertEquals("network UnknownHostException", fetchFailureDetail(
            me.lakko.app.ds.DsNetworkError("/ds/group/0123456789abcdef0123/messages", java.net.UnknownHostException("test.lakko.me")),
        ))
        assertEquals("IllegalStateException", fetchFailureDetail(IllegalStateException("secret text")))
    }

    @Test
    fun `the chat screen polls only while the app is started or a call is up`() {
        assertTrue(me.lakko.app.chat.screenPollRuns(appStarted = true, callUp = false))
        assertTrue(me.lakko.app.chat.screenPollRuns(appStarted = false, callUp = true))
        assertTrue(me.lakko.app.chat.screenPollRuns(appStarted = true, callUp = true))
        assertFalse(me.lakko.app.chat.screenPollRuns(appStarted = false, callUp = false))
    }
}
