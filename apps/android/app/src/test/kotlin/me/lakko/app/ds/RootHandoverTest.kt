// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ds

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0057 S2 finding 1 — the journal root moves with an ownership transfer:
 * when the outgoing owner sends, what it sends, the new owner's receive gate,
 * the takeover (204 / 401 / transient), and that a root seed never reaches the
 * registrar-seed store.
 */
class RootHandoverTest {

    private val journal = ByteArray(32) { 3 }
    private fun key(b: Int) = ByteArray(32) { b.toByte() }

    private val me = key(0x11)
    private val other = key(0x22)
    private val third = key(0x33)

    // ------------------------------------------------------------ sending rule

    @Test
    fun `sending fires when ownership moves from this account to another`() {
        assertArrayEquals(other, ownerMovedAwayFrom(me, before = me, after = other))
    }

    @Test
    fun `a no-op commit, staying owner or becoming owner never sends`() {
        assertNull(ownerMovedAwayFrom(me, before = me, after = me))
        assertNull(ownerMovedAwayFrom(me, before = other, after = other))
        assertNull(ownerMovedAwayFrom(me, before = other, after = me))
        assertNull(ownerMovedAwayFrom(me, before = other, after = third))
    }

    @Test
    fun `an unreadable table or an unknown account never sends`() {
        assertNull(ownerMovedAwayFrom(me, before = null, after = other))
        assertNull(ownerMovedAwayFrom(me, before = me, after = null))
        assertNull(ownerMovedAwayFrom(null, before = me, after = other))
    }

    // ------------------------------------------------------------ sending

    private class SendRecorder {
        var composedFor: ByteArray? = null
        var composedX25519: ByteArray? = null
        var composedSeed: ByteArray? = null
        val sent = mutableListOf<ByteArray>()
        var lastSeed: ByteArray? = null
    }

    private suspend fun send(
        rec: SendRecorder,
        owners: ArrayDeque<ByteArray?>,
        x25519: ByteArray? = key(0x44),
        own: ByteArray? = me,
        sendThrows: Boolean = false,
    ): HandoverSendOutcome = sendRootHandover(
        ownAccountKey = { own },
        liveOwner = { if (owners.size > 1) owners.removeFirst() else owners.first() },
        ownerX25519 = { x25519 },
        rootSeed = { key(0x55).also { rec.lastSeed = it } },
        compose = { newOwner, x, seed ->
            rec.composedFor = newOwner
            rec.composedX25519 = x
            rec.composedSeed = seed.copyOf()
            byteArrayOf(9, 9, 9)
        },
        sendEnvelope = { envelope ->
            if (sendThrows) error("network down")
            rec.sent += envelope
        },
    )

    @Test
    fun `the envelope carries the old root seed sealed to the new owner's X25519 key`() = runTest {
        val rec = SendRecorder()
        val outcome = send(rec, ArrayDeque(listOf(other)))
        assertEquals(HandoverSendOutcome.SENT, outcome)
        assertArrayEquals(other, rec.composedFor)
        assertArrayEquals(key(0x44), rec.composedX25519)
        assertArrayEquals(key(0x55), rec.composedSeed)
        assertEquals(1, rec.sent.size)
        // The root seed handed to compose is wiped afterwards.
        assertTrue(rec.lastSeed!!.all { it == 0.toByte() })
    }

    @Test
    fun `nothing is sent while this account is still owner`() = runTest {
        val rec = SendRecorder()
        assertEquals(HandoverSendOutcome.CANCELLED, send(rec, ArrayDeque(listOf(me))))
        assertTrue(rec.sent.isEmpty())
        assertNull(rec.composedSeed)
    }

    @Test
    fun `this account becoming owner again between the reads cancels`() = runTest {
        val rec = SendRecorder()
        assertEquals(HandoverSendOutcome.CANCELLED, send(rec, ArrayDeque(listOf(other, me))))
        assertTrue(rec.sent.isEmpty())
    }

    @Test
    fun `an unresolved X25519 key or a changed owner is retried later`() = runTest {
        val rec = SendRecorder()
        val pending = send(rec, ArrayDeque(listOf(other)), x25519 = null)
        assertEquals(HandoverSendOutcome.PENDING, pending)
        assertEquals(HandoverSendOutcome.PENDING, send(rec, ArrayDeque(listOf(other, third))))
        assertTrue(rec.sent.isEmpty())
    }

