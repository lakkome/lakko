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

    /**
     * The core's `acting_committer_verdict` fold, restated (ADR-0050 section
     * 2.5a item 6): drop stale beacons, non-candidates and this device's own
     * key; defer to the smallest `(rank, key)` pair strictly below this
     * device's own, the key compared bytewise unsigned; hear nothing, act.
     */
    private fun coreFold(): (UInt?, ByteArray, List<ProjectedObservation>, ULong) -> ActingCommitter =
        { ownRank, ownKey, observed, windowMs ->
            if (ownRank == null) {
                ActingCommitter.NotACandidate
            } else {
                val better = observed
                    .filter { it.ageMs <= windowMs && it.rank != null && !it.signatureKey.contentEquals(ownKey) }
                    .filter { lessThan(it.rank!!, it.signatureKey, ownRank, ownKey) }
                    .sortedWith { a, b -> if (lessThan(a.rank!!, a.signatureKey, b.rank!!, b.signatureKey)) -1 else 1 }
                    .firstOrNull()
                if (better == null) ActingCommitter.Act else ActingCommitter.Defer(better.rank, better.signatureKey)
            }
        }

    private fun lessThan(rankA: UInt, keyA: ByteArray, rankB: UInt, keyB: ByteArray): Boolean {
        if (rankA != rankB) return rankA < rankB
        for (i in keyA.indices) {
            val x = keyA[i].toInt() and 0xFF
            val y = keyB[i].toInt() and 0xFF
            if (x != y) return x < y
        }
        return false
    }

    /** A 32-byte signature key named in a test: [first] leads, the rest is
     *  filled from the name so two names never share a key. */
    private fun keyOf(name: String, first: Int = name.hashCode() and 0x7F): ByteArray =
        ByteArray(32) { i -> if (i == 0) first.toByte() else (name.hashCode() + i).toByte() }

    /** The store spells keys as names here; this is their decoder. */
    private val names = mutableMapOf<String, ByteArray>()

    private fun nameKey(name: String): ByteArray = names.getOrPut(name) { keyOf(name) }

    private val ownKey: ByteArray get() = nameKey("self")

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
            decodeKey = ::nameKey,
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
            decodeKey = ::nameKey,
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
            ownSignatureKey = ownKey,
            rankOf = { key -> if (key == "stillAdmin") 1u else null },
            nowMs = 1_100,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
            decodeKey = ::nameKey,
        )

        assertEquals(ActingCommitter.Defer(1u, nameKey("stillAdmin")), verdict)
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
            ownSignatureKey = ownKey,
            rankOf = { 1u },
            nowMs = 1_000 + 100_000,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
            decodeKey = ::nameKey,
        )
        assertEquals("inside the window, this device stands down", ActingCommitter.Defer(1u, nameKey("sleepyOwner")), fresh)

        val stale = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 4u,
            ownSignatureKey = ownKey,
            rankOf = { 1u },
            nowMs = 1_000 + 200_000,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
            decodeKey = ::nameKey,
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
            ownSignatureKey = ownKey,
            rankOf = { null },
            nowMs = 1_000,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
            decodeKey = ::nameKey,
        )
        assertEquals(ActingCommitter.Act, verdict)
    }

    /** RULE: this device's own beacon, echoed back on its next poll, is
     *  ignored — it never defers to itself. */
    @Test
    fun `its own echoed beacon never defers this device`() {
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "self", atMs = 1_000)
        val verdict = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 3u,
            ownSignatureKey = ownKey,
            rankOf = { 3u },
            nowMs = 1_100,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
            decodeKey = ::nameKey,
        )
        assertEquals(ActingCommitter.Act, verdict)
    }

    // ------------------------------- ADR-0050 section 2.5a item 6: same rank

    /** Two devices of one account share its rank; the lower-keyed one with a
     *  fresh beacon acts, so the higher-keyed sibling defers to it. */
    @Test
    fun `a sibling device with a lower key defers this device`() {
        names["self"] = keyOf("self", first = 0x50)
        names["sibling"] = keyOf("sibling", first = 0x10)
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "sibling", atMs = 1_000)
        recordBeaconObservation(store, "acct", "grp", "self", atMs = 1_000)
        val verdict = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 4u,
            ownSignatureKey = ownKey,
            rankOf = { 4u },
            nowMs = 1_100,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
            decodeKey = ::nameKey,
        )
        assertEquals(ActingCommitter.Defer(4u, nameKey("sibling")), verdict)
    }

    /** The higher-keyed sibling's beacon does not stand the lower-keyed device
     *  down: it is the account's acting device. */
    @Test
    fun `a sibling device with a higher key does not defer this device`() {
        names["self"] = keyOf("self", first = 0x10)
        names["sibling"] = keyOf("sibling", first = 0x50)
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "sibling", atMs = 1_000)
        val verdict = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 4u,
            ownSignatureKey = ownKey,
            rankOf = { 4u },
            nowMs = 1_100,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
            decodeKey = ::nameKey,
        )
        assertEquals(ActingCommitter.Act, verdict)
    }

    /** A lost sibling stops beaconing; once its beacon ages out the next
     *  device of the account acts (the acting device is chosen among ACTIVE
     *  devices, not by roster position). */
    @Test
    fun `a silent lower keyed sibling stops deferring this device`() {
        names["self"] = keyOf("self", first = 0x50)
        names["lost"] = keyOf("lost", first = 0x01)
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "lost", atMs = 1_000)
        val verdict = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 0u,
            ownSignatureKey = ownKey,
            rankOf = { 0u },
            nowMs = 1_000 + 200_000,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
            decodeKey = ::nameKey,
        )
        assertEquals(ActingCommitter.Act, verdict)
    }

    /** The adapter contract: the fold is handed THIS device's key and every
     *  live observation's key, its own included (the core drops that one). */
    @Test
    fun `the fold receives the device key and every observation key`() {
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "self", atMs = 1_000)
        recordBeaconObservation(store, "acct", "grp", "admin", atMs = 1_000)
        var seenOwn: ByteArray? = null
        var seenKeys: List<ByteArray> = emptyList()
        decideActingCommitter(
            store, "acct", "grp",
            ownRank = 2u,
            ownSignatureKey = ownKey,
            rankOf = { 2u },
            nowMs = 1_100,
            freshnessWindowMs = 150_000uL,
            fold = { _, own, observed, _ ->
                seenOwn = own
                seenKeys = observed.map { it.signatureKey }
                ActingCommitter.Act
            },
            decodeKey = ::nameKey,
        )
        assertTrue(ownKey.contentEquals(seenOwn))
        assertEquals(2, seenKeys.size)
        assertTrue(seenKeys.any { it.contentEquals(nameKey("admin")) })
        assertTrue(seenKeys.any { it.contentEquals(ownKey) })
    }

    /** A stored key that is not a 32-byte signature key would make the core
     *  refuse the whole fold; it is dropped and pruned instead. */
    @Test
    fun `a malformed stored key is dropped and pruned`() {
        val store = MemStore()
        recordBeaconObservation(store, "acct", "grp", "not base64!", atMs = 1_000)
        val good = java.util.Base64.getEncoder().encodeToString(ByteArray(32) { 9 })
        recordBeaconObservation(store, "acct", "grp", good, atMs = 1_000)
        val verdict = decideActingCommitter(
            store, "acct", "grp",
            ownRank = 5u,
            ownSignatureKey = ByteArray(32) { 1 },
            rankOf = { 1u },
            nowMs = 1_100,
            freshnessWindowMs = 150_000uL,
            fold = coreFold(),
        )
        assertEquals(ActingCommitter.Defer(1u, ByteArray(32) { 9 }), verdict)
        assertEquals(setOf(good), store.load("acct", "grp").keys)
    }

    // ----------------------------- ADR-0050 section 2.5a item 8: registration

    /** A ranked device holding neither the root nor a registrar seed emits no
     *  beacon: it never stages a sweep, and a beacon would make others defer. */
    @Test
    fun `a device that cannot register emits no beacon`() {
        assertEquals(
            BeaconEmitVerdict.CANNOT_REGISTER,
            beaconEmitVerdict(
                ownRank = 3u,
                foreground = true,
                lastEmittedAtMs = null,
                nowMs = 1_000,
                throttle = coreThrottle(),
                canRegister = false,
            ),
        )
        assertEquals(
            BeaconEmitVerdict.EMIT,
            beaconEmitVerdict(
                ownRank = 3u,
                foreground = true,
                lastEmittedAtMs = null,
                nowMs = 1_000,
                throttle = coreThrottle(),
                canRegister = true,
            ),
        )
        // No rank at all stays the stronger answer.
        assertEquals(
            BeaconEmitVerdict.NOT_A_CANDIDATE,
            beaconEmitVerdict(
                ownRank = null,
                foreground = true,
                lastEmittedAtMs = null,
                nowMs = 1_000,
                throttle = coreThrottle(),
                canRegister = false,
            ),
        )
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
            decodeKey = ::nameKey,
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
