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
 * Pins when the identity bundle is published, and (ADR-0073 section 4.2)
 * that nothing ever publishes a differing key back: every landing but signup
 * reads the directory first, and stale or mismatching keys are reported,
 * never overwritten.
 *
 * Each landing is driven through [identityPublicationFor] + [publishIdentity],
 * the exact pair `AuthRepository.publishIdentityFor` calls, over recording
 * ports. (`AuthRepository` itself needs the native core and Keystore.)
 */
class IdentityPublicationTest {

    private val ourSig = ByteArray(32) { 1 }
    private val ourX = ByteArray(32) { 2 }
    private val otherSig = ByteArray(32) { 7 }

    private class RecordingPorts(
        override val signaturePubkey: ByteArray,
        override val x25519Pubkey: ByteArray,
        override val ownKeyGeneration: Long,
        private val published: () -> PublishedIdentityKeys?,
        private val uploadFailure: Throwable? = null,
        var current: Boolean = true,
    ) : IdentityPublicationPorts {
        /** Every port call in order, so read-before-write is assertable. */
        val calls = mutableListOf<String>()
        val stale = mutableListOf<LandingVerdict>()

        override suspend fun fetchPublished(): PublishedIdentityKeys? {
            calls += "fetch"
            return published()
        }

        override suspend fun upload(withGenesisAuth: Boolean) {
            calls += "upload(genesis=$withGenesisAuth)"
            uploadFailure?.let { throw it }
        }

        override fun isStillCurrent(): Boolean = current

        override fun onStaleKeys(verdict: LandingVerdict) {
            stale += verdict
        }
    }

    private fun ports(
        published: () -> PublishedIdentityKeys? = { null },
        uploadFailure: Throwable? = null,
        ownGeneration: Long = 1L,
    ) = RecordingPorts(ourSig, ourX, ownGeneration, published, uploadFailure)

    private fun served(
        sig: ByteArray = ourSig,
        x: ByteArray = ourX,
        generation: Long = 1L,
        bindingValid: Boolean = true,
    ) = PublishedIdentityKeys(sig, x, generation, x25519BindingValid = bindingValid)

    private val logged = mutableListOf<String>()
    private val log: (String, Throwable?) -> Unit = { message, _ -> logged += message }

    private suspend fun land(landing: IdentityLanding, ports: RecordingPorts) =
        publishIdentity(identityPublicationFor(landing), ports, log)

    // --- the landing rule, as a pure function -----------------------------

    @Test
    fun `the landing rule proceeds on our key or nothing, and refuses other keys by generation`() {
        assertTrue(landingVerdict(null, ourSig, 1L) is LandingVerdict.Proceed)
        assertTrue(landingVerdict(served(), ourSig, 1L) is LandingVerdict.Proceed)
        // Another key at a higher generation: this device's keys are stale.
        assertEquals(LandingVerdict.StaleKeys, landingVerdict(served(sig = otherSig, generation = 2L), ourSig, 1L))
        // Another key at the same generation, or a lower one: an anomaly.
        assertEquals(LandingVerdict.KeyMismatch, landingVerdict(served(sig = otherSig, generation = 1L), ourSig, 1L))
        assertEquals(LandingVerdict.KeyMismatch, landingVerdict(served(sig = otherSig, generation = 1L), ourSig, 2L))
        // Our own key at any generation proceeds (the directory lags a reset
        // whose publish is owed; that path is the owed retry's).
        assertTrue(landingVerdict(served(generation = 1L), ourSig, 2L) is LandingVerdict.Proceed)
    }

    // --- signup -----------------------------------------------------------

