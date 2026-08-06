// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the ADR-0042 [GroupPushRouting] orchestration
 * (the FFI-free, native-`.so`-free half). Mirror of the web
 * `apps/web/src/push/group-routing.test.ts`.
 *
 * The FFI derivation / compose + the stores + push-gw subscribe + the
 * journal broadcast are injected behind the [PushRoutingPorts] seam
 * ([FakePorts]) — the SAME host-vs-instrumented split as
 * [me.lakko.app.transparency.TransparencyVerifyTest] and
 * [me.lakko.app.push.PushPreviewDecryptTest]. The REAL keyed-BLAKE3
 * `derivePushGid` golden equality is proven in the instrumented
 * [GroupPushRoutingInstrumentedTest] (needs `liblakko_ffi_uniffi.so`).
 *
 * What this suite pins:
 *   - the OWNER mints the secret exactly once, persists it, broadcasts the
 *     distribution envelope, and subscribes under the derived `push_gid`
 *     — a second provision is a no-op (no re-mint / re-broadcast);
 *   - a NON-owner never mints;
 *   - a member that receives the envelope persists + subscribes under the
 *     SAME `push_gid` a golden derivation would give (structural golden-
 *     equality: publisher + subscriber both route through `derivePushGid`);
 *   - subscribe routes under `push_gid`, NEVER the DS `group_id`;
 *   - a member WITHOUT the secret DEFERS (no subscription at all);
 *   - the reverse index (`push_gid -> account/group`) is populated on
 *     every secret persist;
 *   - the receive-side envelope pre-filter is fail-closed.
 */
private fun hex(bytes: ByteArray): String =
    bytes.joinToString("") { "%02x".format(it.toInt() and 0xFF) }

class GroupPushRoutingTest {

    private val account = "acctB64=="
    private val groupHex = "aa".repeat(32)

    /** A row filed into the fake reverse index. */
    private data class Route(val account: String, val groupHex: String)

    /**
     * Fully in-memory [PushRoutingPorts]. `derivePushGid` is a
     * deterministic, one-way-ish, 32-byte transform (byte-reversed +
     * XOR marker) that is distinct from BOTH the secret and any plausible
     * group-id bytes — enough to prove "subscribe target == derived
     * push_gid, not the secret / group_id" without the real BLAKE3. Two
     * FakePorts sharing this transform derive the SAME push_gid from the
     * SAME secret, which is the structural golden-equality property.
     */
    private class FakePorts(private val accountIdB64: String) : PushRoutingPorts {
        var mintCount = 0
        var broadcastCount = 0
        val secrets = mutableMapOf<String, ByteArray>()
        val routes = mutableMapOf<String, Route>()
        var lastSubscribeTarget: ByteArray? = null
        var subscribeCount = 0

        /** A distinct, fixed secret so mint output is assertable. */
        override fun mintSecret(): ByteArray {
            mintCount++
            return ByteArray(32) { (0x30 + it).toByte() }
        }

        override fun derivePushGid(secret: ByteArray): ByteArray =
            ByteArray(32) { (secret[31 - it].toInt() xor 0x5A).toByte() }

        override fun composeEnvelope(secret: ByteArray): ByteArray =
            ByteArray(1) { 0x6c } + secret

        override fun hasSecret(groupIdHex: String): Boolean = secrets.containsKey(groupIdHex)

        override fun getSecret(groupIdHex: String): ByteArray? = secrets[groupIdHex]

        override fun persist(groupIdHex: String, secret: ByteArray, pushGid: ByteArray) {
            secrets[groupIdHex] = secret
            routes[hex(pushGid)] = Route(accountIdB64, groupIdHex)
        }

        override suspend fun subscribeUnderPushGid(pushGid: ByteArray) {
            subscribeCount++
            lastSubscribeTarget = pushGid
        }

        override suspend fun broadcast(envelope: ByteArray) {
            broadcastCount++
        }
    }

    // -----------------------------------------------------------------
    // Owner-mint funnel
    // -----------------------------------------------------------------

    @Test
    fun `owner mints once, persists, broadcasts, subscribes under push_gid`() = runTest {
        val ports = FakePorts(account)
        GroupPushRouting.ensure(account, groupHex, isOwner = true, ports = ports)

        // Minted + persisted exactly once.
        assertEquals(1, ports.mintCount)
        val secret = ports.secrets[groupHex]
        assertTrue("secret persisted", secret != null)
        assertEquals("secret is 32 bytes", 32, secret!!.size)

        // Broadcast the distribution envelope exactly once.
        assertEquals(1, ports.broadcastCount)

        // Subscribed under the DERIVED push_gid — not the raw secret.
        val pushGid = ports.derivePushGid(secret)
        assertArrayEquals(pushGid, ports.lastSubscribeTarget)
        assertFalse(
            "subscribe target must not be the raw secret",
            secret.contentEquals(ports.lastSubscribeTarget),
        )

        // Reverse index maps push_gid -> (account, group).
        assertEquals(Route(account, groupHex), ports.routes[hex(pushGid)])
    }

