// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/**
 * Pins when the identity bundle is published. On Android signup and password
 * login used to publish nothing, which left an account that no peer — and not
 * its owner's next device — could verify.
 *
 * Each landing is driven through [identityPublicationFor] + [publishIdentity],
 * the exact pair `AuthRepository.publishIdentityFor` calls, over recording
 * ports. (`AuthRepository` itself needs the native core and Keystore.)
 */
class IdentityPublicationTest {

    private val ourSig = ByteArray(32) { 1 }
    private val ourX = ByteArray(32) { 2 }

    private class RecordingPorts(
        override val signaturePubkey: ByteArray,
        override val x25519Pubkey: ByteArray,
        private val published: () -> PublishedIdentityKeys?,
        private val uploadFailure: Throwable? = null,
        var current: Boolean = true,
    ) : IdentityPublicationPorts {
        /** Every port call in order, so read-before-write is assertable. */
        val calls = mutableListOf<String>()
        val alerts = mutableListOf<Boolean>()

        override suspend fun fetchPublished(): PublishedIdentityKeys? {
            calls += "fetch"
            return published()
        }

        override suspend fun upload(withGenesisAuth: Boolean) {
            calls += "upload(genesis=$withGenesisAuth)"
            uploadFailure?.let { throw it }
        }

        override fun isStillCurrent(): Boolean = current

        override fun reportPublishedIdentity(changed: Boolean) {
            alerts += changed
        }
    }

    private fun ports(
        published: () -> PublishedIdentityKeys? = { null },
        uploadFailure: Throwable? = null,
    ) = RecordingPorts(ourSig, ourX, published, uploadFailure)

    private val logged = mutableListOf<String>()
    private val log: (String, Throwable?) -> Unit = { message, _ -> logged += message }

    private suspend fun land(landing: IdentityLanding, ports: RecordingPorts) =
        publishIdentity(identityPublicationFor(landing), ports, log)

    // --- the three landings the defect was about ------------------------

    @Test
    fun `signup publishes the bundle as a genesis entry`() = runTest {
        val p = ports()

        val result = land(IdentityLanding.SIGNUP, p)

        assertEquals(listOf("upload(genesis=true)"), p.calls)
        assertEquals(IdentityPublicationResult.Uploaded(withGenesisAuth = true), result)
    }

    @Test
    fun `signup fails when the bundle cannot be published`() = runTest {
        val failure = RuntimeException("discovery down")
        val p = ports(uploadFailure = failure)

        try {
            land(IdentityLanding.SIGNUP, p)
            fail("signup must not land an account whose bundle is missing")
        } catch (err: Throwable) {
            assertSame(failure, err)
        }
    }

    @Test
    fun `login checks the directory and then re-publishes the bundle`() = runTest {
        val p = ports(published = { PublishedIdentityKeys(ourSig, ourX) })

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(listOf("fetch", "upload(genesis=false)"), p.calls)
        assertEquals(listOf(false), p.alerts)
        assertEquals(IdentityPublicationResult.Uploaded(withGenesisAuth = false), result)
    }

    @Test
    fun `login publishes a missing bundle as a genesis entry`() = runTest {
        val p = ports(published = { null })

        land(IdentityLanding.SIGN_IN, p)

        assertEquals(listOf("fetch", "upload(genesis=true)"), p.calls)
        // A 404 is "nothing yet", never "replaced".
        assertEquals(listOf(false), p.alerts)
    }

    @Test
    fun `recover re-publishes the bundle`() = runTest {
        val p = ports()

        val result = land(IdentityLanding.RECOVER, p)

        assertEquals(listOf("upload(genesis=true)"), p.calls)
        assertEquals(IdentityPublicationResult.Uploaded(withGenesisAuth = true), result)
    }

    // --- sign-in: the ADR-0017 self-check keeps working -----------------

    @Test
    fun `login raises the alert on a replaced bundle before re-publishing over it`() = runTest {
        val p = ports(published = { PublishedIdentityKeys(ByteArray(32) { 7 }, ourX) })

        land(IdentityLanding.SIGN_IN, p)

        assertEquals(listOf(true), p.alerts)
        assertEquals(listOf("fetch", "upload(genesis=false)"), p.calls)
    }

    @Test
    fun `login still publishes when the directory cannot be read, without touching the alert`() =
        runTest {
            val p = ports(published = { throw RuntimeException("timeout") })

            land(IdentityLanding.SIGN_IN, p)

            assertTrue(p.alerts.isEmpty())
            assertEquals(listOf("fetch", "upload(genesis=false)"), p.calls)
        }

    @Test
    fun `a failed sign-in publish is logged, not thrown`() = runTest {
        val p = ports(published = { null }, uploadFailure = RuntimeException("pow"))

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(IdentityPublicationResult.UploadFailed, result)
        assertTrue(logged.isNotEmpty())
    }

    @Test
    fun `a sign-in refresh that outlived its session neither alerts nor publishes`() = runTest {
        val p = ports(published = { PublishedIdentityKeys(ByteArray(32) { 7 }, ourX) })
        p.current = false

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(IdentityPublicationResult.Superseded, result)
        assertEquals(listOf("fetch"), p.calls)
        assertTrue(p.alerts.isEmpty())
    }

    @Test
    fun `cancellation is never swallowed`() = runTest {
        val p = ports(published = { throw CancellationException("scope closed") })
        try {
            land(IdentityLanding.SIGN_IN, p)
            fail("cancellation must propagate")
        } catch (_: CancellationException) {
            // expected
        }
    }

    // --- silent restore --------------------------------------------------

    @Test
    fun `a restore does not re-publish a bundle the directory already serves`() = runTest {
        val p = ports(published = { PublishedIdentityKeys(ourSig, ourX) })

        val result = land(IdentityLanding.SESSION_RESTORE, p)

        assertEquals(IdentityPublicationResult.UpToDate, result)
        assertEquals(listOf("fetch"), p.calls)
    }

    @Test
    fun `a restore publishes a missing or stale bundle and skips when it cannot read`() = runTest {
        val missing = ports(published = { null })
        land(IdentityLanding.SESSION_RESTORE, missing)
        assertEquals(listOf("fetch", "upload(genesis=true)"), missing.calls)

        val staleX = ports(published = { PublishedIdentityKeys(ourSig, ByteArray(32) { 9 }) })
        land(IdentityLanding.SESSION_RESTORE, staleX)
        assertEquals(listOf("fetch", "upload(genesis=false)"), staleX.calls)

        val unreadable = ports(published = { throw RuntimeException("offline") })
        assertEquals(
            IdentityPublicationResult.Unverified,
            land(IdentityLanding.SESSION_RESTORE, unreadable),
        )
        assertEquals(listOf("fetch"), unreadable.calls)
    }

    @Test
    fun `a lost-everything fresh identity is published as a genesis entry and fails loud`() =
        runTest {
            val p = ports()
            land(IdentityLanding.FRESH_IDENTITY, p)
            assertEquals(listOf("upload(genesis=true)"), p.calls)

            val failure = RuntimeException("refused")
            try {
                land(IdentityLanding.FRESH_IDENTITY, ports(uploadFailure = failure))
                fail("a fresh identity must not be installed unpublished")
            } catch (err: Throwable) {
                assertSame(failure, err)
            }
        }
}
