// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import java.util.Base64
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The contact request to a found person (web `send-request.ts`): refused
 *  before anything goes out unless the person can still be reached as found. */
class SendFoundRequestTest {

    private val key = ByteArray(32) { 0xA1.toByte() }
    private val keyB64 = Base64.getEncoder().encodeToString(key)

    private class Fake(
        var reach: Boolean = true,
        var bundleKey: ByteArray? = null,
        var refusal: Throwable? = null,
    ) : SendDeps {
        val sent = mutableListOf<String>()
        val bindings = mutableListOf<String?>()
        override suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean = reach
        override suspend fun identityFetch(accountId: ByteArray): IdentityKeys? =
            bundleKey?.let { IdentityKeys(it, ByteArray(32)) }

        override suspend fun sendByUsername(username: String, intro: ByteArray, grantWireB64: String?, bindingB64: String?) {
            refusal?.let { throw it }
            sent += "username:$username"
            bindings += bindingB64
        }

        override suspend fun sendToBundle(
            accountId: ByteArray,
            signaturePub: ByteArray,
            x25519Pub: ByteArray,
            intro: ByteArray,
            grantWireB64: String?,
            bindingB64: String?,
        ) {
            sent += "bundle:$grantWireB64"
            bindings += bindingB64
        }

        override fun isRefusal(error: Throwable): Boolean = error is IllegalArgumentException
        override fun errorMessage(error: Throwable): me.lakko.app.l10n.UiText = me.lakko.app.l10n.UiText("contacts.error.send")
    }

    private fun person(reach: PersonReach, username: String? = null, grant: String? = null) =
        PersonResult(keyB64, searchHex(key), "01".repeat(16), "Anna", username, reach, grant)

    @Test
    fun `a grant the reach check refuses sends nothing`() = runTest {
        val fake = Fake(reach = false, bundleKey = key)
        assertEquals(SendOutcome.Refused, sendFoundRequest(person(PersonReach.GRANT, grant = "Zw=="), ByteArray(0), fake))
        assertTrue(fake.sent.isEmpty())
    }

    @Test
    fun `a bundle for another key sends nothing`() = runTest {
        val fake = Fake(bundleKey = ByteArray(32) { 1 })
        assertEquals(SendOutcome.Refused, sendFoundRequest(person(PersonReach.GRANT, grant = "Zw=="), ByteArray(0), fake))
        assertTrue(fake.sent.isEmpty())
    }

    @Test
    fun `a granted person goes by the bundle with the grant`() = runTest {
        val fake = Fake(bundleKey = key)
        assertEquals(SendOutcome.Sent, sendFoundRequest(person(PersonReach.GRANT, grant = "Zw=="), ByteArray(0), fake))
        assertEquals(listOf("bundle:Zw=="), fake.sent)
    }

    @Test
    fun `a person in Everyone with a username goes by the username`() = runTest {
        val fake = Fake()
        assertEquals(SendOutcome.Sent, sendFoundRequest(person(PersonReach.OPEN, username = "anna"), ByteArray(0), fake))
        assertEquals(listOf("username:anna"), fake.sent)
    }

    @Test
    fun `a username that no longer resolves is a refusal, not a failure`() = runTest {
        val fake = Fake(refusal = IllegalArgumentException("gone"))
        assertEquals(SendOutcome.Refused, sendFoundRequest(person(PersonReach.OPEN, username = "anna"), ByteArray(0), fake))
    }

    @Test
    fun `nobody without a way in is ever sent to`() = runTest {
        val fake = Fake(bundleKey = key)
        assertEquals(SendOutcome.Refused, sendFoundRequest(person(PersonReach.NONE), ByteArray(0), fake))
        assertEquals(SendOutcome.Refused, sendFoundRequest(person(PersonReach.CONTACT), ByteArray(0), fake))
        assertTrue(fake.sent.isEmpty())
    }

    // --- ADR-0074 section 3.5a: a hit under a proven predecessor key --------

    @Test
    fun `a person found under a key they provably rotated away from is sent to their current keys`() = runTest {
        val current = ByteArray(32) { 0xB2.toByte() }
        var asked: Pair<ByteArray, ByteArray>? = null
        val fake = object : SendDeps by Fake(bundleKey = current) {
            val sentTo = mutableListOf<ByteArray>()
            override fun isProvenPredecessor(foundKey: ByteArray, publishedKey: ByteArray): Boolean {
                asked = foundKey to publishedKey
                return true
            }

            override suspend fun sendToBundle(
                accountId: ByteArray,
                signaturePub: ByteArray,
                x25519Pub: ByteArray,
                intro: ByteArray,
                grantWireB64: String?,
                bindingB64: String?,
            ) {
                sentTo += signaturePub
            }
        }
        assertEquals(SendOutcome.Sent, sendFoundRequest(person(PersonReach.OPEN), ByteArray(0), fake))
        // Sealed to the bundle's keys, never to the key the hit carried.
        assertTrue(fake.sentTo.single().contentEquals(current))
        assertTrue(asked!!.first.contentEquals(key) && asked!!.second.contentEquals(current))
    }

    @Test
    fun `a bundle for a key nothing proved related is still refused`() = runTest {
        val fake = Fake(bundleKey = ByteArray(32) { 0xB2.toByte() })
        assertEquals(SendOutcome.Refused, sendFoundRequest(person(PersonReach.OPEN), ByteArray(0), fake))
        assertTrue(fake.sent.isEmpty())
    }

    @Test
    fun `the found key is the published person only when equal or proven`() {
        val other = ByteArray(32) { 0xB2.toByte() }
        assertTrue(foundKeyIsPublished(key, key.copyOf()) { _, _ -> false })
        assertTrue(foundKeyIsPublished(key, other) { _, _ -> true })
        assertEquals(false, foundKeyIsPublished(key, other) { _, _ -> false })
        // A check that fails proves nothing.
        assertEquals(false, foundKeyIsPublished(key, other) { _, _ -> throw IllegalStateException("bad proof") })
    }

    @Test
    fun `a public record sealed under a proven predecessor key still names the hit`() {
        val accountId = ByteArray(16) { 4 }
        val current = ByteArray(32) { 0xB2.toByte() }
        val hit = LookupHit(accountId, ByteArray(8))
        val open = { _: ByteArray, _: ByteArray -> OpenedPublicRecord(key, accountId, " Anna ") }
        assertEquals(null, acceptPublicRecord(open, hit, ByteArray(64), current))
        assertEquals("Anna", acceptPublicRecord(open, hit, ByteArray(64), current) { found, published ->
            found.contentEquals(key) && published.contentEquals(current)
        })
    }
}
