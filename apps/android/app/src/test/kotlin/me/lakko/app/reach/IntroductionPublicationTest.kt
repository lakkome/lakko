// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
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

    private val nothing = IntroductionsHeld(offer = false, bundle = false, contactGrant = false)

    @Test
    fun `a conversation lacks introductions while no offer and no bundle came, whatever the grant`() {
        assertTrue(lacksIntroductions(nothing))
        assertTrue("a contact grant does not stop the ask", lacksIntroductions(nothing.copy(contactGrant = true)))
        assertFalse(lacksIntroductions(nothing.copy(offer = true)))
        assertFalse(lacksIntroductions(nothing.copy(bundle = true)))
        assertFalse("a group, or a roster that could not be read", lacksIntroductions(null))
    }

    /** The shape of the core's rule for this ask: once a day. */
    private val daily: (ULong?, ULong) -> Boolean = { last, now ->
        last == null || (now >= last && now - last >= 86_400_000uL)
    }

    @Test
    fun `with no contact grant the daily rule decides, and is handed the stored time`() {
        val day = 86_400_000uL
        assertEquals(5uL, introductionsAskStamp(nothing, null, 5uL, daily))
        assertNull(introductionsAskStamp(nothing, 100uL, 100uL + day - 1uL, daily))
        assertEquals(100uL + day, introductionsAskStamp(nothing, 100uL, 100uL + day, daily))
        var seen: Pair<ULong?, ULong>? = null
        introductionsAskStamp(nothing, 3uL, 4uL) { last, now ->
            seen = last to now
            false
        }
        assertEquals(3uL to 4uL, seen)
    }

    @Test
    fun `with a contact grant the first ask goes out at once, later ones a week apart`() {
        val granted = nothing.copy(contactGrant = true)
        val week = INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS.toULong()
        val never: (ULong?, ULong) -> Boolean = { _, _ -> error("the daily rule is not this arm") }
        assertEquals(9uL, introductionsAskStamp(granted, null, 9uL, never))
        assertNull(introductionsAskStamp(granted, 100uL, 100uL + week - 1uL, never))
        assertEquals(100uL + week, introductionsAskStamp(granted, 100uL, 100uL + week, never))
        assertNull("a backwards clock reads as recently asked", introductionsAskStamp(granted, 100uL, 99uL, never))
    }

    @Test
    fun `an ask goes out only where something lacks and the time allows, and is then recorded`() = runTest {
        val sent = mutableListOf<String>()
        val recorded = mutableListOf<Pair<String, ULong>>()
        val decided = mutableListOf<IntroductionsHeld>()
        val withGrant = nothing.copy(contactGrant = true)
        askForIntroductions(
            heldByGroup = linkedMapOf(
                "empty" to nothing,
                "with-grant" to withGrant,
                "throttled" to nothing,
                "has-offer" to nothing.copy(offer = true),
                "has-bundle" to nothing.copy(bundle = true),
                "a-group" to null,
            ),
            decide = { g, held ->
                decided += held
                if (g == "throttled") null else 7uL
            },
            send = { g -> sent += g; true },
            record = { g, stamp -> recorded += g to stamp },
        )
        assertEquals(listOf("empty", "with-grant"), sent)
        assertEquals(listOf("empty" to 7uL, "with-grant" to 7uL), recorded)
        assertEquals("the rule is told about the grant", listOf(nothing, withGrant, nothing), decided)
    }

    @Test
    fun `an ask that did not go out records nothing and does not stop the others`() = runTest {
        val recorded = mutableListOf<String>()
        val failures = mutableListOf<Throwable>()
        askForIntroductions(
            heldByGroup = linkedMapOf("throws" to nothing, "refused" to nothing, "fine" to nothing),
            decide = { _, _ -> 1uL },
            send = { g ->
                when (g) {
                    "throws" -> throw IllegalStateException("offline")
                    "refused" -> false
                    else -> true
                }
            },
            record = { g, _ -> recorded += g },
            noteFailure = { failures += it },
        )
        assertEquals(listOf("fine"), recorded)
        assertEquals(1, failures.size)
    }

    private val sentState = IntroPublicationState(
        offerSentTo = mapOf("g1" to "o", "g2" to "o"),
        bundleSentTo = mapOf("g1" to "b", "g2" to "b"),
    )

    @Test
    fun `an ask from the other person of a one-to-one forgets only that conversation`() {
        val next = reopenedByAsk(sentState, "g1", oneToOne = true, fromOtherPerson = true, nowMs = 5_000)!!
        assertEquals(mapOf("g2" to "o"), next.offerSentTo)
        assertEquals(mapOf("g2" to "b"), next.bundleSentTo)
        assertEquals(mapOf("g1" to 5_000L), next.reopenedAtMs)
    }

    @Test
    fun `an ask from anyone else, or in a group, reopens nothing`() {
        assertNull(reopenedByAsk(sentState, "g1", oneToOne = false, fromOtherPerson = true, nowMs = 5_000))
        assertNull(reopenedByAsk(sentState, "g1", oneToOne = true, fromOtherPerson = false, nowMs = 5_000))
    }

    @Test
    fun `an ask reopens a conversation at most once an hour, each conversation on its own`() {
        val t0 = 1_000_000L
        val once = reopenedByAsk(sentState, "g1", true, true, t0)!!
        assertNull(reopenedByAsk(once, "g1", true, true, t0 + INTRODUCTIONS_RESEND_MIN_MS - 1))
        assertTrue("a backwards clock reads as recently done", reopenedByAsk(once, "g1", true, true, t0 - 1) == null)
        assertTrue(reopenedByAsk(once, "g1", true, true, t0 + INTRODUCTIONS_RESEND_MIN_MS) != null)
        assertTrue("another conversation is not throttled", reopenedByAsk(once, "g2", true, true, t0 + 1) != null)
    }

    @Test
    fun `after a reopen the next run sends the offer and the bundle to that conversation again`() = runTest {
        val r = Recorder()
        val first = publishIntroductions(IntroPublicationState(), inputs(offered = listOf("6162")), ops(r))
        r.sent.clear()
        val reopened = reopenedByAsk(first, "g1", true, true, 5_000)!!
        publishIntroductions(reopened, inputs(offered = listOf("6162")), ops(r))
        assertEquals(listOf("g1" to "offer:record", "g1" to "bundle"), r.sent)
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