    @Test
    fun `signup publishes the bundle as a genesis entry without reading`() = runTest {
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

    // --- sign-in ------------------------------------------------------------

    @Test
    fun `login checks the directory and then re-publishes the bundle`() = runTest {
        val p = ports(published = { served() })

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(listOf("fetch", "upload(genesis=false)"), p.calls)
        assertTrue(p.stale.isEmpty())
        assertEquals(IdentityPublicationResult.Uploaded(withGenesisAuth = false), result)
    }

    @Test
    fun `login publishes a missing bundle as a genesis entry`() = runTest {
        val p = ports(published = { null })

        land(IdentityLanding.SIGN_IN, p)

        assertEquals(listOf("fetch", "upload(genesis=true)"), p.calls)
        assertTrue(p.stale.isEmpty())
    }

    @Test
    fun `login never publishes over other keys at a higher generation - the device is stale`() = runTest {
        val p = ports(published = { served(sig = otherSig, generation = 2L) })

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(IdentityPublicationResult.StaleKeys, result)
        assertEquals(listOf(LandingVerdict.StaleKeys), p.stale)
        assertEquals(listOf("fetch"), p.calls)
    }

    @Test
    fun `login never publishes over other keys at the same generation - the anomaly`() = runTest {
        val p = ports(published = { served(sig = otherSig, generation = 1L) })

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(IdentityPublicationResult.KeyMismatch, result)
        assertEquals(listOf(LandingVerdict.KeyMismatch), p.stale)
        assertEquals(listOf("fetch"), p.calls)
    }

    @Test
    fun `login uploads nothing when the directory cannot be read`() = runTest {
        val p = ports(published = { throw RuntimeException("timeout") })

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(IdentityPublicationResult.Unverified, result)
        assertTrue(p.stale.isEmpty())
        assertEquals(listOf("fetch"), p.calls)
    }

    @Test
    fun `a failed sign-in publish is logged, not thrown`() = runTest {
        val p = ports(published = { null }, uploadFailure = RuntimeException("pow"))

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(IdentityPublicationResult.UploadFailed, result)
        assertTrue(logged.isNotEmpty())
    }

    @Test
    fun `a sign-in refresh that outlived its session neither reports nor publishes`() = runTest {
        val p = ports(published = { served(sig = otherSig, generation = 2L) })
        p.current = false

        val result = land(IdentityLanding.SIGN_IN, p)

        assertEquals(IdentityPublicationResult.Superseded, result)
        assertEquals(listOf("fetch"), p.calls)
        assertTrue(p.stale.isEmpty())
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
        val p = ports(published = { served() })

        val result = land(IdentityLanding.SESSION_RESTORE, p)

        assertEquals(IdentityPublicationResult.UpToDate, result)
        assertEquals(listOf("fetch"), p.calls)
    }

    @Test
    fun `a restore publishes a missing or stale bundle and skips when it cannot read`() = runTest {
        val missing = ports(published = { null })
        land(IdentityLanding.SESSION_RESTORE, missing)
        assertEquals(listOf("fetch", "upload(genesis=true)"), missing.calls)

        val staleX = ports(published = { served(x = ByteArray(32) { 9 }) })
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
    fun `a restore re-publishes our keys when the directory row carries no valid X25519 binding`() = runTest {
        // A row written before discovery required the binding: our keys, no
        // binding. Every client refuses to seal to it, so the restore uploads
        // the same keys again (with the binding), never as a genesis entry.
        val p = ports(published = { served(bindingValid = false) })

        val result = land(IdentityLanding.SESSION_RESTORE, p)

        assertEquals(IdentityPublicationResult.Uploaded(withGenesisAuth = false), result)
        assertEquals(listOf("fetch", "upload(genesis=false)"), p.calls)
        assertTrue(p.stale.isEmpty())
    }

    @Test
    fun `a restore that finds other keys without a binding uploads nothing`() = runTest {
        // A reset or rotation elsewhere is the landing rule's, binding or not.
        val reset = ports(published = { served(sig = otherSig, generation = 3L, bindingValid = false) }, ownGeneration = 2L)
        assertEquals(IdentityPublicationResult.StaleKeys, land(IdentityLanding.SESSION_RESTORE, reset))
        assertEquals(listOf("fetch"), reset.calls)

        val mismatch = ports(published = { served(sig = otherSig, generation = 1L, bindingValid = false) })
        assertEquals(IdentityPublicationResult.KeyMismatch, land(IdentityLanding.SESSION_RESTORE, mismatch))
        assertEquals(listOf("fetch"), mismatch.calls)
    }

    @Test
    fun `a failed binding re-publish is logged and leaves the next launch to try again`() = runTest {
        val p = ports(published = { served(bindingValid = false) }, uploadFailure = RuntimeException("offline"))

        val result = land(IdentityLanding.SESSION_RESTORE, p)

        assertEquals(IdentityPublicationResult.UploadFailed, result)
        assertEquals(listOf("fetch", "upload(genesis=false)"), p.calls)
        // Nothing is recorded: the next launch reads the row again, and an
        // unbound row is re-published again.
        val again = ports(published = { served(bindingValid = false) })
        assertEquals(IdentityPublicationResult.Uploaded(withGenesisAuth = false), land(IdentityLanding.SESSION_RESTORE, again))
    }

    @Test
    fun `a restore that finds other keys at a higher generation signs the device out`() = runTest {
        val p = ports(published = { served(sig = otherSig, generation = 3L) }, ownGeneration = 2L)

        val result = land(IdentityLanding.SESSION_RESTORE, p)

        assertEquals(IdentityPublicationResult.StaleKeys, result)
        assertEquals(listOf(LandingVerdict.StaleKeys), p.stale)
        assertEquals(listOf("fetch"), p.calls)
    }

    // --- recover ------------------------------------------------------------

    @Test
    fun `recover reads first and re-publishes the unchanged key`() = runTest {
        val p = ports(published = { served() })

        val result = land(IdentityLanding.RECOVER, p)

        assertEquals(listOf("fetch", "upload(genesis=false)"), p.calls)
        assertEquals(IdentityPublicationResult.Uploaded(withGenesisAuth = false), result)

        val first = ports(published = { null })
        land(IdentityLanding.RECOVER, first)
        assertEquals(listOf("fetch", "upload(genesis=true)"), first.calls)
    }

    @Test
    fun `recover with the old phrase after a reset elsewhere never publishes the old key back`() = runTest {
        val p = ports(published = { served(sig = otherSig, generation = 2L) })

        val result = land(IdentityLanding.RECOVER, p)

        assertEquals(IdentityPublicationResult.StaleKeys, result)
        assertEquals(listOf("fetch"), p.calls)
    }
}
