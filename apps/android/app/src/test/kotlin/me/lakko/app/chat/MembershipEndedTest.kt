// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.l10n.en
import me.lakko.app.push.GroupPushRouting
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.concurrent.CyclicBarrier
import java.util.concurrent.atomic.AtomicInteger

/**
 * ADR-0045 UX §6.2 — the end of your own membership.
 *
 * The host suite renders no composable and cannot load the JNI library, so
 * every rule §6.2 states lives in [MembershipEnded.kt] where it is executable,
 * and `ChatScreen` / `ChatViewModel` render what it decides — the same split
 * `PendingRequests.kt`, `OwnershipTransfer.kt` and `CommitterBeacon.kt`
 * already use. Mirrors `apps/web/src/chat/membership-ended.test.ts` in
 * coverage.
 *
 * ## Which of these discriminate
 *
 * 32 deliberate regressions were introduced one at a time, `testDebugUnitTest`
 * was RUN against each, the failure recorded, and the probe reverted. The
 * messages below are quoted verbatim from those runs, not predicted. Every one
 * of the 32 produced at least one failure; the count in brackets is how many
 * tests went red.
 *
 *  1. `membershipState` answers `Active` on `isActive == false` — the eviction
 *     is never detected [17]: `eviction is detected once the removing commit
 *     has been applied: expected:<Ended(left=false)> but was:<Active>`. The
 *     composer, the teardown, the terminal row and the chat-list line all hang
 *     off this one predicate.
 *  2. An unknown `isActive` reads as evicted (the fail-closed reflex, which is
 *     the wrong direction here) [1]: `a torn read is NOT evidence of an
 *     eviction: expected:<Active> but was:<Ended(left=true)>`.
 *  3. The leave record is read BEFORE `isActive` — *asked to leave* becomes
 *     *left* [2]: `an active group with a leave request outstanding is still
 *     active: expected:<Active> but was:<Ended(left=true)>`.
 *  4. The fallback string is the LEFT one, narrowing and neutral swapped [2]:
 *     `expected:<You [are no longer a member of] this group.> but was:<You
 *     [left] this group.>`.
 *  5. `recordLeaveRequested` overwrites an existing stamp [1]:
 *     `expected:<1000> but was:<9000>`.
 *  6. `pendingTeardownSteps` ignores `alreadyRun` [7]: `a step is handed out
 *     exactly once under real concurrency: STOP_BEACON_TICKER expected:<1> but
 *     was:<32>`.
 *  7. [EndedTeardownLatch.claim] splits into an unsynchronized read-then-write
 *     [1 with `Thread.yield()`: `STOP_BEACON_TICKER expected:<1> but was:<2>`;
 *     2 with a 2 ms sleep: `expected:<1> but was:<32>` plus the IO-driven
 *     shape test]. Note this probe FAILED TO REPRODUCE against the first
 *     version of the concurrency test — see that test's own doc.
 *  8. `composerState` keeps the composer once the membership has ended [3]:
 *     `ComposerSlot$Composer cannot be cast to ComposerSlot$Withdrawn`.
 *  9. A whitespace-only draft is preserved [1]: `renders no draft block when
 *     the draft was empty: expected null, but was:<   >`.
 * 10. The kept draft is trimmed rather than verbatim [1]: `expected:<[  see you
 *     tomorrow  ]> but was:<[see you tomorrow]>`.
 * 11. [EndedTeardownLatch.release] does nothing [2]: `a released step is
 *     retried and only that one: expected:<[RETIRE_PUSH_SUBSCRIPTION]> but
 *     was:<[]>`.
 * 12. `readIsActive` answers `false` on a throw [1]: `a throwing read reads as
 *     unknown never as evicted: expected null, but was:<false>`.
 * 13. `chatListLine` distinguishes left from removed [2]: `the chat list says
 *     the same whether you left or were removed: expected:<You [left this
 *     group].> but was:<You [are no longer a member].>`.
 * 14. `membershipRotationDecision` drops the `selfEvicted` arm [2]: `an evicted
 *     admin does NOT rotate...: expected:<SKIP_SELF_EVICTED> but
 *     was:<ROTATE>`.
 * 15. ... or checks it after the membership diff [1]: `the eviction beats every
 *     other verdict...: expected:<SKIP_SELF_EVICTED> but
 *     was:<SKIP_UNKNOWN_MEMBERSHIP>`.
 * 16. `exitBlock` checks `membershipEnded` after the `Hidden` gate [1]: `beats
 *     every other arm including a group with no role table: expected:<Ended>
 *     but was:<Hidden>`.
 * 17. ... or only inside the non-owner arm [1]: same failure.
 * 18. `beaconEmitVerdict` consults the foreground gate first [1]: `expected:
 *     <MEMBERSHIP_ENDED> but was:<NOT_FOREGROUND>`.
 * 19. ... or has no eviction gate at all [1]: `expected:<MEMBERSHIP_ENDED> but
 *     was:<EMIT>`.
 * 20. `withoutGroup` mutates / reports an absent group as a write [1]:
 *     `expected null, but was:<MessageLogPayload(...)>`.
 * 21. The teardown driver swallows a failed retire instead of releasing it [1]:
 *     `a failed push retire is released so a later pass retries it:
 *     expected:<2> but was:<1>`.
 * 22. The standing consequence varies with left/removed [1].
 * 23. The removed copy names a remover [2]: `the terminal row names nobody in
 *     either direction: removed by`.
 * 24. The teardown is owed while the membership HOLDS [3]: `nothing is torn
 *     down while the membership holds: expected:<[]> but
 *     was:<[STOP_BEACON_TICKER, RETIRE_PUSH_SUBSCRIPTION]>`.
 * 25. `membershipEnded` is inverted [2].
 * 26. `decodeStamp` coerces a corrupt row to `0` instead of absent [1]:
 *     `expected null, but was:<0>`.
 * 27. `withoutGroup` returns a payload for an absent group [1].
 * 28. `selfEvicted` defaults to `true` [1]: `an omitted eviction flag behaves
 *     exactly as before: expected:<ROTATE> but was:<SKIP_SELF_EVICTED>`.
 * 29. One of the two teardown steps is dropped from the canonical list [6].
 * 30. The delete confirmation body is made sweeping rather than exact [1].
 * 31. The frozen-roster header is made a claim about the ROWS rather than the
 *     LIST [1].
 * 32. `messageActionsState` always returns `AVAILABLE` — the defect it closes,
 *     the per-message menu still offered on a dead group [2]:
 *     `expected:<WITHDRAWN> but was:<AVAILABLE>` and
 *     `disagreed about Ended(left=false) expected:<false> but was:<true>`.
 *     The second is the one that matters: it is the equivalence with
 *     `composerState`, so a menu that stops agreeing with the composer fails
 *     whichever of the two was changed.
 */
