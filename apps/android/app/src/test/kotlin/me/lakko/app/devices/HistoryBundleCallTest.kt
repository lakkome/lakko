// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import me.lakko.app.calls.CallHistoryEntry
import me.lakko.app.calls.CallOutcome
import me.lakko.app.calls.callHistoryWithRestored
import me.lakko.app.chat.ConversationKind
import me.lakko.app.chat.PersistedCall
import me.lakko.app.chat.PersistedMessage
import me.lakko.app.chat.ReentryRouteKind
import me.lakko.app.chat.reentryBundleKeepsRow
import me.lakko.app.chat.toTranscriptEntry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.CallRecordOutcome

/**
 * The history placeholder (version 7) carries a finished call, so a device
 * added to the account draws the same call rows the other device holds. Seen
 * live: a call row ("Declined") never reached the new device. The host JVM
 * loads no native core, so the mapping to and from the placeholder record is
 * what is tested here; the byte codec is tested in Rust.
 */
class HistoryBundleCallTest {
    private fun callRow(id: Long, call: PersistedCall) =
        PersistedMessage(id, "system", "Call", emptyList(), "2026-11-23T10:00:00Z").copy(call = call)

    @Test
    fun `an outgoing video call carries its duration`() {
        val row = callRow(-1, PersistedCall(outcome = "outgoing", video = true, durationSecs = 125))
        val p = HistoryBundleCodec.placeholderOf(row)
        assertEquals(CallRecordOutcome.OUTGOING, p.call?.outcome)
        assertEquals(true, p.call?.video)
        assertEquals(125u, p.call?.durationSecs)

        val back = HistoryBundleCodec.persistedOf(p)
        assertEquals(row.call, back.call)
        assertEquals("system", back.direction)
        val entry = back.call?.toTranscriptEntry()
        assertEquals(CallOutcome.OUTGOING, entry?.outcome)
        assertEquals(125L, entry?.durationSecs)
    }

    @Test
    fun `a declined call carries no duration`() {
        val row = callRow(-2, PersistedCall(outcome = "declined", video = false, durationSecs = null))
        val p = HistoryBundleCodec.placeholderOf(row)
        assertEquals(CallRecordOutcome.DECLINED, p.call?.outcome)
        assertEquals(false, p.call?.video)
        assertNull(p.call?.durationSecs)

        val back = HistoryBundleCodec.persistedOf(p)
        assertEquals(CallOutcome.DECLINED, back.call?.toTranscriptEntry()?.outcome)
        assertNull(back.call?.durationSecs)
    }

    @Test
    fun `every outcome goes there and back by name`() {
        for (outcome in CallOutcome.entries) {
            val stored = PersistedCall(outcome = outcome.name.lowercase(java.util.Locale.ROOT), video = false)
            val p = HistoryBundleCodec.placeholderOf(callRow(-3, stored))
            assertEquals(outcome.name, p.call?.outcome?.name)
            assertEquals(outcome, HistoryBundleCodec.persistedOf(p).call?.toTranscriptEntry()?.outcome)
        }
        // And the core's enum holds no outcome the app cannot read.
        for (wire in CallRecordOutcome.entries) {
            assertTrue(CallOutcome.entries.any { it.name == wire.name })
        }
    }

    @Test
    fun `a row with no call, an unknown outcome or a negative duration`() {
        val plain = PersistedMessage(4, "incoming", "hi", emptyList(), "2026-11-23T10:00:00Z")
        assertNull(HistoryBundleCodec.placeholderOf(plain).call)
        assertNull(HistoryBundleCodec.persistedOf(HistoryBundleCodec.placeholderOf(plain)).call)

        assertNull(HistoryBundleCodec.placeholderOf(callRow(-5, PersistedCall(outcome = "teleported"))).call)

        val negative = HistoryBundleCodec.placeholderOf(
            callRow(-6, PersistedCall(outcome = "incoming", video = false, durationSecs = -4)),
        )
        assertEquals(CallRecordOutcome.INCOMING, negative.call?.outcome)
        assertNull(negative.call?.durationSecs)
    }

    @Test
    fun `a call row is carried, not taken for bookkeeping`() {
        val row = callRow(-7, PersistedCall(outcome = "missed")).copy(text = "[mls commit]")
        assertFalse(HistoryBundleCodec.isBookkeepingSystemRow(row))
        assertEquals(listOf(row), HistoryBundleCodec.bundledMessages(listOf(row)))
    }

