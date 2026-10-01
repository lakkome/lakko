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
        override suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean = reach
        override suspend fun identityFetch(accountId: ByteArray): IdentityKeys? =
            bundleKey?.let { IdentityKeys(it, ByteArray(32)) }

        override suspend fun sendByUsername(username: String, intro: ByteArray, grantWireB64: String?) {
            refusal?.let { throw it }
            sent += "username:$username"
        }

        override suspend fun sendToBundle(
            accountId: ByteArray,
            signaturePub: ByteArray,
            x25519Pub: ByteArray,
            intro: ByteArray,
            grantWireB64: String?,
        ) {
            sent += "bundle:$grantWireB64"
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
}