class MembershipEndedTest {
    init {
        EnglishForTests.install()
    }


    private companion object {
        const val ACCOUNT = "acct-A"
        const val GROUP = "6161616161616161"
        const val OTHER_GROUP = "6262626262626262"
    }

    /** The store convention this suite already uses for its siblings
     *  ([CommitterBeaconTest]'s `MemStore`, [PendingRequestsTest]'s fake): the
     *  production impl is `SharedPreferences`, which the host JVM cannot load,
     *  so the RULES are driven over the same interface the repository binds. */
    private class MemLeaveStore : LeaveRecordStore {
        val rows = mutableMapOf<String, Long>()
        private fun k(a: String, g: String) = "$a/$g"
        override fun load(accountIdB64: String, groupIdHex: String): Long? =
            rows[k(accountIdB64, groupIdHex)]

        override fun save(accountIdB64: String, groupIdHex: String, atMs: Long) {
            rows[k(accountIdB64, groupIdHex)] = atMs
        }

        override fun clear(accountIdB64: String, groupIdHex: String) {
            rows.remove(k(accountIdB64, groupIdHex))
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.filter { it.startsWith("$accountIdB64/") }.forEach { rows.remove(it) }
        }
    }

    /** The pending-observation store, so test 12 can run the real prune. */
    private class MemPendingStore : PendingRequestStore {
        val rows = mutableMapOf<String, MutableMap<String, RequestObservation>>()
        override fun load(accountIdB64: String, groupIdHex: String): Map<String, RequestObservation> =
            rows["$accountIdB64/$groupIdHex"]?.toMap() ?: emptyMap()

        override fun save(
            accountIdB64: String,
            groupIdHex: String,
            observations: Map<String, RequestObservation>,
        ) {
            rows["$accountIdB64/$groupIdHex"] = observations.toMutableMap()
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.filter { it.startsWith("$accountIdB64/") }.forEach { rows.remove(it) }
        }
    }

    // -----------------------------------------------------------------------
    // Detecting the state
    // -----------------------------------------------------------------------

    @Test
    fun `an active group is unaffected`() {
        val state = membershipState(isActive = true, leaveRequestedAtMs = null)
        assertEquals(MembershipState.Active, state)
        assertFalse(membershipEnded(state))
        assertNull(terminalRow(state))
        assertNull(chatListLine(state))
        assertEquals(ComposerSlot.Composer, composerState(state, "half a sentence"))
    }

    @Test
    fun `an active group with a leave request outstanding is still active`() {
        // §6.2.11 — NO optimistic transition. Between publishing a leave
        // request and the commit merging you are still fully in the group:
        // still receiving, still decrypting. The row annotation is §5.2's
        // "Waiting to leave." and nothing else changes.
        val state = membershipState(isActive = true, leaveRequestedAtMs = 1_000L)
        assertEquals(MembershipState.Active, state)
        assertEquals(ComposerSlot.Composer, composerState(state, ""))
    }

