// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.ds.DsHttpError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0073 section 2.3: the reset's step sequencing, outcomes, age rule and
 *  the persisted debt. */
class KeyResetTest {

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

    private val attestation = ByteArray(177) { (it * 3).toByte() }
    private val pubkey = ByteArray(32) { 9 }

    private fun owed(discovery: Boolean = false, deviceGroup: Boolean = false) = KeyResetOwed(
        attestationB64 = base64Encode(attestation),
        keyGeneration = 2L,
        signedAtMs = 5_000L,
        newSignaturePubkeyB64 = base64Encode(pubkey),
        discoveryPublished = discovery,
        deviceGroupReset = deviceGroup,
    )

    @Test
    fun `the directory goes first, then the device group, then nothing`() {
        assertEquals(KeyResetStep.DiscoveryPublish, nextKeyResetStep(owed()))
        assertEquals(KeyResetStep.DeviceGroupReset, nextKeyResetStep(owed(discovery = true)))
        assertEquals(KeyResetStep.Done, nextKeyResetStep(owed(discovery = true, deviceGroup = true)))
        // The device group never goes before the directory.
        assertEquals(KeyResetStep.DiscoveryPublish, nextKeyResetStep(owed(deviceGroup = true)))
    }

    @Test
    fun `the directory upload's answers are landed, already landed, retry or refused`() {
        assertEquals(KeyResetStepOutcome.Landed, discoveryResetOutcome(null))
        assertEquals(
            KeyResetStepOutcome.AlreadyLanded,
            discoveryResetOutcome(DiscoveryHttpError(409, "/discovery/identity", """{"error":"key-generation-not-newer"}""")),
        )
        val refused = discoveryResetOutcome(
            DiscoveryHttpError(400, "/discovery/identity", """{"error":"key-reset-attestation-invalid"}"""),
        )
        assertTrue(refused is KeyResetStepOutcome.Refused)
        // A cool-down, a disabled server, a stranger 409, a network failure:
        // all retried with the same attestation.
        val cooldown = discoveryResetOutcome(
            DiscoveryHttpError(429, "/discovery/identity", """{"error":"identity-changed-too-recently","retry_after_secs":600}"""),
        )
        assertTrue(cooldown is KeyResetStepOutcome.Retry)
        assertEquals(600L, (cooldown as KeyResetStepOutcome.Retry).retryAfterSecs)
        assertTrue(
            discoveryResetOutcome(DiscoveryHttpError(503, "/discovery/identity", """{"error":"key-reset-disabled"}"""))
                is KeyResetStepOutcome.Retry,
        )
        assertTrue(
            discoveryResetOutcome(DiscoveryHttpError(409, "/discovery/identity", """{"error":"identity-change-needs-attestation"}"""))
                is KeyResetStepOutcome.Retry,
        )
        val offline = java.io.IOException("offline")
        val retry = discoveryResetOutcome(offline)
        assertTrue(retry is KeyResetStepOutcome.Retry)
        assertSame(offline, (retry as KeyResetStepOutcome.Retry).cause)
        assertTrue(keyResetStepLanded(KeyResetStepOutcome.Landed))
        assertTrue(keyResetStepLanded(KeyResetStepOutcome.AlreadyLanded))
        assertFalse(keyResetStepLanded(retry))
        assertFalse(keyResetStepLanded(refused))
    }

    @Test
    fun `the device group reset's 409 is landed only when the root probe confirms`() {
        assertEquals(KeyResetStepOutcome.Landed, deviceGroupResetOutcome(null, rootProbeConfirmed = false))
        val conflict = DsHttpError(409, "/ds/group/x/reset", "")
        assertEquals(KeyResetStepOutcome.AlreadyLanded, deviceGroupResetOutcome(conflict, rootProbeConfirmed = true))
        assertTrue(deviceGroupResetOutcome(conflict, rootProbeConfirmed = false) is KeyResetStepOutcome.Retry)
        assertTrue(deviceGroupResetOutcome(DsHttpError(400, "/ds/group/x/reset", ""), false) is KeyResetStepOutcome.Refused)
        // A stranger's 401 (the DS has no pinned key, or the generation was not
        // newer) and a 503 are retried.
        assertTrue(deviceGroupResetOutcome(DsHttpError(401, "/ds/group/x/reset", ""), false) is KeyResetStepOutcome.Retry)
        assertTrue(deviceGroupResetOutcome(DsHttpError(503, "/ds/group/x/reset", ""), false) is KeyResetStepOutcome.Retry)
    }

    @Test
    fun `a stored attestation is used for 30 days, then only after a fresh sign-in`() {
        val signedAt = 1_000_000L
        val day = 24L * 60 * 60 * 1000
        assertEquals(AttestationAge.Fresh, attestationAgeAt(signedAt, signedAt, null))
        assertEquals(AttestationAge.Fresh, attestationAgeAt(signedAt, signedAt + 30 * day, null))
        assertEquals(AttestationAge.StaleNeedsSignIn, attestationAgeAt(signedAt, signedAt + 30 * day + 1, null))
        // A sign-in before the mint confirms nothing; one after it does.
        assertEquals(AttestationAge.StaleNeedsSignIn, attestationAgeAt(signedAt, signedAt + 31 * day, signedAt - 1))
        assertEquals(AttestationAge.StaleConfirmed, attestationAgeAt(signedAt, signedAt + 31 * day, signedAt + 31 * day))
        assertEquals(30L * day, KEY_RESET_ATTESTATION_MAX_AGE_MS)
    }

    @Test
    fun `the debt round-trips through the store and records each step`() {
        val storage = MapStorage()
        val store = KeyResetOwedStore(storage)
        assertNull(store.load("a"))

        store.save("a", owed())
        val loaded = store.load("a")!!
        assertTrue(attestation.contentEquals(loaded.attestation))
        assertTrue(pubkey.contentEquals(loaded.newSignaturePubkey))
        assertEquals(2L, loaded.keyGeneration)
        assertEquals(5_000L, loaded.signedAtMs)
        assertFalse(loaded.discoveryPublished)

        store.markStep("a", KeyResetStep.DiscoveryPublish)
        assertEquals(KeyResetStep.DeviceGroupReset, nextKeyResetStep(store.load("a")!!))
        store.markStep("a", KeyResetStep.DeviceGroupReset)
        assertEquals(KeyResetStep.Done, nextKeyResetStep(store.load("a")!!))
        // The attestation survives every marker.
        assertTrue(attestation.contentEquals(store.load("a")!!.attestation))
        // Another account is untouched; a missing row takes no marker.
        store.markStep("b", KeyResetStep.DiscoveryPublish)
        assertNull(store.load("b"))

        assertTrue(KeyResetOwedStore(storage).load("a") != null)
        store.clear("a")
        assertNull(store.load("a"))
        assertTrue(storage.rows.isEmpty())
    }

    @Test
    fun `a corrupt row reads as no debt`() {
        val storage = MapStorage()
        storage.rows["key-reset-v1:a"] = "{not json"
        assertNull(KeyResetOwedStore(storage).load("a"))
    }
}
