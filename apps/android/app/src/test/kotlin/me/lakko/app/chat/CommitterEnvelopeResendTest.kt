// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import me.lakko.app.reach.ReachManager
import me.lakko.app.ui.chat.InMemoryContext
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0060 section 6 — a proposer re-sends its committer envelope while its
 * Add waits. WHEN one is due is the core's rule, which the host JVM cannot
 * load, so it is passed in here as its contract
 * (`lakko_ds_proof::committer_envelope_due`); what is under test is what the
 * rule is fed, which Adds it is asked about, and what the record keeps.
 */
class CommitterEnvelopeResendTest {

    private val account = "acct"
    private val group = "grp"

    private val owner = "b3duZXI="
    private val admin = "YWRtaW4="
    private val newAdmin = "bmV3YWRtaW4="
    private val carol = "Y2Fyb2w="
    private val carolPhone = "Y2Fyb2wtcGhvbmU="
    private val carolTablet = "Y2Fyb2wtdGFibGV0"
    private val carolAccountId = "Y2Fyb2wtYWNjb3VudA=="

    private val hour = 60L * 60L * 1000L
    private val resendAfter = 12L * hour

    /** The core's `committer_envelope_due`, as its contract. */
    private fun coreRule(row: SentCommitterEnvelope, candidates: List<String>, nowMs: Long): Boolean {
        val sentAt = row.sentAtMs ?: return true
        if (candidates.any { it !in row.coveredB64 }) return true
        return row.signed && (nowMs < sentAt || nowMs - sentAt >= resendAfter)
    }

    private class MemStore : SentCommitterEnvelopeStore {
        val rows = mutableMapOf<String, Map<String, SentCommitterEnvelope>>()
        override fun load(accountIdB64: String, groupIdHex: String) =
            rows["$accountIdB64/$groupIdHex"] ?: emptyMap()

        override fun save(accountIdB64: String, groupIdHex: String, rows: Map<String, SentCommitterEnvelope>) {
            this.rows["$accountIdB64/$groupIdHex"] = rows
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.filter { it.startsWith("$accountIdB64/") }.forEach { rows.remove(it) }
        }
    }

    /** One device's view: the store, what the reach manager holds, the wire. */
    private inner class Device(val store: SentCommitterEnvelopeStore = MemStore()) {
        val inFlight: MutableSet<String> = ConcurrentHashMap.newKeySet()
        val nothing = CommitterEnvelopeNothingToSend()
        var holds: ReachManager.ComposedDelegations? = ReachManager.ComposedDelegations(byteArrayOf(1), signed = true)
        val published = mutableListOf<ByteArray>()
        var composed = 0
        var publishThrows = false
        var publishAllowed = true
        var nowMs = 0L

        suspend fun firstSend(candidates: List<String>, device: String = carolPhone): CommitterEnvelopeSend {
            noteCommitterEnvelopeOwed(store, account, group, carol, device)
            return send(candidates)
        }

        suspend fun send(candidates: List<String>): CommitterEnvelopeSend =
            sendCommitterEnvelopeOnce(
                store = store,
                accountIdB64 = account,
                groupIdHex = group,
                addedKeyB64 = carol,
                candidatesB64 = candidates,
                inFlight = inFlight,
                nothingToSend = nothing,
                compose = {
                    composed += 1
                    // Fresh delegations each time: new bytes.
                    holds?.let { ReachManager.ComposedDelegations(byteArrayOf(composed.toByte()), it.signed) }
                },
                publish = { envelope ->
                    check(!publishThrows) { "ds down" }
                    if (publishAllowed) published += envelope
                    publishAllowed
                },
                nowMs = { nowMs },
            )

        suspend fun tail(
            candidates: List<String>,
            intents: List<RetryableRequest> = listOf(RetryableRequest.Add(carolPhone, carolAccountId)),
            snapshot: RetryGroupSnapshot = waiting(),
        ): CommitterEnvelopeResendResult =
            runCommitterEnvelopeResend(
                store = store,
                accountIdB64 = account,
                groupIdHex = group,
                ownIntents = intents,
                snapshot = snapshot,
                candidatesB64 = candidates,
                nowMs = nowMs,
                due = ::coreRule,
                send = { send(candidates) },
            )

        val row: SentCommitterEnvelope? get() = store.load(account, group)[carol]
    }