    @Test
    fun `a row with a call is rebuilt as a call row whatever its text and direction`() {
        // The web shape: empty text, no author, an id derived from the call id.
        val webId = Long.MIN_VALUE or 0x0102_0304_0506_0708L
        val sent = PersistedMessage(webId, "incoming", "", emptyList(), "2026-11-23T09:58:00Z")
            .copy(authorKeyB64 = java.util.Base64.getEncoder().encodeToString(ByteArray(32) { 7 }))
        val p = HistoryBundleCodec.placeholderOf(sent).copy(
            call = uniffi.lakko_ffi_uniffi.HistoryCall(CallRecordOutcome.DECLINED, video = true, durationSecs = null),
        )
        val row = HistoryBundleCodec.persistedOf(p)
        assertEquals(webId, row.messageId)
        assertEquals("system", row.direction)
        assertNull(row.authorKeyB64)
        assertEquals(me.lakko.app.calls.callRowText(row.call!!.toTranscriptEntry()!!), row.text)
        assertTrue(row.text.isNotEmpty())
        assertEquals(PersistedCall(outcome = "declined", video = true, durationSecs = null), row.call)
        assertEquals("2026-11-23T09:58:00Z", row.createdAt)

        // The same bundle applied twice adds the row once.
        val (once, added) = HistoryBundleCodec.mergeRows(emptyList(), listOf(row))
        assertEquals(1, added)
        val (twice, again) = HistoryBundleCodec.mergeRows(once, listOf(HistoryBundleCodec.persistedOf(p)))
        assertEquals(0, again)
        assertEquals(once, twice)
    }

    @Test
    fun `a gap bundle keeps a call row only from this account's own device`() {
        val call = callRow(-8, PersistedCall(outcome = "outgoing", durationSecs = 30))
        assertTrue(reentryBundleKeepsRow(call, ReentryRouteKind.OWN_DEVICE))
        assertFalse(reentryBundleKeepsRow(call, ReentryRouteKind.MAILBOX))
        assertFalse(reentryBundleKeepsRow(call, ReentryRouteKind.KNOCK))
        val text = PersistedMessage(9, "incoming", "hi", emptyList(), "2026-11-23T10:00:00Z")
        assertTrue(reentryBundleKeepsRow(text, ReentryRouteKind.KNOCK))
    }

    @Test
    fun `a bundle written to disk while no chat layer runs still files its call`() {
        val group = "cd".repeat(32)
        val callId = Long.MIN_VALUE or 0x0A0B_0C0D_0E0F_1011L
        val call = PersistedMessage(callId, "system", "", emptyList(), "2026-11-23T09:01:00Z")
            .copy(call = PersistedCall(outcome = "outgoing", video = true, durationSecs = 42))
        val text = PersistedMessage(7, "incoming", "hi", emptyList(), "2026-11-23T09:00:00Z")
        val written = mutableListOf<me.lakko.app.chat.MessageLogPayload>()
        val filed = mutableListOf<Pair<String, CallHistoryEntry>>()

        // No chat layer: nothing tells the kind, and nothing else runs.
        HistoryBundleCodec.writeBundleTo(
            existing = null,
            decoded = mapOf(group to listOf(text, call)),
            putLog = { written += it },
            fileCalls = { filed += it },
            kindOf = { null },
        )

        assertEquals(1, written.size)
        assertEquals(listOf(7L, callId), written.single().groups[group]?.messages?.map { it.messageId })
        assertEquals(
            listOf(
                me.lakko.app.calls.callRowFiledKey(group, callId) to CallHistoryEntry(
                    conversationIdHex = group,
                    kind = ConversationKind.Group,
                    outcome = CallOutcome.OUTGOING,
                    video = true,
                    startedAtMs = java.time.Instant.parse("2026-11-23T09:01:00Z").toEpochMilli(),
                    durationSecs = 42,
                ),
            ),
            filed,
        )

        // The same bundle again: the same key, which the store files once.
        val again = mutableListOf<Pair<String, CallHistoryEntry>>()
        HistoryBundleCodec.writeBundleTo(written.single(), mapOf(group to listOf(call)), {}, { again += it }, { null })
        assertEquals(filed.map { it.first }, again.map { it.first })
    }

    @Test
    fun `a call of a chat this device holds is filed under its kind, and a bundle with no call files nothing`() {
        val group = "ef".repeat(32)
        val call = callRow(Long.MIN_VALUE or 5L, PersistedCall(outcome = "missed"))
        assertEquals(
            ConversationKind.Chat,
            HistoryBundleCodec.bundleCallEntries(mapOf(group to listOf(call))) { ConversationKind.Chat }.single().second.kind,
        )
        var calls = 0
        HistoryBundleCodec.writeBundleTo(
            null,
            mapOf(group to listOf(PersistedMessage(1, "incoming", "hi", emptyList(), "2026-11-23T09:00:00Z"))),
            {},
            { calls += 1 },
            { null },
        )
        assertEquals(0, calls)
    }

    @Test
    fun `a restored call is filed in the Calls list once`() {
        val entry = CallHistoryEntry(
            conversationIdHex = "ab".repeat(32),
            kind = ConversationKind.Chat,
            outcome = CallOutcome.DECLINED,
            video = false,
            startedAtMs = 1_700_000_000_000,
        )
        val once = callHistoryWithRestored(emptyList(), entry)
        assertEquals(listOf(entry), once)
        // The same call again (a second bundle, or the one-time fill), even
        // with the id in other case: nothing more.
        assertEquals(once, callHistoryWithRestored(once, entry.copy(conversationIdHex = "AB".repeat(32))))
        // A different call in the same conversation is filed.
        val later = entry.copy(startedAtMs = entry.startedAtMs + 1)
        assertEquals(2, callHistoryWithRestored(once, later).size)
    }
}
