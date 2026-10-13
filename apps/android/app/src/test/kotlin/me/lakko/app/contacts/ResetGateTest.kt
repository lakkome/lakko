// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.base64Encode
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.IdentityFetchResponse
import me.lakko.app.ds.DsClient
import me.lakko.app.transparency.LogVerifyStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * ADR-0073 R.2 - the identity gate end to end ([ContactsClient.gateIdentity]),
 * with the two native checks (the attestation verify and the verified log
 * read) injected: the host JVM loads no native library. What is asserted is
 * what the gate WRITES and what it throws.
 */
class ResetGateTest {
    private val accountId = ByteArray(16) { it.toByte() }
    private val accountIdB64 = base64Encode(accountId)
    private val keyA = base64Encode(ByteArray(32) { 0xA1.toByte() })
    private val keyB = base64Encode(ByteArray(32) { 0xB2.toByte() })
    private val x25519 = base64Encode(ByteArray(32) { 0xCC.toByte() })

    private val pins = InMemoryPinStore()
    private val ledger = InMemoryResetLedgerStore()
    private val bindings = InMemoryVerifiedBindingStore()
    private val events = ArrayList<ResetAccepted>()

    private fun client(
        attest: AttestCheck = AttestCheck.Verified(2, 777),
        log: LogCheck = LogCheck(LogVerifyStatus.OFF, null),
        predecessors: List<String> = emptyList(),
    ): ContactsClient = ContactsClient(
        discovery = DiscoveryClient(baseUrl = "http://test.invalid", engine = MockEngine { respond("", HttpStatusCode.OK) }),
        ds = DsClient(baseUrl = "http://test.invalid", engine = MockEngine { respond("", HttpStatusCode.OK) }),
        pinStore = pins,
        transparencyClientFactory = { null },
        ownerAccountIdB64 = "me",
        resetLedger = ledger,
        verifiedBindings = bindings,
        provenPredecessorsOf = { _, _ -> predecessors },
        onResetAccepted = { events += it },
        resetAttestCheck = { _, _, _ -> attest },
        resetLogCheck = { _, _ -> log },
    )

    private fun bundle(key: String, generation: Long = 1, attest: String? = null) =
        IdentityFetchResponse(signature_pubkey = key, x25519_pubkey = x25519, key_generation = generation, key_reset_attest = attest)

