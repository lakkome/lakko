// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.base64Encode
import me.lakko.app.chat.DeviceReadmission
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.IdentityFetchResponse
import me.lakko.app.ds.DsClient
import me.lakko.app.transparency.LogVerifyStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0074 section 3.7 - the identity gate records a conversation link: a
 * contact first met after a rotation pinned only the new key and holds no
 * older key a supersession proof could be checked against, so the link is what
 * makes the key the roster names a proven predecessor of the pin.
 *
 * The link's two signatures are the native core's and are not run here: the
 * check is injected, and what is under test is when its answer is used.
 */
class ConversationLinkGateTest {

    private val accountId = ByteArray(16) { 3 }
    private val accountIdB64 = base64Encode(accountId)
    private val conversationKey = ByteArray(32) { 0xA1.toByte() }
    private val keyA = base64Encode(conversationKey)
    private val keyB = base64Encode(ByteArray(32) { 0xB2.toByte() })
    private val keyC = base64Encode(ByteArray(32) { 0xC3.toByte() })
    private val x25519 = base64Encode(ByteArray(32) { 0xCC.toByte() })
    private val link = base64Encode(ByteArray(201) { 7 })

    private val pins = InMemoryPinStore()
    private val ledger = InMemoryResetLedgerStore()
    private val bindings = InMemoryVerifiedBindingStore()
    private val links = InMemoryProvenLinkStore()
    private val checked = mutableListOf<ByteArray>()

    private fun client(
        check: (ByteArray, ByteArray) -> ByteArray = { _, trusted ->
            checked += trusted
            conversationKey
        },
    ): ContactsClient = ContactsClient(
        discovery = DiscoveryClient(baseUrl = "http://test.invalid", engine = MockEngine { respond("", HttpStatusCode.OK) }),
        ds = DsClient(baseUrl = "http://test.invalid", engine = MockEngine { respond("", HttpStatusCode.OK) }),
        pinStore = pins,
        transparencyClientFactory = { null },
        onProvenRotation = { peer, oldKey, newKey ->
            ProvenPredecessors.recordProven(links, "me", peer, oldKey, newKey)
        },
        ownerAccountIdB64 = "me",
        resetLedger = ledger,
        verifiedBindings = bindings,
        resetAttestCheck = { _, _, _ -> AttestCheck.Verified(2, 777) },
        resetLogCheck = { _, _ -> LogCheck(LogVerifyStatus.OFF, null) },
        conversationLinkCheck = check,
    )

    private fun bundle(key: String, conversationLink: String? = link, generation: Long = 1, attest: String? = null) =
        IdentityFetchResponse(
            signature_pubkey = key,
            x25519_pubkey = x25519,
            key_generation = generation,
            key_reset_attest = attest,
            conversation_link = conversationLink,
        )

    @Test
    fun `a first observation pins the served key and records the key the link names`() = runTest {
        client().use { it.gateIdentity(accountId, bundle(keyB)) }

        assertEquals(keyB, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        assertEquals(listOf(keyA, keyB), keysOfPerson(keyB, links.load("me")))
        // Checked against the key the gate left pinned.
        assertEquals(keyB, base64Encode(checked.single()))
    }

    @Test
    fun `the recorded key is one the re-admission check and the add filter follow`() = runTest {
        client().use { it.gateIdentity(accountId, bundle(keyB)) }

        val proven = links.load("me")
        val chain = keysOfPerson(keyB, proven).let { it.take(it.indexOf(keyB) + 1) }
        assertEquals(
            DeviceReadmission.LookupStanding.SAME_PERSON,
            DeviceReadmission.lookupStanding(keyA, chain, emptySet(), null),
        )
        // The contact card's "not available on this device" came from this set.
        assertEquals(setOf(keyA, keyB), addFilterKeys(keyA, chain, null, emptySet()))
        assertEquals(setOf(keyA, keyB), addFilterKeys(null, chain, null, emptySet()))
    }

    @Test
    fun `the verified-binding path records the link and is followed the same way`() = runTest {
        // A member this device never pinned: the directory serves the roster key.
        client().use { it.gateIdentity(accountId, bundle(keyB), heldKeyB64 = keyB) }

        assertEquals(null, pins.readPin(accountIdB64))
        assertEquals(keyB, bindings.read("me", accountIdB64)?.keyB64)
        val chain = keysOfPerson(keyB, links.load("me"))
        assertEquals(listOf(keyA, keyB), chain)
        assertEquals(
            DeviceReadmission.LookupStanding.SAME_PERSON,
            DeviceReadmission.lookupStanding(keyA, null, emptySet(), bindingKeyB64 = keyB, bindingChainB64 = chain),
        )
    }

    @Test
    fun `a refused link records nothing and refuses nothing`() = runTest {
        client(check = { _, _ -> throw IllegalStateException("bad signature") }).use {
            it.gateIdentity(accountId, bundle(keyB))
        }
        assertEquals(keyB, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        assertTrue(links.load("me").isEmpty())
    }

    @Test
    fun `a link that is not base64 records nothing and refuses nothing`() = runTest {
        client().use { it.gateIdentity(accountId, bundle(keyB, conversationLink = "%%%")) }
        assertEquals(keyB, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        assertTrue(links.load("me").isEmpty())
    }

    @Test
    fun `a bundle with no link asks nothing`() = runTest {
        client().use { it.gateIdentity(accountId, bundle(keyB, conversationLink = null)) }
        assertTrue(checked.isEmpty())
        assertTrue(links.load("me").isEmpty())
    }

    @Test
    fun `a key the gate refused gets no link`() = runTest {
        pins.setPin(accountIdB64, keyC, 0L)
        val refused = client()
        try {
            refused.use { it.gateIdentity(accountId, bundle(keyB, generation = 1).copy(supersession_proof = "%%%")) }
            fail("a change with a broken proof is refused")
        } catch (_: SupersessionRefused) {
            // The gate's own refusal.
        }
        assertTrue(checked.isEmpty())
        assertTrue(links.load("me").isEmpty())
    }

    @Test
    fun `a member whose key the directory does not serve gets no link`() = runTest {
        // A roster key under an id nothing established: no verdict, no trust.
        try {
            client().use { it.gateIdentity(accountId, bundle(keyB), heldKeyB64 = keyA) }
            fail("no verdict")
        } catch (_: IdentityCheckInconclusive) {
            // Nothing was established.
        }
        assertTrue(checked.isEmpty())
        assertFalse(links.load("me").any { it.successorKeyB64 == keyB })
    }

    @Test
    fun `an accepted reset records no link`() = runTest {
        pins.setPin(accountIdB64, keyC, 0L)
        client().use { it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA")) }
        assertEquals(keyB, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        assertTrue(checked.isEmpty())
        assertTrue(links.load("me").isEmpty())
    }
}
