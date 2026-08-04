// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Unit tests for the pure, native-free silent-push decrypt core
 * ([decryptPushPreview]). Drives it with hand-rolled fakes so no
 * Android framework / UniFFI native `.so` is needed (this is a plain
 * JVM `test` source-set suite).
 *
 * The Android counterpart of the web `src/sw/push-decrypt.test.ts`
 * (commit 148753b) — happy path, newest-wins, cursor-vs-`p-1` start,
 * every fail-closed branch, and the explicit NEVER-PERSISTS assertions
 * (the throwaway group is always closed + no write hook is ever
 * reached; [FakeDeps] has no write path at all, and [FakeThrowawayGroup]
 * would flip [FakeThrowawayGroup.persistAttempted] if any persist entry
 * point existed — there is none).
 */
class PushPreviewDecryptTest {

    private val groupHex = "a".repeat(64)
    private val groupIdBytes = ByteArray(32) { 0xAA.toByte() }
    private val accountId = "acct-b64=="

    // -----------------------------------------------------------------
    // Fakes
    // -----------------------------------------------------------------

    /** A canned throwaway group: [rows] maps a wire-payload's first
     *  byte to a [ProcessedRow] (or a thrown decrypt failure). Tracks
     *  close + a would-be-persist flag. */
    private class FakeThrowawayGroup(
        private val decode: (ByteArray) -> ProcessedRow?,
        private val throwOn: Set<Int> = emptySet(),
    ) : ThrowawayGroup {
        var closed = false
        var processCount = 0

        /** Stays false forever — there is no persist entry point on the
         *  interface. The test asserts it, mirroring the web
         *  "never-persists" spy. */
        val persistAttempted = false

        override fun process(payload: ByteArray): ProcessedRow? {
            processCount++
            val tag = if (payload.isEmpty()) -1 else payload[0].toInt() and 0xFF
            if (tag in throwOn) throw RuntimeException("decrypt failure (fail-closed)")
            return decode(payload)
        }

        override fun close() {
            closed = true
        }
    }

    /** Fully-configurable fake deps. Records the resolved account +
     *  the `afterId` the core fetched with, so the cursor math is
     *  assertable. NO write method exists (structural read-only). */
    private class FakeDeps(
        val account: ResolvedPushAccount?,
        val seed: ByteArray? = ByteArray(64) { 1 },
        val snapshot: ByteArray? = ByteArray(8) { 2 },
        val group: FakeThrowawayGroup? = null,
        val rows: List<FetchedRow> = emptyList(),
        val cursor: Long? = null,
        val dsThrows: Boolean = false,
        val openThrows: Boolean = false,
    ) : PushDecryptDeps {
        var resolveCount = 0
        var openCount = 0
        var dsFetchCount = 0
        var lastAfterId: Long? = null
        var lastSeedZeroed = false

        override fun resolveAccount(groupHex: String): ResolvedPushAccount? {
            resolveCount++
            return account
        }

        override fun loadIdentitySeed(accountIdB64: String): ByteArray? = seed

        override fun loadSnapshot(accountIdB64: String): ByteArray? = snapshot

        override fun openThrowawayGroup(
            seed: ByteArray,
            snapshot: ByteArray,
            groupIdBytes: ByteArray,
        ): ThrowawayGroup? {
            openCount++
            if (openThrows) throw RuntimeException("open failure")
            return group
        }

        override suspend fun dsFetch(groupIdBytes: ByteArray, afterId: Long): List<FetchedRow> {
            dsFetchCount++
            lastAfterId = afterId
            if (dsThrows) throw RuntimeException("ds failure")
            return rows
        }

        override fun startCursor(accountIdB64: String, groupHex: String): Long? = cursor
    }

    private fun okAccount() =
        ResolvedPushAccount(accountIdB64 = accountId, groupIdBytes = groupIdBytes)

    private fun row(id: Long, tag: Int): FetchedRow = FetchedRow(id, byteArrayOf(tag.toByte()))

    private fun chat(text: String, attachments: Int = 0) =
        ProcessedRow(ProcessedKind.CHAT, text, attachments)

    // -----------------------------------------------------------------
    // Happy path
    // -----------------------------------------------------------------

    @Test
    fun `decrypts the newest chat message to the real preview`() = runTest {
        val group = FakeThrowawayGroup(decode = { payload ->
            when (payload[0].toInt()) {
                1 -> chat("hello")
                2 -> chat("world")
                else -> null
            }
        })
        val deps = FakeDeps(
            account = okAccount(),
            group = group,
            rows = listOf(row(10, 1), row(11, 2)),
        )
        val preview = decryptPushPreview(PushWake(groupHex, pointer = 12), deps)

        assertEquals("Lakko", preview?.title)
        assertEquals("world", preview?.body) // newest-wins
        assertEquals(groupHex, preview?.tag)
        // Throwaway discarded; never persisted.
        assertTrue("throwaway group must be closed", group.closed)
        assertFalse("no persist entry point may be reached", group.persistAttempted)
    }

