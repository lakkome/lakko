// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import io.ktor.client.engine.mock.MockEngine
import io.ktor.client.engine.mock.respond
import io.ktor.http.HttpStatusCode
import io.ktor.http.headersOf
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.test.runTest
import me.lakko.app.auth.base64Encode
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.IdentityFetchResponse
import me.lakko.app.ds.DsClient
import me.lakko.app.profile.InMemoryProfilePersistence
import me.lakko.app.profile.UsernameResolution
import me.lakko.app.profile.UsernameVerification
import me.lakko.app.profile.UsernameVerificationStore
import me.lakko.app.profile.UsernameVerifier
import me.lakko.app.profile.VerifyOutcome
import me.lakko.app.profile.resolveUsernameForVerification
import me.lakko.app.transparency.LogVerifyStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0048 §3 — verifying a displayed username is a plain comparison and must
 * never touch the G-4 peer pin store.
 *
 * ## The defect this pins
 *
 * The Android profile wave resolved a username claim through
 * `resolveVerifiedRecipient`, which runs the supersession / pin gate. That gate
 * TOFU-pins a peer's account key the first time it sees it, so rendering a
 * name in a chat quietly decided which key a later contact request or
 * add-member would trust. iOS and web do the plain lookup -> bundle -> key
 * comparison; so does Android now.
 *
 * Each test runs the REAL [UsernameVerifier] over the REAL production resolve
 * ([resolveUsernameForVerification] + [ContactsClient.fetchPublishedSignatureKey])
 * against a `MockEngine` discovery, with a pin store that counts every read and
 * write. Only the OPRF lookup is faked (it needs the native core); it is not
 * the half that ever touched a pin. The last test is the control: the gate a
 * contact request runs, on the same bundle, DOES write — so a zero here means
 * "not written", not "the counter is broken".
 */
class UsernameVerificationPinTest {

    private val accountId = ByteArray(16) { (it + 1).toByte() }
    private val keyA = ByteArray(32) { 0xA1.toByte() }
    private val keyB = ByteArray(32) { 0xB2.toByte() }
    private val keyAB64 = base64Encode(keyA)

    private val jsonHeaders = headersOf("Content-Type", "application/json")

    /** A [PinStore] that records every access, delegating to an in-memory one. */
    private class RecordingPinStore : PinStore {
        val inner = InMemoryPinStore()
        val reads = AtomicInteger()
        val writes = AtomicInteger()

        override fun readPin(accountIdB64: String): Pin? {
            reads.incrementAndGet()
            return inner.readPin(accountIdB64)
        }

        override fun setPin(accountIdB64: String, pinnedSignaturePubkeyB64: String, rotationMarker: Long): SetPinResult {
            writes.incrementAndGet()
            return inner.setPin(accountIdB64, pinnedSignaturePubkeyB64, rotationMarker)
        }

        override fun removePin(accountIdB64: String) {
            writes.incrementAndGet()
            inner.removePin(accountIdB64)
        }

        override fun recordLogStatus(accountIdB64: String, logStatus: LogVerifyStatus): Boolean {
            writes.incrementAndGet()
            return inner.recordLogStatus(accountIdB64, logStatus)
        }

        override fun recordLogReset(accountIdB64: String, isReset: Boolean?): Boolean {
            writes.incrementAndGet()
            return inner.recordLogReset(accountIdB64, isReset)
        }

        override fun recordLogVersion(accountIdB64: String, version: Long?): Boolean {
            writes.incrementAndGet()
            return inner.recordLogVersion(accountIdB64, version)
        }

        override fun readLogVersion(accountIdB64: String): Long? {
            reads.incrementAndGet()
            return inner.readLogVersion(accountIdB64)
        }
    }

    private fun bundleJson(signatureKey: ByteArray): String =
        """{"signature_pubkey":"${base64Encode(signatureKey)}",""" +
            """"x25519_pubkey":"${base64Encode(ByteArray(32) { 0x0C })}"}"""

    /** A client whose discovery answers every identity fetch with [respondWith]. */
    private fun client(
        store: PinStore,
        respondWith: () -> Pair<HttpStatusCode, String>,
    ): ContactsClient {
        val discoveryEngine = MockEngine { _ ->
            val (status, body) = respondWith()
            respond(content = body, status = status, headers = jsonHeaders)
        }
        val dsEngine = MockEngine { respond(content = "", status = HttpStatusCode.OK) }
        return ContactsClient(
            discovery = DiscoveryClient(baseUrl = "http://test.invalid", engine = discoveryEngine),
            ds = DsClient(baseUrl = "http://test.invalid", engine = dsEngine),
            pinStore = store,
        )
    }