    @Test
    fun `a failed send is retried later`() = runTest {
        val rec = SendRecorder()
        val outcome = send(rec, ArrayDeque(listOf(other)), sendThrows = true)
        assertEquals(HandoverSendOutcome.FAILED, outcome)
    }

    @Test
    fun `signed out cancels`() = runTest {
        assertEquals(HandoverSendOutcome.CANCELLED, send(SendRecorder(), ArrayDeque(listOf(other)), own = null))
    }

    // ------------------------------------------------------------ owed record

    @Test
    fun `the owed record adds, removes and clears per account`() {
        val store = InMemoryOwedHandoverStore()
        store.add("acct", "AB")
        store.add("acct", "cd")
        store.add("other", "ab")
        assertEquals(setOf("ab", "cd"), store.groups("acct"))
        store.remove("acct", "ab")
        assertEquals(setOf("cd"), store.groups("acct"))
        store.clearForAccount("acct")
        assertTrue(store.groups("acct").isEmpty())
        assertEquals(setOf("ab"), store.groups("other"))
    }

    // ------------------------------------------------------------ receive gate

    private fun header(rank: SeedEnvelopeRank, to: ByteArray = me, journalId: ByteArray = journal) =
        SeedEnvelopeHeader(journalId, to, rank)

    @Test
    fun `the root gate accepts only this journal, addressed to us, while we are owner`() {
        val h = header(SeedEnvelopeRank.ROOT)
        assertEquals(RootHandoverVerdict.ACCEPT, rootHandoverVerdict(me, h, journal, ownerAccountKey = me))
        assertEquals(RootHandoverVerdict.NOT_ADDRESSED_TO_US, rootHandoverVerdict(me, header(SeedEnvelopeRank.ROOT, to = other), journal, me))
        assertEquals(RootHandoverVerdict.NOT_OWNER, rootHandoverVerdict(me, h, journal, ownerAccountKey = other))
        assertEquals(RootHandoverVerdict.NOT_OWNER, rootHandoverVerdict(me, h, journal, ownerAccountKey = null))
        assertEquals(RootHandoverVerdict.WRONG_JOURNAL, rootHandoverVerdict(me, h, key(0x77), me))
        assertEquals(RootHandoverVerdict.UNRESOLVED, rootHandoverVerdict(null, h, journal, me))
    }

    private class ReceiveRecorder {
        val stored = mutableListOf<ByteArray>()
        val takenOver = mutableListOf<ByteArray>()
        var opened = 0
    }

    private fun receive(
        rec: ReceiveRecorder,
        header: SeedEnvelopeHeader,
        sender: ByteArray? = other,
        owner: ByteArray? = me,
        opens: ByteArray? = key(0x66),
    ): SeedEnvelopeOutcome = receiveSeedEnvelope(
        header = header,
        groupId = journal,
        ownAccountKey = me,
        senderAccountKey = sender,
        ownerAccountKey = owner,
        open = { rec.opened += 1; opens?.copyOf() },
        storeRegistrarSeed = { rec.stored += it.copyOf() },
        takeOver = { rec.takenOver += it },
    )

    @Test
    fun `an accepted root handover starts the takeover and is never stored`() {
        val rec = ReceiveRecorder()
        // Even when the sender is the owner (the registrar gate would accept),
        // a root-rank envelope never reaches the registrar-seed store.
        val outcome = receive(rec, header(SeedEnvelopeRank.ROOT), sender = me, owner = me)
        assertEquals(SeedEnvelopeOutcome.HANDOVER_STARTED, outcome)
        assertTrue(rec.stored.isEmpty())
        assertEquals(1, rec.takenOver.size)
        assertArrayEquals(key(0x66), rec.takenOver.single())
    }