    @Test
    fun `attachment-only chat renders the attachment hint`() = runTest {
        val one = FakeThrowawayGroup(decode = { chat("", attachments = 1) })
        assertEquals(
            "Sent an attachment",
            decryptPushPreview(
                PushWake(groupHex, 5),
                FakeDeps(okAccount(), group = one, rows = listOf(row(5, 1))),
            )?.body,
        )
        val many = FakeThrowawayGroup(decode = { chat("", attachments = 3) })
        assertEquals(
            "Sent 3 attachments",
            decryptPushPreview(
                PushWake(groupHex, 5),
                FakeDeps(okAccount(), group = many, rows = listOf(row(5, 1))),
            )?.body,
        )
    }

    @Test
    fun `an edit surfaces the new text`() = runTest {
        val group = FakeThrowawayGroup(decode = { ProcessedRow(ProcessedKind.EDIT, "edited body", 0) })
        assertEquals(
            "edited body",
            decryptPushPreview(
                PushWake(groupHex, 5),
                FakeDeps(okAccount(), group = group, rows = listOf(row(5, 1))),
            )?.body,
        )
    }

    // -----------------------------------------------------------------
    // Newest-wins + kind filtering (delete/reaction/typing/read-receipt)
    // -----------------------------------------------------------------

    @Test
    fun `a later non-preview op keeps the older chat as the preview`() = runTest {
        // rows: chat "hi" (id 10), delete (id 11). Delete yields no
        // preview → the older chat stays.
        val group = FakeThrowawayGroup(decode = { payload ->
            when (payload[0].toInt()) {
                1 -> chat("hi")
                2 -> ProcessedRow(ProcessedKind.DELETE, "", 0)
                else -> null
            }
        })
        val preview = decryptPushPreview(
            PushWake(groupHex, 12),
            FakeDeps(okAccount(), group = group, rows = listOf(row(10, 1), row(11, 2))),
        )
        assertEquals("hi", preview?.body)
    }

    @Test
    fun `only-non-preview ops yield generic`() = runTest {
        for (kind in listOf(
            ProcessedKind.DELETE,
            ProcessedKind.REACTION,
            ProcessedKind.TYPING,
            ProcessedKind.READ_RECEIPT,
        )) {
            val group = FakeThrowawayGroup(decode = { ProcessedRow(kind, "", 0) })
            val preview = decryptPushPreview(
                PushWake(groupHex, 5),
                FakeDeps(okAccount(), group = group, rows = listOf(row(5, 1))),
            )
            assertNull("kind $kind must not produce a preview", preview)
            assertTrue("group closed even with no preview", group.closed)
        }
    }

    @Test
    fun `an empty chat with no attachments yields generic`() = runTest {
        val group = FakeThrowawayGroup(decode = { chat("", attachments = 0) })
        assertNull(
            decryptPushPreview(
                PushWake(groupHex, 5),
                FakeDeps(okAccount(), group = group, rows = listOf(row(5, 1))),
            ),
        )
    }

    // -----------------------------------------------------------------
    // Fail-closed branches → null (→ generic)
    // -----------------------------------------------------------------

    @Test
    fun `non-group wakes yield null`() = runTest {
        val deps = FakeDeps(okAccount())
        assertNull(decryptPushPreview(null, deps))
        assertNull(decryptPushPreview(PushWake(null, 5), deps))
        assertNull(decryptPushPreview(PushWake(groupHex, null), deps))
        assertNull(decryptPushPreview(PushWake("abc", 5), deps)) // wrong length
        assertNull(decryptPushPreview(PushWake("z".repeat(64), 5), deps)) // non-hex
        assertEquals("no account lookup for a malformed wake", 0, deps.resolveCount)
    }

    @Test
    fun `unknown group yields null`() = runTest {
        val deps = FakeDeps(account = null)
        assertNull(decryptPushPreview(PushWake(groupHex, 5), deps))
    }

    @Test
    fun `missing seed yields null`() = runTest {
        val deps = FakeDeps(okAccount(), seed = null)
        assertNull(decryptPushPreview(PushWake(groupHex, 5), deps))
        assertEquals("no group open without a seed", 0, deps.openCount)
    }