    private fun verifier(client: ContactsClient, lookup: ByteArray? = accountId): Pair<UsernameVerifier, UsernameVerificationStore> {
        val results = UsernameVerificationStore(InMemoryProfilePersistence())
        val v = UsernameVerifier(
            store = results,
            resolve = { name ->
                resolveUsernameForVerification(
                    username = name,
                    lookupAccountId = { lookup },
                    fetchSignatureKey = { id -> client.fetchPublishedSignatureKey(id) },
                )
            },
            passesRule = { true },
        )
        return v to results
    }

    @Test
    fun `a verified username writes no pin and reads none`() = runTest {
        val pins = RecordingPinStore()
        client(pins) { HttpStatusCode.OK to bundleJson(keyA) }.use { c ->
            val (v, results) = verifier(c)
            assertEquals(VerifyOutcome.VERIFIED, v.verify(keyAB64, "alice"))
            assertEquals(UsernameVerification.VERIFIED, results.get(keyAB64, "alice"))
        }
        assertEquals("pin writes", 0, pins.writes.get())
        assertEquals("pin reads", 0, pins.reads.get())
        assertNull(pins.inner.readPin(base64Encode(accountId)))
    }

    @Test
    fun `a mismatched key fails without pinning the served key`() = runTest {
        val pins = RecordingPinStore()
        client(pins) { HttpStatusCode.OK to bundleJson(keyB) }.use { c ->
            val (v, _) = verifier(c)
            assertEquals(VerifyOutcome.FAILED, v.verify(keyAB64, "alice"))
        }
        assertEquals(0, pins.writes.get())
        assertNull(pins.inner.readPin(base64Encode(accountId)))
    }

    @Test
    fun `a pinned peer whose served key changed is compared, not refused, and the pin is untouched`() = runTest {
        // A peer this device already pinned to keyA now serves keyB with no
        // proof. The contact-request gate would refuse; verification simply
        // compares against the profile's bound key and leaves the pin alone.
        val pins = RecordingPinStore()
        pins.inner.setPin(base64Encode(accountId), keyAB64, 7L)
        client(pins) { HttpStatusCode.OK to bundleJson(keyB) }.use { c ->
            val (v, _) = verifier(c)
            assertEquals(VerifyOutcome.VERIFIED, v.verify(base64Encode(keyB), "bob"))
        }
        assertEquals(0, pins.writes.get())
        assertEquals(keyAB64, pins.inner.readPin(base64Encode(accountId))?.pinnedSignaturePubkeyB64)
        assertEquals(7L, pins.inner.readPin(base64Encode(accountId))?.rotationMarker)
    }

    @Test
    fun `no bundle and an unregistered username both fail definitively, with no pin`() = runTest {
        val pins = RecordingPinStore()
        client(pins) { HttpStatusCode.NotFound to "not found" }.use { c ->
            val (noBundle, _) = verifier(c)
            assertEquals(VerifyOutcome.FAILED, noBundle.verify(keyAB64, "alice"))
            val (unregistered, _) = verifier(c, lookup = null)
            assertEquals(VerifyOutcome.FAILED, unregistered.verify(keyAB64, "ghost"))
        }
        assertEquals(0, pins.writes.get())
    }

    @Test
    fun `a malformed bundle key is not registered`() = runTest {
        val resolution = resolveUsernameForVerification(
            username = "alice",
            lookupAccountId = { accountId },
            fetchSignatureKey = { ByteArray(31) },
        )
        assertEquals(UsernameResolution.NotRegistered, resolution)
    }

    @Test
    fun `a server failure is unavailable, uncached, and pins nothing`() = runTest {
        val pins = RecordingPinStore()
        var online = false
        client(pins) {
            if (online) HttpStatusCode.OK to bundleJson(keyA) else HttpStatusCode.InternalServerError to "boom"
        }.use { c ->
            val (v, results) = verifier(c)
            assertEquals(VerifyOutcome.UNAVAILABLE, v.verify(keyAB64, "alice"))
            assertNull(results.get(keyAB64, "alice"))
            online = true
            assertEquals(VerifyOutcome.VERIFIED, v.verify(keyAB64, "alice"))
        }
        assertEquals(0, pins.writes.get())
    }

    @Test
    fun `control - the contact-request gate on the same bundle does write a pin`() {
        val pins = RecordingPinStore()
        client(pins) { HttpStatusCode.OK to bundleJson(keyA) }.use { c ->
            c.verifyBundleAccountKey(
                accountId,
                IdentityFetchResponse(
                    signature_pubkey = keyAB64,
                    x25519_pubkey = base64Encode(ByteArray(32)),
                ),
            )
        }
        assertTrue("the recording store must see the gate's TOFU write", pins.writes.get() > 0)
        assertNotNull(pins.inner.readPin(base64Encode(accountId)))
    }
}