    @Test
    fun `eviction is detected once the removing commit has been applied`() {
        val state = membershipState(isActive = false, leaveRequestedAtMs = null)
        assertEquals(MembershipState.Ended(left = false), state)
        assertTrue(membershipEnded(state))
    }

    @Test
    fun `a torn read is NOT evidence of an eviction`() {
        // An unreadable handle says nothing about membership. Claiming
        // "You are no longer a member of this group." on it would be a false
        // statement the user cannot check; leaving the chat as it is today is
        // merely the status quo. So `null` fails toward Active.
        val state = membershipState(isActive = null, leaveRequestedAtMs = 4_000L)
        assertEquals(MembershipState.Active, state)
    }

    @Test
    fun `readIsActive reads the core's own predicate`() {
        assertEquals(true, readIsActive { true })
        assertEquals(false, readIsActive { false })
    }

    @Test
    fun `a throwing read reads as unknown never as evicted`() {
        // `Group.isActive()` is `@Throws(FfiException)` on this platform, so a
        // torn handle is a real, reachable case rather than a hypothetical.
        assertNull(readIsActive { throw IllegalStateException("torn group handle") })
    }

    // -----------------------------------------------------------------------
    // §6.2.3 — left or removed
    // -----------------------------------------------------------------------

    @Test
    fun `the durable leave record selects the LEFT copy`() {
        val state = membershipState(isActive = false, leaveRequestedAtMs = 99L)
        assertEquals(MembershipState.Ended(left = true), state)
        assertEquals(ENDED_LEFT, terminalRow(state)?.headline)
        assertEquals("You left this group.", ENDED_LEFT.en)
    }

    @Test
    fun `an absent record degrades to the REMOVED copy which is still true`() {
        // The fallback is the NEUTRAL string and the narrowing is what needs
        // positive evidence — the inverse of the usual reflex. "You are no
        // longer a member of this group" is true of someone who left as well as
        // of someone who was removed; "you left this group" is true of only
        // one. So a device that lost the record (the ADR-0040 crash window, a
        // reinstall, a restore) degrades to a sentence that is still correct.
        val state = membershipState(isActive = false, leaveRequestedAtMs = null)
        assertEquals(ENDED_REMOVED, terminalRow(state)?.headline)
        assertEquals("You are no longer a member of this group.", ENDED_REMOVED.en)
    }

    @Test
    fun `the standing consequence is the same either way`() {
        val left = membershipState(isActive = false, leaveRequestedAtMs = 1L)
        val removed = membershipState(isActive = false, leaveRequestedAtMs = null)
        assertEquals(ENDED_CONSEQUENCE, terminalRow(left)?.consequence)
        assertEquals(ENDED_CONSEQUENCE, terminalRow(removed)?.consequence)
        // Written to be true whether or not this device was offline across its
        // own removal and lost the tail of its history (§6.2.6): it claims
        // exactly what is claimed and no more.
        assertEquals(
            "The messages you already received are still here. No new ones will arrive.",
            ENDED_CONSEQUENCE.en,
        )
    }

    @Test
    fun `the terminal row names nobody in either direction`() {
        for (at in listOf(null, 12_345L)) {
            val row = terminalRow(membershipState(isActive = false, leaveRequestedAtMs = at))
            assertNotNull(row)
            val text = "${row?.headline?.en} ${row?.consequence?.en}".lowercase()
            for (accusation in listOf("removed by", "by an admin", "by the owner")) {
                assertFalse(accusation, text.contains(accusation))
            }
        }
    }

    // -----------------------------------------------------------------------
    // §6.2.3 / §9 — the durable record, and the prune that must not touch it
    // -----------------------------------------------------------------------

    @Test
    fun `the leave record round-trips per account and group`() {
        val store = MemLeaveStore()
        recordLeaveRequested(store, ACCOUNT, GROUP, 1_700_000L)
        assertEquals(1_700_000L, loadLeaveRequestedAt(store, ACCOUNT, GROUP))
        assertNull(loadLeaveRequestedAt(store, ACCOUNT, OTHER_GROUP))
        assertNull(loadLeaveRequestedAt(store, "acct-B", GROUP))
    }

    @Test
    fun `the leave record SURVIVES the pending-proposal prune`() {
        // This is the whole reason it is a separate field. The observation for
        // a leave lives under `remove:{ownLeaf}` and every fold prunes an
        // observation once the core no longer holds the request — and the sweep
        // that removes you is exactly what stops the core holding it. So the
        // poll pass that carries the eviction also destroys the only evidence
        // that you asked for it, and the order of those two steps is an
        // implementation detail no test pins.
        val pending = MemPendingStore()
        val leave = MemLeaveStore()
        recordPendingRequestObservation(pending, ACCOUNT, GROUP, "remove:3", 1_000L, own = true)
        recordLeaveRequested(leave, ACCOUNT, GROUP, 1_000L)

        // The eviction empties the core's pending set, so the fold prunes
        // everything.
        prunePendingRequestObservations(pending, ACCOUNT, GROUP, emptyList())

        assertTrue(pending.load(ACCOUNT, GROUP).isEmpty())
        assertEquals(1_000L, loadLeaveRequestedAt(leave, ACCOUNT, GROUP))
        assertEquals(
            MembershipState.Ended(left = true),
            membershipState(
                isActive = false,
                leaveRequestedAtMs = loadLeaveRequestedAt(leave, ACCOUNT, GROUP),
            ),
        )
    }

