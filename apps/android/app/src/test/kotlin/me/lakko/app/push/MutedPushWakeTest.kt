// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import me.lakko.app.calls.CallRingSignal
import me.lakko.app.calls.TEST_RING_WINDOW_MS
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 2.1 on the push path — the half that runs with
 * the app closed and the phone locked, which is the half the rule was written
 * for.
 *
 * This is the second of two ring paths. The first is the app's own poll
 * ([me.lakko.app.calls.MutedCallRingTest]); the two share one implementation
 * of the ring itself, so what is asserted here is only what this path
 * contributes: the wake resolves the conversation's mute flag and hands it to
 * the fold, which carries it onto the ring.
 *
 * What a muted offer MEANS is not asserted here or there — it is
 * `crates/lakko-ui-rules/src/ring.rs`, asserted once for all three clients.
 */
class MutedPushWakeTest {

    private val accountId = "acct-1"
    private val groupHex = "ab".repeat(32)
    private val groupIdBytes = ByteArray(32) { 0xAB.toByte() }
    private val callIdHex = "11".repeat(16)
    private val now = 1_800_000_000_000L

    private class FakeGroup(
        private val decode: (ByteArray) -> ProcessedRow?,
    ) : ThrowawayGroup {
        override fun process(payload: ByteArray): ProcessedRow? = decode(payload)
        override fun close() = Unit
    }

    private class FakeDeps(
        val account: ResolvedPushAccount,
        val group: ThrowawayGroup,
        val rows: List<FetchedRow>,
        val muted: Boolean = false,
        val muteThrows: Boolean = false,
    ) : PushDecryptDeps {
        val mutedAsked = mutableListOf<Pair<String, String>>()

        // ADR-0078: the route maps the wake's `g` to itself for the bound account.
        override fun lookupRoute(accountIdB64: String, pushGidHex: String, subscriptionIdHex: String?): String? =
            if (accountIdB64 == account.accountIdB64) pushGidHex else null
        override fun loadIdentitySeed(accountIdB64: String): ByteArray = ByteArray(64) { 1 }
        override fun loadSnapshot(accountIdB64: String): ByteArray = ByteArray(8) { 2 }
        override fun openThrowawayGroup(
            seed: ByteArray,
            snapshot: ByteArray,
            groupIdBytes: ByteArray,
        ): ThrowawayGroup = group

        override suspend fun dsFetch(
            groupIdBytes: ByteArray,
            afterId: Long,
            signer: me.lakko.app.ds.DsJournalSigner?,
        ): List<FetchedRow> = rows
        override fun startCursor(accountIdB64: String, groupHex: String): Long? = null

        override fun isMuted(accountIdB64: String, groupHex: String): Boolean {
            mutedAsked += accountIdB64 to groupHex
            if (muteThrows) throw RuntimeException("mute store failure")
            return muted
        }
    }

    private fun deps(
        muted: Boolean = false,
        muteThrows: Boolean = false,
        signal: CallRingSignal? = CallRingSignal.Offer(
            callIdHex,
            video = false,
            offeredAtMs = 1_800_000_000_000L - 1_000,
        ),
    ) = FakeDeps(
        account = ResolvedPushAccount(accountIdB64 = accountId, groupIdBytes = groupIdBytes),
        group = FakeGroup {
            if (signal == null) {
                ProcessedRow(ProcessedKind.CHAT, "hello", 0)
            } else {
                ProcessedRow(ProcessedKind.CALL_SIGNALING, "", 0, callSignal = signal)
            }
        },
        rows = listOf(FetchedRow(10, byteArrayOf(1))),
        muted = muted,
        muteThrows = muteThrows,
    )

    private suspend fun wake(d: FakeDeps) = decryptPushWake(
        PushWake(groupHex = groupHex, pointer = 11, accountIdB64 = accountId),
        d,
        nowMs = now,
        ringWindowMs = TEST_RING_WINDOW_MS,
    )

    @Test
    fun `a wake for a muted conversation rings marked muted`() = runTest {
        val d = deps(muted = true)
        val outcome = wake(d)

        assertEquals(callIdHex, outcome.ring?.callIdHex)
        assertTrue(
            "the flag has to reach the ringer, which is what withholds the ring",
            outcome.ring?.muted == true,
        )
        assertEquals(listOf(accountId to groupHex), d.mutedAsked)
    }

    @Test
    fun `a wake for an unmuted conversation is not marked muted`() = runTest {
        val outcome = wake(deps(muted = false))
        assertFalse(outcome.ring?.muted ?: true)
    }

    @Test
    fun `a mute store that fails rings, rather than silencing the phone`() = runTest {
        val outcome = wake(deps(muted = true, muteThrows = true))
        assertEquals(callIdHex, outcome.ring?.callIdHex)
        assertFalse("failing toward silence loses calls invisibly", outcome.ring?.muted ?: true)
    }

    @Test
    fun `a muted wake still produces a ring, never a dropped call`() = runTest {
        // The push path is where a muted call would be easiest to throw away:
        // nothing has to be shown, so returning nothing would look correct.
        // It is not — the ring is what starts the window that ends in the
        // missed-call record.
        assertNull(wake(deps(muted = true)).preview)
        assertEquals(callIdHex, wake(deps(muted = true)).ring?.callIdHex)
    }

    @Test
    fun `an ordinary message wake never touches the mute store`() = runTest {
        // Every wake would otherwise pay for a file read and an AES open it
        // has no use for; only a wake that rings needs the answer.
        val d = deps(signal = null)
        val outcome = wake(d)

        assertEquals("hello", outcome.preview?.body)
        assertNull(outcome.ring)
        assertTrue(d.mutedAsked.isEmpty())
    }
}
