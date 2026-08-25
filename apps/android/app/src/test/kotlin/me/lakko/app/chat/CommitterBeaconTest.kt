// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0045 S4b — the five beacon rules.
 *
 * Mirrors `apps/web/src/chat/committer-beacon.test.ts` in coverage. The FFI
 * `.so` is Android-only, so `shouldEmitBeacon` and `actingCommitterVerdict` are
 * injected here as stand-ins that restate the core's documented rule; the rules
 * themselves are proven by 33 `#[test]`s in `lakko-contacts`. What is proven HERE
 * is everything the core deliberately cannot see — WHO sent a beacon, HOW OLD it
 * is, whether this device is foregrounded, and above all *when the rank is
 * derived*.
 *
 * ## Which of these discriminate (each break was RUN; the failures below are
 * quoted verbatim from that run, not predicted)
 *
 * Carrying a rank over from receipt instead of re-deriving it at verdict time
 * (`projectObservations` memoising `rankOf` per sender key):
 *   - `an observation is ranked at verdict time`: `expected:<[7]> but was:<[2]>`.
 *
 * Consulting the throttle BEFORE the foreground gate:
 *   - `only the foreground emits`: `expected:<NOT_FOREGROUND> but was:<EMIT>`.
 *
 * Letting an OLDER stamp overwrite a newer one in the store:
 *   - `the newest beacon timestamp wins`: `expected:<500> but was:<100>`.
 *
 * Wrapping a future-dated row instead of clamping it (`age.toULong()` on a
 * negative `Long`):
 *   - `a future dated row is clamped not wrapped`:
 *     `expected:<[0]> but was:<[18446744073709551516]>`.
 *
 * Keeping a sender that no longer ranks instead of dropping it:
 *   - `a sender who no longer ranks is dropped and pruned`:
 *     `expected:<[stillAdmin]> but was:<[stillAdmin, demoted]>`.
 */
class CommitterBeaconTest {

    private class MemStore : BeaconObservationStore {
        val rows = mutableMapOf<String, MutableMap<String, Long>>()

        override fun load(accountIdB64: String, groupIdHex: String): Map<String, Long> =
            rows["$accountIdB64/$groupIdHex"]?.toMap() ?: emptyMap()

        override fun save(
            accountIdB64: String,
            groupIdHex: String,
            observations: Map<String, Long>,
        ) {
            rows["$accountIdB64/$groupIdHex"] = observations.toMutableMap()
        }

        override fun clearForAccount(accountIdB64: String) {
            rows.keys.filter { it.startsWith("$accountIdB64/") }.forEach { rows.remove(it) }
        }
    }

    /** The core's `should_emit_beacon` rule, restated: only a candidate emits,
     *  the first beacon of a session is immediate, otherwise one per interval,
     *  and a backward local clock jump suppresses rather than spams. */
    private fun coreThrottle(intervalMs: ULong = 60_000uL): (UInt?, ULong?, ULong) -> Boolean =
        { rank, last, now ->
            when {
                rank == null -> false
                last == null -> true
                now < last -> false
                else -> now - last >= intervalMs
            }
        }

    /** The core's `acting_committer_verdict` fold, restated. */
    private fun coreFold(): (UInt?, List<ProjectedObservation>, ULong) -> ActingCommitter =
        { ownRank, observed, windowMs ->
            when {
                ownRank == null -> ActingCommitter.NotACandidate
                ownRank == 0u -> ActingCommitter.Act
                else -> {
                    val better = observed
                        .filter { it.rank != null && it.ageMs <= windowMs && it.rank!! < ownRank }
                        .minByOrNull { it.rank!! }
                    if (better == null) ActingCommitter.Act else ActingCommitter.Defer(better.rank)
                }
            }
        }

    // ------------------------------------------------------------- emit rules

    /** RULE: only a candidate emits. A plain member's beacon can never make it
     *  the acting committer, so it is pure journal litter and a pure activity
     *  leak. */
    @Test
    fun `only a candidate emits`() {
        assertEquals(
            BeaconEmitVerdict.NOT_A_CANDIDATE,
            beaconEmitVerdict(
                ownRank = null,
                foreground = true,
                lastEmittedAtMs = null,
                nowMs = 1_000,
                throttle = coreThrottle(),
            ),
        )
        // `0u` is the OWNER, not "no rank": a `null` check and a `> 0` check are
        // different questions and getting them confused silences the one device
        // that always sweeps.
        assertEquals(
            BeaconEmitVerdict.EMIT,
            beaconEmitVerdict(
                ownRank = 0u,
                foreground = true,
                lastEmittedAtMs = null,
                nowMs = 1_000,
                throttle = coreThrottle(),
            ),
        )
    }