    @Test
    fun `missing snapshot yields null`() = runTest {
        val deps = FakeDeps(okAccount(), snapshot = null)
        assertNull(decryptPushPreview(PushWake(groupHex, 5), deps))
        assertEquals("no group open without a snapshot", 0, deps.openCount)
    }

    @Test
    fun `non-member group (open returns null) yields null`() = runTest {
        val deps = FakeDeps(okAccount(), group = null)
        assertNull(decryptPushPreview(PushWake(groupHex, 5), deps))
        assertEquals(1, deps.openCount)
        assertEquals("no DS fetch when we are not a member", 0, deps.dsFetchCount)
    }

    @Test
    fun `open throwing yields null`() = runTest {
        val deps = FakeDeps(okAccount(), openThrows = true)
        assertNull(decryptPushPreview(PushWake(groupHex, 5), deps))
    }

    @Test
    fun `DS fetch failure yields null and still closes the group`() = runTest {
        val group = FakeThrowawayGroup(decode = { chat("nope") })
        val deps = FakeDeps(okAccount(), group = group, dsThrows = true)
        assertNull(decryptPushPreview(PushWake(groupHex, 5), deps))
        assertTrue("group closed on DS failure", group.closed)
    }

    @Test
    fun `a per-row decrypt failure is skipped, later rows still processed`() = runTest {
        // Row id 10 (tag 1) throws in process(); row id 11 (tag 2) is a
        // good chat. The thrown row is skipped fail-closed; the good one
        // becomes the preview.
        val group = FakeThrowawayGroup(
            decode = { payload -> if (payload[0].toInt() == 2) chat("survivor") else null },
            throwOn = setOf(1),
        )
        val preview = decryptPushPreview(
            PushWake(groupHex, 12),
            FakeDeps(okAccount(), group = group, rows = listOf(row(10, 1), row(11, 2))),
        )
        assertEquals("survivor", preview?.body)
        assertEquals(2, group.processCount)
    }

    @Test
    fun `zero fetched rows yields null`() = runTest {
        val group = FakeThrowawayGroup(decode = { chat("x") })
        val deps = FakeDeps(okAccount(), group = group, rows = emptyList())
        assertNull(decryptPushPreview(PushWake(groupHex, 5), deps))
        assertTrue(group.closed)
    }

    // -----------------------------------------------------------------
    // Cursor selection (the read-only start-cursor math)
    // -----------------------------------------------------------------

    @Test
    fun `no persisted cursor fetches from pointer minus one`() = runTest {
        val group = FakeThrowawayGroup(decode = { null })
        val deps = FakeDeps(okAccount(), group = group, cursor = null)
        decryptPushPreview(PushWake(groupHex, pointer = 20), deps)
        assertEquals(19L, deps.lastAfterId)
    }

    @Test
    fun `a persisted cursor below the pointer is preferred`() = runTest {
        val group = FakeThrowawayGroup(decode = { null })
        val deps = FakeDeps(okAccount(), group = group, cursor = 5L)
        decryptPushPreview(PushWake(groupHex, pointer = 20), deps)
        assertEquals("intervening commits replay from the app cursor", 5L, deps.lastAfterId)
    }

    @Test
    fun `a persisted cursor at or above the pointer is ignored`() = runTest {
        val group = FakeThrowawayGroup(decode = { null })
        val deps = FakeDeps(okAccount(), group = group, cursor = 25L)
        decryptPushPreview(PushWake(groupHex, pointer = 20), deps)
        assertEquals("stale/ahead cursor ignored → pointer-1", 19L, deps.lastAfterId)
    }

    @Test
    fun `after is clamped to zero`() = runTest {
        val group = FakeThrowawayGroup(decode = { null })
        val deps = FakeDeps(okAccount(), group = group, cursor = null)
        decryptPushPreview(PushWake(groupHex, pointer = 0), deps)
        assertEquals(0L, deps.lastAfterId)
    }

    // -----------------------------------------------------------------
    // NEVER-PERSISTS guard (read-only-throwaway discipline)
    // -----------------------------------------------------------------

    @Test
    fun `a full happy-path run never persists and always discards`() = runTest {
        val group = FakeThrowawayGroup(decode = { chat("final") })
        val deps = FakeDeps(okAccount(), group = group, rows = listOf(row(9, 1)))
        val preview = decryptPushPreview(PushWake(groupHex, 10), deps)

        assertEquals("final", preview?.body)
        // The throwaway is discarded exactly once...
        assertTrue("group must be closed (discarded)", group.closed)
        // ...and no persist / snapshot / cursor-advance was reachable.
        assertFalse("no persist may be attempted", group.persistAttempted)
        // The DS was read exactly once (no re-fetch that a cursor
        // advance would imply).
        assertEquals(1, deps.dsFetchCount)
    }
}