    @Test
    fun `a first observation pins and records the generation baseline`() = runTest {
        client().use { it.gateIdentity(accountId, bundle(keyA, generation = 4)) }
        assertEquals(keyA, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        assertEquals(4L, ledger.load("me", accountIdB64)?.acceptedKeyGeneration)
        assertTrue(events.isEmpty())
    }

    @Test
    fun `an attested reset is accepted silently - re-pin, ledger, one event`() = runTest {
        client().use {
            it.gateIdentity(accountId, bundle(keyA))
            it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"))
            // Seen again: no second event, nothing refused.
            it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"))
        }
        assertEquals(keyB, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        val row = ledger.load("me", accountIdB64)!!
        assertEquals(2L, row.acceptedKeyGeneration)
        assertEquals(listOf(keyA), row.supersededKeysB64)
        assertEquals(777L, row.resetSignedAtMs)
        assertEquals(1, events.size)
        assertEquals(keyA, events[0].previousKeyB64)
        assertEquals(keyB, events[0].newKeyB64)
        assertEquals(listOf(keyA), events[0].retiredKeysB64)
        assertEquals("me", events[0].ownerAccountIdB64)
    }

    @Test
    fun `a reset retires the proven predecessors of the pinned key too`() = runTest {
        client(predecessors = listOf("pred")).use {
            it.gateIdentity(accountId, bundle(keyA))
            it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"))
        }
        assertEquals(listOf("pred", keyA), ledger.load("me", accountIdB64)?.supersededKeysB64)
    }

    @Test
    fun `a change with no attestation is refused and writes nothing`() = runTest {
        client(attest = AttestCheck.Absent).use {
            it.gateIdentity(accountId, bundle(keyA))
            try {
                it.gateIdentity(accountId, bundle(keyB, generation = 2))
                fail("an unattested change must be refused")
            } catch (refused: SupersessionRefused) {
                assertEquals(SupersessionRefusedReason.ProofAbsent, refused.reason)
                assertNull(refused.loggedReset)
            }
        }
        assertEquals(keyA, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        assertEquals(1L, ledger.load("me", accountIdB64)?.acceptedKeyGeneration)
        assertTrue(events.isEmpty())
    }

    @Test
    fun `the log showing the key without a reset is the unexplained refusal`() = runTest {
        client(log = LogCheck(LogVerifyStatus.CONFIRMED, false)).use {
            it.gateIdentity(accountId, bundle(keyA))
            try {
                it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"))
                fail("refused")
            } catch (refused: SupersessionRefused) {
                assertEquals(false, refused.loggedReset)
            }
        }
        assertEquals(keyA, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
    }

    @Test
    fun `no attest key or an unreachable log is no verdict and writes nothing`() = runTest {
        for (c in listOf(client(attest = AttestCheck.NoKey), client(log = LogCheck(LogVerifyStatus.UNREACHABLE, null)))) {
            pins.removePin(accountIdB64)
            c.use {
                it.gateIdentity(accountId, bundle(keyA))
                try {
                    it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"))
                    fail("no verdict")
                } catch (_: IdentityCheckInconclusive) {
                    // nothing changes, nothing is shown
                }
            }
            assertEquals(keyA, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        }
        assertTrue(events.isEmpty())
    }

    @Test
    fun `an old attestation cannot be replayed - the generation must rise`() = runTest {
        client().use {
            it.gateIdentity(accountId, bundle(keyA, generation = 2))
            try {
                it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"))
                fail("a generation that is not above the accepted one is refused")
            } catch (_: SupersessionRefused) {
                // refused
            }
        }
        assertEquals(keyA, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
    }

    @Test
    fun `a member never pinned - a claimed account id establishes nothing`() = runTest {
        client().use {
            // The directory serves another key than the roster's for the
            // claimed id: no binding, no verdict, nothing written.
            try {
                it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"), heldKeyB64 = keyA)
                fail("no verdict")
            } catch (_: IdentityCheckInconclusive) {
                // expected
            }
        }
        assertNull(pins.readPin(accountIdB64))
        assertNull(bindings.read("me", accountIdB64))
        assertTrue(events.isEmpty())
    }

    @Test
    fun `a member never pinned - a binding is recorded first and a reset judged from it`() = runTest {
        client().use {
            it.gateIdentity(accountId, bundle(keyA, generation = 1), heldKeyB64 = keyA)
            assertEquals(keyA, bindings.read("me", accountIdB64)?.keyB64)
            assertNull(pins.readPin(accountIdB64))
            it.gateIdentity(accountId, bundle(keyA, generation = 1), heldKeyB64 = keyA)
            it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"), heldKeyB64 = keyA)
        }
        assertEquals(keyB, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
        assertNull(bindings.read("me", accountIdB64))
        assertEquals(listOf(keyA), ledger.load("me", accountIdB64)?.supersededKeysB64)
        assertEquals(1, events.size)
    }

    @Test
    fun `a binding that names another key than the roster's is no verdict`() = runTest {
        client().use {
            it.gateIdentity(accountId, bundle(keyA), heldKeyB64 = keyA)
            try {
                it.gateIdentity(accountId, bundle(keyB, generation = 2, attest = "AAAA"), heldKeyB64 = "claimed")
                fail("no verdict")
            } catch (_: IdentityCheckInconclusive) {
                // expected
            }
        }
        assertNull(pins.readPin(accountIdB64))
        assertTrue(events.isEmpty())
    }

    @Test
    fun `keys handed to the client for a pinned account go through the gate first`() = runTest {
        val engine = MockEngine { request ->
            if (request.url.encodedPath.contains("/discovery/identity/")) {
                respond(
                    """{"signature_pubkey":"$keyB","x25519_pubkey":"$x25519","key_generation":2,"key_reset_attest":"AAAA"}""",
                    HttpStatusCode.OK,
                    io.ktor.http.headersOf("Content-Type", "application/json"),
                )
            } else {
                respond("", HttpStatusCode.OK)
            }
        }
        val c = ContactsClient(
            discovery = DiscoveryClient(baseUrl = "http://test.invalid", engine = engine),
            ds = DsClient(baseUrl = "http://test.invalid", engine = MockEngine { respond("", HttpStatusCode.OK) }),
            pinStore = pins,
            transparencyClientFactory = { null },
            ownerAccountIdB64 = "me",
            resetLedger = ledger,
            verifiedBindings = bindings,
            onResetAccepted = { events += it },
            resetAttestCheck = { _, _, _ -> AttestCheck.Verified(2, 1) },
            resetLogCheck = { _, _ -> LogCheck(LogVerifyStatus.OFF, null) },
        )
        c.use {
            // A stranger costs no request and is never refused.
            it.gateBundleRecipient(accountId, me.lakko.app.auth.base64Decode(keyA))
            pins.setPin(accountIdB64, keyA, 0)
            ledger.save("me", accountIdB64, ResetLedgerRow(acceptedKeyGeneration = 1))
            // The new keys of a contact who reset: accepted silently, sent.
            it.gateBundleRecipient(accountId, me.lakko.app.auth.base64Decode(keyB))
            assertEquals(keyB, pins.readPin(accountIdB64)?.pinnedSignaturePubkeyB64)
            // An old link naming the retired key: old keys do not come back.
            try {
                it.gateBundleRecipient(accountId, me.lakko.app.auth.base64Decode(keyA))
                fail("a retired key is refused")
            } catch (refused: SupersessionRefused) {
                assertEquals(SupersessionRefusedReason.KeyMismatch, refused.reason)
            }
        }
        assertEquals(1, events.size)
    }
}