    @Test
    fun `a root handover not addressed to us, or received while not owner, is dropped unopened`() {
        val rec = ReceiveRecorder()
        assertEquals(SeedEnvelopeOutcome.DROPPED, receive(rec, header(SeedEnvelopeRank.ROOT, to = other)))
        assertEquals(SeedEnvelopeOutcome.DROPPED, receive(rec, header(SeedEnvelopeRank.ROOT), owner = other))
        assertEquals(SeedEnvelopeOutcome.DROPPED, receive(rec, header(SeedEnvelopeRank.ROOT, journalId = key(0x77))))
        assertEquals(0, rec.opened)
        assertTrue(rec.stored.isEmpty())
        assertTrue(rec.takenOver.isEmpty())
    }

    @Test
    fun `a root handover that does not open, or opens to the wrong width, is dropped`() {
        val rec = ReceiveRecorder()
        assertEquals(SeedEnvelopeOutcome.DROPPED, receive(rec, header(SeedEnvelopeRank.ROOT), opens = null))
        assertEquals(SeedEnvelopeOutcome.DROPPED, receive(rec, header(SeedEnvelopeRank.ROOT), opens = ByteArray(16) { 1 }))
        assertTrue(rec.takenOver.isEmpty())
        assertTrue(rec.stored.isEmpty())
    }

    @Test
    fun `the registrar gate is unchanged for member and admin ranks`() {
        val rec = ReceiveRecorder()
        assertEquals(
            SeedEnvelopeOutcome.REGISTRAR_STORED,
            receive(rec, header(SeedEnvelopeRank.ADMIN), sender = other, owner = other),
        )
        assertEquals(1, rec.stored.size)
        // Sender is not the owner: refused.
        assertEquals(SeedEnvelopeOutcome.DROPPED, receive(rec, header(SeedEnvelopeRank.MEMBER), sender = third, owner = other))
        assertEquals(1, rec.stored.size)
        assertTrue(rec.takenOver.isEmpty())
    }

    // ------------------------------------------------------------ takeover

    private class TakeOverRecorder {
        val puts = mutableListOf<String>()
        var transferNewRoot: ByteArray? = null
        var transferSeed: ByteArray? = null
        var reset = 0
        var reconciled = 0
        var marked = 0
        var probes = 0
        val sleeps = mutableListOf<Long>()
    }

    private suspend fun takeOver(
        rec: TakeOverRecorder,
        seed: ByteArray,
        newRoot: RecordingStatementSigner = RecordingStatementSigner("new-root"),
        stillOwner: Boolean = true,
        rootFailures: MutableList<Throwable> = mutableListOf(),
        probeSaysHeld: Boolean = false,
    ): TakeOverOutcome = takeOverJournal(
        journalId = journal,
        oldRootSeed = seed,
        ownRootPublicKey = { newRoot.publicKey },
        stillOwner = { stillOwner },
        oldRootTransfer = { s, root ->
            rec.transferSeed = s.copyOf()
            rec.transferNewRoot = root
            DsStatement("/ds/group/j/root", "{}")
        },
        newRoot = { newRoot },
        desiredRegistrars = { mapOf(hexOf(other) to JournalRank.ADMIN, hexOf(third) to JournalRank.MEMBER) },
        newRegistrarPublicKey = { account -> ByteArray(32) { (account[0] + 1).toByte() } },
        probeOwnRoot = {
            rec.probes += 1
            probeSaysHeld
        },
        markRootHeld = { rec.marked += 1 },
        put = { make ->
            val statement = make()
            rec.puts += statement.path
            if (statement.path.endsWith("/root") && rootFailures.isNotEmpty()) throw rootFailures.removeAt(0)
        },
        resetAppointments = { rec.reset += 1 },
        reconcile = { rec.reconciled += 1 },
        sleep = { rec.sleeps += it },
    )

    @Test
    fun `a 204 moves the root, marks it held, resets the registrar set from the roster and re-appoints`() = runTest {
        val rec = TakeOverRecorder()
        val newRoot = RecordingStatementSigner("new-root")
        val seed = key(0x66)
        val outcome = takeOver(rec, seed, newRoot = newRoot)
        assertEquals(TakeOverOutcome.TOOK_OVER, outcome)
        // The root statement is signed with the handed-over seed and names
        // this account's own derived root.
        assertArrayEquals(key(0x66), rec.transferSeed)
        assertArrayEquals(newRoot.publicKey, rec.transferNewRoot)
        assertEquals(1, rec.marked)
        // Then ONE registrars-set under the NEW root with the whole desired
        // set (no one-by-one removal of the old registrars).
        assertEquals(listOf("/ds/group/j/root", "/ds/group/${hexOf(journal)}/registrars-set"), rec.puts)
        assertEquals(listOf("registrars-set"), newRoot.calls)
        assertEquals(listOf(JournalRank.ADMIN, JournalRank.MEMBER), newRoot.lastRegistrarSet.map { it.rank })
        assertArrayEquals(ByteArray(32) { 0x23 }, newRoot.lastRegistrarSet[0].publicKey)
        assertArrayEquals(ByteArray(32) { 0x34 }, newRoot.lastRegistrarSet[1].publicKey)
        assertEquals(1, rec.reset)
        assertEquals(1, rec.reconciled)
    }