    private fun waiting(pending: List<String> = listOf("add:$carolPhone")) = snapshot(pending = pending)

    private fun snapshot(
        roster: List<RetryRosterMember>? = listOf(RetryRosterMember(0u, owner), RetryRosterMember(1u, admin)),
        pending: List<String> = emptyList(),
        membershipEnded: Boolean = false,
    ) = RetryGroupSnapshot(
        roster = roster,
        pendingKeys = pending,
        pendingCount = pending.size,
        roleTable = RetryRoleTable(owner, listOf(admin)),
        membershipEnded = membershipEnded,
        retainedJoinKeys = emptyList(),
    )

    private val candidates get() = listOf(owner, admin)

    @Test
    fun `the first send is recorded with what it covered, only after it was published`() = runBlocking {
        val d = Device()
        d.nowMs = 1_000
        noteCommitterEnvelopeOwed(d.store, account, group, carol, carolPhone)
        // On the wire with nothing sent yet: the row says so.
        assertNull(d.row?.sentAtMs)
        assertEquals(listOf(carolPhone), d.row?.deviceKeysB64)

        assertEquals(CommitterEnvelopeSend.SENT, d.send(candidates))
        assertEquals(1_000L, d.row?.sentAtMs)
        assertEquals(true, d.row?.signed)
        assertEquals(candidates, d.row?.coveredB64)
        assertEquals(1, d.published.size)
    }

    @Test
    fun `a first send that failed leaves the row due, and the next tail sends it`() = runBlocking {
        val d = Device()
        d.publishThrows = true
        try {
            d.firstSend(candidates)
            fail("the publish failure is the caller's to log")
        } catch (_: IllegalStateException) {
            // expected
        }
        assertNull(d.row?.sentAtMs)

        d.publishThrows = false
        d.nowMs = 5_000
        assertEquals(1, d.tail(candidates).sent)
        assertEquals(5_000L, d.row?.sentAtMs)
    }

    @Test
    fun `a young envelope is not sent again`() = runBlocking {
        val d = Device()
        d.firstSend(candidates)
        d.nowMs = resendAfter - 1
        assertEquals(CommitterEnvelopeResendResult(sent = 0, forgotten = 0), d.tail(candidates))
        assertEquals(1, d.published.size)
        // Not even composed: signing is work.
        assertEquals(1, d.composed)
    }

    @Test
    fun `after twelve hours it is re-sent with fresh delegations and the record moves`() = runBlocking {
        val d = Device()
        d.firstSend(candidates)
        d.nowMs = resendAfter
        assertEquals(1, d.tail(candidates).sent)
        assertEquals(2, d.published.size)
        assertFalse(d.published[0].contentEquals(d.published[1]))
        assertEquals(resendAfter, d.row?.sentAtMs)

        // Young again until twelve hours after THAT send.
        d.nowMs = resendAfter * 2 - 1
        assertEquals(0, d.tail(candidates).sent)
        d.nowMs = resendAfter * 2
        assertEquals(1, d.tail(candidates).sent)
    }

    @Test
    fun `a new admin makes it due at once and the record then covers them`() = runBlocking {
        val d = Device()
        d.firstSend(candidates)
        d.nowMs = 1_000
        val now = candidates + newAdmin
        assertEquals(1, d.tail(now).sent)
        assertEquals(now, d.row?.coveredB64)
        // Covered now: nothing more is due.
        assertEquals(0, d.tail(now).sent)
    }

    @Test
    fun `a candidate who left makes nothing due`() = runBlocking {
        val d = Device()
        d.firstSend(candidates)
        d.nowMs = 1_000
        assertEquals(0, d.tail(listOf(owner)).sent)
    }

    @Test
    fun `an open envelope never ages, only a new candidate re-sends it`() = runBlocking {
        val d = Device()
        d.holds = ReachManager.ComposedDelegations(byteArrayOf(1), signed = false)
        d.firstSend(candidates)
        assertEquals(false, d.row?.signed)

        d.nowMs = 100 * resendAfter
        assertEquals(0, d.tail(candidates).sent)

        assertEquals(1, d.tail(candidates + newAdmin).sent)
        assertEquals(2, d.published.size)
    }

