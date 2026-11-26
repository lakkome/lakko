// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import me.lakko.app.account.AccountStoreGate
import me.lakko.app.auth.MarkerStorage
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

/** ADR-0101 section 8 - a received profile is filed only under its signer;
 *  this account's own, returned after its reset, comes back as its own.
 *  Mirror of `apps/web/src/profile/profile-signer.test.ts`; the core's
 *  verifier is faked. */
class ReturnedProfileTest {

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    @Before
    fun setUp() = AccountStoreGate.resetForTest()

    @After
    fun tearDown() = AccountStoreGate.resetForTest()

    private val enc = java.util.Base64.getEncoder()
    private val person = ByteArray(32) { 1 }
    private val peer = ByteArray(32) { 2 }

    /** An "envelope" is the signer's key; the fake verifier accepts it under
     *  that key only. */
    private val verify: (ByteArray, ByteArray) -> Unit = { env, key ->
        if (!env.contentEquals(key)) throw IllegalArgumentException("badSignature")
    }

    @Test
    fun `the route files only what the sender signed`() {
        assertEquals(ProfileRoute.FILE, routeReceivedProfile(verifiesUnderSender = true, verifiesUnderPreviousOwnKey = true))
        assertEquals(ProfileRoute.RETURNED_OWN, routeReceivedProfile(verifiesUnderSender = false, verifiesUnderPreviousOwnKey = true))
        assertEquals(ProfileRoute.DROP, routeReceivedProfile(verifiesUnderSender = false, verifiesUnderPreviousOwnKey = false))
    }

    @Test
    fun `a profile a peer carries back verifies under the person's old key, not the peer's`() {
        val env = person.copyOf()
        val underPeer = profileVerifiesUnder(verify, env, enc.encodeToString(peer))
        val underPerson = profileVerifiesUnder(verify, env, enc.encodeToString(person))
        assertFalse(underPeer)
        assertTrue(underPerson)
        assertEquals(ProfileRoute.RETURNED_OWN, routeReceivedProfile(underPeer, underPerson))
        assertFalse(profileVerifiesUnder(verify, env, null))
        assertFalse(profileVerifiesUnder(verify, env, "not base64!"))
    }

    @Test
    fun `filing ingests a signed profile and drops one signed by someone else`() {
        var ingested = 0
        fileReceivedProfile(null, null, enc.encodeToString(peer), peer.copyOf(), { ingested += 1 }, verify, previousOwnKeyB64 = null)
        assertEquals(1, ingested)
        fileReceivedProfile(null, null, enc.encodeToString(peer), person.copyOf(), { ingested += 1 }, verify, previousOwnKeyB64 = null)
        assertEquals(1, ingested)
        // No sender or no envelope: nothing.
        fileReceivedProfile(null, null, null, peer.copyOf(), { ingested += 1 }, verify, previousOwnKeyB64 = null)
        fileReceivedProfile(null, null, enc.encodeToString(peer), null, { ingested += 1 }, verify, previousOwnKeyB64 = null)
        assertEquals(1, ingested)
    }

    @Test
    fun `a returned own profile is adopted only with a strictly newer, non-zero stamp`() {
        assertTrue(adoptsReturnedProfile(5uL, 4uL))
        assertFalse(adoptsReturnedProfile(5uL, 5uL))
        assertFalse(adoptsReturnedProfile(0uL, 0uL))
    }

    @Test
    fun `the newest profile held for a reset person, across their retired keys`() {
        val held = mapOf(
            "A" to HeldEnvelope(byteArrayOf(1), 5uL),
            "B" to HeldEnvelope(byteArrayOf(2), 9uL),
        )
        assertArrayEquals(byteArrayOf(2), newestHeldEnvelope(listOf("A", "B", "C")) { held[it] })
        assertNull(newestHeldEnvelope(listOf("C")) { null })
        // A tie keeps the first key's.
        val tie = mapOf("A" to HeldEnvelope(byteArrayOf(1), 5uL), "B" to HeldEnvelope(byteArrayOf(2), 5uL))
        assertArrayEquals(byteArrayOf(1), newestHeldEnvelope(listOf("A", "B")) { tie[it] })
    }

    @Test
    fun `a profile is carried into each successor once`() {
        val store = CarriedProfileStore(MapStorage())
        assertFalse(store.carriedInto("me", "s1"))
        store.markCarried("me", "s1")
        store.markCarried("me", "s1")
        assertTrue(store.carriedInto("me", "s1"))
        assertFalse(store.carriedInto("other", "s1"))
    }

    @Test
    fun `the received envelope is kept verbatim beside the fields`() {
        val envelope = byteArrayOf(9, 8, 7)
        val profile = uniffi.lakko_ffi_uniffi.FfiProfile(
            displayName = "Anna",
            username = null,
            updatedAtMs = 7uL,
            accountId = null,
            usernameCertificate = null,
            grants = emptyList(),
            avatar = null,
            cardShareable = true,
        )
        assertEquals(enc.encodeToString(envelope), peerProfileOf(profile, envelope).envelopeB64)
        assertNull(peerProfileOf(profile).envelopeB64)
    }
}
