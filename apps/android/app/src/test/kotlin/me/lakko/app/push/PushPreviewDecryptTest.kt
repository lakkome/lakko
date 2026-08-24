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
        /** ADR-0042: the reverse-index row `lookupRoute` returns
         *  (`null` = miss → legacy raw-group_id path). */
        val route: Pair<String, String>? = null,
        val routeThrows: Boolean = false,
    ) : PushDecryptDeps {
        var lookupCount = 0
        var lastLookupGid: String? = null
        var resolveCount = 0
        var openCount = 0
        var dsFetchCount = 0
        var lastAfterId: Long? = null
        var lastFetchGroupBytes: ByteArray? = null
        var lastCursorGroupHex: String? = null
        var lastSeedZeroed = false

        override fun lookupRoute(pushGidHex: String): Pair<String, String>? {
            lookupCount++
            lastLookupGid = pushGidHex
            if (routeThrows) throw RuntimeException("route lookup failure")
            return route
        }

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
            lastFetchGroupBytes = groupIdBytes
            if (dsThrows) throw RuntimeException("ds failure")
            return rows
        }

        override fun startCursor(accountIdB64: String, groupHex: String): Long? {
            lastCursorGroupHex = groupHex
            return cursor
        }
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
    // ADR-0042 (Sprint 4): opaque push_gid reverse-resolution
    // -----------------------------------------------------------------

    /** An opaque push_gid — same 64-hex shape, deliberately NOT a
     *  known group id (legacy resolveAccount would miss it). */
    private val pushGidHex = "b".repeat(64)

    @Test
    fun `a push_gid wake resolves via the route index and decrypts`() = runTest {
        val group = FakeThrowawayGroup(decode = { chat("routed hello") })
        val deps = FakeDeps(
            // Legacy resolution would FAIL for the push_gid — the route
            // index is the only way home.
            account = null,
            group = group,
            rows = listOf(row(10, 1)),
            cursor = 4L,
            route = accountId to groupHex,
        )
        val preview = decryptPushPreview(PushWake(pushGidHex, pointer = 11), deps)

        assertEquals("routed hello", preview?.body)
        // The route hit skipped the legacy path entirely...
        assertEquals(1, deps.lookupCount)
        assertEquals(pushGidHex, deps.lastLookupGid)
        assertEquals("legacy resolveAccount must not run on a hit", 0, deps.resolveCount)
        // ...and everything downstream used the REAL group_id.
        assertTrue(
            "DS fetch must use the real group_id bytes",
            groupIdBytes.contentEquals(deps.lastFetchGroupBytes),
        )
        assertEquals("cursor read keyed by the real group hex", groupHex, deps.lastCursorGroupHex)
        assertEquals("app cursor below the pointer preferred", 4L, deps.lastAfterId)
        assertEquals("notification tag is the real group hex", groupHex, preview?.tag)
        // Read-only-throwaway discipline unchanged on the routed path.
        assertTrue("throwaway group must be closed", group.closed)
        assertFalse("no persist entry point may be reached", group.persistAttempted)
    }

    @Test
    fun `a route miss falls back to the legacy raw-group_id path`() = runTest {
        val group = FakeThrowawayGroup(decode = { chat("legacy") })
        val deps = FakeDeps(
            account = okAccount(),
            group = group,
            rows = listOf(row(5, 1)),
            route = null, // miss
        )
        val preview = decryptPushPreview(PushWake(groupHex, 6), deps)

        assertEquals("legacy", preview?.body)
        assertEquals(1, deps.lookupCount)
        assertEquals("legacy path consulted on a miss", 1, deps.resolveCount)
        assertEquals(groupHex, preview?.tag)
    }

    @Test
    fun `a route lookup error degrades to the legacy path`() = runTest {
        val group = FakeThrowawayGroup(decode = { chat("survived the store error") })
        val deps = FakeDeps(
            account = okAccount(),
            group = group,
            rows = listOf(row(5, 1)),
            routeThrows = true,
        )
        assertEquals(
            "survived the store error",
            decryptPushPreview(PushWake(groupHex, 6), deps)?.body,
        )
        assertEquals(1, deps.resolveCount)
    }

    @Test
    fun `a route lookup error with no legacy match yields generic`() = runTest {
        val deps = FakeDeps(account = null, routeThrows = true)
        assertNull(decryptPushPreview(PushWake(pushGidHex, 6), deps))
        assertEquals("no group open on a dead end", 0, deps.openCount)
    }

    @Test
    fun `a malformed route row falls back to the legacy path`() = runTest {
        for (badGroupIdHex in listOf("zz", "Z".repeat(64), "a".repeat(63))) {
            val deps = FakeDeps(
                account = okAccount(),
                group = FakeThrowawayGroup(decode = { chat("legacy after bad row") }),
                rows = listOf(row(5, 1)),
                route = accountId to badGroupIdHex,
            )
            assertEquals(
                "malformed stored row '$badGroupIdHex' must fall back",
                "legacy after bad row",
                decryptPushPreview(PushWake(groupHex, 6), deps)?.body,
            )
            assertEquals(1, deps.resolveCount)
        }
    }

    @Test
    fun `a malformed wake never reaches the route lookup`() = runTest {
        val deps = FakeDeps(okAccount(), route = accountId to groupHex)
        assertNull(decryptPushPreview(PushWake("abc", 5), deps))
        assertEquals(0, deps.lookupCount)
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

    // -----------------------------------------------------------------
    // SERVICE-ENVELOPE FILTER (isServiceEnvelope)
    //
    // Not every Application message is chat. The ADR-0042/0044
    // push-routing-secret envelope and the call-signaling proto are
    // machine-to-machine. `parse_body`'s deliberate legacy fallback
    // (`String::from_utf8_lossy`, kept for pre-1.19 plain-UTF-8 wire
    // compat) degrades BOTH to `Chat`, so without a filter the
    // background handler rendered the domain string plus raw binary as
    // a notification body.
    //
    // The fakes below stand in for the two native pieces:
    //   * [fakeRoutingSecretDecode] mimics the FFI
    //     `decodePushRoutingSecret` (domain + length + version gate),
    //   * [lossyChatDecode] mimics `parse_body`'s fallback, so a payload
    //     that clears the filter surfaces as chat text EXACTLY as the
    //     real parser would. That is what makes these tests
    //     discriminate: delete the `isServiceEnvelope` call and the
    //     envelope bytes become the preview.
    // -----------------------------------------------------------------

    /** `lakko/push-routing-secret/v1\0` — 29 bytes. The trailing NUL is
     *  APPENDED PROGRAMMATICALLY, never written into this source file:
     *  a raw NUL makes git treat the file as binary and silently drops
     *  it from `grep -r` (the ADR-0042 MEDIUM-3 / ADR-0043 F5 class). */
    private val routingDomain: ByteArray =
        "lakko/push-routing-secret/v1".toByteArray(Charsets.US_ASCII) +
            byteArrayOf(0)

    /** A well-formed v2 routing-secret envelope:
     *  `domain(29) || version(1)=0x02 || mint_epoch(8) || secret(32)`. */
    private fun routingSecretEnvelope(version: Int = 0x02): ByteArray {
        val out = ByteArray(GroupPushRouting.ENVELOPE_LEN)
        routingDomain.copyInto(out, 0)
        out[29] = version.toByte()
        // mint_epoch stays 0; fill the 32-byte secret with a marker.
        for (i in 38 until out.size) out[i] = 0xAB.toByte()
        return out
    }

    /** A `CallSignalingPayload` proto: first byte is one of the oneof
     *  body tags (fields 10..13). */
    private fun callSignalingPayload(tag: Int = 0x52): ByteArray =
        byteArrayOf(tag.toByte(), 0x04, 0x01, 0x02, 0x03, 0x04)

    /** Stand-in for the native `decodePushRoutingSecret`: accepts only a
     *  genuine current-version envelope, exactly like the fail-closed
     *  FFI (wrong domain / wrong version / wrong length -> reject). */
    private fun fakeRoutingSecretDecode(bytes: ByteArray): Boolean =
        bytes.size == GroupPushRouting.ENVELOPE_LEN &&
            bytes.copyOfRange(0, routingDomain.size).contentEquals(routingDomain) &&
            bytes[29].toInt() == 0x02

    /** `lakko/committer-beacon/v1\0` — 26 bytes. The trailing NUL is
     *  APPENDED PROGRAMMATICALLY for the same reason as [routingDomain]:
     *  a raw NUL makes git treat the file as binary. */
    private val beaconDomain: ByteArray =
        "lakko/committer-beacon/v1".toByteArray(Charsets.US_ASCII) +
            byteArrayOf(0)

    /** A well-formed ADR-0045 beacon: `domain(26) || version(1)=0x01`.
     *  FIELD-LESS, so every beacon on the wire is byte-identical. */
    private fun committerBeacon(version: Int = 0x01): ByteArray =
        beaconDomain + byteArrayOf(version.toByte())

    /** Stand-in for the FFI's `committerBeaconEnvelopeLen()` — DERIVED
     *  from the fixture builder, never a literal `27`, so the double and
     *  the length it is tested against cannot diverge. */
    private val beaconEnvelopeLen: Int = committerBeacon().size

    /** Stand-in for the native `decodeCommitterBeacon`: the same
     *  fail-closed gate the Rust codec applies — domain prefix, THEN
     *  exact payload length, THEN schema version. `true` models the Rust
     *  `Ok(())`; the beacon carries nothing to hand back. */
    private fun fakeCommitterBeaconDecode(bytes: ByteArray): Boolean =
        bytes.size == beaconEnvelopeLen &&
            bytes.copyOfRange(0, beaconDomain.size).contentEquals(beaconDomain) &&
            bytes[beaconDomain.size].toInt() == 0x01

    /** Stand-in for the real per-row extractor: the production filter,
     *  then `parse_body`'s lossy-UTF-8 legacy fallback. */
    private val lossyChatDecode: (ByteArray) -> ProcessedRow? = { payload ->
        if (isServiceEnvelope(
                payload,
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            )) {
            null
        } else {
            chat(String(payload, Charsets.UTF_8))
        }
    }

    private fun rowOf(id: Long, payload: ByteArray) = FetchedRow(id, payload)

    private fun textRow(id: Long, text: String) =
        FetchedRow(id, text.toByteArray(Charsets.UTF_8))

    @Test
    fun `isServiceEnvelope classifies both service kinds and passes real chat`() {
        // 1. The ADR-0042/0044 routing-secret envelope.
        assertTrue(
            isServiceEnvelope(
                routingSecretEnvelope(),
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            ),
        )
        // 2. Call signaling — every oneof body tag (fields 10..13).
        for (tag in listOf(0x52, 0x5A, 0x62, 0x6A)) {
            assertTrue(
                "tag 0x${tag.toString(16)} must classify as service",
                isServiceEnvelope(
                    callSignalingPayload(tag),
                    ::fakeRoutingSecretDecode,
                    ::fakeCommitterBeaconDecode,
                    beaconEnvelopeLen,
                ),
            )
        }
        // A real chat envelope starts with `{` (0x7B) — never a service
        // envelope, and it must not pay for the throwing codec.
        assertFalse(
            isServiceEnvelope(
                """{"v":"1.0","text":"hi"}""".toByteArray(Charsets.UTF_8),
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            ),
        )
        assertFalse(
            isServiceEnvelope(
                ByteArray(0),
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            ),
        )
    }

    @Test
    fun `isServiceEnvelope defers to the codec, not the cheap peek`() {
        // A 70-byte legacy plaintext starting with 'l' CLEARS the cheap
        // length + first-byte pre-filter. Only the authoritative decode
        // separates it from a real envelope, so a peek-only filter would
        // silently swallow this genuine message.
        val legacy = "let's meet at the usual place tomorrow".padEnd(70, '.')
        val bytes = legacy.toByteArray(Charsets.US_ASCII)
        assertEquals(GroupPushRouting.ENVELOPE_LEN, bytes.size)
        assertTrue(GroupPushRouting.looksLikeSecretEnvelope(bytes)) // peek says yes...
        assertFalse(
            isServiceEnvelope(
                bytes,
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            ),
        ) // ...codec says no

        // A retired v1 / wrong-version envelope is likewise refused and
        // falls through to the chat parse (fail-closed, never a
        // downgrade).
        assertFalse(
            isServiceEnvelope(
                routingSecretEnvelope(version = 0x01),
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            ),
        )
    }

    @Test
    fun `a routing-secret envelope alone yields no preview`() = runTest {
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(rowOf(10, routingSecretEnvelope())),
        )
        // Pre-fix this returned a preview whose body was the domain
        // string plus binary.
        assertNull(decryptPushPreview(PushWake(groupHex, 11), deps))
        assertTrue(group.closed)
    }

    @Test
    fun `a call-signaling payload alone yields no preview`() = runTest {
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(rowOf(10, callSignalingPayload())),
        )
        assertNull(decryptPushPreview(PushWake(groupHex, 11), deps))
        assertTrue(group.closed)
    }

    @Test
    fun `a service envelope before a chat message still previews the chat`() = runTest {
        // Non-suppression guard: the filter must reject ONLY the
        // envelope, never the real message that follows it.
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(
                rowOf(10, routingSecretEnvelope()),
                textRow(11, "hello there"),
            ),
        )
        assertEquals("hello there", decryptPushPreview(PushWake(groupHex, 12), deps)?.body)
    }

    @Test
    fun `a service envelope after a chat message falls back to the chat`() = runTest {
        // The loop keeps the LAST previewable row, so a trailing service
        // envelope is exactly the case that used to overwrite a real
        // message with garbage.
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(
                textRow(10, "hello there"),
                rowOf(11, routingSecretEnvelope()),
            ),
        )
        assertEquals("hello there", decryptPushPreview(PushWake(groupHex, 12), deps)?.body)
    }

    @Test
    fun `a trailing call-signaling payload falls back to the chat`() = runTest {
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(
                textRow(10, "hello there"),
                rowOf(11, callSignalingPayload()),
            ),
        )
        assertEquals("hello there", decryptPushPreview(PushWake(groupHex, 12), deps)?.body)
    }

    // -----------------------------------------------------------------
    // KIND 3 — the ADR-0045 committer-liveness beacon.
    //
    // MORE reachable than the routing secret, not less: a routing secret
    // is broadcast on a membership change, a beacon every 60 s from every
    // owner and admin, so in an active group the newest journal row (the
    // one previewed) is frequently a beacon.
    // -----------------------------------------------------------------

    @Test
    fun `an unfiltered beacon really would render as garbage text`() {
        // Pins the DEFECT rather than the fix: `lossyChatDecode` models
        // `parse_body`'s legacy fallback, so this is the notification body
        // a beacon produces with nothing filtering it. If this stops being
        // true the tests below stop discriminating.
        val text = String(committerBeacon(), Charsets.UTF_8)
        assertTrue(
            "the lossy fallback surfaces the domain string",
            text.startsWith("lakko/committer-beacon/v1"),
        )
    }

    @Test
    fun `isServiceEnvelope classifies a committer beacon`() {
        assertTrue(
            isServiceEnvelope(
                committerBeacon(),
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            ),
        )
    }

    @Test
    fun `the beacon peek is exact, and the codec still has the last word`() {
        // The beacon is FIELD-LESS, so exact length + leading `l` fully
        // determines the value — the peek is a complete test here, unlike
        // the routing one. It must still ACCEPT a real beacon...
        assertTrue(looksLikeCommitterBeacon(committerBeacon(), beaconEnvelopeLen))
        // ...and REJECT the near-misses, keeping the codec off the hot path.
        assertFalse(looksLikeCommitterBeacon(ByteArray(0), beaconEnvelopeLen))
        assertFalse(
            "a JSON chat body starts with `{`",
            looksLikeCommitterBeacon(
                """{"v":"1.0","text":"hi"}""".toByteArray(Charsets.UTF_8),
                beaconEnvelopeLen,
            ),
        )
        // A 27-byte legacy plaintext starting with 'l' CLEARS the peek —
        // the codec is what stops it being swallowed as a service envelope.
        val legacy = "lunch at six".padEnd(beaconEnvelopeLen, '.')
            .toByteArray(Charsets.US_ASCII)
        assertEquals(beaconEnvelopeLen, legacy.size)
        assertTrue(looksLikeCommitterBeacon(legacy, beaconEnvelopeLen)) // peek says yes...
        assertFalse(
            isServiceEnvelope(
                legacy,
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            ),
        ) // ...codec says no
        // An unknown schema version is refused too (fail-closed).
        assertFalse(
            isServiceEnvelope(
                committerBeacon(version = 0xFF),
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen,
            ),
        )
    }

    @Test
    fun `the beacon length comes from the caller, not a hardcoded 27`() {
        // Discriminates "reads committerBeaconEnvelopeLen()" from
        // "hardcodes 27": told a DIFFERENT length, the filter must miss a
        // 27-byte beacon. A filter carrying the literal would suppress it
        // regardless — the drift shape ADR-0044 documents, caught here
        // rather than at the next envelope revision.
        assertFalse(looksLikeCommitterBeacon(committerBeacon(), beaconEnvelopeLen + 1))
        assertFalse(
            isServiceEnvelope(
                committerBeacon(),
                ::fakeRoutingSecretDecode,
                ::fakeCommitterBeaconDecode,
                beaconEnvelopeLen + 1,
            ),
        )
    }

    @Test
    fun `the beacon and routing codecs do not cross-accept`() {
        assertFalse(fakeCommitterBeaconDecode(routingSecretEnvelope()))
        assertFalse(fakeRoutingSecretDecode(committerBeacon()))
    }

    @Test
    fun `a committer beacon alone yields no preview`() = runTest {
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(rowOf(10, committerBeacon())),
        )
        // Pre-fix this returned a preview whose body was the domain
        // string plus two control bytes.
        assertNull(decryptPushPreview(PushWake(groupHex, 11), deps))
        assertTrue(group.closed)
    }

    @Test
    fun `a beacon before a chat message still previews the chat`() = runTest {
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(
                rowOf(10, committerBeacon()),
                textRow(11, "hello there"),
            ),
        )
        assertEquals("hello there", decryptPushPreview(PushWake(groupHex, 12), deps)?.body)
    }

    @Test
    fun `a trailing beacon falls back to the chat`() = runTest {
        // The load-bearing ordering, and the common case in an active
        // group: the loop keeps the LAST previewable row, so a trailing
        // beacon is exactly what used to overwrite a real message.
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(
                textRow(10, "hello there"),
                rowOf(11, committerBeacon()),
            ),
        )
        assertEquals("hello there", decryptPushPreview(PushWake(groupHex, 12), deps)?.body)
    }

    @Test
    fun `a beacon burst around a chat keeps the chat`() = runTest {
        // Three admins beaconing while one person types — an ordinary
        // minute on a busy group.
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(
                rowOf(10, committerBeacon()),
                textRow(11, "hello there"),
                rowOf(12, committerBeacon()),
                rowOf(13, committerBeacon()),
            ),
        )
        assertEquals("hello there", decryptPushPreview(PushWake(groupHex, 14), deps)?.body)
    }

    @Test
    fun `a batch of only beacons yields no preview`() = runTest {
        val group = FakeThrowawayGroup(decode = lossyChatDecode)
        val deps = FakeDeps(
            okAccount(),
            group = group,
            rows = listOf(
                rowOf(10, committerBeacon()),
                rowOf(11, committerBeacon()),
            ),
        )
        assertNull(decryptPushPreview(PushWake(groupHex, 12), deps))
        assertTrue(group.closed)
    }
}
