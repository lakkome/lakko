// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
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
 *   - that flag is per-GROUP, so a failed post-add-member `redistribute`
 *     REWINDS it to pending — otherwise a group whose mint landed stays
 *     flagged distributed and the newly-added member (the only one
 *     missing the secret) defers push forever; a successful one leaves it
 *     distributed;
 *   - the re-send is NOT owner-gated, and that is the other half of the
 *     same hole: ADR-0022 lets an ADMIN add members, so the device whose
 *     `redistribute` failed is often not the owner, and an owner-only
 *     retry would leave its rewound flag inert while the owner (row still
 *     distributed) had no reason to act. So ANY holder re-sends a pending
 *     row — safe because it re-sends byte-identical bytes, which the
 *     receive gate accepts from any sender before rank is consulted;
 *   - but a NON-owner holding NOTHING still never mints and never
 *     broadcasts — rank still gates the MINT, since two independent mints
 *     of 32 random bytes would diverge the group's `push_gid`;
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

/** Big-endian wire encoding of an ADR-0044 mint epoch (8 bytes). */
private fun beBytes(epoch: ULong): ByteArray =
    ByteArray(8) { ((epoch shr (56 - 8 * it)) and 0xFFuL).toByte() }

/**
 * Compare two `(mintEpoch, secret)` pairs as their concatenated
 * big-endian wire bytes — the flat form the Rust
 * `is_newer_matches_lexicographic_wire_bytes` test pins as equivalent to
 * the structured comparison, so the fake order cannot drift from the
 * core's.
 */
private fun compareWire(
    aEpoch: ULong,
    aSecret: ByteArray,
    bEpoch: ULong,
    bSecret: ByteArray,
): Int {
    val a = beBytes(aEpoch) + aSecret
    val b = beBytes(bEpoch) + bSecret
    for (i in 0 until minOf(a.size, b.size)) {
        val d = (a[i].toInt() and 0xFF) - (b[i].toInt() and 0xFF)
        if (d != 0) return d
    }
    return a.size - b.size
}

/** The mint epoch a [GroupPushRouting.PushRoutingPorts.composeEnvelope]
 *  envelope was stamped with, read back off the fake wire form
 *  `0x6c || mint_epoch_be(8) || secret(32)`. */
private fun stampOf(envelope: ByteArray): ULong {
    var out = 0uL
    for (i in 1..8) out = (out shl 8) or (envelope[i].toLong() and 0xFF).toULong()
    return out
}

