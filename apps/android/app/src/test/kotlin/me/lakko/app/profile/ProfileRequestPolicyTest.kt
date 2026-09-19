// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0048 trigger 4 — the plumbing around the core's "should this device ask
 * the group to introduce itself" rule: the wire peek, the unnamed-member
 * count, the store round-trip, and the decide / record-after-send split.
 *
 * ## The rule is a double
 *
 * The real rule is the core's `shouldRequestProfiles`, reached over UniFFI,
 * and this is a plain JVM `test` source set where the `.so` does not exist.
 * [coreRule] restates the three clauses of
 * `crates/lakko-contacts/src/profile_request.rs::should_request_profiles`
 * (join always asks; nothing to ask when nobody is unnamed; otherwise once per
 * interval, a backwards clock reading as "recently asked") so the plumbing is
 * exercised against the shape it will meet — but the authority on those
 * clauses is the Rust test suite, not this file. What THIS suite pins is that
 * the plumbing hands the rule the right inputs and honours its answer.
 */
class ProfileRequestPolicyTest {

    private val dayMs: ULong = 24uL * 60uL * 60uL * 1_000uL

    /** Stand-in for the FFI `shouldRequestProfiles`. */
    private fun coreRule(joinedJustNow: Boolean, unnamed: UInt, last: ULong?, now: ULong): Boolean {
        if (joinedJustNow) return true
        if (unnamed == 0u) return false
        if (last == null) return true
        val since = if (now >= last) now - last else 0uL
        return since >= dayMs
    }

    private val requestDomain: ByteArray =
        "lakko/profile-request/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    private fun request(version: Int = 0x01): ByteArray = requestDomain + byteArrayOf(version.toByte())

    private fun key(b: Int): ByteArray = ByteArray(32) { b.toByte() }

    // -----------------------------------------------------------------
    // The wire peek
    // -----------------------------------------------------------------

    @Test
    fun `the peek needs the whole domain including its NUL`() {
        assertTrue(looksLikeProfileRequestEnvelope(request()))
        // The bare separator is still a request envelope (a malformed one —
        // dropped downstream, never handed to the chat parser).
        assertTrue(looksLikeProfileRequestEnvelope(requestDomain))
        assertFalse(looksLikeProfileRequestEnvelope(requestDomain.copyOf(requestDomain.size - 1)))
        assertFalse(looksLikeProfileRequestEnvelope("lakko/profile-request/v1 is a fine subject".toByteArray()))
        assertFalse(looksLikeProfileRequestEnvelope(ByteArray(0)))
    }

    @Test
    fun `the peek does not confuse the sibling domains`() {
        val profile = "lakko/profile/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0, 1)
        val groupName = "lakko/group-name/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0, 1)
        val beacon = "lakko/committer-beacon/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0, 1)
        assertFalse(looksLikeProfileRequestEnvelope(profile))
        assertFalse(looksLikeProfileRequestEnvelope(groupName))
        assertFalse(looksLikeProfileRequestEnvelope(beacon))
    }

    // -----------------------------------------------------------------
    // The unnamed-member count
    // -----------------------------------------------------------------

    @Test
    fun `the count excludes the viewer and counts only placeholders`() {
        val own = key(1)
        val named = key(2)
        val unnamedA = key(3)
        val unnamedB = key(4)
        val placeholder = setOf(1, 3, 4)
        val n = countUnnamedMembers(
            memberKeys = listOf(own, named, unnamedA, unnamedB),
            ownKey = own,
            isPlaceholder = { k -> k[0].toInt() in placeholder },
        )
        // The viewer is a placeholder to the resolver too (nobody stores their
        // own profile as a peer), and must not be counted.
        assertEquals(2, n)
    }

    @Test
    fun `an unreadable own key errs toward asking`() {
        val n = countUnnamedMembers(
            memberKeys = listOf(key(1), key(2)),
            ownKey = null,
            isPlaceholder = { true },
        )
        assertEquals(2, n)
    }