    @Test
    fun `a 401 drops the handover without retrying`() = runTest {
        val rec = TakeOverRecorder()
        val newRoot = RecordingStatementSigner("new-root")
        val outcome = takeOver(rec, key(0x66), newRoot = newRoot, rootFailures = mutableListOf(DsHttpError(401, "/root", "")))
        assertEquals(TakeOverOutcome.STALE, outcome)
        assertTrue(outcome.dropsPendingSeed())
        assertEquals(listOf("/ds/group/j/root"), rec.puts)
        assertEquals(1, rec.probes)
        assertEquals(0, rec.marked)
        assertTrue(rec.sleeps.isEmpty())
        assertTrue(newRoot.calls.isEmpty())
        assertEquals(0, rec.reconciled)
    }

    @Test
    fun `a 401 after another device of this account took over marks the root held`() = runTest {
        val rec = TakeOverRecorder()
        val outcome = takeOver(rec, key(0x66), rootFailures = mutableListOf(DsHttpError(401, "/root", "")), probeSaysHeld = true)
        assertEquals(TakeOverOutcome.HELD_ELSEWHERE, outcome)
        assertTrue(outcome.dropsPendingSeed())
        assertEquals(1, rec.marked)
        assertEquals(1, rec.reconciled)
    }

    @Test
    fun `another 4xx is not retried and keeps the pending seed`() = runTest {
        val rec = TakeOverRecorder()
        val outcome = takeOver(rec, key(0x66), rootFailures = mutableListOf(DsHttpError(403, "/root", "")))
        assertEquals(TakeOverOutcome.FAILED, outcome)
        assertFalse(outcome.dropsPendingSeed())
        assertEquals(1, rec.puts.size)
        assertTrue(rec.sleeps.isEmpty())
        assertEquals(0, rec.reconciled)
        assertEquals(0, rec.marked)
    }

    @Test
    fun `network errors and 5xx are retried twice, then kept for a later attempt`() = runTest {
        val rec = TakeOverRecorder()
        val seed = key(0x66)
        val outcome = takeOver(
            rec,
            seed,
            rootFailures = mutableListOf(
                java.io.IOException("reset"),
                DsHttpError(503, "/root", ""),
                DsHttpError(500, "/root", ""),
            ),
        )
        assertEquals(TakeOverOutcome.FAILED, outcome)
        assertFalse(outcome.dropsPendingSeed())
        assertEquals(3, rec.puts.size)
        assertEquals(ROOT_HANDOVER_RETRY_DELAYS_MS, rec.sleeps)
        assertEquals(0, rec.reconciled)
        // The takeover does not wipe the seed: its owner keeps it for the retry.
        assertArrayEquals(key(0x66), seed)
    }

    @Test
    fun `a transient failure followed by a 204 takes over`() = runTest {
        val rec = TakeOverRecorder()
        val outcome = takeOver(rec, key(0x66), rootFailures = mutableListOf(java.io.IOException("reset")))
        assertEquals(TakeOverOutcome.TOOK_OVER, outcome)
        assertEquals(1, rec.sleeps.size)
        assertEquals(1, rec.reconciled)
    }

    @Test
    fun `no root statement is sent once this account is no longer owner`() = runTest {
        val rec = TakeOverRecorder()
        assertEquals(TakeOverOutcome.NOT_OWNER, takeOver(rec, key(0x66), stillOwner = false))
        assertTrue(rec.puts.isEmpty())
    }