    @Test
    fun `the FIRST record wins - a re-request never re-stamps it`() {
        val store = MemLeaveStore()
        recordLeaveRequested(store, ACCOUNT, GROUP, 1_000L)
        recordLeaveRequested(store, ACCOUNT, GROUP, 9_000L)
        assertEquals(1_000L, loadLeaveRequestedAt(store, ACCOUNT, GROUP))
    }

    @Test
    fun `the leave record is discarded with the group record and with the account`() {
        val store = MemLeaveStore()
        recordLeaveRequested(store, ACCOUNT, GROUP, 1_000L)
        recordLeaveRequested(store, ACCOUNT, OTHER_GROUP, 2_000L)
        store.clear(ACCOUNT, GROUP)
        assertNull(loadLeaveRequestedAt(store, ACCOUNT, GROUP))
        assertEquals(2_000L, loadLeaveRequestedAt(store, ACCOUNT, OTHER_GROUP))

        recordLeaveRequested(store, "acct-B", GROUP, 3_000L)
        store.clearForAccount(ACCOUNT)
        assertNull(loadLeaveRequestedAt(store, ACCOUNT, OTHER_GROUP))
        assertEquals(3_000L, loadLeaveRequestedAt(store, "acct-B", GROUP))
    }

    @Test
    fun `a corrupt row reads as absent which degrades to the removed copy`() {
        // The `SharedPreferences` row is a decimal string. Everything else is
        // absent, and absent is the removed copy, which is still true.
        assertEquals(1_000L, SharedPrefsLeaveRecordStore.decodeStamp("1000"))
        assertNull(SharedPrefsLeaveRecordStore.decodeStamp("not-a-number"))
        assertNull(SharedPrefsLeaveRecordStore.decodeStamp(""))
        assertNull(SharedPrefsLeaveRecordStore.decodeStamp(null))
    }

    // -----------------------------------------------------------------------
    // §6.2.5 — the composer and the draft it was holding
    // -----------------------------------------------------------------------

    @Test
    fun `is REPLACED not disabled`() {
        // §5.3 argues a pending row's `Remove` must stay visible-but-disabled,
        // because hiding it would read as the permission having vanished when
        // it is merely suspended. The permission here has vanished
        // permanently, so the rule inverts: disable while the state is
        // TRANSIENT, remove when it is TERMINAL. A disabled Send on a group you
        // will never be in again is a control promising a future it does not
        // have.
        val slot = composerState(endedRemoved(), "")
        assertTrue(slot is ComposerSlot.Withdrawn)
        slot as ComposerSlot.Withdrawn
        assertEquals(COMPOSER_WITHDRAWN, slot.notice)
        assertEquals("You can no longer send messages to this group.", COMPOSER_WITHDRAWN.en)
    }

    @Test
    fun `preserves an unsent draft verbatim and labelled`() {
        // The commit lands during a poll, which can be mid-sentence. Silently
        // discarding what the user typed is a small data loss with no upside.
        // Preserved VERBATIM — leading and trailing whitespace included; this
        // decides whether to show the user's text, not what it should have
        // been.
        val slot = composerState(endedRemoved(), "  see you tomorrow  ")
        slot as ComposerSlot.Withdrawn
        assertEquals("  see you tomorrow  ", slot.draft)
        assertEquals(UNSENT_DRAFT_LABEL, slot.draftLabel)
        assertEquals("Unsent draft", UNSENT_DRAFT_LABEL.en)
    }

    @Test
    fun `renders no draft block when the draft was empty`() {
        // An `Unsent draft` heading over three spaces is furniture pretending
        // to be data.
        for (draft in listOf("", "   ", "\n\t")) {
            val slot = composerState(endedRemoved(), draft)
            slot as ComposerSlot.Withdrawn
            assertNull(draft, slot.draft)
            assertNull(draft, slot.draftLabel)
        }
    }

    // -----------------------------------------------------------------------
    // §6.2.5 — the per-message action menu, on the composer's own predicate
    // -----------------------------------------------------------------------