    @Test
    fun `an Add that landed is forgotten and nothing is sent`() = runBlocking {
        val d = Device()
        d.firstSend(candidates)
        d.nowMs = resendAfter
        val landed = snapshot(roster = listOf(RetryRosterMember(0u, owner), RetryRosterMember(1u, carolPhone)))
        assertEquals(CommitterEnvelopeResendResult(sent = 0, forgotten = 1), d.tail(candidates, snapshot = landed))
        assertNull(d.row)
        assertEquals(1, d.published.size)
    }

    @Test
    fun `an Add whose request ended is forgotten`() = runBlocking {
        val d = Device()
        d.firstSend(candidates)
        d.nowMs = resendAfter
        assertEquals(1, d.tail(candidates, intents = emptyList()).forgotten)
        assertNull(d.row)
    }

    @Test
    fun `another device's Add is not this device's to re-send`() = runBlocking {
        val d = Device()
        d.nowMs = resendAfter
        // The same person pending from another device: no record, no send.
        assertEquals(
            CommitterEnvelopeResendResult(sent = 0, forgotten = 0),
            d.tail(candidates, intents = emptyList()),
        )
        assertTrue(d.published.isEmpty())
        // An intent of this device for somebody else keeps no row alive either.
        d.firstSend(candidates)
        val other = listOf(RetryableRequest.Add(owner, carolAccountId))
        assertEquals(1, d.tail(candidates, intents = other).forgotten)
    }

    @Test
    fun `an Add a sweep dropped waits for the superseded pass, and a torn read decides nothing`() = runBlocking {
        val d = Device()
        d.firstSend(candidates)
        d.nowMs = resendAfter
        // Not pending and not in the roster: the re-request sends its own.
        assertEquals(
            CommitterEnvelopeResendResult(sent = 0, forgotten = 0),
            d.tail(candidates, snapshot = snapshot()),
        )
        assertEquals(
            CommitterEnvelopeResendResult(sent = 0, forgotten = 0),
            d.tail(candidates, snapshot = snapshot(roster = null)),
        )
        assertEquals(1, d.published.size)
        assertEquals(0L, d.row?.sentAtMs)
    }

    @Test
    fun `one device of the person still waiting keeps the row`() = runBlocking {
        val d = Device()
        d.firstSend(candidates, device = carolPhone)
        noteCommitterEnvelopeOwed(d.store, account, group, carol, carolTablet)
        assertEquals(listOf(carolPhone, carolTablet), d.row?.deviceKeysB64)

        d.nowMs = resendAfter
        val intents = listOf(
            RetryableRequest.Add(carolPhone, carolAccountId),
            RetryableRequest.Add(carolTablet, carolAccountId),
        )
        val partly = snapshot(
            roster = listOf(RetryRosterMember(0u, owner), RetryRosterMember(1u, carolPhone)),
            pending = listOf("add:$carolTablet"),
        )
        assertEquals(1, d.tail(candidates, intents = intents, snapshot = partly).sent)
    }

    @Test
    fun `nothing to send does not repeat at every poll`() = runBlocking {
        val d = Device()
        d.holds = null
        assertEquals(CommitterEnvelopeSend.NOTHING_TO_SEND, d.firstSend(candidates))
        assertEquals(1, d.composed)

        // Due every poll by the rule, composed no more.
        repeat(5) { assertEquals(0, d.tail(candidates).sent) }
        assertEquals(1, d.composed)
        assertNull(d.row?.sentAtMs)
    }

    @Test
    fun `a grant that arrives, or a new candidate, asks again`() = runBlocking {
        val d = Device()
        d.holds = null
        d.firstSend(candidates)

        // A new candidate is a new question.
        d.tail(candidates + newAdmin)
        assertEquals(2, d.composed)

        // What the owed-Welcome hook does when a grant is kept.
        d.nothing.forgetAll()
        d.holds = ReachManager.ComposedDelegations(byteArrayOf(1), signed = true)
        assertEquals(1, d.tail(candidates).sent)
        assertEquals(1, d.published.size)
    }

    @Test
    fun `a group that takes no message counts as nothing to send`() = runBlocking {
        val d = Device()
        d.publishAllowed = false
        assertEquals(CommitterEnvelopeSend.NOTHING_TO_SEND, d.firstSend(candidates))
        assertNull(d.row?.sentAtMs)
        repeat(3) { d.tail(candidates) }
        assertEquals(1, d.composed)
    }