/** The 32 secret bytes of a fake envelope. */
private fun secretOf(envelope: ByteArray): ByteArray = envelope.copyOfRange(9, envelope.size)

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

        /** ADR-0044 — the durable mint epoch, per group. A group present in
         *  [secrets] but ABSENT here is a row written by a build that
         *  predates the stamp, which the real store reads as
         *  [PushRoutingSecretStore.LEGACY_MINT_EPOCH]. */
        val mintEpochs = mutableMapOf<String, ULong>()

        /** ADR-0044 — this device's current MLS group epoch, as
         *  [freshMintStamp] would read it. `null` models a torn `Group`
         *  read, which fails a mint CLOSED. */
        var groupEpoch: ULong? = 0uL

        /** Every `(stamp, secret)` pair handed to [composeEnvelope], in
         *  call order — so a test can assert WHICH epoch an emitted
         *  envelope was stamped at, and whether the stamp was a FRESH
         *  read or a REPLAY of a stored one. */
        val composedStamps = mutableListOf<GroupPushRouting.MintStamp>()

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

        /**
         * ADR-0044 — the canonical order. In PRODUCTION this is bound to
         * the audited Rust `pushRoutingSecretIsNewer` FFI, which the host
         * JVM cannot load; the fake implements the documented rule
         * directly — compare the flat wire bytes
         * `mint_epoch_be(8) || secret(32)` lexicographically, strictly
         * greater wins, fail-closed on a wrong-length secret.
         *
         * That flat form is not a second interpretation of the order: the
         * Rust `is_newer_matches_lexicographic_wire_bytes` test pins that
         * comparing the structured `(u64, [u8;32])` pair and comparing the
         * concatenated big-endian wire bytes agree on every input, so this
         * fake and the core cannot disagree. The REAL FFI order is
         * exercised in [GroupPushRoutingInstrumentedTest].
         */
        override val isNewer: GroupPushRouting.PushRoutingSecretIsNewer =
            GroupPushRouting.PushRoutingSecretIsNewer { inEpoch, inSecret, stEpoch, stSecret ->
                if (inSecret.size != 32 || stSecret.size != 32) {
                    false
                } else {
                    compareWire(inEpoch, inSecret, stEpoch, stSecret) > 0
                }
            }

        /** The envelope carries its stamp so a test can read it back:
         *  `0x6c || mint_epoch_be(8) || secret(32)`. */
        override fun composeEnvelope(
            stamp: GroupPushRouting.MintStamp,
            secret: ByteArray,
        ): ByteArray {
            composedStamps.add(stamp)
            return ByteArray(1) { 0x6c } + beBytes(stamp.epoch) + secret
        }

        override suspend fun freshMintStamp(): GroupPushRouting.MintStamp.Fresh? =
            groupEpoch?.let { GroupPushRouting.MintStamp.Fresh(it) }

        override fun hasSecret(groupIdHex: String): Boolean = secrets.containsKey(groupIdHex)

        override fun getRecord(groupIdHex: String): GroupPushRouting.StoredSecret? =
            secrets[groupIdHex]?.let {
                GroupPushRouting.StoredSecret(
                    it,
                    // An absent entry models a pre-ADR-0044 row.
                    mintEpochs[groupIdHex] ?: PushRoutingSecretStore.LEGACY_MINT_EPOCH,
                )
            }

        // Absent flag == a legacy row == already distributed, exactly as
        // `PushRoutingSecretStore.parseValue` reads a suffix-less value.
        override fun isDistributed(groupIdHex: String): Boolean = distributed[groupIdHex] ?: true

        /** Mirrors the real store: secret + flag + stamp are ONE atomic
         *  row write, which is what makes an accepted DIFFERENT secret
         *  clear a stale `pending` re-broadcast for free. */
        override fun persist(
            groupIdHex: String,
            secret: ByteArray,
            mintEpoch: ULong,
            pushGid: ByteArray,
            distributed: Boolean,
        ) {
            secrets[groupIdHex] = secret
            this.distributed[groupIdHex] = distributed
            mintEpochs[groupIdHex] = mintEpoch
            routes[hex(pushGid)] = Route(accountIdB64, groupIdHex)
        }

        override fun markDistributed(groupIdHex: String) {
            if (secrets.containsKey(groupIdHex)) distributed[groupIdHex] = true
        }

        /** Mirrors `PushRoutingSecretStore.markPending`: reads the row
         *  first and writes nothing when there is none, so an absent row
         *  can never gain a phantom flag entry. */
        override fun markPending(groupIdHex: String) {
            markPendingCount++
            if (secrets.containsKey(groupIdHex)) distributed[groupIdHex] = false
        }

        var markPendingCount = 0

        override suspend fun subscribeUnderPushGid(pushGid: ByteArray) {
            subscribeCount++
            lastSubscribeTarget = pushGid
        }

        /** ADR-0043 — every gid this device dropped its push-gw row under,
         *  in call order. The privacy-carrying half of a rotation. */
        val unsubscribed = mutableListOf<ByteArray>()
        var unsubscribeCount = 0

        /** When true, [unsubscribeFromPushGid] throws — the offline case.
         *  The rotation must survive it (the new id is already armed). */
        var unsubscribeFails = false

        override suspend fun unsubscribeFromPushGid(pushGid: ByteArray) {
            unsubscribeCount++
            if (unsubscribeFails) throw IllegalStateException("push-gw unreachable")
            unsubscribed.add(pushGid)
        }

        /** Mirrors `PushRouteStore.remove`: deletes exactly the one keyed
         *  row and never touches [secrets], so it can never strand a group
         *  by dropping the material a LIVE push_gid derives from. */
        override fun deleteRoute(pushGid: ByteArray) {
            routes.remove(hex(pushGid))
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
            "the envelope carries the STORED secret",
            minted,
            secretOf(ports.broadcastEnvelopes.single()),
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

        GroupPushRouting.receive(g, secretA, defaultStamp, ports)
        assertTrue("arrived off the journal, so it is by definition on the wire", ports.isDistributed(g))

        // Even if this device were the owner, there is nothing pending.
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        assertEquals(0, ports.broadcastCount)
        assertEquals(0, ports.mintCount)
    }

    @Test
    fun `a NON-owner holding a PENDING row DOES re-broadcast it (identical bytes need no rank)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "26".repeat(32)
            ports.secrets[g] = secretA
            ports.distributed[g] = false

            GroupPushRouting.ensure(account, g, isOwner = false, ports = ports)

            // The re-send is the whole point of the pending flag, and it is
            // NOT owner-gated: the device that failed to send is often an
            // admin (ADR-0022 lets an admin add members), and it is the only
            // one whose row records the failure.
            assertEquals("the pending envelope was re-sent", 1, ports.broadcastCount)
            assertArrayEquals(
                "recomposed from the STORED secret, never re-minted",
                secretA,
                secretOf(ports.broadcastEnvelopes.single()),
            )
            assertEquals("a non-owner still NEVER mints", 0, ports.mintCount)
            assertTrue("flagged distributed once it landed", ports.isDistributed(g))
            assertEquals("still subscribes under its held secret", 1, ports.subscribeCount)

            // ... and settles — no broadcast storm on later sweeps.
            repeat(3) { GroupPushRouting.ensure(account, g, isOwner = false, ports = ports) }
            assertEquals("exactly one re-send in total", 1, ports.broadcastCount)
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
        GroupPushRouting.redistribute(account, g, ports)
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
        runCatching { GroupPushRouting.redistribute(account, g, ports) }
        assertFalse(ports.isDistributed(g))

        ports.broadcastFails = false
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        assertEquals(1, ports.broadcastCount)
        assertTrue(ports.isDistributed(g))
    }

    // -----------------------------------------------------------------
    // The per-group flag is also what a failed REDISTRIBUTE rewinds
    //
    // `distributed` is per-GROUP, not per-member, so on its own it heals
    // only a failed MINT broadcast. The uncovered case: the mint LANDED
    // (flag `true`), a member was then added, and the post-add-member
    // `redistribute` failed. The flag being already `true` made
    // `decideEnsureAction` return NONE, so no sweep ever re-sent and the
    // NEW member — the only one missing the secret — deferred push
    // forever. Same terminal bug, scoped to one member.
    //
    // A failure now flips the row back to pending, handing it to the
    // existing REBROADCAST branch. Safe + cheap: the retry re-sends the
    // SAME bytes, which every existing member accepts idempotently.
    // -----------------------------------------------------------------

    @Test
    fun `a FAILING redistribute rewinds a DISTRIBUTED row to pending for ensure to re-send`() =
        runTest {
            val ports = FakePorts(account)
            val g = "2a".repeat(32)

            // 1. The mint's own broadcast LANDED — fully provisioned group.
            GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
            assertEquals(1, ports.broadcastCount)
            assertTrue("mint confirmed on the wire", ports.isDistributed(g))

            // 2. A member is added; the post-add-member re-send fails.
            ports.broadcastFails = true
            runCatching { GroupPushRouting.redistribute(account, g, ports) }
            assertEquals("nothing reached the wire", 1, ports.broadcastCount)
            assertFalse("row rewound to pending", ports.isDistributed(g))

            // 3. Next launch / chat-open: re-broadcast, never re-mint.
            ports.broadcastFails = false
            GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
            assertEquals("the pending envelope was re-sent", 2, ports.broadcastCount)
            assertEquals("NEVER re-minted (a second mint would diverge)", 1, ports.mintCount)
            assertArrayEquals(
                "byte-identical re-send (idempotent for every existing member)",
                ports.broadcastEnvelopes[0],
                ports.broadcastEnvelopes[1],
            )
            assertTrue("flagged distributed once it landed", ports.isDistributed(g))

            // 4. ... and settles: no broadcast storm on later sweeps.
            repeat(3) { GroupPushRouting.ensure(account, g, isOwner = true, ports = ports) }
            assertEquals("exactly one retry in total", 2, ports.broadcastCount)
        }

    @Test
    fun `a SUCCESSFUL redistribute leaves the row distributed and the next ensure silent`() =
        runTest {
            val ports = FakePorts(account)
            val g = "2b".repeat(32)

            GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
            GroupPushRouting.redistribute(account, g, ports)
            assertEquals("mint + the add-member re-send", 2, ports.broadcastCount)
            assertTrue("a send that landed stays flagged distributed", ports.isDistributed(g))

            // The rewind must fire ONLY on a failure — a successful
            // redistribute must not arm a retry it does not need.
            repeat(3) { GroupPushRouting.ensure(account, g, isOwner = true, ports = ports) }
            assertEquals("no re-broadcast after a send that landed", 2, ports.broadcastCount)
            assertEquals(1, ports.mintCount)
        }

    @Test
    fun `marking pending is a no-op on an ABSENT row`() = runTest {
        val ports = FakePorts(account)
        val g = "2c".repeat(32)

        // Holding no secret, `redistribute` returns BEFORE any send, so it
        // never marks at all — there is nothing pending to re-send, and the
        // owner-mint branch (not the re-broadcast branch) owns "no secret".
        GroupPushRouting.redistribute(account, g, ports)
        assertEquals(0, ports.broadcastCount)
        assertEquals("never even attempted a mark", 0, ports.markPendingCount)

        // A direct mark on an absent row writes nothing either — mirroring
        // `PushRoutingSecretStore.markPending`, which reads the entry first
        // and returns when there is none. A phantom flag row would make
        // `isDistributed` answer `false` for a group holding no secret and
        // send the owner into a re-broadcast with nothing to send.
        ports.markPending(g)
        assertTrue("no secret row conjured", ports.secrets.isEmpty())
        assertTrue("no flag row conjured", ports.distributed.isEmpty())
        assertTrue("an absent row still reads as distributed", ports.isDistributed(g))
    }

    @Test
    fun `a failed redistribute on a NON-owner (admin) is retried BY THAT DEVICE`() = runTest {
        val ports = FakePorts(account)
        val g = "2d".repeat(32)
        // ADR-0022 lets an ADMIN add members, so a NON-owner reaches
        // `redistribute` too — and its failure rewinds its OWN row.
        ports.secrets[g] = secretA
        ports.distributed[g] = true

        ports.broadcastFails = true
        runCatching { GroupPushRouting.redistribute(account, g, ports) }
        assertFalse("its own row is flagged pending", ports.isDistributed(g))

        // The other half of the hole: with an owner-only retry that pending
        // flag was INERT. The owner's row is still flagged distributed and it
        // never learns the send failed, so nothing anywhere re-sends and the
        // member the admin just added defers push forever. The rewind has to
        // be actionable on the device that observed the failure.
        ports.broadcastFails = false
        GroupPushRouting.ensure(account, g, isOwner = false, ports = ports)
        assertEquals("the admin re-sent its own pending row", 1, ports.broadcastCount)
        assertArrayEquals(
            "byte-identical bytes — accepted by every peer regardless of rank",
            secretA,
            secretOf(ports.broadcastEnvelopes.single()),
        )
        assertTrue("settled", ports.isDistributed(g))
        assertEquals("re-sending is not minting — a non-owner still never mints", 0, ports.mintCount)

        // And it settles: the retry runs until one send lands, then stops.
        repeat(3) { GroupPushRouting.ensure(account, g, isOwner = false, ports = ports) }
        assertEquals("exactly one retry in total", 1, ports.broadcastCount)
    }

    @Test
    fun `a NON-owner holding NOTHING still never mints nor broadcasts`() = runTest {
        val ports = FakePorts(account)
        val g = "2f".repeat(32)
        // Rank still gates the MINT: two independent mints of 32 RANDOM
        // bytes would diverge the group's push_gid and break routing for
        // everyone. Opening the retry to any holder must not leak into this.
        repeat(3) { GroupPushRouting.ensure(account, g, isOwner = false, ports = ports) }

        assertEquals("never mints", 0, ports.mintCount)
        assertEquals("never broadcasts — there is nothing to re-send", 0, ports.broadcastCount)
        assertTrue("nothing persisted", ports.secrets.isEmpty())
        assertEquals("deferred — no subscribe under group_id", 0, ports.subscribeCount)
    }

    /** [PushRoutingPorts] whose `markPending` throws (an unwritable
     *  keystore), delegating everything else. */
    private class FlakyMarkPending(inner: FakePorts) : PushRoutingPorts by inner {
        override fun markPending(groupIdHex: String): Unit =
            throw IllegalStateException("keystore unavailable")
    }

    @Test
    fun `a throwing markPending does not mask the broadcast failure`() = runTest {
        val inner = FakePorts(account)
        val g = "2e".repeat(32)
        inner.secrets[g] = secretA
        inner.distributed[g] = true
        inner.broadcastFails = true

        val result = runCatching {
            GroupPushRouting.redistribute(account, g, FlakyMarkPending(inner))
        }

        // The caller must see the ORIGINAL send failure — re-flagging is a
        // best-effort repair, not a correctness gate.
        assertTrue("the broadcast failure propagates", result.isFailure)
        assertEquals("DS unreachable", result.exceptionOrNull()?.message)
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

        // A non-owner NEVER mints — rank gates the mint, because two
        // independent mints of 32 random bytes diverge the group's push_gid.
        assertEquals(none, decide(isOwner = false, hasSecret = false, distributed = false))
        assertEquals(none, decide(isOwner = false, hasSecret = false, distributed = true))
        // ... but a non-owner HOLDER re-sends a pending row. Re-sending
        // byte-identical bytes needs no authority (the receive gate accepts
        // them from any sender before rank is consulted), and without this
        // an admin's failed `redistribute` is never retried by anyone.
        assertEquals(rebroadcast, decide(isOwner = false, hasSecret = true, distributed = false))
        assertEquals(none, decide(isOwner = false, hasSecret = true, distributed = true))

        // The rank input matters ONLY when nothing is held: once a secret is
        // in hand, owner and non-owner behave identically.
        for (distributed in listOf(false, true)) {
            assertEquals(
                "holding a secret, rank is irrelevant (distributed=$distributed)",
                decide(isOwner = true, hasSecret = true, distributed = distributed),
                decide(isOwner = false, hasSecret = true, distributed = distributed),
            )
        }
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
        GroupPushRouting.receive(g, secret, defaultStamp, peerPorts)

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
        GroupPushRouting.redistribute(account, g, ports)
        assertEquals(0, ports.broadcastCount)

        // Provision (owner mint) then redistribute -> one more broadcast.
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)
        val afterMint = ports.broadcastCount
        GroupPushRouting.redistribute(account, g, ports)
        assertEquals(afterMint + 1, ports.broadcastCount)
    }

    // -----------------------------------------------------------------
    // Fail-closed receive-side pre-filter
    // -----------------------------------------------------------------

    @Test
    fun `looksLikeSecretEnvelope is fail-closed`() {
        // ADR-0044 — the v2 envelope is 70 bytes: the 29-byte domain
        // separator + 1 version byte + the 8-byte big-endian mint_epoch +
        // the 32-byte secret.
        assertEquals(70, GroupPushRouting.ENVELOPE_LEN)
        val ok = ByteArray(70).also { it[0] = 0x6c }
        assertTrue(GroupPushRouting.looksLikeSecretEnvelope(ok))

        // The RETIRED 62-byte v1 envelope no longer clears the pre-filter.
        // The flag day is hard (no dual-accept): a v1 envelope carries no
        // mint_epoch, so it has no position in the recency order, and
        // admitting one would re-open the very F-A replay v2 closes. The
        // refusal is fail-closed to DEFER — a member never falls back to
        // the leaky group_id — so invariant #4 holds across the
        // transition.
        assertFalse(
            "a retired v1 envelope must not pass the pre-filter",
            GroupPushRouting.looksLikeSecretEnvelope(ByteArray(62).also { it[0] = 0x6c }),
        )

        // A plain chat JSON envelope (first byte '{' = 0x7B) is rejected.
        val chat = "{\"v\":1,\"t\":\"hello\"}".toByteArray(Charsets.UTF_8)
        assertFalse(GroupPushRouting.looksLikeSecretEnvelope(chat))

        // Right length, wrong first byte.
        assertFalse(GroupPushRouting.looksLikeSecretEnvelope(ByteArray(70) { 0x7b }))

        // Wrong length, right first byte.
        assertFalse(GroupPushRouting.looksLikeSecretEnvelope(ByteArray(69).also { it[0] = 0x6c }))

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
    //   present + changed            | ADMIN         | ACCEPT (ADR-0043 (b))
    //   any changed                  | plain member  | REJECT
    //   changed, no role table       | anyone        | ACCEPT (legacy/open)
    //   changed, roles unreadable    | anyone        | REJECT (fail closed)
    //
    // Neither admin row is a nicety. SEEDING: ADR-0022 permits owner OR
    // admin to add members and `redistribute` fires after ANY add-member
    // commit, so without it a member added by a non-owner admin would hold
    // no secret forever (the owner only broadcasts at mint time) and never
    // get push. OVERWRITING (ADR-0043 ratified option (b)): ADR-0022 also
    // lets an admin commit the REMOVE, and refusing the overwrite left that
    // admin able to evict a member but unable to retire the evicted
    // member's routing id — the eviction completed with the ex-member's
    // wake stream intact, which is the whole defect ADR-0043 closes.
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

    // -----------------------------------------------------------------
    // ADR-0044 call-shape helpers
    //
    // The ADR-0044 gate takes two more inputs than the ADR-0042 one: the
    // incoming stamp and this device's own epoch (the clamp ceiling).
    // Every pre-0044 test is about AUTHORITY, so these helpers default
    // them to a pair that clears both new steps — a stamp EQUAL to the
    // ceiling (so the clamp passes, and is not off-by-one) and strictly
    // above the epoch a seeded row carries (so the ORDER passes) — which
    // keeps those tests asserting exactly what they always asserted.
    // The ordering and clamp rules get their own dedicated sections
    // below, where the defaults are overridden explicitly.
    // -----------------------------------------------------------------

    /** The stamp a test's incoming envelope carries unless it says
     *  otherwise. Strictly above [PushRoutingSecretStore.LEGACY_MINT_EPOCH],
     *  which is what an un-stamped seeded row reads as. */
    private val defaultStamp: ULong = 9uL

    private suspend fun receiveGated(
        groupIdHex: String,
        secret: ByteArray,
        senderSignatureKey: ByteArray?,
        owner: GroupPushRouting.OwnerLookup,
        ports: PushRoutingPorts,
        mintEpoch: ULong = defaultStamp,
        ownGroupEpoch: ULong? = defaultStamp,
        accountIdB64: String = account,
    ): GroupPushRouting.ReceiveVerdict = GroupPushRouting.receiveGated(
        accountIdB64 = accountIdB64,
        groupIdHex = groupIdHex,
        secret = secret,
        mintEpoch = mintEpoch,
        senderSignatureKey = senderSignatureKey,
        owner = owner,
        ownGroupEpoch = ownGroupEpoch,
        ports = ports,
    )

    /** The pure gate, with the ADR-0044 inputs defaulted as above and the
     *  order supplied by a [FakePorts] instance (never re-derived here —
     *  production routes through the audited Rust FFI). */
    private fun decide(
        sender: ByteArray?,
        owner: GroupPushRouting.OwnerLookup,
        incoming: ByteArray,
        stored: ByteArray?,
        storedEpoch: ULong = PushRoutingSecretStore.LEGACY_MINT_EPOCH,
        storedUnreadable: Boolean = false,
        mintEpoch: ULong = defaultStamp,
        ownGroupEpoch: ULong? = defaultStamp,
    ): GroupPushRouting.ReceiveVerdict = GroupPushRouting.decideAcceptReceivedSecret(
        senderSignatureKey = sender,
        owner = owner,
        incomingSecret = incoming,
        incomingMintEpoch = mintEpoch,
        stored = stored?.let { GroupPushRouting.StoredSecret(it, storedEpoch) },
        storedUnreadable = storedUnreadable,
        ownGroupEpoch = ownGroupEpoch,
        isNewer = FakePorts("order").isNewer,
    )

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

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertTrue("verdict must accept: " + verdict, verdict.accepts)
        assertArrayEquals("rotated to the owner's new secret", secretB, ports.secrets[g])
        assertEquals(1, ports.subscribeCount)
        assertArrayEquals(ports.derivePushGid(secretB), ports.lastSubscribeTarget)
    }

    @Test
    fun `plain MEMBER changing the secret is REJECTED - no persist, no subscribe`() = runTest {
        val ports = FakePorts(account)
        val g = "02".repeat(32)
        ports.secrets[g] = secretA

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertFalse("verdict must refuse: " + verdict, verdict.accepts)
        assertDropped(ports, g, before = secretA)
    }

    @Test
    fun `plain MEMBER seeding a FIRST secret is REJECTED (holder had none)`() = runTest {
        val ports = FakePorts(account)
        val g = "03".repeat(32)

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertFalse("verdict must refuse: " + verdict, verdict.accepts)
        assertDropped(ports, g, before = null)
    }

    @Test
    fun `plain MEMBER relaying the IDENTICAL secret is accepted (redistribute stays idempotent)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "04".repeat(32)
            ports.secrets[g] = secretA

            val verdict = receiveGated(
                groupIdHex = g,
                secret = secretA.copyOf(),
                senderSignatureKey = memberKey,
                owner = knownRoles(),
                ports = ports,
            )

            // Identical bytes rotate nothing, so the relayer's identity is moot.
            assertTrue("verdict must accept: " + verdict, verdict.accepts)
            assertArrayEquals(secretA, ports.secrets[g])
            assertEquals(1, ports.subscribeCount)
        }

    // ---- ADMIN rank: may SEED a hole AND rotate (ADR-0043 option (b)) ----

    @Test
    fun `ADMIN seeding a FIRST secret is ACCEPTED (closes the admin-add delivery gap)`() = runTest {
        val ports = FakePorts(account)
        val g = "0a".repeat(32)
        // Holder has NOTHING — exactly the state of a member an admin just
        // added: the owner already broadcast at mint time, so the ONLY
        // envelope this device will ever see is the admin's `redistribute`.

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = adminKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertTrue("verdict must accept: " + verdict, verdict.accepts)
        assertArrayEquals("admin filled the hole", secretB, ports.secrets[g])
        assertEquals("subscribed under the derived push_gid", 1, ports.subscribeCount)
        assertArrayEquals(ports.derivePushGid(secretB), ports.lastSubscribeTarget)
    }

    @Test
    fun `ADMIN changing an EXISTING secret is ACCEPTED (ADR-0043 ratified option (b))`() = runTest {
        // The rewritten half of the ADR-0042 contract. Under ADR-0042 this
        // was REJECT ("an admin may seed but never overwrite"), which left an
        // admin able to commit a Remove (ADR-0022 permits it) but unable to
        // retire the evicted member's routing id — so the rotation ADR-0043
        // requires was unreachable from the very device that performed the
        // eviction.
        val ports = FakePorts(account)
        val g = "0b".repeat(32)
        ports.secrets[g] = secretA
        ports.routes[hex(ports.derivePushGid(secretA))] = Route(account, g)

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = adminKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertTrue("verdict must accept: " + verdict, verdict.accepts)
        assertArrayEquals("admin's rotation applied", secretB, ports.secrets[g])
        assertArrayEquals(
            "subscribed under the NEW push_gid",
            ports.derivePushGid(secretB),
            ports.lastSubscribeTarget,
        )
        // ...and the receiving side tore down its OWN old row (ADR-0043).
        assertEquals(1, ports.unsubscribeCount)
        assertArrayEquals(ports.derivePushGid(secretA), ports.unsubscribed.single())
        assertNull(
            "old reverse-index row deleted",
            ports.routes[hex(ports.derivePushGid(secretA))],
        )
    }

    @Test
    fun `OWNER outranks an ADMIN-seeded secret (owner wins whatever the arrival order)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "0c".repeat(32)

            // 1. An admin seeds the hole first.
            val seeded = receiveGated(
                groupIdHex = g,
                secret = secretA,
                senderSignatureKey = adminKey,
                owner = knownRoles(),
                ports = ports,
            )
            assertTrue("verdict must accept: " + seeded, seeded.accepts)
            assertArrayEquals(secretA, ports.secrets[g])

            // 2. The owner's own secret lands AFTER it and still wins — owner
            //    acceptance never depends on what is already stored.
            val rotated = receiveGated(
                groupIdHex = g,
                secret = secretB,
                senderSignatureKey = ownerKey,
                owner = knownRoles(),
                ports = ports,
            )
            assertTrue("verdict must accept: " + rotated, rotated.accepts)
            assertArrayEquals("owner overwrote the admin-seeded secret", secretB, ports.secrets[g])
            assertArrayEquals(ports.derivePushGid(secretB), ports.lastSubscribeTarget)
        }

    @Test
    fun `a SECOND admin in the set is recognised (whole set is matched, not just the first)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "0d".repeat(32)

            val verdict = receiveGated(
                groupIdHex = g,
                secret = secretB,
                senderSignatureKey = otherAdminKey,
                owner = knownRoles(),
                ports = ports,
            )

            assertTrue("verdict must accept: " + verdict, verdict.accepts)
            assertArrayEquals(secretB, ports.secrets[g])
        }

    @Test
    fun `an ADMIN relaying the IDENTICAL secret is accepted (idempotent, no rank needed)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "0e".repeat(32)
            ports.secrets[g] = secretA

            val verdict = receiveGated(
                groupIdHex = g,
                secret = secretA.copyOf(),
                senderSignatureKey = adminKey,
                owner = knownRoles(),
                ports = ports,
            )

            assertTrue("verdict must accept: " + verdict, verdict.accepts)
            assertArrayEquals(secretA, ports.secrets[g])
        }

    @Test
    fun `a group with NO role table keeps pre-gate behaviour (accept)`() = runTest {
        val ports = FakePorts(account)
        val g = "05".repeat(32)
        ports.secrets[g] = secretA

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = GroupPushRouting.OwnerLookup.NoRoleTable,
            ports = ports,
        )

        // A legacy / open group has no owner to check against — unchanged
        // behaviour, deliberately not a regression for pre-ADR-0022 groups.
        assertTrue("verdict must accept: " + verdict, verdict.accepts)
        assertArrayEquals(secretB, ports.secrets[g])
    }

    @Test
    fun `an UNREADABLE role table fails CLOSED on a changed secret`() = runTest {
        val ports = FakePorts(account)
        val g = "06".repeat(32)
        ports.secrets[g] = secretA

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = GroupPushRouting.OwnerLookup.Unavailable,
            ports = ports,
        )

        // Even a would-be OWNER is refused: an unreadable roster / role table
        // must never widen who may rotate (invariant #4).
        assertFalse("verdict must refuse: " + verdict, verdict.accepts)
        assertDropped(ports, g, before = secretA)
    }

    @Test
    fun `an UNREADABLE role table still accepts a byte-identical secret`() = runTest {
        val ports = FakePorts(account)
        val g = "07".repeat(32)
        ports.secrets[g] = secretA

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretA.copyOf(),
            senderSignatureKey = null,
            owner = GroupPushRouting.OwnerLookup.Unavailable,
            ports = ports,
        )

        assertTrue("verdict must accept: " + verdict, verdict.accepts)
    }

    @Test
    fun `an UNRESOLVED sender (external or absent leaf) cannot change the secret`() = runTest {
        val ports = FakePorts(account)
        val g = "08".repeat(32)
        ports.secrets[g] = secretA

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            // `null` = external sender / leaf absent from the roster / FFI throw.
            senderSignatureKey = null,
            owner = knownRoles(),
            ports = ports,
        )

        assertFalse("verdict must refuse: " + verdict, verdict.accepts)
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

        override fun getRecord(groupIdHex: String): GroupPushRouting.StoredSecret? {
            reads++
            if (reads <= failures) throw IllegalStateException("keystore unavailable")
            return inner.getRecord(groupIdHex)
        }
    }

    @Test
    fun `a throwing getRecord degrades to strict, never permissive`() = runTest {
        val inner = FakePorts(account)
        val verdict = receiveGated(
            groupIdHex = "09".repeat(32),
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = FlakyStore(Int.MAX_VALUE, inner),
        )

        // Losing the byte-identical fast path can only make the gate stricter.
        assertFalse("verdict must refuse: " + verdict, verdict.accepts)
        assertTrue("nothing persisted", inner.secrets.isEmpty())
    }

    @Test
    fun `a throwing getRecord still lets an ADMIN through (ADR-0043 (b) - stored state is moot)`() =
        runTest {
            val inner = FakePorts(account)
            val g = "0f".repeat(32)
            val verdict = receiveGated(
                groupIdHex = g,
                secret = secretB,
                senderSignatureKey = adminKey,
                owner = knownRoles(),
                ports = FlakyStore(Int.MAX_VALUE, inner),
            )

            // Under ADR-0042 the gate STRIPPED the admin set on a read
            // failure, because `storedSecret = null` would otherwise read as
            // a hole an admin may fill. ADR-0043 option (b) removed the
            // premise — an admin may write whatever is stored — so the
            // narrowing became vestigial, and keeping it would refuse the
            // very admin rotation option (b) exists to permit, on nothing
            // more than a transient store error. The check that still guards
            // WHO may write is the role table, and it still fails closed
            // (see the Unavailable cases).
            assertTrue("verdict must accept: " + verdict, verdict.accepts)
            assertArrayEquals("admin's secret persisted", secretB, inner.secrets[g])
            // Nothing is retired: an unreadable store yields no OLD bytes, so
            // no old gid can be NAMED — the teardown is skipped rather than
            // guessed at.
            assertEquals("no teardown without a readable old secret", 0, inner.unsubscribeCount)
        }

    @Test
    fun `a throwing getRecord still lets the OWNER through (it may rotate anything)`() = runTest {
        val inner = FakePorts(account)
        val g = "10".repeat(32)
        // Only the GATE read fails; the owner may rotate whatever is stored,
        // so an unknown stored state changes nothing for it.
        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = FlakyStore(1, inner),
        )

        assertTrue("verdict must accept: " + verdict, verdict.accepts)
        assertArrayEquals("owner's secret persisted", secretB, inner.secrets[g])
    }

    @Test
    fun `the pure decision function covers the whole matrix`() {
        val known = knownRoles()

        val identical = GroupPushRouting.ReceiveVerdict.ACCEPT_IDENTICAL
        val owner = GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER
        val seed = GroupPushRouting.ReceiveVerdict.ACCEPT_ADMIN_SEED
        val adminRotate = GroupPushRouting.ReceiveVerdict.ACCEPT_ADMIN_ROTATE
        val noOwner = GroupPushRouting.ReceiveVerdict.ACCEPT_NO_OWNER
        val notAuthorized = GroupPushRouting.ReceiveVerdict.REJECT_NOT_AUTHORIZED
        val unverifiable = GroupPushRouting.ReceiveVerdict.REJECT_UNVERIFIABLE

        // --- OWNER: may seed AND may rotate, whatever is stored ---
        assertEquals(owner, decide(ownerKey, known, secretB, secretA))
        assertEquals(owner, decide(ownerKey, known, secretA, null))

        // --- ADMIN: may SEED a hole AND rotate (ADR-0043 option (b)).
        //     The two are reported SEPARATELY — a provable hole is a seed,
        //     anything else is a rotate — but both accept.
        assertEquals(seed, decide(adminKey, known, secretA, null))
        assertEquals(seed, decide(otherAdminKey, known, secretA, null))
        assertEquals(adminRotate, decide(adminKey, known, secretB, secretA))
        assertEquals(adminRotate, decide(otherAdminKey, known, secretB, secretA))
        // An UNREADABLE store makes an admin write a ROTATE, not a seed:
        // `stored == null` then means "unknown", not "provably absent".
        assertEquals(
            adminRotate,
            decide(adminKey, known, secretA, null, storedUnreadable = true),
        )

        // --- plain MEMBER: may never change it, seed or overwrite ---
        assertEquals(notAuthorized, decide(memberKey, known, secretB, secretA))
        assertEquals(notAuthorized, decide(memberKey, known, secretA, null))

        // --- identical always accepted, whoever relayed it (needs no rank) ---
        assertEquals(identical, decide(memberKey, known, secretA, secretA.copyOf()))
        assertEquals(identical, decide(adminKey, known, secretA, secretA.copyOf()))
        assertEquals(identical, decide(null, known, secretA, secretA.copyOf()))

        // --- no role table -> open; unreadable -> fail closed for EVERY rank ---
        val open = GroupPushRouting.OwnerLookup.NoRoleTable
        val unreadable = GroupPushRouting.OwnerLookup.Unavailable
        assertEquals(noOwner, decide(memberKey, open, secretB, secretA))
        assertEquals(noOwner, decide(memberKey, open, secretA, null))
        assertEquals(unverifiable, decide(ownerKey, unreadable, secretB, secretA))
        assertEquals(unverifiable, decide(adminKey, unreadable, secretA, null))

        // --- an unresolved sender matches neither owner nor any admin ---
        assertEquals(unverifiable, decide(null, known, secretB, secretA))
        assertEquals(unverifiable, decide(null, known, secretA, null))

        // --- near-miss keys (one byte off) match neither rank ---
        val nearOwner = ownerKey.copyOf().also { it[31] = 0x12 }
        val nearAdmin = adminKey.copyOf().also { it[31] = 0x34 }
        assertEquals(notAuthorized, decide(nearOwner, known, secretB, secretA))
        assertEquals(notAuthorized, decide(nearAdmin, known, secretA, null))

        // --- an EMPTY admin set degenerates to the owner-only gate ---
        val ownerOnly = GroupPushRouting.OwnerLookup.Known(ownerKey)
        assertEquals(owner, decide(ownerKey, ownerOnly, secretB, secretA))
        assertEquals(notAuthorized, decide(adminKey, ownerOnly, secretA, null))
    }

    @Test
    fun `the accepts partition is exhaustive and matches the verdict names`() {
        // Nothing may sit outside the accept/refuse partition, and the
        // naming must not drift from it — the orchestration branches on
        // `.accepts` alone.
        for (verdict in GroupPushRouting.ReceiveVerdict.entries) {
            assertEquals(
                "$verdict: `accepts` must agree with the name",
                verdict.name.startsWith("ACCEPT"),
                verdict.accepts,
            )
        }
        assertEquals(
            "five accepts",
            5,
            GroupPushRouting.ReceiveVerdict.entries.count { it.accepts },
        )
        assertEquals(
            "four refusals",
            4,
            GroupPushRouting.ReceiveVerdict.entries.count { !it.accepts },
        )
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
    // ADR-0043 — rotate the routing secret when a member is REMOVED
    //
    // The defect: a removed member keeps the group's push_routing_secret
    // on disk, so it can still derive the current push_gid — and since
    // per-group unsubscribe was dead code on every client, push-gw kept
    // waking that device for a group it was evicted from. It learns "this
    // group was active at time T" on every send, indefinitely. No remaining
    // member can stop it (unsubscribe deletes only the caller's OWN
    // (device, group) row) and the ex-member's client has no removal
    // handling, so RE-KEYING is the only mechanism the group has.
    //
    // Rotation and unsubscribe are inseparable: rotating WITHOUT the
    // per-member teardown leaves one dead row per remaining device per
    // rotation, turning a live cluster into a versioned archive of
    // clusters — strictly worse than not rotating at all.
    // -----------------------------------------------------------------

    /** Distinct member identities for the diff (hex signature keys). */
    private val mA = "aa".repeat(32)
    private val mB = "bb".repeat(32)
    private val mC = "cc".repeat(32)
    private val mD = "dd".repeat(32)

    private fun decideShrink(
        before: List<String>?,
        after: List<String>?,
        role: GroupPushRouting.RotationRole,
    ) = GroupPushRouting.membershipRotationDecision(before, after, role)

    @Test
    fun `a SHRINK rotates for the OWNER and for an ADMIN, never for a plain member`() {
        val before = listOf(mA, mB, mC)
        val after = listOf(mA, mB)

        assertEquals(
            GroupPushRouting.RotationDecision.ROTATE,
            decideShrink(before, after, GroupPushRouting.RotationRole.OWNER),
        )
        // ADR-0043 ratified option (b): ADR-0022 lets an ADMIN commit the
        // Remove, so an admin that cannot rotate can evict a member without
        // being able to retire that member's routing id.
        assertEquals(
            GroupPushRouting.RotationDecision.ROTATE,
            decideShrink(before, after, GroupPushRouting.RotationRole.ADMIN),
        )
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_NOT_AUTHORIZED,
            decideShrink(before, after, GroupPushRouting.RotationRole.MEMBER),
        )
    }

    @Test
    fun `a SHRINK does NOT rotate in a group with no role table`() {
        // No role table means no owner, so nobody ever minted a secret for
        // this group — there is nothing to rotate, and letting every member
        // rotate would have them each mint DIVERGENT secrets.
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_NOT_AUTHORIZED,
            decideShrink(listOf(mA, mB), listOf(mA), GroupPushRouting.RotationRole.NONE),
        )
    }

    @Test
    fun `GROWTH never rotates - the routing id stays stable across an add`() {
        // An added member is SUPPOSED to route; `redistribute` already hands
        // it the current secret. Rotating on add would turn that into a
        // CHANGED secret against every existing member for zero gain.
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_NO_SHRINK,
            decideShrink(listOf(mA, mB), listOf(mA, mB, mC), GroupPushRouting.RotationRole.OWNER),
        )
    }

    @Test
    fun `an UNCHANGED roster never rotates (an ordinary commit or a bare poll)`() {
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_NO_SHRINK,
            decideShrink(listOf(mA, mB), listOf(mA, mB), GroupPushRouting.RotationRole.OWNER),
        )
        // Order is irrelevant — it is a SET comparison.
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_NO_SHRINK,
            decideShrink(listOf(mA, mB), listOf(mB, mA), GroupPushRouting.RotationRole.OWNER),
        )
    }

    @Test
    fun `a remove plus an add in ONE batch still rotates (the count is equal, the SET is not)`() {
        // The reason the diff is over signature KEYS and not counts or leaf
        // indices: OpenMLS reuses a vacated leaf for the next Add, so this
        // batch leaves BOTH the member count and the leaf-index set
        // unchanged while a member genuinely was evicted — and that member
        // must still lose its routing id.
        assertEquals(
            GroupPushRouting.RotationDecision.ROTATE,
            decideShrink(
                listOf(mA, mB, mC),
                listOf(mA, mB, mD),
                GroupPushRouting.RotationRole.OWNER,
            ),
        )
    }

    @Test
    fun `an UNKNOWN snapshot never rotates (a torn members() read is not 'everyone left')`() {
        val known = listOf(mA, mB)
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_UNKNOWN_MEMBERSHIP,
            decideShrink(null, known, GroupPushRouting.RotationRole.OWNER),
        )
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_UNKNOWN_MEMBERSHIP,
            decideShrink(known, null, GroupPushRouting.RotationRole.OWNER),
        )
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_UNKNOWN_MEMBERSHIP,
            decideShrink(null, null, GroupPushRouting.RotationRole.OWNER),
        )
    }

    @Test
    fun `a FIRST observation (empty baseline) never rotates`() {
        // An empty baseline against a real roster is a set difference of
        // zero, but it is also indistinguishable from "we have never looked"
        // — so it is classified UNKNOWN rather than allowed to fall through.
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_UNKNOWN_MEMBERSHIP,
            decideShrink(emptyList(), listOf(mA, mB), GroupPushRouting.RotationRole.OWNER),
        )
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_UNKNOWN_MEMBERSHIP,
            decideShrink(emptyList(), emptyList(), GroupPushRouting.RotationRole.OWNER),
        )
    }

    // ---- the rotation itself ----

    /** Seeds [ports] with a held, already-distributed secret + its
     *  reverse-index row — the steady state a rotation starts from. */
    private fun seedRouting(ports: FakePorts, groupHex: String, secret: ByteArray) {
        ports.secrets[groupHex] = secret
        ports.distributed[groupHex] = true
        ports.routes[hex(ports.derivePushGid(secret))] = Route(account, groupHex)
    }

    @Test
    fun `the OWNER re-keys - new secret broadcast, NEW gid subscribed, OLD gid unsubscribed`() =
        runTest {
            val ports = FakePorts(account)
            val g = "30".repeat(32)
            seedRouting(ports, g, secretA)
            val oldGid = ports.derivePushGid(secretA)

            val outcome = GroupPushRouting.rotate(
                accountIdB64 = account,
                groupIdHex = g,
                ownRole = GroupPushRouting.RotationRole.OWNER,
                ports = ports,
            )

            assertEquals(GroupPushRouting.RotationOutcome.ROTATED, outcome)
            // A NEW secret replaced the old one...
            assertEquals(1, ports.mintCount)
            val next = ports.secrets[g]!!
            assertFalse("the secret actually changed", next.contentEquals(secretA))
            // ...was redistributed at the POST-removal epoch...
            assertEquals(1, ports.broadcastCount)
            assertArrayEquals(next, secretOf(ports.broadcastEnvelopes.single()))
            assertTrue("broadcast confirmed", ports.isDistributed(g))
            // ...this device re-armed under the NEW id...
            val newGid = ports.derivePushGid(next)
            assertArrayEquals(newGid, ports.lastSubscribeTarget)
            assertEquals(Route(account, g), ports.routes[hex(newGid)])
            // ...and the OLD id was retired: subscription dropped + row gone.
            assertEquals(1, ports.unsubscribeCount)
            assertArrayEquals(oldGid, ports.unsubscribed.single())
            assertNull("stale reverse-index row deleted", ports.routes[hex(oldGid)])
            assertEquals("exactly one live route row", 1, ports.routes.size)
        }

    @Test
    fun `an ADMIN may rotate too (ADR-0043 ratified option (b))`() = runTest {
        val ports = FakePorts(account)
        val g = "31".repeat(32)
        seedRouting(ports, g, secretA)

        val outcome = GroupPushRouting.rotate(
            account,
            g,
            GroupPushRouting.RotationRole.ADMIN,
            ports,
        )

        assertEquals(GroupPushRouting.RotationOutcome.ROTATED, outcome)
        assertEquals(1, ports.broadcastCount)
        assertEquals(1, ports.unsubscribeCount)
    }

    @Test
    fun `a PLAIN MEMBER never rotates - nothing minted, nothing sent, nothing torn down`() =
        runTest {
            for (role in listOf(
                GroupPushRouting.RotationRole.MEMBER,
                GroupPushRouting.RotationRole.NONE,
            )) {
                val ports = FakePorts(account)
                val g = "32".repeat(32)
                seedRouting(ports, g, secretA)

                val outcome = GroupPushRouting.rotate(account, g, role, ports)

                assertEquals(GroupPushRouting.RotationOutcome.SKIP_NOT_AUTHORIZED, outcome)
                assertEquals("no mint", 0, ports.mintCount)
                assertEquals("no broadcast", 0, ports.broadcastCount)
                assertEquals("no teardown", 0, ports.unsubscribeCount)
                assertArrayEquals("secret untouched", secretA, ports.secrets[g])
            }
        }

    @Test
    fun `rotation NEVER mints a first secret (minting stays owner-only in ensure)`() = runTest {
        // Holding nothing there is nothing to ROTATE. If rotation minted
        // here, an ADMIN — which ADR-0043 (b) lets rotate — would become a
        // SECOND minter, and two independent mints of 32 random bytes
        // diverge the group's push_gid and break the notify JOIN outright.
        val ports = FakePorts(account)
        val g = "33".repeat(32)

        val outcome = GroupPushRouting.rotate(
            account,
            g,
            GroupPushRouting.RotationRole.ADMIN,
            ports,
        )

        assertEquals(GroupPushRouting.RotationOutcome.SKIP_NO_SECRET, outcome)
        assertEquals(0, ports.mintCount)
        assertEquals(0, ports.broadcastCount)
        assertTrue("nothing persisted", ports.secrets.isEmpty())
        assertEquals("no teardown", 0, ports.unsubscribeCount)
    }

    @Test
    fun `an UNREADABLE secret row is not rotated (the old id could never be named)`() = runTest {
        val inner = FakePorts(account)
        val g = "34".repeat(32)
        seedRouting(inner, g, secretA)

        val outcome = GroupPushRouting.rotate(
            account,
            g,
            GroupPushRouting.RotationRole.OWNER,
            FlakyStore(Int.MAX_VALUE, inner),
        )

        // Rotating a row whose OLD bytes cannot be read would strand the old
        // gid subscribed forever — the exact defect being fixed.
        assertEquals(GroupPushRouting.RotationOutcome.SKIP_NO_SECRET, outcome)
        assertEquals(0, inner.mintCount)
        assertEquals(0, inner.unsubscribeCount)
    }

    @Test
    fun `the OLD secret is read BEFORE the overwrite - the teardown targets the OLD gid`() =
        runTest {
            // Ordering (b) of ADR-0043. The secret row is keyed
            // (account, group) and is CLOBBERED in place, so persisting
            // first makes the retiring id unnameable: the unsubscribe would
            // target the id just armed, leaving the superseded one live and
            // undeletable — an orphan that keeps receiving wakes.
            val ports = FakePorts(account)
            val g = "35".repeat(32)
            seedRouting(ports, g, secretA)
            val oldGid = ports.derivePushGid(secretA)

            GroupPushRouting.rotate(account, g, GroupPushRouting.RotationRole.OWNER, ports)

            val newGid = ports.derivePushGid(ports.secrets[g]!!)
            assertArrayEquals("unsubscribed the OLD gid", oldGid, ports.unsubscribed.single())
            assertFalse(
                "must NOT unsubscribe the id just armed",
                newGid.contentEquals(ports.unsubscribed.single()),
            )
            // The route row that survives is the NEW one, not the old one.
            assertNull(ports.routes[hex(oldGid)])
            assertEquals(Route(account, g), ports.routes[hex(newGid)])
        }

    @Test
    fun `a FAILED rotation broadcast leaves the row PENDING and the retry re-sends the NEW secret`() =
        runTest {
            // The crash window ADR-0043 calls materially worse than at mint
            // time: the rotator is alone on the new id, everyone else is on
            // the old one, and nobody notifies either — the WHOLE group
            // loses push. The existing `distributed` flag is the whole
            // durability mechanism, reused verbatim.
            val ports = FakePorts(account)
            val g = "36".repeat(32)
            seedRouting(ports, g, secretA)
            ports.broadcastFails = true

            val outcome = GroupPushRouting.rotate(
                account,
                g,
                GroupPushRouting.RotationRole.OWNER,
                ports,
            )

            // The LOCAL switch is durable and complete regardless...
            assertEquals(GroupPushRouting.RotationOutcome.ROTATED, outcome)
            val next = ports.secrets[g]!!
            assertEquals(1, ports.unsubscribeCount)
            // ...but the send is recorded as NOT landed.
            assertFalse("row left pending", ports.isDistributed(g))
            assertEquals(0, ports.broadcastCount)

            // The existing re-broadcast branch — open to ANY holder — re-sends
            // the STORED (i.e. the NEW) secret, never a re-mint.
            ports.broadcastFails = false
            GroupPushRouting.ensure(account, g, isOwner = false, ports = ports)

            assertEquals("no second mint", 1, ports.mintCount)
            assertEquals(1, ports.broadcastCount)
            assertArrayEquals(next, secretOf(ports.broadcastEnvelopes.single()))
            assertTrue("now confirmed", ports.isDistributed(g))
        }

    @Test
    fun `a FAILING unsubscribe does not unwind the rotation (the new id is already armed)`() =
        runTest {
            val ports = FakePorts(account)
            val g = "37".repeat(32)
            seedRouting(ports, g, secretA)
            ports.unsubscribeFails = true

            val outcome = GroupPushRouting.rotate(
                account,
                g,
                GroupPushRouting.RotationRole.OWNER,
                ports,
            )

            assertEquals(GroupPushRouting.RotationOutcome.ROTATED, outcome)
            assertEquals("it was attempted", 1, ports.unsubscribeCount)
            // The route row is still dropped — the cost of a failed
            // unsubscribe is a stale push-gw row nothing notifies again, not
            // a broken group.
            assertNull(ports.routes[hex(ports.derivePushGid(secretA))])
        }

    /** Mints a DISTINCT secret per call, so two rotations are
     *  distinguishable. */
    private class DistinctMintPorts(accountIdB64: String) : FakePorts(accountIdB64) {
        val broadcastGate = CompletableDeferred<Unit>()

        override fun mintSecret(): ByteArray {
            mintCount++
            return ByteArray(32) { (0x40 + mintCount).toByte() }
        }

        override suspend fun broadcast(envelope: ByteArray) {
            broadcastGate.await()
            super.broadcast(envelope)
        }
    }

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `the per-group lock SERIALISES concurrent rotations (never two mints off one old secret)`() =
        runTest {
            // The damaging interleave: both triggers (this device's own
            // remove + a concurrent poll-loop diff) read the SAME old secret,
            // both mint, the second persist wins — leaving the first new
            // push_gid subscribed and indexed but never notified again, i.e.
            // exactly the orphan this ADR exists to prevent.
            val ports = DistinctMintPorts(account)
            val g = "38".repeat(32)
            seedRouting(ports, g, secretA)
            val oldGid = ports.derivePushGid(secretA)

            val owner = GroupPushRouting.RotationRole.OWNER
            launch { GroupPushRouting.rotate(account, g, owner, ports) }
            launch { GroupPushRouting.rotate(account, g, owner, ports) }
            runCurrent()

            // The first rotation is parked inside its broadcast. Without the
            // lock the second would ALREADY have minted off the same old
            // secret; behind it, it has not started.
            assertEquals("second rotation is behind the lock", 1, ports.mintCount)

            ports.broadcastGate.complete(Unit)
            advanceUntilIdle()

            // Serialised, so the second rotation's "old" secret is the FIRST
            // one's output — every intermediate id is retired, none stranded.
            assertEquals(2, ports.mintCount)
            assertEquals(2, ports.unsubscribeCount)
            val finalGid = ports.derivePushGid(ports.secrets[g]!!)
            assertEquals("exactly one live route row", 1, ports.routes.size)
            assertEquals(Route(account, g), ports.routes[hex(finalGid)])
            assertNull("the original id was retired", ports.routes[hex(oldGid)])
        }

    // -----------------------------------------------------------------
    // The routing row is held across network I/O, but BOUNDED
    //
    // Spanning the whole read-decide-write with the lock is correct — it is
    // what closes the ADR-0044 HIGH — but while the network calls under it
    // were UNBOUNDED, one hung request wedged the row permanently: every
    // later envelope queued behind it forever, so this device never applied
    // a peer's rotation and kept notifying, and stayed subscribed to, a
    // routing id an evicted member can still derive. An ADR-0043 guarantee
    // lost to a stalled socket, with no error raised anywhere.
    //
    // These run on `runTest`'s VIRTUAL clock, so `advanceUntilIdle` crosses
    // the production 15 s deadline instantly — which is why the Android
    // bound can stay a plain constant while the iOS mirror has to inject a
    // short one (XCTest has no virtual clock).
    // -----------------------------------------------------------------

    /** A DS that accepts the envelope and then never answers. The hang is a
     *  `CompletableDeferred.await()` — cancellable, exactly like a stalled
     *  Ktor/OkHttp call, which is what [GroupPushRouting.bounded]'s
     *  `withTimeoutOrNull` relies on. */
    private class HungBroadcastPorts(accountIdB64: String) : FakePorts(accountIdB64) {
        private val neverAnswers = CompletableDeferred<Unit>()

        /** How many broadcasts have PARKED (recorded before hanging, so a
         *  test can tell "never attempted" from "attempted and stuck"). */
        var broadcastsEntered = 0

        /** A distinct secret per mint, so a rotation is distinguishable from
         *  the seeded row. */
        override fun mintSecret(): ByteArray {
            mintCount++
            return ByteArray(32) { (0x40 + mintCount).toByte() }
        }

        override suspend fun broadcast(envelope: ByteArray) {
            broadcastsEntered++
            broadcastEnvelopes.add(envelope)
            neverAnswers.await()
        }
    }

    /** A push-gw that accepts the subscribe and then never answers. */
    private class HungSubscribePorts(accountIdB64: String) : FakePorts(accountIdB64) {
        private val neverAnswers = CompletableDeferred<Unit>()

        override fun mintSecret(): ByteArray {
            mintCount++
            return ByteArray(32) { (0x40 + mintCount).toByte() }
        }

        override suspend fun subscribeUnderPushGid(pushGid: ByteArray) {
            super.subscribeUnderPushGid(pushGid)
            neverAnswers.await()
        }
    }

    /**
     * **The finding.** The counterpart of
     * `an inbound secret cannot be applied mid-rotation`: there the rotation
     * eventually completes and the queued envelope must WAIT its turn; here
     * the broadcast never completes and it must still GET a turn.
     */
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `a HUNG broadcast does not wedge the row - a later envelope still applies`() = runTest {
        val ports = HungBroadcastPorts(account)
        val g = "77".repeat(32)
        seedRouting(ports, g, secretA)
        // The rotation stamps 2; the peer envelope 3, so it strictly
        // out-ranks rather than tying and falling to a byte compare.
        ports.groupEpoch = 2uL

        launch {
            GroupPushRouting.rotate(account, g, GroupPushRouting.RotationRole.OWNER, ports)
        }
        runCurrent()
        assertEquals("the rotation is parked in its broadcast", 1, ports.broadcastsEntered)

        val peer = ByteArray(32) { 0xEE.toByte() }
        var verdict: GroupPushRouting.ReceiveVerdict? = null
        launch {
            verdict = receiveGated(
                g, peer, ownerKey, knownRoles(), ports,
                mintEpoch = 3uL, ownGroupEpoch = 3uL,
            )
        }
        runCurrent()
        // Serializing, not coalescing — still true. Bounding the I/O did not
        // let the queued envelope skip ahead of the in-flight rotation.
        assertNull("the envelope waits while the broadcast is still in flight", verdict)

        // Virtual time crosses the deadline, which is the ONLY thing that
        // frees the row here. Unbounded, this advances nothing and the
        // assertions below fail.
        advanceUntilIdle()

        assertEquals(
            "the queued envelope applies once the deadline frees the row",
            GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER,
            verdict,
        )
        assertArrayEquals(peer, ports.secrets[g])
        // The receive read the rotation's OUTPUT as its stored value, so it
        // retired the id the rotation had armed — nothing stranded.
        assertEquals(2, ports.unsubscribeCount)
    }

    /**
     * The deadline fails CLOSED **for correctness**: a broadcast that merely
     * stopped answering is treated as FAILED, never as optimistically
     * delivered — so the row is left `pending`, which is the state
     * [GroupPushRouting.EnsureAction.REBROADCAST] repairs.
     *
     * The rest of the section still runs: the deadline aborts ONE call, not
     * the whole locked body, which is why no half-applied shape exists.
     */
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `a TIMED-OUT broadcast leaves the row PENDING for the retry`() = runTest {
        val ports = HungBroadcastPorts(account)
        val g = "78".repeat(32)
        seedRouting(ports, g, secretA)
        ports.groupEpoch = 2uL
        val oldGid = ports.derivePushGid(secretA)

        var outcome: GroupPushRouting.RotationOutcome? = null
        launch {
            outcome = GroupPushRouting.rotate(
                account, g, GroupPushRouting.RotationRole.OWNER, ports,
            )
        }
        advanceUntilIdle()

        assertEquals(
            "the rotation returns despite a broadcast that never does",
            GroupPushRouting.RotationOutcome.ROTATED,
            outcome,
        )
        assertFalse(
            "a broadcast that timed out must NEVER be recorded as delivered — " +
                "that would disarm the only branch that re-sends it",
            ports.distributed[g]!!,
        )
        assertFalse("the local re-key is durable", ports.secrets[g]!!.contentEquals(secretA))
        // Steps (6)-(8) still ran: the new id armed, the old one retired.
        assertEquals(1, ports.subscribeCount)
        assertArrayEquals(ports.derivePushGid(ports.secrets[g]!!), ports.lastSubscribeTarget)
        assertArrayEquals(oldGid, ports.unsubscribed.single())
    }

    /**
     * A `freshMintStamp` that never answers.
     *
     * NOT network I/O — it reads `Group.epoch()`. It is bounded because in
     * production it hops through `ChatRepository.groupsLock`, and that lock
     * IS held across unbounded DS publishes (`publishGroup` on the
     * role-change / add-member / remove-member commits, `publishMailbox` on
     * the invite paths). So a hung DS publish wedges the routing row through
     * it, one hop removed. This fake stands in for that whole chain.
     */
    private class HungStampPorts(accountIdB64: String) : FakePorts(accountIdB64) {
        private val neverAnswers = CompletableDeferred<Unit>()

        /** How many stamp reads have PARKED. */
        var stampsEntered = 0

        override fun mintSecret(): ByteArray {
            mintCount++
            return ByteArray(32) { (0x40 + mintCount).toByte() }
        }

        override suspend fun freshMintStamp(): GroupPushRouting.MintStamp.Fresh? {
            stampsEntered++
            neverAnswers.await()
            return super.freshMintStamp()
        }
    }

    /**
     * The stamp read is the fourth awaited call under the lock, and the only
     * one whose hazard is EXTERNAL to this file (see [HungStampPorts]).
     *
     * It fails closed into the SHAPE THAT ALREADY EXISTS for an unreadable
     * epoch — [GroupPushRouting.RotationOutcome.SKIP_UNKNOWN_EPOCH] — so
     * nothing is minted, nothing is sent, and the held secret is left intact
     * for the next shrink or the next authorised envelope to repair.
     */
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `a HUNG mint-stamp read does not wedge the row - it fails closed`() = runTest {
        val ports = HungStampPorts(account)
        val g = "7a".repeat(32)
        seedRouting(ports, g, secretA)
        ports.groupEpoch = 2uL

        var outcome: GroupPushRouting.RotationOutcome? = null
        launch {
            outcome = GroupPushRouting.rotate(
                account, g, GroupPushRouting.RotationRole.OWNER, ports,
            )
        }
        runCurrent()
        assertEquals("the rotation is parked reading its mint stamp", 1, ports.stampsEntered)

        val peer = ByteArray(32) { 0xEE.toByte() }
        var verdict: GroupPushRouting.ReceiveVerdict? = null
        launch {
            verdict = receiveGated(
                g, peer, ownerKey, knownRoles(), ports,
                mintEpoch = 3uL, ownGroupEpoch = 3uL,
            )
        }
        runCurrent()
        assertNull("the envelope waits while the stamp read is still in flight", verdict)

        advanceUntilIdle()

        assertEquals(
            "an unanswered stamp read fails CLOSED into the existing shape",
            GroupPushRouting.RotationOutcome.SKIP_UNKNOWN_EPOCH,
            outcome,
        )
        // Fail-closed means the rotation left the row exactly as it found it.
        assertEquals("no mint without a stamp", 0, ports.mintCount)
        assertEquals("nothing broadcast without a stamp", 0, ports.broadcastCount)
        // Which the queued envelope then proves from the other side: its
        // "old" secret was still `secretA`, so that is the id it retired.
        assertEquals(
            "the queued envelope applies once the deadline frees the row",
            GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER,
            verdict,
        )
        assertArrayEquals(peer, ports.secrets[g])
        assertArrayEquals(ports.derivePushGid(secretA), ports.unsubscribed.single())
    }

    /** [GroupPushRouting.ensure] awaits the same stamp read under the same
     *  lock, so it carries the same wedge and the same bound. Here the owner
     *  holds nothing, so it is the MINT branch that parks. */
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `a HUNG mint-stamp read does not wedge ensure's row either`() = runTest {
        val ports = HungStampPorts(account)
        val g = "7b".repeat(32)

        launch { GroupPushRouting.ensure(account, g, isOwner = true, ports) }
        runCurrent()
        assertEquals("the mint is parked reading its stamp", 1, ports.stampsEntered)

        val peer = ByteArray(32) { 0xEE.toByte() }
        var verdict: GroupPushRouting.ReceiveVerdict? = null
        launch {
            verdict = receiveGated(
                g, peer, ownerKey, knownRoles(), ports,
                mintEpoch = 3uL, ownGroupEpoch = 3uL,
            )
        }
        runCurrent()
        assertNull("the envelope waits while the stamp read is still in flight", verdict)

        advanceUntilIdle()

        assertEquals("no mint without a stamp", 0, ports.mintCount)
        assertEquals("nothing broadcast without a stamp", 0, ports.broadcastCount)
        assertEquals(
            "the queued envelope applies once the deadline frees the row",
            GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER,
            verdict,
        )
        assertArrayEquals(peer, ports.secrets[g])
    }

    /**
     * The push-gw subscribe is the OTHER seam taken under the lock, and it
     * is bounded for the same reason — one bounded call does not make the
     * next one bounded. Here the broadcast is fast and the SUBSCRIBE is what
     * hangs, mid-rotation, with a peer envelope queued behind it.
     */
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `a HUNG subscribe does not wedge the row either`() = runTest {
        val ports = HungSubscribePorts(account)
        val g = "79".repeat(32)
        seedRouting(ports, g, secretA)
        ports.groupEpoch = 2uL

        launch {
            GroupPushRouting.rotate(account, g, GroupPushRouting.RotationRole.OWNER, ports)
        }
        runCurrent()
        assertEquals("the rotation is parked in its subscribe", 1, ports.subscribeCount)

        val peer = ByteArray(32) { 0xEE.toByte() }
        var verdict: GroupPushRouting.ReceiveVerdict? = null
        launch {
            verdict = receiveGated(
                g, peer, ownerKey, knownRoles(), ports,
                mintEpoch = 3uL, ownGroupEpoch = 3uL,
            )
        }
        runCurrent()
        assertNull("the envelope waits while the subscribe is still in flight", verdict)

        advanceUntilIdle()

        assertEquals(
            "the queued envelope applies once the deadline frees the row",
            GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER,
            verdict,
        )
        assertArrayEquals(peer, ports.secrets[g])
        // The receive's OWN subscribe hangs too and is bounded the same way,
        // so it applied the envelope and returned regardless.
        assertEquals(2, ports.subscribeCount)
    }

    // ---- the membership-shrink entry point ----

    @Test
    fun `an observed SHRINK rotates for an owner and an admin, not for a plain member`() = runTest {
        for (role in listOf(
            GroupPushRouting.RotationRole.OWNER,
            GroupPushRouting.RotationRole.ADMIN,
        )) {
            val ports = FakePorts(account)
            val g = "39".repeat(32)
            seedRouting(ports, g, secretA)

            val outcome = GroupPushRouting.rotateIfMembershipShrank(
                accountIdB64 = account,
                groupIdHex = g,
                ownRole = role,
                previousMemberKeys = listOf(mA, mB, mC),
                currentMemberKeys = listOf(mA, mB),
                ports = ports,
            )

            assertEquals(GroupPushRouting.RotationOutcome.ROTATED, outcome)
            assertEquals(1, ports.mintCount)
            assertEquals(1, ports.unsubscribeCount)
        }

        val ports = FakePorts(account)
        val g = "3a".repeat(32)
        seedRouting(ports, g, secretA)
        val outcome = GroupPushRouting.rotateIfMembershipShrank(
            accountIdB64 = account,
            groupIdHex = g,
            ownRole = GroupPushRouting.RotationRole.MEMBER,
            previousMemberKeys = listOf(mA, mB, mC),
            currentMemberKeys = listOf(mA, mB),
            ports = ports,
        )
        assertEquals(GroupPushRouting.RotationOutcome.SKIP_NOT_AUTHORIZED, outcome)
        assertEquals(0, ports.mintCount)
    }

    @Test
    fun `an ADD, an unchanged roster, and a torn snapshot never rotate on the wire path`() =
        runTest {
            var n = 0
            suspend fun assertNoRotation(
                label: String,
                expected: GroupPushRouting.RotationOutcome,
                before: List<String>?,
                after: List<String>?,
            ) {
                n += 1
                val ports = FakePorts(account)
                val g = "%02x".format(0x40 + n).repeat(32)
                seedRouting(ports, g, secretA)
                val outcome = GroupPushRouting.rotateIfMembershipShrank(
                    accountIdB64 = account,
                    groupIdHex = g,
                    ownRole = GroupPushRouting.RotationRole.OWNER,
                    previousMemberKeys = before,
                    currentMemberKeys = after,
                    ports = ports,
                )
                assertEquals(label, expected, outcome)
                assertEquals("$label: no mint", 0, ports.mintCount)
                assertEquals("$label: no broadcast", 0, ports.broadcastCount)
                assertEquals("$label: no teardown", 0, ports.unsubscribeCount)
                assertArrayEquals("$label: secret untouched", secretA, ports.secrets[g])
            }

            assertNoRotation(
                "an add",
                GroupPushRouting.RotationOutcome.SKIP_NO_SHRINK,
                listOf(mA, mB),
                listOf(mA, mB, mC),
            )
            assertNoRotation(
                "an unchanged roster",
                GroupPushRouting.RotationOutcome.SKIP_NO_SHRINK,
                listOf(mA, mB),
                listOf(mA, mB),
            )
            assertNoRotation(
                "a torn post-poll read",
                GroupPushRouting.RotationOutcome.SKIP_UNKNOWN_MEMBERSHIP,
                listOf(mA, mB),
                null,
            )
            assertNoRotation(
                "a torn baseline",
                GroupPushRouting.RotationOutcome.SKIP_UNKNOWN_MEMBERSHIP,
                null,
                listOf(mA, mB),
            )
        }

    // ---- the RECEIVE-side teardown (every member retires its own row) ----

    @Test
    fun `a NON-rotator applying a CHANGED secret retires its OWN old gid`() = runTest {
        // This is what keeps rotation from being a NET LOSS. `group_unsubscribe`
        // deletes exactly the (device, group) row named in the request and a
        // device knows only its own pseudonym, so the rotator cannot tear
        // down anyone else's. Without this every member would leave a live
        // row under the old id — one dead subscription per member per
        // rotation, "a live cluster plus an archive of every past cluster".
        val ports = FakePorts(account)
        val g = "3c".repeat(32)
        seedRouting(ports, g, secretA)
        val oldGid = ports.derivePushGid(secretA)

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertTrue("verdict must accept: " + verdict, verdict.accepts)
        assertArrayEquals(secretB, ports.secrets[g])
        assertArrayEquals(ports.derivePushGid(secretB), ports.lastSubscribeTarget)
        assertEquals(1, ports.unsubscribeCount)
        assertArrayEquals(oldGid, ports.unsubscribed.single())
        assertNull(ports.routes[hex(oldGid)])
        assertEquals("exactly one live route row", 1, ports.routes.size)
        // A receiver NEVER re-broadcasts — it is not the rotator.
        assertEquals(0, ports.broadcastCount)
        assertEquals(0, ports.mintCount)
    }

    @Test
    fun `an IDENTICAL re-broadcast retires nothing (the id did not change)`() = runTest {
        val ports = FakePorts(account)
        val g = "3d".repeat(32)
        seedRouting(ports, g, secretA)

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretA.copyOf(),
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertTrue("verdict must accept: " + verdict, verdict.accepts)
        assertEquals("nothing to retire", 0, ports.unsubscribeCount)
        assertEquals(Route(account, g), ports.routes[hex(ports.derivePushGid(secretA))])
    }

    @Test
    fun `a FIRST-EVER secret retires nothing (there is no old id)`() = runTest {
        val ports = FakePorts(account)
        val g = "3e".repeat(32)

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = adminKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertTrue("verdict must accept: " + verdict, verdict.accepts)
        assertEquals(0, ports.unsubscribeCount)
    }

    @Test
    fun `a REFUSED secret retires nothing (the device keeps its id)`() = runTest {
        val ports = FakePorts(account)
        val g = "3f".repeat(32)
        seedRouting(ports, g, secretA)

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertFalse("verdict must refuse: " + verdict, verdict.accepts)
        assertEquals("a refusal must not tear down a LIVE subscription", 0, ports.unsubscribeCount)
        assertArrayEquals(secretA, ports.secrets[g])
        assertEquals(Route(account, g), ports.routes[hex(ports.derivePushGid(secretA))])
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

    // =================================================================
    // ADR-0044 — the RECENCY ORDER (lakko-security F-A, G-36 residual 9)
    //
    // The ADR-0042 / ADR-0043 gate asked only WHO sent a secret, never
    // whether it was NEWER. So a retry of the current secret and a replay
    // of a superseded one were byte-indistinguishable, which made a
    // silent, PERMANENT rotation undo reachable with no malice and no
    // crash. The sections below pin the two rules that close it (the
    // total order and the future clamp), the two places the fix is
    // easiest to void silently (a re-stamped re-broadcast, and an
    // epoch-inflating identical accept), and the sequencing rule the
    // whole closure rests on (C-61).
    // =================================================================

    /** The pre-rotation secret an evicted member still holds. Sorts
     *  HIGHER bytewise than every fake mint (0x30, 0x31, ...), which is
     *  what makes the EPOCH — not the byte tiebreak — the thing under
     *  test, and what makes the C-61 swapped companion below a real
     *  demonstration rather than a coin flip. */
    private val staleSecret = ByteArray(32) { 0xC3.toByte() }

    @Test
    fun `REJECTS a stamp ahead of this device's own epoch (reject-future)`() {
        // An honest stamp can never exceed the receiver's own epoch:
        // `stamp <= seal epoch <= receiver's epoch at dispatch`. So the
        // clamp never refuses an honest envelope, and it is what stops an
        // authorised writer stamping ULong.MAX_VALUE and FREEZING the
        // group's routing id forever — strictly worse than the churn
        // G-36 residual 3 already records, because a churn self-repairs at
        // the next owner rotation and a freeze does not.
        assertEquals(
            GroupPushRouting.ReceiveVerdict.REJECT_FUTURE,
            decide(
                ownerKey, knownRoles(), secretB, secretA,
                mintEpoch = 12uL, ownGroupEpoch = 11uL,
            ),
        )
    }

    @Test
    fun `the future clamp runs BEFORE authority - even the OWNER is refused`() {
        // The ordering of steps (2) and (3) is the whole point: if
        // authority ran first, the owner would clear it and go on to
        // freeze the group with an out-of-range stamp. Asserted for every
        // authorised rank, plus the legacy open group.
        for (sender in listOf(ownerKey, adminKey)) {
            assertEquals(
                GroupPushRouting.ReceiveVerdict.REJECT_FUTURE,
                decide(
                    sender, knownRoles(), secretB, secretA,
                    mintEpoch = ULong.MAX_VALUE, ownGroupEpoch = 11uL,
                ),
            )
        }
        assertEquals(
            GroupPushRouting.ReceiveVerdict.REJECT_FUTURE,
            decide(
                memberKey, GroupPushRouting.OwnerLookup.NoRoleTable, secretB, secretA,
                mintEpoch = ULong.MAX_VALUE, ownGroupEpoch = 11uL,
            ),
        )
    }

    @Test
    fun `a stamp EQUAL to this device's own epoch is accepted (the clamp is not off-by-one)`() {
        // The common case: a peer mints at epoch N and this device applies
        // the commit carrying that envelope, so it too is at N. An
        // off-by-one clamp would refuse every rotation in the group.
        assertEquals(
            GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER,
            decide(
                ownerKey, knownRoles(), secretB, secretA,
                storedEpoch = 10uL, mintEpoch = 11uL, ownGroupEpoch = 11uL,
            ),
        )
    }

    @Test
    fun `an UNREADABLE own epoch fails CLOSED (no ceiling, no accept)`() {
        // With no ceiling a stamp cannot be bounded at all, so the
        // envelope is unverifiable rather than merely unordered.
        assertEquals(
            GroupPushRouting.ReceiveVerdict.REJECT_UNVERIFIABLE,
            decide(ownerKey, knownRoles(), secretB, secretA, ownGroupEpoch = null),
        )
    }

    @Test
    fun `REJECTS an AUTHORISED envelope older than what is held (reject-stale)`() {
        // The F-A shape at the decision layer, for BOTH authorised ranks:
        // authority alone is no longer enough, the envelope must also win
        // the order.
        for (sender in listOf(ownerKey, adminKey)) {
            assertEquals(
                GroupPushRouting.ReceiveVerdict.REJECT_STALE,
                decide(
                    sender, knownRoles(), staleSecret, secretB,
                    storedEpoch = 11uL, mintEpoch = 10uL, ownGroupEpoch = 11uL,
                ),
            )
        }
    }

    @Test
    fun `a legacy no-role-table group is ORDERED too (accept-no-owner is not exempt)`() {
        // The open-group branch clears AUTHORITY, not the order. Skipping
        // the order there would leave the F-A replay wide open for exactly
        // the groups that have no owner to repair it.
        assertEquals(
            GroupPushRouting.ReceiveVerdict.REJECT_STALE,
            decide(
                memberKey, GroupPushRouting.OwnerLookup.NoRoleTable, staleSecret, secretB,
                storedEpoch = 11uL, mintEpoch = 10uL, ownGroupEpoch = 11uL,
            ),
        )
        assertEquals(
            GroupPushRouting.ReceiveVerdict.ACCEPT_NO_OWNER,
            decide(
                memberKey, GroupPushRouting.OwnerLookup.NoRoleTable, secretB, staleSecret,
                storedEpoch = 10uL, mintEpoch = 11uL, ownGroupEpoch = 11uL,
            ),
        )
    }

    @Test
    fun `the EPOCH dominates the byte tiebreak`() {
        // A newer envelope wins even when its secret sorts LOWER, and an
        // older one loses even when its secret sorts HIGHER. Without this
        // the order would be decided by the bytes, i.e. by nothing.
        assertEquals(
            GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER,
            decide(
                ownerKey, knownRoles(), secretA, staleSecret,
                storedEpoch = 10uL, mintEpoch = 11uL, ownGroupEpoch = 11uL,
            ),
        )
        assertEquals(
            GroupPushRouting.ReceiveVerdict.REJECT_STALE,
            decide(
                ownerKey, knownRoles(), staleSecret, secretA,
                storedEpoch = 11uL, mintEpoch = 10uL, ownGroupEpoch = 11uL,
            ),
        )
    }

    @Test
    fun `at an EXACT epoch tie the bytes decide - and the OWNER can LOSE`() {
        // The documented semantic narrowing, asserted so it cannot be
        // "fixed" by accident. A total order and unconditional
        // owner-priority cannot both hold; under ADR-0043 option (b) one
        // removal yields N+1 same-epoch mints, so the tie is the common
        // path. What owner-priority existed to provide (arrival-order
        // independence) is preserved by a STRICTLY STRONGER mechanism.
        assertEquals(
            "the owner's lower-sorting mint LOSES a tie",
            GroupPushRouting.ReceiveVerdict.REJECT_STALE,
            decide(
                ownerKey, knownRoles(), secretA, staleSecret,
                storedEpoch = 42uL, mintEpoch = 42uL, ownGroupEpoch = 42uL,
            ),
        )
        assertEquals(
            "and its higher-sorting mint wins one",
            GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER,
            decide(
                ownerKey, knownRoles(), staleSecret, secretA,
                storedEpoch = 42uL, mintEpoch = 42uL, ownGroupEpoch = 42uL,
            ),
        )
    }

    @Test
    fun `a SEED (nothing stored) is never ordered - any stamp is accepted`() {
        // There is nothing to order against, so step (5) is skipped. A
        // fresh device joining an old group must not be refused because
        // its first envelope's stamp happens to be low.
        assertEquals(
            GroupPushRouting.ReceiveVerdict.ACCEPT_ADMIN_SEED,
            decide(adminKey, knownRoles(), secretA, null, mintEpoch = 0uL, ownGroupEpoch = 9uL),
        )
    }

    @Test
    fun `accept-identical short-circuits BOTH the clamp and the order`() {
        // Rule (1) is unchanged by ADR-0044 and deliberately runs first:
        // a retry that reached us over a different route is still the same
        // value, whatever epoch it claims and whoever relayed it. That is
        // what keeps the any-holder re-broadcast idempotent.
        assertEquals(
            "an out-of-range stamp on identical bytes is still identical",
            GroupPushRouting.ReceiveVerdict.ACCEPT_IDENTICAL,
            decide(
                memberKey, knownRoles(), secretA, secretA.copyOf(),
                storedEpoch = 5uL, mintEpoch = ULong.MAX_VALUE, ownGroupEpoch = 9uL,
            ),
        )
        assertEquals(
            "a torn own-epoch read does not break the retry path",
            GroupPushRouting.ReceiveVerdict.ACCEPT_IDENTICAL,
            decide(
                null, GroupPushRouting.OwnerLookup.Unavailable, secretA, secretA.copyOf(),
                ownGroupEpoch = null,
            ),
        )
    }

    @Test
    fun `a wrong-length incoming secret can never displace a stored one`() {
        // The order is fail-closed on either side: a malformed incoming
        // secret never displaces a stored one, and a corrupt stored value
        // is left in place rather than overwritten on the strength of a
        // comparison against something that was never a secret.
        assertEquals(
            GroupPushRouting.ReceiveVerdict.REJECT_STALE,
            decide(
                ownerKey, knownRoles(), ByteArray(31), secretA,
                storedEpoch = 1uL, mintEpoch = 9uL, ownGroupEpoch = 9uL,
            ),
        )
    }

    // -----------------------------------------------------------------
    // The F-A replay, end to end
    // -----------------------------------------------------------------

    @Test
    fun `a stale re-broadcast is refused and does NOT retire the live gid`() = runTest {
        // The literal finding. A device's post-add redistribute broadcast
        // failed, so it holds the PRE-rotation secret S0 in a pending row;
        // another admin has since rotated the group to S1. That device's
        // any-holder re-broadcast re-sends S0 — and before ADR-0044 every
        // member accepted it (authorised sender, changed bytes), persisted
        // S0, and RETIRED gid(S1), migrating the group back onto the id
        // the EVICTED member still derives. With no further shrink there
        // was no further rotation, so it was PERMANENT.
        val ports = FakePorts(account)
        val g = "50".repeat(32)
        // This member is on S1, minted at the post-removal epoch 11.
        ports.secrets[g] = secretB
        ports.mintEpochs[g] = 11uL
        ports.distributed[g] = true
        val liveGid = ports.derivePushGid(secretB)
        ports.routes[hex(liveGid)] = Route(account, g)

        // The stale S0 arrives from an AUTHORISED sender, stamped 10.
        val verdict = receiveGated(
            groupIdHex = g,
            secret = staleSecret,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = ports,
            mintEpoch = 10uL,
            ownGroupEpoch = 11uL,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.REJECT_STALE, verdict)
        // Nothing moved: the live secret, its stamp, its subscription and
        // its route row all survive...
        assertArrayEquals("the live secret survives", secretB, ports.secrets[g])
        assertEquals("its stamp survives", 11uL, ports.mintEpochs[g])
        assertEquals("no re-subscribe", 0, ports.subscribeCount)
        assertEquals(Route(account, g), ports.routes[hex(liveGid)])
        // ...and — the assertion that carries the security value — the
        // LIVE gid was NOT retired.
        assertEquals("a stale replay must retire NOTHING", 0, ports.unsubscribeCount)
        assertTrue("no gid torn down at all", ports.unsubscribed.isEmpty())
    }

    @Test
    fun `the same stale replay from an ADMIN is refused too`() = runTest {
        // ADR-0043 option (b) widened OVERWRITE authority to admins, which
        // is what makes the replay reachable from more devices. Authority
        // is not what refuses it — the order is.
        val ports = FakePorts(account)
        val g = "51".repeat(32)
        ports.secrets[g] = secretB
        ports.mintEpochs[g] = 11uL
        ports.distributed[g] = true

        val verdict = receiveGated(
            groupIdHex = g,
            secret = staleSecret,
            senderSignatureKey = adminKey,
            owner = knownRoles(),
            ports = ports,
            mintEpoch = 10uL,
            ownGroupEpoch = 11uL,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.REJECT_STALE, verdict)
        assertArrayEquals(secretB, ports.secrets[g])
        assertEquals(0, ports.unsubscribeCount)
    }

    @Test
    fun `a LEGACY stored row (no recorded stamp) reads as 0 and IS superseded`() = runTest {
        // An upgrade must converge, and it converges in this direction:
        // the legacy row sits at the FLOOR of the order, so the first
        // authorised stamped mint replaces it. The inverse default
        // ("current") would let a legacy secret out-rank a live rotation —
        // the F-A shape itself.
        val ports = FakePorts(account)
        val g = "52".repeat(32)
        ports.secrets[g] = staleSecret // no `mintEpochs` entry == legacy
        ports.distributed[g] = true
        val oldGid = ports.derivePushGid(staleSecret)
        ports.routes[hex(oldGid)] = Route(account, g)

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = ports,
            mintEpoch = 1uL,
            ownGroupEpoch = 9uL,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER, verdict)
        assertArrayEquals(secretB, ports.secrets[g])
        assertEquals("the stamp is now recorded", 1uL, ports.mintEpochs[g])
        assertEquals("the legacy gid was retired", 1, ports.unsubscribeCount)
        assertArrayEquals(oldGid, ports.unsubscribed.single())
    }

    @Test
    fun `an IDENTICAL relay of a legacy secret is still idempotent`() = runTest {
        val ports = FakePorts(account)
        val g = "53".repeat(32)
        ports.secrets[g] = staleSecret // legacy: no recorded stamp
        ports.distributed[g] = true

        val verdict = receiveGated(
            groupIdHex = g,
            secret = staleSecret.copyOf(),
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = ports,
        )

        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT_IDENTICAL, verdict)
        assertEquals("nothing retired", 0, ports.unsubscribeCount)
    }

    @Test
    fun `accept-identical cannot INFLATE the stored epoch (a plain-member freeze)`() = runTest {
        // The defect the web suite caught, pinned here so this client
        // cannot reproduce it.
        //
        // `accept-identical` clears NEITHER the authority gate NOR the
        // future clamp — it short-circuits both by design, so a retry
        // stays idempotent for any sender. That makes the re-persist an
        // EPOCH-INFLATION CHANNEL if it writes the INCOMING stamp: a PLAIN
        // MEMBER relays the group's current secret bytes stamped
        // ULong.MAX_VALUE, the stored epoch jumps to the ceiling, and
        // every later rotation — the OWNER's included — is refused
        // REJECT_STALE forever. That is precisely the permanent freeze the
        // clamp exists to prevent, reached AROUND it. So the stored stamp
        // must survive the write.
        val ports = FakePorts(account)
        val g = "54".repeat(32)
        ports.secrets[g] = staleSecret
        ports.mintEpochs[g] = 10uL
        ports.distributed[g] = true

        val relayed = receiveGated(
            groupIdHex = g,
            secret = staleSecret.copyOf(),
            senderSignatureKey = memberKey,
            owner = knownRoles(),
            ports = ports,
            mintEpoch = ULong.MAX_VALUE,
            ownGroupEpoch = 11uL,
        )
        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT_IDENTICAL, relayed)
        assertEquals("the STORED stamp must survive", 10uL, ports.mintEpochs[g])

        // ... and a legitimate owner rotation at a modest epoch still
        // lands, which is the property the inflation would have destroyed.
        val rotated = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = ports,
            mintEpoch = 11uL,
            ownGroupEpoch = 11uL,
        )
        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER, rotated)
        assertArrayEquals(secretB, ports.secrets[g])
        assertEquals(11uL, ports.mintEpochs[g])
    }

    @Test
    fun `an accepted DIFFERENT secret clears a stale PENDING re-broadcast`() = runTest {
        // ADR-0044's complementary repair. The order rule stops a stale
        // value from SPREADING; this stops the device still holding one
        // from re-emitting it.
        //
        // On Android that falls out of the row encoding rather than out of
        // any extra machinery: `PushRoutingSecretStore` keeps the secret,
        // the flag and the stamp in ONE value string and `receive`
        // persists with `distributed = true`, so applying an accepted
        // DIFFERENT secret replaces the bytes AND clears the pending flag
        // in the SAME atomic write. Pinned here rather than left
        // incidental, because it is a property of the row encoding —
        // which could change — not of the gate.
        val ports = FakePorts(account)
        val g = "55".repeat(32)
        ports.secrets[g] = staleSecret
        ports.mintEpochs[g] = 10uL
        ports.distributed[g] = false // its broadcast failed

        val verdict = receiveGated(
            groupIdHex = g,
            secret = secretB,
            senderSignatureKey = ownerKey,
            owner = knownRoles(),
            ports = ports,
            mintEpoch = 11uL,
            ownGroupEpoch = 11uL,
        )
        assertEquals(GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER, verdict)

        // The pending flag is gone ...
        assertTrue("the pending re-broadcast was cleared", ports.isDistributed(g))
        // ... so the next provisioning pass re-broadcasts NOTHING.
        ports.groupEpoch = 11uL
        GroupPushRouting.ensure(account, g, isOwner = false, ports = ports)
        assertEquals("nothing stale is ever re-emitted", 0, ports.broadcastCount)
    }

    // -----------------------------------------------------------------
    // The stamp is minted ONCE and REPLAYED verbatim
    //
    // The single easiest thing in ADR-0044 to get wrong, and getting it
    // wrong makes the whole fix a silent no-op: if a path recomposing an
    // envelope from an ALREADY-STORED secret re-stamps the CURRENT epoch
    // at send time, the stale envelope arrives carrying a NEWER stamp than
    // the live rotation, wins the comparison, and F-A is reproduced with
    // the ordering gate fully in place and working as specified.
    // -----------------------------------------------------------------

    @Test
    fun `the OWNER's MINT stamps the group's CURRENT epoch`() = runTest {
        val ports = FakePorts(account)
        val g = "60".repeat(32)
        ports.groupEpoch = 7uL

        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        assertEquals(1, ports.broadcastCount)
        assertEquals("stamped at the live epoch", 7uL, stampOf(ports.broadcastEnvelopes.single()))
        assertEquals("and recorded durably", 7uL, ports.mintEpochs[g])
        assertTrue(
            "a mint is the ONLY fresh stamp",
            ports.composedStamps.single() is GroupPushRouting.MintStamp.Fresh,
        )
    }

    @Test
    fun `a MINT is REFUSED when the group epoch cannot be read (fail-closed)`() = runTest {
        // An unstamped secret has no position in the order, which is
        // exactly what the v2 envelope exists to eliminate. Better no push
        // for this group than an unorderable secret.
        val ports = FakePorts(account)
        val g = "61".repeat(32)
        ports.groupEpoch = null

        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        assertEquals("nothing minted", 0, ports.mintCount)
        assertEquals("nothing broadcast", 0, ports.broadcastCount)
        assertTrue("nothing persisted", ports.secrets.isEmpty())
    }

    @Test
    fun `the PENDING re-broadcast REPLAYS the stored stamp, not the current epoch`() = runTest {
        // The exact site the F-A replay travels through.
        val ports = FakePorts(account)
        val g = "62".repeat(32)
        ports.secrets[g] = staleSecret
        ports.mintEpochs[g] = 3uL
        ports.distributed[g] = false
        // The group has moved on a long way since that secret was minted.
        ports.groupEpoch = 99uL

        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        assertEquals(1, ports.broadcastCount)
        assertEquals(
            "the MINT epoch is replayed, never refreshed",
            3uL,
            stampOf(ports.broadcastEnvelopes.single()),
        )
        assertTrue(
            "and the stamp is typed as a REPLAY",
            ports.composedStamps.single() is GroupPushRouting.MintStamp.Replayed,
        )
        assertEquals("the stored stamp is untouched", 3uL, ports.mintEpochs[g])
        assertEquals("never re-minted", 0, ports.mintCount)
    }

    @Test
    fun `the post-add REDISTRIBUTE replays the stored stamp too`() = runTest {
        // The secret did not change, only its audience did. Re-stamping
        // would let a routine add-member out-rank a rotation minted at a
        // higher epoch.
        val ports = FakePorts(account)
        val g = "63".repeat(32)
        ports.secrets[g] = staleSecret
        ports.mintEpochs[g] = 4uL
        ports.distributed[g] = true
        ports.groupEpoch = 40uL

        GroupPushRouting.redistribute(account, g, ports)

        assertEquals(4uL, stampOf(ports.broadcastEnvelopes.single()))
        assertTrue(ports.composedStamps.single() is GroupPushRouting.MintStamp.Replayed)
    }

    @Test
    fun `a LEGACY row re-broadcasts at 0 rather than inventing a stamp`() = runTest {
        val ports = FakePorts(account)
        val g = "64".repeat(32)
        ports.secrets[g] = staleSecret // no recorded stamp
        ports.distributed[g] = false
        ports.groupEpoch = 55uL

        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        assertEquals(
            "a legacy secret keeps its floor position",
            PushRoutingSecretStore.LEGACY_MINT_EPOCH,
            stampOf(ports.broadcastEnvelopes.single()),
        )
    }

    @Test
    fun `a ROTATION stamps the CURRENT epoch, and its RETRY replays that stamp`() = runTest {
        val ports = FakePorts(account)
        val g = "65".repeat(32)
        ports.secrets[g] = staleSecret
        ports.mintEpochs[g] = 10uL
        ports.distributed[g] = true
        ports.groupEpoch = 11uL // post-removal
        ports.broadcastFails = true

        val outcome = GroupPushRouting.rotate(
            account, g, GroupPushRouting.RotationRole.OWNER, ports,
        )
        assertEquals(GroupPushRouting.RotationOutcome.ROTATED, outcome)
        assertEquals("the rotation stamped the post-removal epoch", 11uL, ports.mintEpochs[g])

        // The retry re-sends the STORED (new) secret at the STORED stamp —
        // even though the group has since advanced again.
        ports.broadcastFails = false
        ports.groupEpoch = 20uL
        GroupPushRouting.ensure(account, g, isOwner = true, ports = ports)

        assertEquals(1, ports.broadcastCount)
        assertEquals(
            "the retry replays 11, not 20",
            11uL,
            stampOf(ports.broadcastEnvelopes.single()),
        )
        assertEquals("no second mint", 1, ports.mintCount)
    }

    @Test
    fun `a ROTATION is ABANDONED when the epoch cannot be read (fail-closed)`() = runTest {
        val ports = FakePorts(account)
        val g = "66".repeat(32)
        ports.secrets[g] = staleSecret
        ports.mintEpochs[g] = 10uL
        ports.distributed[g] = true
        ports.groupEpoch = null

        val outcome = GroupPushRouting.rotate(
            account, g, GroupPushRouting.RotationRole.OWNER, ports,
        )

        assertEquals(GroupPushRouting.RotationOutcome.SKIP_UNKNOWN_EPOCH, outcome)
        assertEquals("nothing minted", 0, ports.mintCount)
        assertArrayEquals("the live secret is untouched", staleSecret, ports.secrets[g])
        assertEquals("nothing torn down", 0, ports.unsubscribeCount)
    }

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    @Test
    fun `a QUEUED rotation stamps the epoch it MINTS at, not the one it was triggered at`() =
        runTest {
            // ADR-0044 requires the stamp to be read INSIDE the serializing
            // rotation lock, AFTER any queue wait. A value resolved at the
            // call site would be read BEFORE the wait, so a rotation queued
            // behind a predecessor would stamp a stale epoch — and could
            // then TIE with, or lose to, the predecessor's own output.
            val ports = DistinctMintPorts(account)
            val g = "67".repeat(32)
            seedRouting(ports, g, secretA)
            ports.mintEpochs[g] = 10uL
            ports.groupEpoch = 11uL

            val owner = GroupPushRouting.RotationRole.OWNER
            launch { GroupPushRouting.rotate(account, g, owner, ports) }
            launch { GroupPushRouting.rotate(account, g, owner, ports) }
            runCurrent()

            // The first rotation is parked inside its broadcast; the group
            // advances again (another commit lands) before the second one
            // gets the lock.
            assertEquals("second rotation is behind the lock", 1, ports.mintCount)
            ports.groupEpoch = 12uL
            ports.broadcastGate.complete(Unit)
            advanceUntilIdle()

            assertEquals(2, ports.mintCount)
            assertEquals(
                "the queued rotation stamped 12 (read after the wait), not 11",
                12uL,
                ports.mintEpochs[g],
            )
            assertEquals(11uL, stampOf(ports.broadcastEnvelopes[0]))
            assertEquals(12uL, stampOf(ports.broadcastEnvelopes[1]))
        }

    // -----------------------------------------------------------------
    // C-61 — publish the Remove commit, THEN rotate
    //
    // ADR-0043 required this ordering but enforced it STRUCTURALLY only,
    // with no test on any client. ADR-0044 promotes it from a property
    // that protects one guarantee (the envelope's forward secrecy) to a
    // PRECONDITION OF TWO: the rotation's stamp is `Group.epoch()` read at
    // mint time, so rotating first stamps the PRE-removal epoch, TIES with
    // the secret the evicted member holds, and falls to the byte tiebreak
    // — the guarantee degrades from "always wins" to "coin flip",
    // silently.
    // -----------------------------------------------------------------

    /** The epoch before the Remove commit is applied locally. */
    private val preRemovalEpoch: ULong = 10uL

    /** ... and after. */
    private val postRemovalEpoch: ULong = 11uL

    /** A device holding the secret the EVICTED member also holds, minted
     *  pre-removal. [staleSecret] sorts HIGHER than every fake mint, so at
     *  an epoch TIE the stale secret wins OUTRIGHT — which is what makes
     *  the swapped companion below a real demonstration. */
    private fun seedC61(ports: FakePorts, g: String) {
        ports.secrets[g] = staleSecret
        ports.mintEpochs[g] = preRemovalEpoch
        ports.distributed[g] = true
        ports.groupEpoch = preRemovalEpoch
    }

    @Test
    fun `C-61 - the rotation is stamped POST-removal and OUT-RANKS the stale secret`() = runTest {
        // HOW THIS FAILS UNDER A SWAP: the assertion below reads the epoch
        // the EMITTED envelope was stamped with. `rotate` reads
        // `freshMintStamp()` at mint time, and applying the Remove commit
        // is what moves that value from 10 to 11. Start the rotation
        // before the removal and the stamp is 10, so
        // `assertEquals(postRemovalEpoch, stampOf(...))` fails outright;
        // the `isNewer` assertion after it then flips from "the rotation
        // always wins" to a same-epoch tie decided by the 32 secret bytes
        // — which, with this fixture, the STALE secret wins. The companion
        // test below runs exactly that swap and asserts both consequences.
        val ports = FakePorts(account)
        val g = "70".repeat(32)
        seedC61(ports, g)

        val commitId = GroupPushRouting.publishRemoveThenRotate(
            scope = this,
            publishRemove = {
                // Applying + publishing the Remove commit advances the
                // local MLS epoch.
                ports.groupEpoch = postRemovalEpoch
                4242L
            },
            rotate = {
                GroupPushRouting.rotate(account, g, GroupPushRouting.RotationRole.OWNER, ports)
            },
        )
        advanceUntilIdle()

        assertEquals(4242L, commitId)
        assertEquals(1, ports.broadcastCount)

        // The rotation was sealed AND stamped after the removal.
        val emitted = ports.broadcastEnvelopes.single()
        assertEquals(postRemovalEpoch, stampOf(emitted))

        // ... which is what makes the ordering a GUARANTEE rather than a
        // coin flip: the evicted member's secret sorts HIGHER bytewise, so
        // ONLY the strictly greater epoch puts the rotation on top.
        val rotated = secretOf(emitted)
        assertTrue(
            "the rotation supersedes the secret it replaces",
            ports.isNewer.isNewer(postRemovalEpoch, rotated, preRemovalEpoch, staleSecret),
        )
        assertFalse(
            "and the stale secret can never supersede the rotation",
            ports.isNewer.isNewer(preRemovalEpoch, staleSecret, postRemovalEpoch, rotated),
        )
    }

    @Test
    fun `C-61 - the INVERTED order stamps PRE-removal and the rotation LOSES`() = runTest {
        // Not a test of production code — a demonstration that the
        // assertion above actually DISCRIMINATES. Rotating BEFORE the
        // Remove commit lands stamps epoch 10, tying with the secret the
        // evicted member holds, so the outcome falls to the byte
        // comparison — and with this fixture the stale secret sorts
        // higher, so the rotation loses OUTRIGHT.
        val ports = FakePorts(account)
        val g = "71".repeat(32)
        seedC61(ports, g)

        GroupPushRouting.rotate(account, g, GroupPushRouting.RotationRole.OWNER, ports)
        ports.groupEpoch = postRemovalEpoch // the Remove lands afterwards — too late

        val emitted = ports.broadcastEnvelopes.single()
        assertEquals(
            "the swapped order stamps the PRE-removal epoch",
            preRemovalEpoch,
            stampOf(emitted),
        )
        assertNotEquals(
            "which is exactly what the ordered test asserts against",
            postRemovalEpoch,
            stampOf(emitted),
        )

        val rotated = secretOf(emitted)
        // Tied on epoch, so the bytes decide — and here the STALE secret
        // wins, i.e. the rotation is silently undone.
        assertFalse(
            "the rotation does NOT supersede the stale secret",
            ports.isNewer.isNewer(preRemovalEpoch, rotated, preRemovalEpoch, staleSecret),
        )
        assertTrue(
            "the stale secret supersedes the ROTATION - F-A, reproduced",
            ports.isNewer.isNewer(preRemovalEpoch, staleSecret, preRemovalEpoch, rotated),
        )

        // And the gate agrees: a peer holding the rotation would ACCEPT the
        // stale replay back over it.
        assertEquals(
            GroupPushRouting.ReceiveVerdict.ACCEPT_OWNER,
            decide(
                ownerKey, knownRoles(), staleSecret, rotated,
                storedEpoch = preRemovalEpoch,
                mintEpoch = preRemovalEpoch,
                ownGroupEpoch = postRemovalEpoch,
            ),
        )
    }

    @Test
    fun `C-61 - the rotation is not STARTED until the removal returns`() = runTest {
        // The ordering is about START time, not completion: the rotation
        // stays fire-and-forget so a push failure never unwinds a
        // membership change.
        val ports = FakePorts(account)
        val g = "72".repeat(32)
        seedC61(ports, g)
        val order = mutableListOf<String>()

        GroupPushRouting.publishRemoveThenRotate(
            scope = this,
            publishRemove = {
                order.add("remove")
                ports.groupEpoch = postRemovalEpoch
                1L
            },
            rotate = {
                order.add("rotate")
                GroupPushRouting.rotate(account, g, GroupPushRouting.RotationRole.OWNER, ports)
            },
        )
        advanceUntilIdle()

        assertEquals(listOf("remove", "rotate"), order)
    }

    @Test
    fun `C-61 - a FAILED removal never rotates`() = runTest {
        val ports = FakePorts(account)
        val g = "73".repeat(32)
        seedC61(ports, g)

        val result = runCatching {
            GroupPushRouting.publishRemoveThenRotate(
                scope = this,
                publishRemove = { throw IllegalStateException("DS unreachable") },
                rotate = {
                    GroupPushRouting.rotate(account, g, GroupPushRouting.RotationRole.OWNER, ports)
                },
            )
        }
        advanceUntilIdle()

        assertTrue("the removal failure propagates", result.isFailure)
        assertEquals("no rotation for a removal that never landed", 0, ports.mintCount)
        assertEquals(0, ports.broadcastCount)
    }

    @Test
    fun `C-61 - a rotation failure is reported, never thrown at the removal's caller`() = runTest {
        val ports = FakePorts(account)
        val g = "74".repeat(32)
        seedC61(ports, g)
        var reported: Throwable? = null

        val commitId = GroupPushRouting.publishRemoveThenRotate(
            scope = this,
            publishRemove = { 7L },
            rotate = { throw IllegalStateException("rotation exploded") },
            onRotationError = { reported = it },
        )
        advanceUntilIdle()

        assertEquals("the removal still lands", 7L, commitId)
        assertEquals("rotation exploded", reported?.message)
    }

    @Test
    fun `C-61 - no rotation step means the removal still lands`() = runTest {
        val commitId = GroupPushRouting.publishRemoveThenRotate<Long>(
            scope = this,
            publishRemove = { 9L },
            rotate = null,
        )
        advanceUntilIdle()
        assertEquals(9L, commitId)
    }

    // -----------------------------------------------------------------
    // The receive-side RACE — `receiveGated` must serialize
    //
    // `receiveGated` is a read-modify-write: it reads the stored
    // `(secret, mintEpoch)` pair at the top and persists several calls
    // down. Its production caller fires it PER ENVELOPE, fire-and-forget,
    // on the multi-threaded `pushScope` — so two routing envelopes in one
    // poll batch used to run as two genuinely parallel coroutines and both
    // could finish their read of the PRE-batch row before either wrote.
    // The survivor was then decided by SCHEDULING rather than by the
    // ADR-0044 total order, which restores F-A with no malice and no
    // crash — see the [GroupPushRouting] lock doc for the full trace.
    //
    // These tests run on REAL threads (`Dispatchers.Default`, not the
    // virtual-time test dispatcher) because the defect is genuine
    // parallelism: there is no suspension point between the read and the
    // write inside ONE coroutine, so a confined dispatcher would hide it
    // rather than close it.
    // -----------------------------------------------------------------

    /**
     * ONE routing row, shared by two or more [RacePorts] instances — so
     * concurrently dispatched receives contend on a single row exactly as
     * the per-envelope launches of one poll batch do.
     */
    private class RaceStore {
        @Volatile
        var secret: ByteArray? = null

        @Volatile
        var mintEpoch: ULong = 0uL

        @Volatile
        var distributed: Boolean = true

        /** Shared across every [RacePorts] over this row, so a concurrent
         *  mint is counted once for the GROUP rather than once per port. */
        val mintCount = java.util.concurrent.atomic.AtomicInteger(0)

        val routes: MutableSet<String> =
            java.util.Collections.synchronizedSet(LinkedHashSet<String>())
        val subscribed: MutableList<String> =
            java.util.Collections.synchronizedList(mutableListOf<String>())
        val unsubscribed: MutableList<String> =
            java.util.Collections.synchronizedList(mutableListOf<String>())
    }

    /**
     * A [PushRoutingPorts] over a SHARED [RaceStore], with a one-shot
     * [afterRead] hook that fires on this instance's FIRST
     * [PushRoutingPorts.getRecord] — i.e. exactly the gate read at the top
     * of `receiveGated`, never the incidental re-read inside
     * `subscribeOrDefer`. That hook is what lets a test park one receive
     * between its read and its write and drive the interleave
     * deterministically instead of hoping for it.
     *
     * The pure transforms ([PushRoutingPorts.derivePushGid], the ADR-0044
     * order, the envelope codec) are DELEGATED to a [FakePorts] instance
     * rather than re-implemented, so this class cannot drift from the rest
     * of the suite.
     */
    private class RacePorts(
        private val store: RaceStore,
        private val stampEpoch: ULong = 0uL,
        private val beforeStamp: suspend () -> Unit = {},
        private val afterRead: () -> Unit = {},
    ) : PushRoutingPorts {
        private val pure = FakePorts("race")
        private val hookArmed = java.util.concurrent.atomic.AtomicBoolean(true)

        init {
            pure.groupEpoch = stampEpoch
        }

        /** A distinct 32-byte secret per GROUP-wide mint, so two rotations
         *  off one row are always distinguishable. */
        override fun mintSecret(): ByteArray {
            val n = store.mintCount.incrementAndGet()
            return ByteArray(32) { (0x40 + n).toByte() }
        }

        override fun derivePushGid(secret: ByteArray): ByteArray = pure.derivePushGid(secret)

        override val isNewer: GroupPushRouting.PushRoutingSecretIsNewer = pure.isNewer

        override fun composeEnvelope(
            stamp: GroupPushRouting.MintStamp,
            secret: ByteArray,
        ): ByteArray = pure.composeEnvelope(stamp, secret)

        /** The REAL suspension inside `rotate`'s read-modify-write: it sits
         *  between the old-secret read and the persist (in production a hop
         *  through `ChatRepository`'s own group lock to read `Group.epoch()`).
         *  [beforeStamp] parks a rotation exactly there — the F-E window. */
        override suspend fun freshMintStamp(): GroupPushRouting.MintStamp.Fresh? {
            beforeStamp()
            return pure.freshMintStamp()
        }

        override fun hasSecret(groupIdHex: String): Boolean = store.secret != null

        override fun getRecord(groupIdHex: String): GroupPushRouting.StoredSecret? {
            val held = store.secret?.let { GroupPushRouting.StoredSecret(it, store.mintEpoch) }
            if (hookArmed.compareAndSet(true, false)) afterRead()
            return held
        }

        override fun isDistributed(groupIdHex: String): Boolean = store.distributed

        override fun persist(
            groupIdHex: String,
            secret: ByteArray,
            mintEpoch: ULong,
            pushGid: ByteArray,
            distributed: Boolean,
        ) {
            store.secret = secret
            store.mintEpoch = mintEpoch
            store.distributed = distributed
            store.routes.add(hex(pushGid))
        }

        override fun markDistributed(groupIdHex: String) {
            store.distributed = true
        }

        override fun markPending(groupIdHex: String) {
            store.distributed = false
        }

        override suspend fun subscribeUnderPushGid(pushGid: ByteArray) {
            store.subscribed.add(hex(pushGid))
        }

        override suspend fun unsubscribeFromPushGid(pushGid: ByteArray) {
            store.unsubscribed.add(hex(pushGid))
        }

        override fun deleteRoute(pushGid: ByteArray) {
            store.routes.remove(hex(pushGid))
        }

        override suspend fun broadcast(envelope: ByteArray) = Unit
    }

    /** Long enough that a coroutine reliably reaches its gate read. */
    private val startupWaitMs = 5_000L

    /** How long a parked receive waits for its counterpart before giving
     *  up. WITH the lock the counterpart is queued behind it and can never
     *  arrive, so this timeout is the (bounded) cost of the test; WITHOUT
     *  it the handoff happens immediately and the timeout is never hit. */
    private val interleaveWaitMs = 500L

    private fun await(latch: java.util.concurrent.CountDownLatch, ms: Long) {
        latch.await(ms, java.util.concurrent.TimeUnit.MILLISECONDS)
    }

    @Test
    fun `two receives from ONE poll batch cannot let the STALE one win - the F-A race`() =
        kotlinx.coroutines.runBlocking {
            // The literal F-A batch. This device holds S_OLD minted at epoch
            // 10 and polls `[rotation S1@11, stale S_OLD@10]`:
            //
            //   gate A reads (S_OLD,10) -> ACCEPT_ADMIN_ROTATE -> persists
            //     (S1,11), retires gid(S_OLD)
            //   gate B reads that SAME pre-batch snapshot, so its own bytes
            //     are byte-IDENTICAL -> ACCEPT_IDENTICAL, which by design
            //     short-circuits BOTH the clamp and the order -> re-persists
            //     S_OLD one turn behind A
            //
            // The device ends on the superseded secret: the id the evicted
            // member still derives and is still subscribed to.
            //
            // Note the failure is NOT a lost update — it is the
            // ACCEPT_IDENTICAL fast path. That is exactly why every
            // SEQUENTIAL F-A test in this file passed throughout: only a
            // concurrent read makes a stale envelope look byte-identical to
            // what the reader believes is stored, and ACCEPT_IDENTICAL by
            // design bypasses BOTH the clamp and the order. So the verdict
            // itself is asserted below, not just its after-effects.
            val store = RaceStore()
            val g = "80".repeat(32)
            val sOld = ByteArray(32) { 0x0A }
            val s1 = ByteArray(32) { 0x0B }
            store.secret = sOld
            store.mintEpoch = 10uL

            val rotationRead = java.util.concurrent.CountDownLatch(1)
            val staleRead = java.util.concurrent.CountDownLatch(1)
            val rotationDone = java.util.concurrent.CountDownLatch(1)

            // A parks between its read and its write until B has read...
            val rotationPorts = RacePorts(store) {
                rotationRead.countDown()
                await(staleRead, interleaveWaitMs)
            }
            // ...and B parks after its read until A has fully applied, so
            // its write lands LAST. Both waits are bounded, so with the lock
            // in place (where neither handoff can happen — the second
            // receive is queued behind the first) the test still terminates.
            val stalePorts = RacePorts(store) {
                staleRead.countDown()
                await(rotationDone, interleaveWaitMs)
            }
            val gidOld = hex(rotationPorts.derivePushGid(sOld))
            val gidNew = hex(rotationPorts.derivePushGid(s1))
            store.routes.add(gidOld)
            var staleVerdict: GroupPushRouting.ReceiveVerdict? = null

            kotlinx.coroutines.coroutineScope {
                val rotation = launch(kotlinx.coroutines.Dispatchers.Default) {
                    GroupPushRouting.receiveGated(
                        accountIdB64 = account,
                        groupIdHex = g,
                        secret = s1,
                        mintEpoch = 11uL,
                        senderSignatureKey = adminKey,
                        owner = knownRoles(),
                        ownGroupEpoch = 11uL,
                        ports = rotationPorts,
                    )
                    rotationDone.countDown()
                }
                // Dispatch the stale envelope only once the rotation is at
                // its gate read — the interleave being probed.
                await(rotationRead, startupWaitMs)
                val stale = launch(kotlinx.coroutines.Dispatchers.Default) {
                    staleVerdict = GroupPushRouting.receiveGated(
                        accountIdB64 = account,
                        groupIdHex = g,
                        secret = sOld,
                        mintEpoch = 10uL,
                        senderSignatureKey = adminKey,
                        owner = knownRoles(),
                        ownGroupEpoch = 11uL,
                        ports = stalePorts,
                    )
                }
                kotlinx.coroutines.joinAll(rotation, stale)
            }

            // The load-bearing assertion: the stale envelope must be judged
            // against the row the rotation COMMITTED, so it can never present
            // as ACCEPT_IDENTICAL.
            assertEquals(
                "the stale envelope must lose the ORDER, not slip through as identical",
                GroupPushRouting.ReceiveVerdict.REJECT_STALE,
                staleVerdict,
            )
            assertArrayEquals(
                "the device must end on the ROTATION's secret, not the stale one",
                s1,
                store.secret,
            )
            assertEquals("filed at the rotation's epoch", 11uL, store.mintEpoch)
            assertTrue("gid(S1) is live", store.routes.contains(gidNew))
            assertTrue("subscribed under gid(S1)", store.subscribed.contains(gidNew))
            assertFalse(
                "gid(S_OLD) must not be resurrected — the evicted member derives it",
                store.routes.contains(gidOld),
            )
            assertTrue("gid(S_OLD) was retired", store.unsubscribed.contains(gidOld))
        }

    @Test
    fun `N concurrent same-epoch rotations converge on the total-order winner`() =
        kotlinx.coroutines.runBlocking {
            // ADR-0043 option (b) lets an ADMIN rotate, so ONE removal
            // yields N+1 same-epoch mints that typically arrive in a single
            // poll batch. They TIE on the epoch, so the canonical order
            // falls to the 32 secret bytes and there is exactly one right
            // answer — the same one whatever order they land in.
            //
            // Unserialized, all N decide against the SAME pre-batch row, all
            // clear the order against it, and the winner is whichever
            // coroutine happens to write last — plus one live route row per
            // accepted secret instead of one.
            val store = RaceStore()
            val g = "81".repeat(32)
            val sOld = ByteArray(32) { 0x0A }
            store.secret = sOld
            store.mintEpoch = 10uL

            // Deliberately NOT dispatched in byte order: the winner must be
            // the total-order maximum, not the last arrival.
            val candidates = listOf(
                ByteArray(32) { 0x31 },
                ByteArray(32) { 0x77 },
                ByteArray(32) { 0x12 },
                ByteArray(32) { 0x55 },
            )
            val winner = candidates.maxWithOrNull(
                Comparator { a, b -> compareWire(11uL, a, 11uL, b) },
            )!!

            // A soft barrier: each receive parks after its read until every
            // one of them has read (or the bound elapses). Unserialized that
            // makes every gate decide against the pre-batch row; serialized
            // it can never complete, so each simply pays the bound.
            val allRead = java.util.concurrent.CountDownLatch(candidates.size)
            val portsList = candidates.map {
                RacePorts(store) {
                    allRead.countDown()
                    await(allRead, interleaveWaitMs)
                }
            }
            val gidOld = hex(portsList[0].derivePushGid(sOld))
            val gidWinner = hex(portsList[0].derivePushGid(winner))
            store.routes.add(gidOld)

            kotlinx.coroutines.coroutineScope {
                candidates.forEachIndexed { i, secret ->
                    launch(kotlinx.coroutines.Dispatchers.Default) {
                        GroupPushRouting.receiveGated(
                            accountIdB64 = account,
                            groupIdHex = g,
                            secret = secret,
                            mintEpoch = 11uL,
                            senderSignatureKey = adminKey,
                            owner = knownRoles(),
                            ownGroupEpoch = 11uL,
                            ports = portsList[i],
                        )
                    }
                }
            }

            assertArrayEquals(
                "the total-order maximum survives, not the last writer",
                winner,
                store.secret,
            )
            assertEquals("filed at the shared epoch", 11uL, store.mintEpoch)
            assertEquals(
                "exactly one live route row — every superseded id was retired",
                setOf(gidWinner),
                store.routes.toSet(),
            )
            assertFalse("the pre-batch id is gone", store.routes.contains(gidOld))
        }

    @Test
    fun `a receive landing mid-rotation cannot strand an orphan gid - F-E`() =
        kotlinx.coroutines.runBlocking {
            // The ADR-0043 **F-E** residual, closed by SHARING the lock with
            // `rotate` rather than giving the receive path its own.
            //
            // `rotate`'s read-modify-write straddles a REAL suspension: it
            // reads the old secret, AWAITS `freshMintStamp` (in production a
            // hop through `ChatRepository`'s group lock), and only then
            // persists. A receive that commits S1 inside that window is
            // overwritten by the rotation's S2 — and the rotation then
            // retires `gid(S_OLD)`, the id it captured BEFORE the wait,
            // leaving `gid(S1)` subscribed, indexed, and un-retirable by
            // anyone: nothing ever notifies it again and no later rotation
            // can name it.
            //
            // Parking the rotation inside its BROADCAST instead would prove
            // nothing — `rotate` persists BEFORE it broadcasts, so by then
            // the receive already reads the fresh row and the test passes
            // against unfixed code.
            val store = RaceStore()
            val g = "82".repeat(32)
            val sOld = ByteArray(32) { 0x0A }
            val s1 = ByteArray(32) { 0x0B }
            store.secret = sOld
            store.mintEpoch = 10uL

            val atStamp = java.util.concurrent.CountDownLatch(1)
            val receiveDone = java.util.concurrent.CountDownLatch(1)

            // The rotation parks between its old-secret read and its persist.
            // WITH the shared lock the receive is queued behind it and can
            // never arrive, so this simply pays the bound.
            val rotationPorts = RacePorts(
                store = store,
                stampEpoch = 12uL,
                beforeStamp = {
                    atStamp.countDown()
                    await(receiveDone, interleaveWaitMs)
                },
            )
            val receivePorts = RacePorts(store, stampEpoch = 12uL)
            val gidOld = hex(rotationPorts.derivePushGid(sOld))
            val gidS1 = hex(rotationPorts.derivePushGid(s1))
            store.routes.add(gidOld)

            kotlinx.coroutines.coroutineScope {
                val rotation = launch(kotlinx.coroutines.Dispatchers.Default) {
                    GroupPushRouting.rotate(
                        accountIdB64 = account,
                        groupIdHex = g,
                        ownRole = GroupPushRouting.RotationRole.OWNER,
                        ports = rotationPorts,
                    )
                }
                await(atStamp, startupWaitMs)
                val receive = launch(kotlinx.coroutines.Dispatchers.Default) {
                    GroupPushRouting.receiveGated(
                        accountIdB64 = account,
                        groupIdHex = g,
                        secret = s1,
                        mintEpoch = 11uL,
                        senderSignatureKey = adminKey,
                        owner = knownRoles(),
                        ownGroupEpoch = 12uL,
                        ports = receivePorts,
                    )
                    receiveDone.countDown()
                }
                kotlinx.coroutines.joinAll(rotation, receive)
            }

            // Whichever of the two ends up holding the row, the invariant is
            // the same and it is the one F-E broke: there is exactly ONE live
            // routing id, and every id this device ever armed other than that
            // one was retired. An orphan shows up as a second live row.
            val live = hex(rotationPorts.derivePushGid(store.secret!!))
            assertEquals(
                "exactly one live route row — an orphaned gid is a second one",
                setOf(live),
                store.routes.toSet(),
            )
            val stranded = store.subscribed.toSet() - store.unsubscribed.toSet() - setOf(live)
            assertEquals(
                "no gid was left subscribed but un-retirable",
                emptySet<String>(),
                stranded,
            )
            assertFalse("gid(S1) must not be a second live id", store.routes.contains(gidS1))
            assertTrue("the pre-rotation id was retired", store.unsubscribed.contains(gidOld))
        }

    @Test
    fun `concurrent provisioning still mints exactly once`() =
        kotlinx.coroutines.runBlocking {
            // Collapsing `ensure`'s own lock into the shared serializing one
            // must not weaken mint-once. It does not, and the mechanism is
            // worth naming because it is a STRONGER one than a coalescing
            // lock: `ensure` re-reads `hasSecret` / `isDistributed` INSIDE
            // the critical section, so a queued second pass sees the row the
            // first one committed and takes EnsureAction.NONE. Coalescing
            // only ever held while two calls genuinely overlapped; the
            // in-lock re-read holds however they are staggered.
            val store = RaceStore()
            val g = "83".repeat(32)
            val ports = List(4) { RacePorts(store, stampEpoch = 5uL) }

            kotlinx.coroutines.coroutineScope {
                ports.forEach { p ->
                    launch(kotlinx.coroutines.Dispatchers.Default) {
                        GroupPushRouting.ensure(
                            accountIdB64 = account,
                            groupIdHex = g,
                            isOwner = true,
                            ports = p,
                        )
                    }
                }
            }

            assertEquals("minted exactly once for the group", 1, store.mintCount.get())
            assertEquals(
                "exactly one live route row",
                setOf(hex(ports[0].derivePushGid(store.secret!!))),
                store.routes.toSet(),
            )
        }

    // -----------------------------------------------------------------
    // ADR-0045 UX §6.2.8 item 5 — the evicted device retires its OWN row
    // -----------------------------------------------------------------

    @Test
    fun `an evicted device retires its own push_gid and drops the route row`() = runTest {
        // ADR-0043 recorded this row as never deleted, because "nobody else can
        // delete it and no client has 'you were removed' handling": the
        // endpoint deletes exactly the `(device, group)` row named in the
        // request and a client knows only its OWN device pseudonym. This IS
        // that handling, and it closes the other half of G-36 residual 2.
        val ports = FakePorts(account)
        val gid = ports.derivePushGid(secretA)
        ports.persist(groupHex, secretA, mintEpoch = 5uL, pushGid = gid, distributed = true)

        val done = GroupPushRouting.retireOwnSubscription(account, groupHex, ports)

        assertTrue("the step is done", done)
        assertEquals(1, ports.unsubscribeCount)
        assertEquals(hex(gid), hex(ports.unsubscribed.single()))
        assertTrue("route row dropped", ports.routes.isEmpty())
    }

    @Test
    fun `holding no secret is DONE rather than owed`() = runTest {
        // Nothing was ever subscribed under a `push_gid` for this group, so
        // there is nothing to retire — done, not failed, so the caller's
        // teardown latch keeps its claim and does not retry forever.
        val ports = FakePorts(account)
        val done = GroupPushRouting.retireOwnSubscription(account, groupHex, ports)
        assertTrue(done)
        assertEquals(0, ports.unsubscribeCount)
    }

    @Test
    fun `an unreadable store leaves the retire OWED so a later pass retries`() = runTest {
        // The read decides WHICH gid gets unsubscribed, so a store this device
        // cannot read is not "nothing to do" — it is "cannot act yet".
        val inner = FakePorts(account)
        val gid = inner.derivePushGid(secretA)
        inner.persist(groupHex, secretA, mintEpoch = 5uL, pushGid = gid, distributed = true)

        val done = GroupPushRouting.retireOwnSubscription(
            account,
            groupHex,
            FlakyStore(Int.MAX_VALUE, inner),
        )

        assertFalse("the step stays owed", done)
        assertEquals(0, inner.unsubscribeCount)
        assertTrue("route row untouched", inner.routes.isNotEmpty())
    }

    @Test
    fun `an unreachable push-gw still reports DONE, leaving an inert row`() = runTest {
        // `retireOldPushGid` is bounded and swallows its own failure: the cost
        // of a failed unsubscribe is a stale row that nothing notifies again,
        // which is exactly the pre-ADR-0045 status quo. Reporting it as owed
        // would have the teardown retry a network call on every membership
        // emission for a group that is already inert.
        val ports = FakePorts(account)
        val gid = ports.derivePushGid(secretA)
        ports.persist(groupHex, secretA, mintEpoch = 5uL, pushGid = gid, distributed = true)
        ports.unsubscribeFails = true

        assertTrue(GroupPushRouting.retireOwnSubscription(account, groupHex, ports))
        assertEquals(1, ports.unsubscribeCount)
        assertTrue("unsubscribe threw, so nothing was recorded", ports.unsubscribed.isEmpty())
    }
}