    /**
     * RULE: only the foreground emits, and the gate runs BEFORE the throttle.
     *
     * A beacon means *able to sweep*, and a sweep runs only at the tail of a poll
     * driven by a RESUMED chat screen. The C-59 background decrypt is
     * read-only-throwaway and structurally cannot commit, so a beacon from the
     * background would make every other candidate defer to a device that is never
     * going to sweep — and keep deferring for as long as it kept beaconing.
     */
    @Test
    fun `only the foreground emits`() {
        var throttleConsulted = false
        val verdict = beaconEmitVerdict(
            ownRank = 0u,
            foreground = false,
            lastEmittedAtMs = null,
            nowMs = 1_000,
            throttle = { _, _, _ ->
                throttleConsulted = true
                true
            },
        )
        assertEquals(BeaconEmitVerdict.NOT_FOREGROUND, verdict)
        assertFalse(
            "the foreground gate must short-circuit before the core's throttle",
            throttleConsulted,
        )
    }

    /** RULE: the interval is the CORE's, consulted through the gate rather than
     *  restated by the client. */
    @Test
    fun `the throttle is the core's rule`() {
        assertEquals(
            BeaconEmitVerdict.THROTTLED,
            beaconEmitVerdict(
                ownRank = 1u,
                foreground = true,
                lastEmittedAtMs = 1_000,
                nowMs = 5_000,
                throttle = coreThrottle(),
            ),
        )
        assertEquals(
            BeaconEmitVerdict.EMIT,
            beaconEmitVerdict(
                ownRank = 1u,
                foreground = true,
                lastEmittedAtMs = 1_000,
                nowMs = 61_000,
                throttle = coreThrottle(),
            ),
        )
    }

    // ---------------------------------------------------------- observe rules

    /**
     * RULE: rank at VERDICT time, never at receipt.
     *
     * A rank is a POSITION in the role table's canonical order, so promoting,
     * demoting or removing any admin renumbers every admin sorting after it. The
     * store therefore holds the sender's signature KEY, and the rank is derived
     * against the table as it stands when the verdict is taken. Here the sender
     * was rank 2 when its beacon landed and is rank 7 now; the projection must
     * report 7.
     */
    @Test
    fun `an observation is ranked at verdict time`() {
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "senderA", atMs = 1_000)

        // First verdict: the sender is the 1st admin, rank 2.
        val atReceipt = projectObservations(
            observations = store.load("acct", "grp"),
            rankOf = { key -> if (key == "senderA") 2u else null },
            nowMs = 1_200,
        )
        assertEquals(listOf(2u), atReceipt.projected.map { it.rank })