    @Test
    fun `a second provision of the same owned group is a no-op`() = runTest {
        val ports = FakePorts(account)
        val g = "bb".repeat(32)
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        assertEquals("minted exactly once", 1, ports.mintCount)
        assertEquals("broadcast exactly once", 1, ports.broadcastCount)
    }

    @Test
    fun `a non-owner never mints and defers`() = runTest {
        val ports = FakePorts(account)
        val g = "cc".repeat(32)
        GroupPushRouting.ensure(account, g, isOwner = false, ports = ports)

        assertEquals("no mint for a non-owner", 0, ports.mintCount)
        assertEquals("no broadcast for a non-owner", 0, ports.broadcastCount)
        assertEquals("deferred — no subscribe under group_id", 0, ports.subscribeCount)
    }

    // -----------------------------------------------------------------
    // Receive side + structural golden-equality
    // -----------------------------------------------------------------

    @Test
    fun `a member receiving the envelope derives the SAME push_gid (golden-equal)`() = runTest {
        // Owner mints + would broadcast; capture its secret + push_gid.
        val ownerPorts = FakePorts("acctOwner")
        val g = "dd".repeat(32)
        GroupPushRouting.ensure("acctOwner", g, isOwner = true, ports = ownerPorts)
        val secret = ownerPorts.secrets[g]!!
        val ownerPushGid = ownerPorts.lastSubscribeTarget!!

        // A different account receives the SAME secret bytes off the journal.
        val peerPorts = FakePorts("acctPeer")
        GroupPushRouting.receive(g, secret, peerPorts)

        // Peer persisted under its OWN account + subscribed under the SAME
        // push_gid the owner derived (the property that makes the notify
        // JOIN match). No mint on the receive side.
        assertEquals(0, peerPorts.mintCount)
        assertArrayEquals(ownerPushGid, peerPorts.lastSubscribeTarget)
        assertEquals(Route("acctPeer", g), peerPorts.routes[hex(ownerPushGid)])
    }

    @Test
    fun `a member WITHOUT the secret defers (never subscribes)`() = runTest {
        val ports = FakePorts(account)
        val outcome = GroupPushRouting.subscribeOrDefer("ee".repeat(32), ports)
        assertEquals(GroupPushRouting.SubscribeOutcome.DEFERRED, outcome)
        assertEquals(0, ports.subscribeCount)
    }

    @Test
    fun `redistribute broadcasts the held secret, no-op without one`() = runTest {
        val ports = FakePorts(account)
        val g = "ff".repeat(32)
        // No secret held yet -> no broadcast.
        GroupPushRouting.redistribute(g, ports)
        assertEquals(0, ports.broadcastCount)

        // Provision (owner mint) then redistribute -> one more broadcast.
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        val afterMint = ports.broadcastCount
        GroupPushRouting.redistribute(g, ports)
        assertEquals(afterMint + 1, ports.broadcastCount)
    }

    // -----------------------------------------------------------------
    // Fail-closed receive-side pre-filter
    // -----------------------------------------------------------------

    @Test
    fun `looksLikeSecretEnvelope is fail-closed`() {
        // A well-formed 62-byte, 0x6c-prefixed envelope passes the pre-filter.
        val ok = ByteArray(62).also { it[0] = 0x6c }
        assertTrue(GroupPushRouting.looksLikeSecretEnvelope(ok))

        // A plain chat JSON envelope (first byte '{' = 0x7B) is rejected.
        val chat = "{\"v\":1,\"t\":\"hello\"}".toByteArray(Charsets.UTF_8)
        assertFalse(GroupPushRouting.looksLikeSecretEnvelope(chat))

        // Right length, wrong first byte.
        assertFalse(GroupPushRouting.looksLikeSecretEnvelope(ByteArray(62) { 0x7b }))

        // Wrong length, right first byte.
        assertFalse(GroupPushRouting.looksLikeSecretEnvelope(ByteArray(61).also { it[0] = 0x6c }))

        // Empty.
        assertFalse(GroupPushRouting.looksLikeSecretEnvelope(ByteArray(0)))
    }

    @Test
    fun `subscribeOrDefer with a held secret subscribes under push_gid`() = runTest {
        val ports = FakePorts(account)
        val g = "12".repeat(32)
        ports.secrets[g] = ByteArray(32) { it.toByte() }
        val outcome = GroupPushRouting.subscribeOrDefer(g, ports)
        assertEquals(GroupPushRouting.SubscribeOutcome.SUBSCRIBED, outcome)
        assertArrayEquals(ports.derivePushGid(ports.secrets[g]!!), ports.lastSubscribeTarget)
        assertNull("no reverse-index write on a bare subscribe", ports.routes[g])
    }
}