    @Test
    fun `the per-message menu is WITHDRAWN once the membership has ended`() {
        // §6.2.5's rule reached from the other end. Edit / Delete / React each
        // compose an application message, and `createApplicationMessage` gates
        // on `is_operational()`, so on a dead group all three are refused —
        // permanently, for the same reason and at the same instant as Send. A
        // greyed React item that can never become live again is the shape §5.3
        // reserves for a permission that is merely suspended.
        assertEquals(MessageActions.WITHDRAWN, messageActionsState(endedRemoved()))
        assertEquals(MessageActions.WITHDRAWN, messageActionsState(endedLeft()))
        assertEquals(MessageActions.AVAILABLE, messageActionsState(active()))
    }

    @Test
    fun `the menu answers the composer's question, so the two cannot drift`() {
        // This is the point of the pair, not a restatement of the case above.
        // The menu and the composer are refused by the SAME core gate, so they
        // must never disagree about whether this group is writable — and the
        // way two gates over one predicate drift is that somebody later adds a
        // condition to one of them. Enumerating every state and asserting the
        // equivalence is what catches that; asserting each in isolation is not.
        val states = listOf(
            active(),
            membershipState(isActive = null, leaveRequestedAtMs = null),
            endedRemoved(),
            endedLeft(),
        )
        for (state in states) {
            val composerLive = composerState(state, "draft") is ComposerSlot.Composer
            val actionsLive = messageActionsState(state) == MessageActions.AVAILABLE
            assertEquals("disagreed about $state", composerLive, actionsLive)
        }
    }

    @Test
    fun `the menu does not fail closed on an unreadable handle`() {
        // Same direction as `membershipState`'s own rule and for the same
        // reason: a torn handle is not evidence of an eviction, and
        // withdrawing the menu on one would hide working controls on a group
        // the user is still in.
        assertEquals(
            MessageActions.AVAILABLE,
            messageActionsState(membershipState(isActive = null, leaveRequestedAtMs = 5L)),
        )
    }

    // -----------------------------------------------------------------------
    // §6.2.4 / §6.2.7 — the chat row and the frozen roster
    // -----------------------------------------------------------------------

    @Test
    fun `the chat list row gains one muted line and nothing else`() {
        assertEquals(CHAT_LIST_ENDED_LINE, chatListLine(endedLeft()))
        assertEquals("You are no longer a member.", CHAT_LIST_ENDED_LINE.en)
    }

    @Test
    fun `the chat list says the same whether you left or were removed`() {
        // The row line is deliberately neutral between the two cases: the list
        // is a glance surface and the distinction belongs in the transcript.
        assertEquals(chatListLine(endedLeft()), chatListLine(endedRemoved()))
    }

    @Test
    fun `the frozen-roster header is a statement about the LIST not a row`() {
        // The role table is untouched by a `Remove` — a removal rewrites no
        // `GroupContext` — so a removed admin's frozen roster still names them
        // an admin. Nothing may present that as a live role, which is why the
        // header is about the list.
        assertEquals(
            "This list stopped updating when your membership ended.",
            FROZEN_ROSTER_HEADER.en,
        )
    }

    // -----------------------------------------------------------------------
    // §6.2.8 — what must stop, and what must be torn down
    // -----------------------------------------------------------------------

    @Test
    fun `nothing is torn down while the membership holds`() {
        assertEquals(emptyList<EndedTeardownStep>(), pendingTeardownSteps(active(), emptySet()))
    }

    @Test
    fun `every step is owed exactly once and the second pass is a no-op`() {
        val done = mutableSetOf<EndedTeardownStep>()
        val first = pendingTeardownSteps(endedRemoved(), done)
        assertEquals(ENDED_TEARDOWN_STEPS.toSet(), first.toSet())
        done.addAll(first)
        assertEquals(emptyList<EndedTeardownStep>(), pendingTeardownSteps(endedRemoved(), done))
        // Idempotent under repetition, which is what makes it safe to drive
        // from a flow collector that re-runs for unrelated reasons.
        assertEquals(emptyList<EndedTeardownStep>(), pendingTeardownSteps(endedRemoved(), done))
    }

    @Test
    fun `a partially-completed teardown resumes rather than restarting`() {
        val done = setOf(EndedTeardownStep.STOP_BEACON_TICKER)
        assertEquals(
            listOf(EndedTeardownStep.RETIRE_PUSH_SUBSCRIPTION),
            pendingTeardownSteps(endedRemoved(), done),
        )
    }

    @Test
    fun `the latch claims what it hands out so a re-entry gets nothing`() {
        val latch = EndedTeardownLatch()
        assertEquals(ENDED_TEARDOWN_STEPS.size, latch.claim(endedRemoved()).size)
        assertEquals(emptyList<EndedTeardownStep>(), latch.claim(endedRemoved()))
    }

    @Test
    fun `a released step is retried and only that one`() {
        val latch = EndedTeardownLatch()
        latch.claim(endedRemoved())
        latch.release(EndedTeardownStep.RETIRE_PUSH_SUBSCRIPTION)
        assertEquals(
            listOf(EndedTeardownStep.RETIRE_PUSH_SUBSCRIPTION),
            latch.claim(endedRemoved()),
        )
        assertEquals(emptyList<EndedTeardownStep>(), latch.claim(endedRemoved()))
    }