    @Test
    fun `a send already out is not doubled`() = runBlocking {
        val d = Device()
        noteCommitterEnvelopeOwed(d.store, account, group, carol, carolPhone)
        d.inFlight += "$account/$group/$carol"
        assertEquals(CommitterEnvelopeSend.ALREADY_OUT, d.send(candidates))
        assertEquals(0, d.composed)
    }

    @Test
    fun `a failed re-send stays due and does not stop the pass`() = runBlocking {
        val d = Device()
        d.firstSend(candidates)
        d.nowMs = resendAfter
        d.publishThrows = true
        val failures = mutableListOf<Throwable>()
        val result = runCommitterEnvelopeResend(
            store = d.store,
            accountIdB64 = account,
            groupIdHex = group,
            ownIntents = listOf(RetryableRequest.Add(carolPhone, carolAccountId)),
            snapshot = waiting(),
            candidatesB64 = candidates,
            nowMs = d.nowMs,
            due = ::coreRule,
            send = { d.send(candidates) },
            onFailure = { failures += it },
        )
        assertEquals(0, result.sent)
        assertEquals(1, failures.size)
        assertEquals(0L, d.row?.sentAtMs)
        assertTrue(d.inFlight.isEmpty())
    }

    @Test
    fun `a cancellation is not a failed send`() {
        val d = Device()
        runBlocking { d.firstSend(candidates) }
        d.nowMs = resendAfter
        try {
            runBlocking {
                runCommitterEnvelopeResend(
                    store = d.store,
                    accountIdB64 = account,
                    groupIdHex = group,
                    ownIntents = listOf(RetryableRequest.Add(carolPhone, carolAccountId)),
                    snapshot = waiting(),
                    candidatesB64 = candidates,
                    nowMs = d.nowMs,
                    due = ::coreRule,
                    send = { throw CancellationException("session ended") },
                )
            }
            fail("a cancellation stops the pass")
        } catch (_: CancellationException) {
            // expected
        }
    }

    @Test
    fun `no candidates sends nothing, whatever the record says`() = runBlocking {
        val d = Device()
        noteCommitterEnvelopeOwed(d.store, account, group, carol, carolPhone)
        assertEquals(0, d.tail(emptyList()).sent)
        assertEquals(0, d.composed)
    }

    @Test
    fun `a send that lands after the Add was forgotten writes no row back`() {
        val store = MemStore()
        recordCommitterEnvelopeSent(store, account, group, carol, signed = true, coveredB64 = candidates, sentAtMs = 1L)
        assertTrue(store.load(account, group).isEmpty())
    }

    @Test
    fun `the record survives a new store instance, and goes with its account`() = runBlocking {
        val context = InMemoryContext()
        val d = Device(SharedPrefsSentCommitterEnvelopeStore(context))
        d.nowMs = 7_000
        d.firstSend(candidates)

        // A restart: nothing in memory, the row read back as written.
        val after = SharedPrefsSentCommitterEnvelopeStore(context)
        assertEquals(
            SentCommitterEnvelope(carol, listOf(carolPhone), 7_000L, signed = true, coveredB64 = candidates),
            after.load(account, group)[carol],
        )
        val fresh = Device(after)
        fresh.nowMs = 7_000 + resendAfter - 1
        assertEquals(0, fresh.tail(candidates).sent)
        fresh.nowMs = 7_000 + resendAfter
        assertEquals(1, fresh.tail(candidates).sent)

        // Other accounts' rows stay.
        after.save("other", group, after.load(account, group))
        after.clearForAccount(account)
        assertTrue(after.load(account, group).isEmpty())
        assertEquals(1, after.load("other", group).size)
    }

    @Test
    fun `a corrupt row reads as empty`() {
        val context = InMemoryContext()
        val prefs = context.getSharedPreferences("lakko-sent-committer-envelopes", 0)
        prefs.edit().putString("sent-committer-envelopes-v1:acct\u0000grp", "not json").apply()
        assertTrue(SharedPrefsSentCommitterEnvelopeStore(context).load(account, group).isEmpty())
    }

    @Test
    fun `saving no rows removes the row`() {
        val context = InMemoryContext()
        val store = SharedPrefsSentCommitterEnvelopeStore(context)
        store.save(account, group, mapOf(carol to SentCommitterEnvelope(carol, listOf(carolPhone))))
        store.save(account, group, emptyMap())
        assertTrue(context.getSharedPreferences("lakko-sent-committer-envelopes", 0).all.isEmpty())
    }
}