    @Test
    fun `transient failures are everything but an HTTP status below 500`() {
        assertTrue(isTransientStatementFailure(java.io.IOException("x")))
        assertTrue(isTransientStatementFailure(DsHttpError(502, "/p", "")))
        assertFalse(isTransientStatementFailure(DsHttpError(401, "/p", "")))
        assertFalse(isTransientStatementFailure(DsHttpError(409, "/p", "")))
    }

    // ------------------------------------------------------------ the receiver's pending seed

    @Test
    fun `a pending root seed survives a restart and is wiped after the takeover`() = runTest {
        // One store instance per "process": the backing map stands for disk.
        val disk = InMemoryPendingRootSeedStore()
        val account = "acct=="
        val group = "ab".repeat(32)
        disk.put(account, group, key(0x66))
        // Restart: a fresh reader finds it and runs the takeover with it.
        val restored = disk.get(account, group)!!
        assertEquals(setOf(group), disk.groups(account))
        val failed = takeOver(TakeOverRecorder(), restored, rootFailures = mutableListOf(java.io.IOException("x"), java.io.IOException("y"), java.io.IOException("z")))
        assertFalse(failed.dropsPendingSeed())
        assertArrayEquals(key(0x66), disk.get(account, group))
        val landed = takeOver(TakeOverRecorder(), disk.get(account, group)!!)
        assertTrue(landed.dropsPendingSeed())
        disk.remove(account, group)
        assertNull(disk.get(account, group))
        assertTrue(disk.groups(account).isEmpty())
    }

    @Test
    fun `root-held markers and pending seeds are per account and cleared on forget`() {
        val held = InMemoryRootHeldStore()
        held.mark("a", "g1")
        assertTrue(held.isHeld("a", "G1"))
        assertFalse(held.isHeld("b", "g1"))
        held.clearForAccount("a")
        assertFalse(held.isHeld("a", "g1"))
        val seeds = InMemoryPendingRootSeedStore()
        seeds.put("a", "g1", key(1))
        seeds.clearForAccount("a")
        assertNull(seeds.get("a", "g1"))
    }

    // ------------------------------------------------------------ the outgoing owner's owed record

    @Test
    fun `the outgoing owner re-sends while its probe answers 204 and stops at 401`() = runTest {
        var probe = true
        var sends = 0
        var sentAt: Long? = null
        var now = 1_000L
        suspend fun step(): OwedHandoverOutcome = runOwedHandover(
            probeOwnRoot = { probe },
            lastSentAtMs = { sentAt },
            nowMs = { now },
            send = {
                sends += 1
                HandoverSendOutcome.SENT
            },
        ).also { if (it == OwedHandoverOutcome.SENT) sentAt = now }
        assertEquals(OwedHandoverOutcome.SENT, step())
        assertFalse(OwedHandoverOutcome.SENT.settlesObligation())
        now += HANDOVER_RESEND_MS - 1
        assertEquals(OwedHandoverOutcome.THROTTLED, step())
        now += 1
        assertEquals(OwedHandoverOutcome.SENT, step())
        assertEquals(2, sends)
        probe = false
        val done = step()
        assertEquals(OwedHandoverOutcome.DONE, done)
        assertTrue(done.settlesObligation())
        assertEquals(2, sends)
    }

    @Test
    fun `a probe that cannot run keeps the record and sends nothing`() = runTest {
        var sends = 0
        val outcome = runOwedHandover(
            probeOwnRoot = { throw java.io.IOException("offline") },
            lastSentAtMs = { null },
            nowMs = { 0L },
            send = {
                sends += 1
                HandoverSendOutcome.SENT
            },
        )
        assertEquals(OwedHandoverOutcome.RETRY, outcome)
        assertFalse(outcome.settlesObligation())
        assertEquals(0, sends)
    }

    @Test
    fun `this account becoming owner again settles the record`() = runTest {
        val outcome = runOwedHandover({ true }, { null }, { 0L }, { HandoverSendOutcome.CANCELLED })
        assertEquals(OwedHandoverOutcome.CANCELLED, outcome)
        assertTrue(outcome.settlesObligation())
        assertEquals(
            OwedHandoverOutcome.RETRY,
            runOwedHandover({ true }, { null }, { 0L }, { HandoverSendOutcome.PENDING }),
        )
    }
}