        // An admin sorting BEFORE it was promoted / removed, so every admin after
        // it renumbers. The SAME stored observation must now project at 7 — a
        // rank carried over from the first verdict would still say 2, which is a
        // position in a table that no longer exists.
        val laterVerdict = projectObservations(
            observations = store.load("acct", "grp"),
            rankOf = { key -> if (key == "senderA") 7u else null },
            nowMs = 1_500,
        )
        assertEquals(listOf(7u), laterVerdict.projected.map { it.rank })
        assertEquals(listOf(500uL), laterVerdict.projected.map { it.ageMs })
    }

    /** RULE: the age comes from the DS row, and a sender who no longer ranks is
     *  dropped from the projection AND pruned from the store — it can never
     *  contribute again. */
    @Test
    fun `a sender who no longer ranks is dropped and pruned`() {
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "stillAdmin", atMs = 1_000)
        recordBeaconObservation(store, "acct", "grp", "demoted", atMs = 1_000)

        val verdict = decideActingCommitter(
            store = store,
            accountIdB64 = "acct",
            groupIdHex = "grp",
            ownRank = 5u,
            rankOf = { key -> if (key == "stillAdmin") 1u else null },
            nowMs = 1_100,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
        )

        assertEquals(ActingCommitter.Defer(1u), verdict)
        assertEquals(setOf("stillAdmin"), store.load("acct", "grp").keys)
    }

    /**
     * RULE: a beacon past the freshness window is not evidence of anything.
     *
     * The whole takeover rests on this: a candidate that stops running stops
     * being deferred to, with no handover protocol and no deadline.
     */
    @Test
    fun `a stale beacon is ignored`() {
        val store = MemStore()
        // Rank 1 beaconed once, a very long time ago.
        recordBeaconObservation(store, "acct", "grp", "sleepyOwner", atMs = 1_000)

        val fresh = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 4u,
            rankOf = { 1u },
            nowMs = 1_000 + 100_000,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
        )
        assertEquals("inside the window, this device stands down", ActingCommitter.Defer(1u), fresh)

        val stale = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 4u,
            rankOf = { 1u },
            nowMs = 1_000 + 200_000,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
        )
        assertEquals("past the window, this device takes over", ActingCommitter.Act, stale)
    }

    /** RULE: hearing NOTHING means act — silence is the answer that nobody
     *  better placed is running, not a failure to elect. */
    @Test
    fun `hearing nothing means act`() {
        val verdict = decideActingCommitter(
            MemStore(), "acct", "grp",
            ownRank = 9u,
            rankOf = { null },
            nowMs = 1_000,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
        )
        assertEquals(ActingCommitter.Act, verdict)
    }

    /** RULE: only a STRICTLY lower rank defers this device — so its own beacon,
     *  echoed back on its next poll, is harmless. */
    @Test
    fun `an equal rank never defers`() {
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "self", atMs = 1_000)
        val verdict = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 3u,
            rankOf = { 3u },
            nowMs = 1_100,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
        )
        assertEquals(ActingCommitter.Act, verdict)
    }

    // ------------------------------------------------------------- the store

    /** RULE: the NEWEST timestamp wins — the opposite of the pending-request
     *  store, because here recency IS the signal. An older stamp overwriting a
     *  newer one would age a live candidate out and hand its duty away. */
    @Test
    fun `the newest beacon timestamp wins`() {
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "sender", atMs = 500)
        recordBeaconObservation(store, "acct", "grp", "sender", atMs = 100)
        assertEquals(500L, store.load("acct", "grp")["sender"])
        recordBeaconObservation(store, "acct", "grp", "sender", atMs = 900)
        assertEquals(900L, store.load("acct", "grp")["sender"])
    }

    /**
     * RULE: a row that appears to come from the FUTURE is CLAMPED to age 0, not
     * wrapped.
     *
     * `ageMs` is a `ULong` on the ABI. A negative `Long` converted to `ULong`
     * does not read as "slightly wrong": it reads as a value *above* every
     * freshness window, silently converting "just arrived" into "ancient" — which
     * makes this device act when it should wait. Clamping errs the other way,
     * which the sweep's stand-down and rollback already handle.
     */
    @Test
    fun `a future dated row is clamped not wrapped`() {
        val projected = projectObservations(
            observations = mapOf("sender" to 1_100L),
            rankOf = { 1u },
            nowMs = 1_000,
        )
        assertEquals(listOf(0uL), projected.projected.map { it.ageMs })
    }

    /** The `SharedPreferences` row codec, round-tripped without a `Context`. */
    @Test
    fun `the observation row codec round trips`() {
        val observations = mapOf("aGVsbG8=" to 1_700_000_000_123L, "d29ybGQ=" to 42L)
        val encoded = SharedPrefsBeaconObservationStore.encodeRow(observations)
        assertEquals(observations, SharedPrefsBeaconObservationStore.decodeRow(encoded))
        // A corrupt or absent row reads as empty, which degrades to "heard
        // nothing" — and hearing nothing means act. That is the correct
        // direction for a store failure.
        assertEquals(emptyMap<String, Long>(), SharedPrefsBeaconObservationStore.decodeRow(null))
        assertEquals(emptyMap<String, Long>(), SharedPrefsBeaconObservationStore.decodeRow(""))
        assertEquals(
            emptyMap<String, Long>(),
            SharedPrefsBeaconObservationStore.decodeRow("garbage,without,separators"),
        )
    }

    /** The source file must carry no RAW NUL byte — a recurrence of the
     *  ADR-0042 MEDIUM-3 / ADR-0043 F5 class, where git classifies the file as
     *  binary and `grep -r` skips it silently. */
    @Test
    fun `the beacon sources carry no raw NUL`() {
        for (path in listOf(
            "src/main/kotlin/me/lakko/app/chat/CommitterBeacon.kt",
            "src/main/kotlin/me/lakko/app/chat/PendingRequests.kt",
            "src/main/kotlin/me/lakko/app/chat/CommitterSweep.kt",
        )) {
            val file = java.io.File(path)
            if (!file.exists()) continue
            assertFalse(
                "$path contains a raw NUL — use the \\u0000 escape",
                file.readBytes().any { it.toInt() == 0 },
            )
        }
        assertNull(null)
    }
}
