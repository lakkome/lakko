// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
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
 *   - a FAILED broadcast is RECOVERABLE, not terminal: the secret stays
 *     stored flagged NOT distributed and the next `ensure` re-broadcasts
 *     it (recomposed from the store, never re-minted) until one lands —
 *     while a legacy row carrying no flag is read as already distributed,
 *     so an upgrade does not re-broadcast every working group;
 *   - a NON-owner never mints, and never broadcasts even holding a
 *     pending row;
 *   - a member that receives the envelope persists + subscribes under the
 *     SAME `push_gid` a golden derivation would give (structural golden-
 *     equality: publisher + subscriber both route through `derivePushGid`);
 *   - subscribe routes under `push_gid`, NEVER the DS `group_id`;
 *   - a member WITHOUT the secret DEFERS (no subscription at all);
 *   - the reverse index (`push_gid -> account/group`) is populated on
 *     every secret persist;
 *   - the receive-side envelope pre-filter is fail-closed;
 *   - the receive-side RANK gate — owner outranks admin outranks member:
 *     the owner may always rotate, an admin may only SEED a device that
 *     holds nothing, a plain member may only relay a byte-identical
 *     secret, a legacy group with no role table stays open, and an
 *     unreadable role table (or an unreadable store) fails closed.
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
    private open class FakePorts(private val accountIdB64: String) : PushRoutingPorts {
        var mintCount = 0
        var broadcastCount = 0
        val secrets = mutableMapOf<String, ByteArray>()

        /** The durable `distributed` flag, per group — mirrors the
         *  `PushRoutingSecretStore` row column. A group present in
         *  [secrets] but absent here is a row written by an older build
         *  (no flag suffix), which the real store reads as `true`. */
        val distributed = mutableMapOf<String, Boolean>()
        val routes = mutableMapOf<String, Route>()
        var lastSubscribeTarget: ByteArray? = null
        var subscribeCount = 0
        val broadcastEnvelopes = mutableListOf<ByteArray>()

        /** When true, [broadcast] throws — the offline / DS-error case. */
        var broadcastFails = false

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

        // Absent flag == a legacy row == already distributed, exactly as
        // `PushRoutingSecretStore.parseValue` reads a suffix-less value.
        override fun isDistributed(groupIdHex: String): Boolean = distributed[groupIdHex] ?: true

        override fun persist(
            groupIdHex: String,
            secret: ByteArray,
            pushGid: ByteArray,
            distributed: Boolean,
        ) {
            secrets[groupIdHex] = secret
            this.distributed[groupIdHex] = distributed
            routes[hex(pushGid)] = Route(accountIdB64, groupIdHex)
        }

        override fun markDistributed(groupIdHex: String) {
            if (secrets.containsKey(groupIdHex)) distributed[groupIdHex] = true
        }

        override suspend fun subscribeUnderPushGid(pushGid: ByteArray) {
            subscribeCount++
            lastSubscribeTarget = pushGid
        }

        override suspend fun broadcast(envelope: ByteArray) {
            if (broadcastFails) throw IllegalStateException("DS unreachable")
            broadcastCount++
            broadcastEnvelopes.add(envelope)
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
    // Durable `distributed` flag — the failed-broadcast recovery path
    //
    // The guard used to be exactly `isOwner && !hasSecret`, with the
    // secret persisted BEFORE the broadcast and the broadcast's failure
    // swallowed. So an owner that minted while offline stored the secret,
    // permanently satisfied its own mint guard, and NEVER re-sent the
    // distribution envelope — every other member deferred forever and the
    // whole group silently lost push for good (only an unrelated
    // add-member `redistribute` could accidentally repair it).
    //
    // Persist-before-broadcast stays (a second random mint would DIVERGE
    // the group's push_gid; a second broadcast is inert); the durable flag
    // is what turns the failure from terminal into recoverable.
    // -----------------------------------------------------------------

    @Test
    fun `a FAILED broadcast stores the secret but leaves it NOT distributed`() = runTest {
        val ports = FakePorts(account)
        val g = "20".repeat(32)
        ports.broadcastFails = true

        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        assertEquals("minted once", 1, ports.mintCount)
        assertTrue("secret persisted despite the failed broadcast", ports.secrets[g] != null)
        assertEquals("nothing reached the wire", 0, ports.broadcastCount)
        assertFalse("row is flagged pending", ports.isDistributed(g))
        // The local half still works — this device subscribes under its
        // own push_gid; it is the OTHER members that are missing the secret.
        assertEquals(1, ports.subscribeCount)
    }

    @Test
    fun `the NEXT ensure RE-BROADCASTS a pending secret without re-minting`() = runTest {
        val ports = FakePorts(account)
        val g = "21".repeat(32)

        // 1. Mint while the broadcast fails.
        ports.broadcastFails = true
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        val minted = ports.secrets[g]!!
        assertEquals(0, ports.broadcastCount)

        // 2. Next launch / chat-open, connectivity restored.
        ports.broadcastFails = false
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        assertEquals("NEVER re-minted (a second mint would diverge push_gid)", 1, ports.mintCount)
        assertEquals("the pending envelope was re-sent", 1, ports.broadcastCount)
        assertArrayEquals("same secret, recomposed from the store", minted, ports.secrets[g])
        assertArrayEquals(
            "the envelope is the one composed from the STORED secret",
            ports.composeEnvelope(minted),
            ports.broadcastEnvelopes.single(),
        )
        assertTrue("flagged distributed on success", ports.isDistributed(g))
    }

    @Test
    fun `a re-broadcast keeps retrying until one actually lands`() = runTest {
        val ports = FakePorts(account)
        val g = "22".repeat(32)
        ports.broadcastFails = true

        repeat(4) { GroupPushRouting.ensure(account, g, isOwner = true, ports = ports) }
        assertEquals("still minted exactly once", 1, ports.mintCount)
        assertEquals(0, ports.broadcastCount)
        assertFalse(ports.isDistributed(g))

        ports.broadcastFails = false
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        assertEquals(1, ports.broadcastCount)
        assertTrue(ports.isDistributed(g))
    }

    @Test
    fun `a SUCCESSFUL mint does not re-broadcast on the next ensure`() = runTest {
        val ports = FakePorts(account)
        val g = "23".repeat(32)

        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        assertEquals(1, ports.broadcastCount)
        assertTrue("flagged distributed by the successful broadcast", ports.isDistributed(g))

        // Re-provisioning (bootstrap sweep + every chat-open) must stay a
        // no-op — the flag is what stops this becoming a broadcast storm.
        repeat(3) { GroupPushRouting.ensure(account, g, isOwner = true, ports = ports) }
        assertEquals("broadcast exactly once in total", 1, ports.broadcastCount)
        assertEquals(1, ports.mintCount)
    }

    @Test
    fun `a LEGACY row with no flag is treated as distributed (no upgrade broadcast storm)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "24".repeat(32)
            // A row written by an older build: a secret with NO flag entry,
            // exactly what `PushRoutingSecretStore.parseValue` sees for a
            // value carrying no ':' suffix.
            ports.secrets[g] = secretA

            GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

            assertEquals("no re-mint", 0, ports.mintCount)
            assertEquals(
                "a working legacy group must NOT be re-broadcast on upgrade",
                0,
                ports.broadcastCount,
            )
            assertEquals("still subscribes under its push_gid", 1, ports.subscribeCount)
        }

    @Test
    fun `a RECEIVED secret is stored distributed (a receiver never re-broadcasts)`() = runTest {
        val ports = FakePorts(account)
        val g = "25".repeat(32)

        GroupPushRouting.receive(g, secretA, ports)
        assertTrue("arrived off the journal, so it is by definition on the wire", ports.isDistributed(g))

        // Even if this device were the owner, there is nothing pending.
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        assertEquals(0, ports.broadcastCount)
        assertEquals(0, ports.mintCount)
    }

    @Test
    fun `a NON-owner holding a pending row still never broadcasts`() = runTest {
        val ports = FakePorts(account)
        val g = "26".repeat(32)
        ports.secrets[g] = secretA
        ports.distributed[g] = false

        GroupPushRouting.ensure(account, g, isOwner = false, ports = ports)

        // Only the owner distributes — the re-broadcast branch must not
        // become a back door around that.
        assertEquals(0, ports.broadcastCount)
        assertEquals(0, ports.mintCount)
        assertEquals("still subscribes under its held secret", 1, ports.subscribeCount)
    }

    @Test
    fun `redistribute marks the row distributed (heals a mint whose broadcast failed)`() = runTest {
        val ports = FakePorts(account)
        val g = "27".repeat(32)
        ports.broadcastFails = true
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        assertFalse(ports.isDistributed(g))

        // An add-member commit fires `redistribute`; that send IS the
        // envelope `ensure` would have re-sent, so it clears the flag too.
        ports.broadcastFails = false
        GroupPushRouting.redistribute(g, ports)
        assertTrue(ports.isDistributed(g))

        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        assertEquals("no duplicate re-broadcast after the heal", 1, ports.broadcastCount)
    }

    @Test
    fun `a failing redistribute leaves the row pending for ensure to retry`() = runTest {
        val ports = FakePorts(account)
        val g = "28".repeat(32)
        ports.broadcastFails = true
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        // `redistribute` propagates (its caller is best-effort) and must
        // NOT mark a send that never happened.
        runCatching { GroupPushRouting.redistribute(g, ports) }
        assertFalse(ports.isDistributed(g))

        ports.broadcastFails = false
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        assertEquals(1, ports.broadcastCount)
        assertTrue(ports.isDistributed(g))
    }

    /** [PushRoutingPorts] whose flag read always throws (an unreadable
     *  keystore), delegating everything else. */
    private class FlakyFlag(inner: FakePorts) : PushRoutingPorts by inner {
        override fun isDistributed(groupIdHex: String): Boolean =
            throw IllegalStateException("keystore unavailable")
    }

    @Test
    fun `an UNREADABLE distributed flag re-broadcasts rather than assuming distributed`() =
        runTest {
            val inner = FakePorts(account)
            val g = "29".repeat(32)
            inner.secrets[g] = secretA
            inner.distributed[g] = true

            GroupPushRouting.ensure(account, g, isOwner = true, ports = FlakyFlag(inner))

            // A needless re-broadcast is inert (byte-identical bytes are
            // idempotently accepted by every member); guessing "distributed"
            // would resurrect the permanent-no-push bug.
            assertEquals(1, inner.broadcastCount)
            assertEquals("still never re-mints", 0, inner.mintCount)
        }

    @Test
    fun `the pure decideEnsureAction covers the whole matrix`() {
        fun decide(isOwner: Boolean, hasSecret: Boolean, distributed: Boolean) =
            GroupPushRouting.decideEnsureAction(isOwner, hasSecret, distributed)

        val none = GroupPushRouting.EnsureAction.NONE
        val mint = GroupPushRouting.EnsureAction.MINT_AND_BROADCAST
        val rebroadcast = GroupPushRouting.EnsureAction.REBROADCAST

        // Owner: mint when nothing is held, re-broadcast when the held
        // secret never made it to the wire, otherwise idle.
        assertEquals(mint, decide(isOwner = true, hasSecret = false, distributed = false))
        assertEquals(mint, decide(isOwner = true, hasSecret = false, distributed = true))
        assertEquals(rebroadcast, decide(isOwner = true, hasSecret = true, distributed = false))
        assertEquals(none, decide(isOwner = true, hasSecret = true, distributed = true))

        // A non-owner never mints and never broadcasts, whatever it holds.
        assertEquals(none, decide(isOwner = false, hasSecret = false, distributed = false))
        assertEquals(none, decide(isOwner = false, hasSecret = false, distributed = true))
        assertEquals(none, decide(isOwner = false, hasSecret = true, distributed = false))
        assertEquals(none, decide(isOwner = false, hasSecret = true, distributed = true))
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

    // -----------------------------------------------------------------
    // Receive-side AUTHORITY GATE (lakko-security MEDIUM-1)
    //
    // Before the gate, `receive` persisted + re-subscribed unconditionally,
    // so ONE plain member could unilaterally rotate the group's
    // `push_gid`: every member overwrote its secret and re-subscribed, and
    // since per-group unsubscribe is never called, each churn stranded an
    // orphan push-gw subscription that keeps receiving wakes.
    //
    // Rule — OWNER outranks ADMIN outranks MEMBER, expressed as who may
    // overwrite what (no authority level is persisted alongside the secret):
    //
    //   identical to stored          | anyone        | ACCEPT
    //   any (or none) stored         | OWNER         | ACCEPT
    //   NONE stored (first-ever)     | ADMIN         | ACCEPT (fill a hole)
    //   present + changed            | ADMIN         | REJECT (no overwrite)
    //   any changed                  | plain member  | REJECT
    //   changed, no role table       | anyone        | ACCEPT (legacy/open)
    //   changed, roles unreadable    | anyone        | REJECT (fail closed)
    //
    // The admin row is not a nicety: ADR-0022 permits owner OR admin to add
    // members and `redistribute` fires after ANY add-member commit, so
    // without it a member added by a non-owner admin would hold no secret
    // forever (the owner only broadcasts at mint time) and never get push.
    // -----------------------------------------------------------------

    private val ownerKey = ByteArray(32) { 0x11 }
    private val memberKey = ByteArray(32) { 0x22 }
    private val adminKey = ByteArray(32) { 0x33 }
    private val otherAdminKey = ByteArray(32) { 0x44 }
    private val secretA = ByteArray(32) { 0xA1.toByte() }
    private val secretB = ByteArray(32) { 0xB2.toByte() }

    /** A role table with an owner and a two-entry admin set. */
    private fun knownRoles() =
        GroupPushRouting.OwnerLookup.Known(ownerKey, listOf(adminKey, otherAdminKey))

    /** Asserts the REJECT contract: nothing persisted, nothing subscribed,
     *  no reverse-index row — and any pre-existing secret left untouched. */
    private fun assertDropped(ports: FakePorts, groupHex: String, before: ByteArray?) {
        assertEquals("no subscribe on a refused secret", 0, ports.subscribeCount)
        assertTrue("no reverse-index row on a refused secret", ports.routes.isEmpty())
        if (before == null) {
            assertNull("nothing persisted", ports.secrets[groupHex])
        } else {
            assertArrayEquals("stored secret untouched", before, ports.secrets[groupHex])
        }
    }

    @Test
    fun `OWNER may change the secret - accepted, persisted, subscribed`() = runTest {
        val ports = FakePorts(account)
        val g = "01".repeat(32)
        ports.secrets[g] = secretA

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, verdict)
        assertArrayEquals("rotated to the owner's new secret", secretB, ports.secrets[g])
        assertEquals(1, ports.subscribeCount)
        assertArrayEquals(ports.derivePushGid(secretB), ports.lastSubscribeTarget)
    }

    @Test
    fun `plain MEMBER changing the secret is REJECTED - no persist, no subscribe`() = runTest {
        val ports = FakePorts(account)
        val g = "02".repeat(32)
        ports.secrets[g] = secretA

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.REJECT, verdict)
        assertDropped(ports, g, before = secretA)
    }

    @Test
    fun `plain MEMBER seeding a FIRST secret is REJECTED (holder had none)`() = runTest {
        val ports = FakePorts(account)
        val g = "03".repeat(32)

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.REJECT, verdict)
        assertDropped(ports, g, before = null)
    }

    @Test
    fun `plain MEMBER relaying the IDENTICAL secret is accepted (redistribute stays idempotent)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "04".repeat(32)
            ports.secrets[g] = secretA

            val verdict = GroupPushRouting.receiveGated(
                groupIdHex = g,
                secret = secretA.copyOf(),
                senderSignatureKey = memberKey,
                owner = knownRoles(),
                ports = ports,
            )

            // Identical bytes rotate nothing, so the relayer's identity is moot.
            assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, verdict)
            assertArrayEquals(secretA, ports.secrets[g])
            assertEquals(1, ports.subscribeCount)
        }

    // ---- ADMIN rank: may SEED a hole, may never OVERWRITE ----

    @Test
    fun `ADMIN seeding a FIRST secret is ACCEPTED (closes the admin-add delivery gap)`() = runTest {
        val ports = FakePorts(account)
        val g = "0a".repeat(32)
        // Holder has NOTHING — exactly the state of a member an admin just
        // added: the owner already broadcast at mint time, so the ONLY
        // envelope this device will ever see is the admin's `redistribute`.

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = adminKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, verdict)
        assertArrayEquals("admin filled the hole", secretB, ports.secrets[g])
        assertEquals("subscribed under the derived push_gid", 1, ports.subscribeCount)
        assertArrayEquals(ports.derivePushGid(secretB), ports.lastSubscribeTarget)
    }

    @Test
    fun `ADMIN changing an EXISTING secret is REJECTED (seed only, never overwrite)`() = runTest {
        val ports = FakePorts(account)
        val g = "0b".repeat(32)
        ports.secrets[g] = secretA

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = adminKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.REJECT, verdict)
        assertDropped(ports, g, before = secretA)
    }

    @Test
    fun `OWNER outranks an ADMIN-seeded secret (owner wins whatever the arrival order)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "0c".repeat(32)

            // 1. An admin seeds the hole first.
            val seeded = GroupPushRouting.receiveGated(
                groupIdHex = g,
                secret = secretA,
                senderSignatureKey = adminKey,
                owner = knownRoles(),
                ports = ports,
            )
            assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, seeded)
            assertArrayEquals(secretA, ports.secrets[g])

            // 2. The owner's own secret lands AFTER it and still wins — owner
            //    acceptance never depends on what is already stored.
            val rotated = GroupPushRouting.receiveGated(
                groupIdHex = g,
                secret = secretB,
                senderSignatureKey = ownerKey,
                owner = knownRoles(),
                ports = ports,
            )
            assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, rotated)
            assertArrayEquals("owner overwrote the admin-seeded secret", secretB, ports.secrets[g])
            assertArrayEquals(ports.derivePushGid(secretB), ports.lastSubscribeTarget)
        }

    @Test
    fun `a SECOND admin in the set is recognised (whole set is matched, not just the first)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "0d".repeat(32)

            val verdict = GroupPushRouting.receiveGated(
                groupIdHex = g,
                secret = secretB,
                senderSignatureKey = otherAdminKey,
                owner = knownRoles(),
                ports = ports,
            )

            assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, verdict)
            assertArrayEquals(secretB, ports.secrets[g])
        }

    @Test
    fun `an ADMIN relaying the IDENTICAL secret is accepted (idempotent, no rank needed)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "0e".repeat(32)
            ports.secrets[g] = secretA

            val verdict = GroupPushRouting.receiveGated(
                groupIdHex = g,
                secret = secretA.copyOf(),
                senderSignatureKey = adminKey,
                owner = knownRoles(),
                ports = ports,
            )

            assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, verdict)
            assertArrayEquals(secretA, ports.secrets[g])
        }

    @Test
    fun `a group with NO role table keeps pre-gate behaviour (accept)`() = runTest {
        val ports = FakePorts(account)
        val g = "05".repeat(32)
        ports.secrets[g] = secretA

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = GroupPushRouting.OwnerLookup.NoRoleTable,
            ports = ports,
        )

        // A legacy / open group has no owner to check against — unchanged
        // behaviour, deliberately not a regression for pre-ADR-0022 groups.
        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, verdict)
        assertArrayEquals(secretB, ports.secrets[g])
    }

    @Test
    fun `an UNREADABLE role table fails CLOSED on a changed secret`() = runTest {
        val ports = FakePorts(account)
        val g = "06".repeat(32)
        ports.secrets[g] = secretA

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = GroupPushRouting.OwnerLookup.Unavailable,
            ports = ports,
        )

        // Even a would-be OWNER is refused: an unreadable roster / role table
        // must never widen who may rotate (invariant #4).
        assertEquals(GroupPushRouting.ReceiveVerdict.REJECT, verdict)
        assertDropped(ports, g, before = secretA)
    }

    @Test
    fun `an UNREADABLE role table still accepts a byte-identical secret`() = runTest {
        val ports = FakePorts(account)
        val g = "07".repeat(32)
        ports.secrets[g] = secretA

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretA.copyOf(),
            senderSignatureKey = null,
            owner = GroupPushRouting.OwnerLookup.Unavailable,
            ports = ports,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, verdict)
    }

    @Test
    fun `an UNRESOLVED sender (external or absent leaf) cannot change the secret`() = runTest {
        val ports = FakePorts(account)
        val g = "08".repeat(32)
        ports.secrets[g] = secretA

        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            // `null` = external sender / leaf absent from the roster / FFI throw.
            senderSignatureKey = null,
            owner = knownRoles(),
            ports = ports,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.REJECT, verdict)
        assertDropped(ports, g, before = secretA)
    }

    /**
     * [PushRoutingPorts] whose store read throws on the first [failures]
     * calls, then delegates normally. `failures = 1` fails exactly the
     * GATE read (so the strip logic is exercised) while letting an ACCEPT
     * still run through [GroupPushRouting.receive]; a huge value makes it
     * throw forever, which is only usable on a REJECT path.
     */
    private class FlakyStore(
        private val failures: Int,
        private val inner: FakePorts,
    ) : PushRoutingPorts by inner {
        private var reads = 0

        override fun getSecret(groupIdHex: String): ByteArray? {
            reads++
            if (reads <= failures) throw IllegalStateException("keystore unavailable")
            return inner.getSecret(groupIdHex)
        }
    }

    @Test
    fun `a throwing getSecret degrades to strict, never permissive`() = runTest {
        val inner = FakePorts(account)
        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = "09".repeat(32),
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = FlakyStore(Int.MAX_VALUE, inner),
        )

        // Losing the byte-identical fast path can only make the gate stricter.
        assertEquals(GroupPushRouting.ReceiveVerdict.REJECT, verdict)
        assertTrue("nothing persisted", inner.secrets.isEmpty())
    }

    @Test
    fun `a throwing getSecret does NOT let an ADMIN seed (unknown stored state is not a hole)`() =
        runTest {
            val inner = FakePorts(account)
            val verdict = GroupPushRouting.receiveGated(
                groupIdHex = "0f".repeat(32),
                secret = secretB,
                senderSignatureKey = adminKey,
                owner = knownRoles(),
                ports = FlakyStore(Int.MAX_VALUE, inner),
            )

            // An unreadable store yields `storedSecret = null`, which for an
            // admin would otherwise READ AS a hole to fill — the one place the
            // degrade could have been permissive. `receiveGated` strips the
            // admin set on a read failure, so it stays fail-closed.
            assertEquals(GroupPushRouting.ReceiveVerdict.REJECT, verdict)
            assertTrue("nothing persisted", inner.secrets.isEmpty())
        }

    @Test
    fun `a throwing getSecret still lets the OWNER through (it may rotate anything)`() = runTest {
        val inner = FakePorts(account)
        val g = "10".repeat(32)
        // Only the GATE read fails; the strip narrows the authority to the
        // owner alone, which is exactly who is sending.
        val verdict = GroupPushRouting.receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = FlakyStore(1, inner),
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT, verdict)
        assertArrayEquals("owner's secret persisted", secretB, inner.secrets[g])
    }

    @Test
    fun `the pure decision function covers the whole matrix`() {
        val known = knownRoles()
        fun decide(
            sender: ByteArray?,
            owner: GroupPushRouting.OwnerLookup,
            incoming: ByteArray,
            stored: ByteArray?,
        ) = GroupPushRouting.decideAcceptReceivedSecret(sender, owner, incoming, stored)

        val accept = GroupPushRouting.ReceiveVerdict.ACCEPT
        val reject = GroupPushRouting.ReceiveVerdict.REJECT

        // --- OWNER: may seed AND may rotate, whatever is stored ---
        assertEquals(accept, decide(ownerKey, known, secretB, secretA))
        assertEquals(accept, decide(ownerKey, known, secretA, null))

        // --- ADMIN: may SEED a hole, may NEVER overwrite ---
        assertEquals(accept, decide(adminKey, known, secretA, null))
        assertEquals(accept, decide(otherAdminKey, known, secretA, null))
        assertEquals(reject, decide(adminKey, known, secretB, secretA))
        assertEquals(reject, decide(otherAdminKey, known, secretB, secretA))

        // --- plain MEMBER: may never change it, seed or overwrite ---
        assertEquals(reject, decide(memberKey, known, secretB, secretA))
        assertEquals(reject, decide(memberKey, known, secretA, null))

        // --- identical always accepted, whoever relayed it (needs no rank) ---
        assertEquals(accept, decide(memberKey, known, secretA, secretA.copyOf()))
        assertEquals(accept, decide(adminKey, known, secretA, secretA.copyOf()))
        assertEquals(accept, decide(null, known, secretA, secretA.copyOf()))

        // --- no role table -> open; unreadable -> fail closed for EVERY rank ---
        val open = GroupPushRouting.OwnerLookup.NoRoleTable
        val unreadable = GroupPushRouting.OwnerLookup.Unavailable
        assertEquals(accept, decide(memberKey, open, secretB, secretA))
        assertEquals(accept, decide(memberKey, open, secretA, null))
        assertEquals(reject, decide(ownerKey, unreadable, secretB, secretA))
        assertEquals(reject, decide(adminKey, unreadable, secretA, null))

        // --- an unresolved sender matches neither owner nor any admin ---
        assertEquals(reject, decide(null, known, secretB, secretA))
        assertEquals(reject, decide(null, known, secretA, null))

        // --- near-miss keys (one byte off) match neither rank ---
        val nearOwner = ownerKey.copyOf().also { it[31] = 0x12 }
        val nearAdmin = adminKey.copyOf().also { it[31] = 0x34 }
        assertEquals(reject, decide(nearOwner, known, secretB, secretA))
        assertEquals(reject, decide(nearAdmin, known, secretA, null))

        // --- an EMPTY admin set degenerates to the owner-only gate ---
        val ownerOnly = GroupPushRouting.OwnerLookup.Known(ownerKey)
        assertEquals(accept, decide(ownerKey, ownerOnly, secretB, secretA))
        assertEquals(reject, decide(adminKey, ownerOnly, secretA, null))
    }

    @Test
    fun `OwnerLookup Known compares by CONTENT, not array identity`() {
        // The gate's inputs are freshly-decoded FFI arrays every poll, so
        // reference equality would silently break any equals-based assertion.
        assertEquals(knownRoles(), knownRoles())
        assertEquals(knownRoles().hashCode(), knownRoles().hashCode())
        assertEquals(
            GroupPushRouting.OwnerLookup.Known(ownerKey.copyOf(), listOf(adminKey.copyOf())),
            GroupPushRouting.OwnerLookup.Known(ownerKey, listOf(adminKey)),
        )
        // A different admin set is a different authority.
        assertNotEquals(
            GroupPushRouting.OwnerLookup.Known(ownerKey, listOf(adminKey)),
            GroupPushRouting.OwnerLookup.Known(ownerKey, listOf(memberKey)),
        )
        assertNotEquals(
            GroupPushRouting.OwnerLookup.Known(ownerKey),
            GroupPushRouting.OwnerLookup.Known(ownerKey, listOf(adminKey)),
        )
    }

    // -----------------------------------------------------------------
    // Fix A guard — the NUL separator escape is byte-identical at runtime
    // -----------------------------------------------------------------

    @Test
    fun `the Kotlin NUL escape is the raw NUL byte (stored rows stay readable)`() {
        // `PushRoutingSecretStore.ROW_SEPARATOR` / `GroupPushRouting.lockKey`
        // write `\u0000` instead of a RAW NUL byte so git renders a text diff
        // and grep does not skip the file. This pins that the swap is purely
        // lexical: the runtime value is unchanged, so EncryptedSharedPreferences
        // rows written before the swap still resolve.
        assertEquals(0, "\u0000"[0].code)
        assertEquals(1, "\u0000".length)
        assertArrayEquals(byteArrayOf(0), "\u0000".toByteArray(Charsets.UTF_8))
        // The composed key shape is `<a>NUL<b>` — unambiguous because neither
        // base64 nor lowercase hex contains a NUL.
        assertEquals("acct\u0000group", "acct" + "\u0000" + "group")
    }
}