    @Test
    fun `a fully named group counts zero`() {
        assertEquals(0, countUnnamedMembers(listOf(key(1), key(2)), key(1)) { false })
    }

    // -----------------------------------------------------------------
    // The store
    // -----------------------------------------------------------------

    @Test
    fun `the in-memory store is keyed per account and group`() {
        val store = InMemoryProfileRequestStore()
        assertNull(store.lastRequestedAtMs("acct", "ABCD"))
        store.markRequested("acct", "ABCD", 1_000uL)
        // Group ids compare case-insensitively (hex).
        assertEquals(1_000uL, store.lastRequestedAtMs("acct", "abcd"))
        assertNull(store.lastRequestedAtMs("acct", "ffff"))
        assertNull(store.lastRequestedAtMs("other", "abcd"))
        store.deleteAccount("acct")
        assertNull(store.lastRequestedAtMs("acct", "abcd"))
    }

    // -----------------------------------------------------------------
    // The decision
    // -----------------------------------------------------------------

    private fun policy(store: ProfileRequestStore, now: ULong): ProfileRequestPolicy =
        ProfileRequestPolicy(store = store, nowMs = { now }, rule = ::coreRule)

    @Test
    fun `a join asks whatever was asked before`() {
        val store = InMemoryProfileRequestStore()
        store.markRequested("acct", "g1", 999_000uL)
        val stamp = policy(store, now = 1_000_000uL).decide("acct", "g1", joinedJustNow = true, unnamedMembers = 0)
        assertEquals(1_000_000uL, stamp)
    }

    @Test
    fun `an open with everyone named asks nothing`() {
        val store = InMemoryProfileRequestStore()
        assertNull(policy(store, now = 5uL).decide("acct", "g1", joinedJustNow = false, unnamedMembers = 0))
    }

    @Test
    fun `an open with an unnamed member asks once per interval`() {
        val store = InMemoryProfileRequestStore()
        val p = policy(store, now = 1_000_000uL)
        val first = p.decide("acct", "g1", joinedJustNow = false, unnamedMembers = 3)
        assertEquals(1_000_000uL, first)
        // Not stamped until the caller reports the send: a second decide in
        // the same moment still says ask (a failed publish must be retried).
        assertEquals(1_000_000uL, p.decide("acct", "g1", joinedJustNow = false, unnamedMembers = 3))
        p.recordSent("acct", "g1", first!!)
        // Now recently asked: silent for the day.
        assertNull(policy(store, now = 1_000_000uL + dayMs - 1uL).decide("acct", "g1", false, 3))
        assertEquals(
            1_000_000uL + dayMs,
            policy(store, now = 1_000_000uL + dayMs).decide("acct", "g1", false, 3),
        )
    }

    @Test
    fun `the throttle is per group`() {
        val store = InMemoryProfileRequestStore()
        val p = policy(store, now = 1_000_000uL)
        p.recordSent("acct", "g1", 1_000_000uL)
        assertNull(p.decide("acct", "g1", joinedJustNow = false, unnamedMembers = 1))
        assertEquals(1_000_000uL, p.decide("acct", "g2", joinedJustNow = false, unnamedMembers = 1))
    }

    @Test
    fun `the plumbing hands the rule exactly what it read`() {
        val store = InMemoryProfileRequestStore()
        store.markRequested("acct", "g1", 42uL)
        var seen: List<Any?>? = null
        val p = ProfileRequestPolicy(
            store = store,
            nowMs = { 77uL },
            rule = { joined, unnamed, last, now ->
                seen = listOf(joined, unnamed, last, now)
                false
            },
        )
        assertNull(p.decide("acct", "g1", joinedJustNow = false, unnamedMembers = 2))
        assertEquals(listOf<Any?>(false, 2u, 42uL, 77uL), seen)
    }
}