    @Test
    fun `the latch claims nothing while the membership holds`() {
        val latch = EndedTeardownLatch()
        assertEquals(emptyList<EndedTeardownStep>(), latch.claim(active()))
        assertTrue(latch.completed().isEmpty())
    }

    @Test
    fun `the teardown names the push subscription ADR-0043 called unclosable`() {
        // ADR-0043 recorded the ex-member's push-gw row as never deleted,
        // because "nobody else can delete it and no client has 'you were
        // removed' handling". This IS that handling: the evicted device deletes
        // its own row.
        assertTrue(ENDED_TEARDOWN_STEPS.contains(EndedTeardownStep.RETIRE_PUSH_SUBSCRIPTION))
        assertTrue(ENDED_TEARDOWN_STEPS.contains(EndedTeardownStep.STOP_BEACON_TICKER))
    }

    @Test
    fun `a step is handed out exactly once under real concurrency`() {
        // Deliberately NOT a confined dispatcher: a previous sprint on this
        // codebase found one MASKING a real defect — an unserialized
        // read-modify-write that a single-threaded scheduler happened to
        // interleave safely.
        //
        // **And deliberately not `Dispatchers.IO` either, which is a MEASURED
        // finding rather than a preference.** The first version of this test
        // fanned 32 `async` blocks across `Dispatchers.IO`, and a probe that
        // split [EndedTeardownLatch.claim] into an unsynchronized
        // read-then-write — with a `Thread.yield()`, and then with a 2 ms
        // `Thread.sleep` between the two halves — did NOT reproduce: the
        // coroutine scheduler queued all 32 on the submitting worker's local
        // queue and drained them serially, so the claims never overlapped and
        // the test passed over a genuinely broken latch. A test that cannot
        // fail is worse than no test, so this uses platform threads and a
        // [CyclicBarrier], which makes the overlap a fact rather than a hope:
        // every thread is parked until all 32 have arrived, and only then does
        // any of them call `claim`.
        //
        // What it pins: the claim is decided BEFORE the first suspension point
        // and atomically, so no two callers can both be handed the same step
        // and fire a second concurrent unsubscribe.
        val threads = 32
        val latch = EndedTeardownLatch()
        val handed = ENDED_TEARDOWN_STEPS.associateWith { AtomicInteger(0) }
        val startTogether = CyclicBarrier(threads)
        val workers = (1..threads).map {
            Thread {
                startTogether.await()
                for (step in latch.claim(endedRemoved())) {
                    handed.getValue(step).incrementAndGet()
                }
            }
        }
        workers.forEach { it.start() }
        workers.forEach { it.join() }
        for (step in ENDED_TEARDOWN_STEPS) {
            assertEquals(step.name, 1, handed.getValue(step).get())
        }
    }

    @Test
    fun `the driver fires each effect once when driven from Dispatchers IO`() {
        // The production shape: `ChatViewModel` collects the membership flow
        // and the push retire runs on the repository's IO scope. Weaker than
        // the barrier test above at forcing an overlap (see its doc), so it is
        // kept as the SHAPE check — the same latch, driven the way production
        // drives it, through the real suspending driver.
        val latch = EndedTeardownLatch()
        val stops = AtomicInteger(0)
        val retires = AtomicInteger(0)
        runBlocking {
            withContext(Dispatchers.IO) {
                (1..16).map {
                    async {
                        runEndedTeardown(
                            state = endedRemoved(),
                            latch = latch,
                            stopBeaconTicker = { stops.incrementAndGet() },
                            retirePushSubscription = {
                                retires.incrementAndGet()
                                true
                            },
                        )
                    }
                }.awaitAll()
            }
        }
        assertEquals(1, stops.get())
        assertEquals(1, retires.get())
    }

    @Test
    fun `the teardown driver fires each effect once and is idempotent`() = runBlocking {
        val latch = EndedTeardownLatch()
        val stops = AtomicInteger(0)
        val retires = AtomicInteger(0)
        repeat(5) {
            runEndedTeardown(
                state = endedRemoved(),
                latch = latch,
                stopBeaconTicker = { stops.incrementAndGet() },
                retirePushSubscription = {
                    retires.incrementAndGet()
                    true
                },
            )
        }
        assertEquals(1, stops.get())
        assertEquals(1, retires.get())
    }

    @Test
    fun `the teardown driver does nothing while the membership holds`() = runBlocking {
        val latch = EndedTeardownLatch()
        val stops = AtomicInteger(0)
        val retires = AtomicInteger(0)
        runEndedTeardown(
            state = active(),
            latch = latch,
            stopBeaconTicker = { stops.incrementAndGet() },
            retirePushSubscription = {
                retires.incrementAndGet()
                true
            },
        )
        assertEquals(0, stops.get())
        assertEquals(0, retires.get())
    }

