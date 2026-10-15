// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiIntroductionPayload
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachGrant
import uniffi.lakko_ffi_uniffi.OprfResult

/**
 * ADR-0060 section 4 — contacts of contacts, the sending half (web
 * `publication-model.ts` + `ScopedDiscoveryRuntime.tsx` steps 2-4) and the
 * receiving decode (web `scoped-intake.ts`), over fake native steps.
 */
class IntroductionPublicationTest {

    private val fof = FfiReachGrant(FfiReachClass.FOF, 2u, ByteArray(32) { 7 }, ByteArray(97) { 8 })

    private class Recorder {
        val oprf = mutableListOf<String>()
        val sent = mutableListOf<Pair<String, String>>()
        var composedPayload: FfiIntroductionPayload? = null
        var composedOutputs: List<String> = emptyList()
        var bundleRecords: List<String> = emptyList()
    }

    private fun ops(r: Recorder, maxBundle: Int = 64) = IntroPublicationOps(
        nameSearchTerms = { name -> name.lowercase().split(" ") },
        oprfNameTerm = { t -> r.oprf += "n:$t"; OprfResult("out:n:$t".toByteArray(), "key:n:$t".toByteArray()) },
        oprfUsername = { u -> r.oprf += "u:$u"; OprfResult("out:u:$u".toByteArray(), "key:u:$u".toByteArray()) },
        composeIntroduction = { payload, outputs, _ ->
            r.composedPayload = payload
            r.composedOutputs = outputs.map { String(it) }
            "record".toByteArray()
        },
        composeOffer = { record -> "offer:${String(record)}".toByteArray() },
        composeBundle = { records, _ ->
            r.bundleRecords = records.map { String(it) }
            "bundle".toByteArray()
        },
        maxBundleRecords = maxBundle,
        send = { g, envelope -> r.sent += g to String(envelope) },
        save = {},
    )

    private fun inputs(
        fofOn: Boolean = true,
        fofVersion: Int = 2,
        name: String? = "Anna Weber",
        username: String? = "anna",
        offered: List<String> = emptyList(),
        groups: List<String> = listOf("g1", "g2"),
    ) = IntroPublicationInputs(
        contactsOfContacts = fofOn,
        fofVersion = fofVersion,
        fofGrant = fof,
        displayName = name,
        username = username,
        certificate = "cert".toByteArray(),
        identityPk = ByteArray(32) { 1 },
        accountId = ByteArray(16) { 2 },
        offeredRecordsHex = offered,
        oneToOneGroups = groups,
        nowMs = 1000,
    )

    @Test
    fun `with contacts of contacts on, the record is sealed under the outputs and offered to every contact`() = runTest {
        val r = Recorder()
        val state = publishIntroductions(IntroPublicationState(), inputs(), ops(r))
        assertEquals(listOf("out:n:anna", "out:n:weber", "out:u:anna"), r.composedOutputs)
        assertEquals(fof, r.composedPayload!!.fof)
        // ADR-0078: the record carries the owner's mailbox binding, the one
        // the `fof` grant carries.
        org.junit.Assert.assertArrayEquals(fof.binding, r.composedPayload!!.mailboxBinding)
        assertEquals("anna", r.composedPayload!!.username)
        assertEquals(listOf("g1" to "offer:record", "g2" to "offer:record"), r.sent)
        assertEquals(setOf("g1", "g2"), state.offerSentTo.keys)
    }

    @Test
    fun `with contacts of contacts off, no record is built and none offered`() = runTest {
        val r = Recorder()
        val state = publishIntroductions(IntroPublicationState(), inputs(fofOn = false), ops(r))
        assertNull(state.ownOffer)
        assertTrue(r.oprf.isEmpty())
        assertTrue(r.sent.isEmpty())
    }

    @Test
    fun `a second run with nothing changed evaluates and sends nothing`() = runTest {
        val first = Recorder()
        val state = publishIntroductions(IntroPublicationState(), inputs(), ops(first))
        val second = Recorder()
        publishIntroductions(state, inputs(), ops(second))
        assertTrue(second.oprf.isEmpty())
        assertTrue(second.sent.isEmpty())
    }

    @Test
    fun `a new fof version rebuilds from the cached outputs and offers again`() = runTest {
        val first = Recorder()
        val state = publishIntroductions(IntroPublicationState(), inputs(), ops(first))
        val second = Recorder()
        publishIntroductions(state, inputs(fofVersion = 3), ops(second))
        assertTrue("outputs come from the cache", second.oprf.isEmpty())
        assertEquals(2, second.sent.size)
    }

    @Test
    fun `the contacts' records are relayed once per set, capped at the bundle maximum`() = runTest {
        val r = Recorder()
        val offered = listOf(reachHex("a".toByteArray()), reachHex("b".toByteArray()), reachHex("c".toByteArray()))
        val state = publishIntroductions(
            IntroPublicationState(),
            inputs(fofOn = false, offered = offered, groups = listOf("g1")),
            ops(r, maxBundle = 2),
        )
        assertEquals(listOf("a", "b"), r.bundleRecords)
        assertEquals(listOf("g1" to "bundle"), r.sent)
        val again = Recorder()
        publishIntroductions(state, inputs(fofOn = false, offered = offered, groups = listOf("g1")), ops(again))
        assertTrue(again.sent.isEmpty())
    }

    @Test
    fun `an offer decodes to its record, a bundle to its stamp, anything else is dropped`() {
        val offer = decideIntroIntake(
            ScopedEnvelopeKind.INTRODUCTION_OFFER,
            "x".toByteArray(),
            decodeOffer = { "record".toByteArray() },
            decodeBundleStamp = { 0uL },
        )
        assertEquals("record", String((offer as IntroIntake.Offer).record))
        val bundle = decideIntroIntake(
            ScopedEnvelopeKind.INTRODUCTIONS,
            "b".toByteArray(),
            decodeOffer = { error("unused") },
            decodeBundleStamp = { 9uL },
        )
        assertEquals(9uL, (bundle as IntroIntake.Bundle).updatedAtMs)
        val malformed = decideIntroIntake(
            ScopedEnvelopeKind.INTRODUCTION_OFFER,
            "x".toByteArray(),
            decodeOffer = { throw IllegalArgumentException("malformed") },
            decodeBundleStamp = { 0uL },
        )
        assertEquals(IntroIntake.Drop, malformed)
        assertEquals(
            IntroIntake.Drop,
            decideIntroIntake(ScopedEnvelopeKind.VISIBILITY, ByteArray(0), { it }, { 0uL }),
        )
    }
}