    @Test
    fun `a failed push retire is released so a later pass retries it`() = runBlocking {
        val latch = EndedTeardownLatch()
        val stops = AtomicInteger(0)
        val retires = AtomicInteger(0)
        // First pass: the retire throws. Claim-then-release, never
        // mark-on-success — the two orderings differ only under concurrency,
        // and that is precisely the case this exists for.
        runEndedTeardown(
            state = endedRemoved(),
            latch = latch,
            stopBeaconTicker = { stops.incrementAndGet() },
            retirePushSubscription = {
                retires.incrementAndGet()
                throw IllegalStateException("push-gw unreachable")
            },
        )
        assertEquals(1, stops.get())
        assertEquals(1, retires.get())
        // Second pass: only the failed step is owed again.
        runEndedTeardown(
            state = endedRemoved(),
            latch = latch,
            stopBeaconTicker = { stops.incrementAndGet() },
            retirePushSubscription = {
                retires.incrementAndGet()
                true
            },
        )
        assertEquals(1, stops.get())
        assertEquals(2, retires.get())
    }

    // -----------------------------------------------------------------------
    // §6.2.8 item 3 — the ADR-0043 shrink rotation must be suppressed
    // -----------------------------------------------------------------------

    @Test
    fun `an evicted admin does NOT rotate though the role table still names it admin`() {
        // `membershipRotationDecision` sees a member depart (us) and reads
        // `ownRole` from the UNTOUCHED role table — a `Remove` rewrites no
        // `GroupContext` — so before this an evicted admin returned ROTATE and
        // then threw on the broadcast, because `createApplicationMessage` is
        // refused for an inactive group.
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_SELF_EVICTED,
            GroupPushRouting.membershipRotationDecision(
                previousMemberKeys = listOf("ours", "theirs", "other"),
                currentMemberKeys = listOf("theirs", "other"),
                ownRole = GroupPushRouting.RotationRole.ADMIN,
                selfEvicted = true,
            ),
        )
    }

    @Test
    fun `the eviction beats every other verdict including the authority check`() {
        // Evaluated FIRST: a device that is out of the group has no business
        // rotating whatever its stale role table says, and the answer must not
        // depend on which of two skip reasons happened to be checked first.
        // Here the roster did NOT shrink and the role is a plain member, so
        // both of the other skip arms would also fire.
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_SELF_EVICTED,
            GroupPushRouting.membershipRotationDecision(
                previousMemberKeys = listOf("ours", "theirs"),
                currentMemberKeys = listOf("ours", "theirs"),
                ownRole = GroupPushRouting.RotationRole.MEMBER,
                selfEvicted = true,
            ),
        )
        // ... and ahead of the unknown-membership arm too.
        assertEquals(
            GroupPushRouting.RotationDecision.SKIP_SELF_EVICTED,
            GroupPushRouting.membershipRotationDecision(
                previousMemberKeys = null,
                currentMemberKeys = null,
                ownRole = GroupPushRouting.RotationRole.OWNER,
                selfEvicted = true,
            ),
        )
    }

    @Test
    fun `a SURVIVOR of someone else's removal still rotates`() {
        // The whole point of ADR-0043: the remaining admin re-keys so the
        // evicted member's `push_gid` stops being notified. Suppressing that
        // would undo the ADR.
        assertEquals(
            GroupPushRouting.RotationDecision.ROTATE,
            GroupPushRouting.membershipRotationDecision(
                previousMemberKeys = listOf("ours", "theirs", "other"),
                currentMemberKeys = listOf("ours", "other"),
                ownRole = GroupPushRouting.RotationRole.ADMIN,
                selfEvicted = false,
            ),
        )
    }

    @Test
    fun `an omitted eviction flag behaves exactly as before`() {
        // Every existing caller and every existing test passes no flag at all.
        assertEquals(
            GroupPushRouting.RotationDecision.ROTATE,
            GroupPushRouting.membershipRotationDecision(
                previousMemberKeys = listOf("ours", "theirs"),
                currentMemberKeys = listOf("ours"),
                ownRole = GroupPushRouting.RotationRole.OWNER,
            ),
        )
    }

    // -----------------------------------------------------------------------
    // §6.2.7 / §5 item 6 — the exit block's third state
    // -----------------------------------------------------------------------

    @Test
    fun `the exit block holds Delete this chat and neither other control`() {
        // One names a group you are not in; the other is owner-only and you are
        // not. By §6.1.1's own argument the exit block is the right home for
        // the one action still honest: its meaning is *the exit*, and once you
        // are out the only exit-shaped action left is closing the door behind
        // you.
        assertEquals(ExitBlock.Ended, exitBlock(exitInput(membershipEnded = true)))
    }

    @Test
    fun `beats every other arm including a group with no role table`() {
        // A legacy group with no role table still evicts you by direct commit,
        // so the ended arm must be reached before the `hidden` gate.
        assertEquals(
            ExitBlock.Ended,
            exitBlock(exitInput(membershipEnded = true, usesProposalPath = false)),
        )
        assertEquals(
            ExitBlock.Ended,
            exitBlock(exitInput(membershipEnded = true, ownLeafKnown = false)),
        )
        // ... and an owner, who cannot reach the state through the core but
        // must not be able to reach a different arm if they somehow do.
        assertEquals(
            ExitBlock.Ended,
            exitBlock(exitInput(membershipEnded = true, isOwner = true)),
        )
    }

    @Test
    fun `the live exit arms are untouched`() {
        assertEquals(
            ExitBlock.Member(GuardedControl(enabled = true, disabledReason = null)),
            exitBlock(exitInput(membershipEnded = false)),
        )
    }

    @Test
    fun `the delete confirmation states exactly what is destroyed`() {
        // Precise rather than sweeping, and the precision is load-bearing: the
        // messages really are gone, and the inert MLS handle is not.
        assertEquals("Delete this chat", DELETE_CHAT_CONTROL.en)
        assertEquals("Delete this chat?", DELETE_CHAT_TITLE.en)
        assertEquals(
            "This removes the conversation and its messages from this device. " +
                "There is no copy anywhere else and this cannot be undone.",
            DELETE_CHAT_BODY.en,
        )
    }

    // -----------------------------------------------------------------------
    // §6.2.8 item 2 — the beacon emitter must stop
    // -----------------------------------------------------------------------

    @Test
    fun `an evicted device never beacons even foregrounded at rank 0`() {
        // A beacon means *able to sweep*, and this device can never sweep
        // again: the envelope is an ordinary MLS application message and
        // `createApplicationMessage` is refused for an inactive group. Checked
        // AHEAD of the foreground gate because the eviction is the permanent
        // condition of the two.
        assertEquals(
            BeaconEmitVerdict.MEMBERSHIP_ENDED,
            beaconEmitVerdict(
                ownRank = 0u,
                foreground = true,
                membershipEnded = true,
                lastEmittedAtMs = null,
                nowMs = 1_000L,
                throttle = { _, _, _ -> true },
            ),
        )
        assertEquals(
            BeaconEmitVerdict.MEMBERSHIP_ENDED,
            beaconEmitVerdict(
                ownRank = 0u,
                foreground = false,
                membershipEnded = true,
                lastEmittedAtMs = null,
                nowMs = 1_000L,
                throttle = { _, _, _ -> true },
            ),
        )
    }

    @Test
    fun `an active candidate still emits`() {
        assertEquals(
            BeaconEmitVerdict.EMIT,
            beaconEmitVerdict(
                ownRank = 0u,
                foreground = true,
                membershipEnded = false,
                lastEmittedAtMs = null,
                nowMs = 1_000L,
                throttle = { _, _, _ -> true },
            ),
        )
    }

    // -----------------------------------------------------------------------
    // §6.2.4 — the per-group history delete
    // -----------------------------------------------------------------------

    @Test
    fun `withoutGroup drops exactly one group and keeps the rest`() {
        val next = withoutGroup(twoGroupLog(), "aaaa")
        assertNotNull(next)
        assertEquals(setOf("bbbb"), next?.groups?.keys)
        assertEquals(1, next?.version)
    }

    @Test
    fun `withoutGroup reports an absent group so the caller can skip the write`() {
        assertNull(withoutGroup(twoGroupLog(), "cccc"))
    }

    @Test
    fun `withoutGroup does not mutate the payload it was given`() {
        val payload = twoGroupLog()
        withoutGroup(payload, "aaaa")
        assertEquals(setOf("aaaa", "bbbb"), payload.groups.keys)
    }

    // -----------------------------------------------------------------------

    private fun active() = membershipState(isActive = true, leaveRequestedAtMs = null)
    private fun endedLeft() = membershipState(isActive = false, leaveRequestedAtMs = 5L)
    private fun endedRemoved() = membershipState(isActive = false, leaveRequestedAtMs = null)

    private fun exitInput(
        membershipEnded: Boolean,
        usesProposalPath: Boolean = true,
        isOwner: Boolean = false,
        ownLeafKnown: Boolean = true,
    ) = ExitBlockInput(
        usesProposalPath = usesProposalPath,
        isOwner = isOwner,
        ownLeafKnown = ownLeafKnown,
        candidateCount = 3,
        waitingRoleChange = null,
        ownRemovalPending = false,
        membershipEnded = membershipEnded,
    )

    private fun twoGroupLog() = MessageLogPayload(
        version = 1,
        groups = mapOf(
            "aaaa" to PersistedGroup(cursor = 7L, messages = emptyList()),
            "bbbb" to PersistedGroup(cursor = 9L, messages = emptyList()),
        ),
    )
}
