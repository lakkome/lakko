// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.coroutines.test.runTest
import me.lakko.app.discovery.DiscoveryHttpError
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiContactsForm
import uniffi.lakko_ffi_uniffi.FfiIntroductionPayload
import uniffi.lakko_ffi_uniffi.FfiReachClass
import uniffi.lakko_ffi_uniffi.FfiReachGrant
import uniffi.lakko_ffi_uniffi.FfiVisibilityMode
import uniffi.lakko_ffi_uniffi.FfiVisibilitySetting
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
        /** ADR-0091 walk 3, W3-R2 — every send answers "held". */
        var held = false
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
        send = { g, envelope ->
            r.sent += g to String(envelope)
            !r.held
        },
        save = {},
    )

    private fun inputs(
        fofOn: Boolean = true,
        fofVersion: Int = 2,
        name: String? = "Anna Weber",
        username: String? = "anna",
        offered: List<String> = emptyList(),
        groups: List<String> = listOf("g1", "g2"),
        offerTo: List<String> = groups,
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
        offerGroups = offerTo,
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
    fun `a held send is not recorded, so the next run sends it`() = runTest {
        // ADR-0091 walk 3, W3-R2 — the device may be behind in the
        // conversation (a refused read): the send is held, not done.
        val held = Recorder().also { it.held = true }
        val state = publishIntroductions(IntroPublicationState(), inputs(), ops(held))
        assertTrue(state.offerSentTo.isEmpty())
        val next = Recorder()
        val after = publishIntroductions(state, inputs(), ops(next))
        assertEquals(listOf("g1" to "offer:record", "g2" to "offer:record"), next.sent)
        assertEquals(setOf("g1", "g2"), after.offerSentTo.keys)
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
    fun `a failed build keeps the terms it paid for and resumes after them`() = runTest {
        val first = Recorder()
        var saved = IntroPublicationState()
        val failing = ops(first).let { base ->
            IntroPublicationOps(
                nameSearchTerms = base.nameSearchTerms,
                oprfNameTerm = { t -> if (first.oprf.size == 1) error("offline") else base.oprfNameTerm(t) },
                oprfUsername = base.oprfUsername,
                composeIntroduction = base.composeIntroduction,
                composeOffer = base.composeOffer,
                composeBundle = base.composeBundle,
                maxBundleRecords = base.maxBundleRecords,
                send = base.send,
                save = { saved = it },
            )
        }
        runCatching { publishIntroductions(IntroPublicationState(), inputs(), failing) }
        assertEquals(1, saved.termOutputs.size)

        val second = Recorder()
        publishIntroductions(saved, inputs(), ops(second))
        assertFalse("the paid term is not paid for again", second.oprf.any { it in saved.termOutputs })
    }

    @Test
    fun `a failure building the own record still relays the contacts' records, then is thrown`() = runTest {
        val r = Recorder()
        val failing = ops(r).let { base ->
            IntroPublicationOps(
                nameSearchTerms = base.nameSearchTerms,
                oprfNameTerm = { error("token refused") },
                oprfUsername = { error("token refused") },
                composeIntroduction = base.composeIntroduction,
                composeOffer = base.composeOffer,
                composeBundle = base.composeBundle,
                maxBundleRecords = base.maxBundleRecords,
                send = base.send,
                save = {},
            )
        }
        val offered = listOf(reachHex("a".toByteArray()))
        val thrown = runCatching { publishIntroductions(IntroPublicationState(), inputs(offered = offered), failing) }
        assertTrue(thrown.isFailure)
        assertEquals(listOf("g1" to "bundle", "g2" to "bundle"), r.sent)
    }

    @Test
    fun `a record built under another mailbox binding is rebuilt`() = runTest {
        assertFalse(
            introductionOfferKey(2, "Anna", "anna", "aa") == introductionOfferKey(2, "Anna", "anna", "bb"),
        )
        assertFalse(introductionOfferKey(2, "Anna", "anna") == introductionOfferKey(2, "Anna", "anna", reachHex(fof.binding)))
    }

    @Test
    fun `a rate limited term waits and is asked once more`() = runTest {
        var calls = 0
        val answer = waitingOutRateLimit {
            if (++calls == 1) throw DiscoveryHttpError(429, "/discovery/oprf", "")
            "output"
        }
        assertEquals("output", answer)
        assertEquals(2, calls)
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
    fun `a conversation lacks introductions while no offer came, whatever the bundle or the grant`() {
        assertTrue(lacksIntroductions(nothing))
        assertTrue("a contact grant does not stop the ask", lacksIntroductions(nothing.copy(contactGrant = true)))
        assertFalse(lacksIntroductions(nothing.copy(offer = true)))
        // A bundle does not stand in for the contact's own offer.
        assertTrue(lacksIntroductions(nothing.copy(bundle = true)))
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
        // A bundle without the contact's own offer still asks.
        assertEquals(listOf("empty", "with-grant", "has-bundle"), sent)
        assertEquals(listOf("empty" to 7uL, "with-grant" to 7uL, "has-bundle" to 7uL), recorded)
        assertEquals(
            "the rule is told about the grant",
            listOf(nothing, withGrant, nothing, nothing.copy(bundle = true)),
            decided,
        )
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

    // --- only the contacts I choose ------------------------------------------

    private val idA = ByteArray(16) { 0xA }
    private val idB = ByteArray(16) { 0xB }

    private fun allowedTo(form: FfiContactsForm, peers: Map<String, ByteArray?>, chosen: Set<ByteArray>) =
        offerGroups(form, peers.keys.toList(), { peers[it] }) { id ->
            form == FfiContactsForm.ALL || chosen.any { it.contentEquals(id) }
        }

    @Test
    fun `under all every contact gets the record, known id or not`() {
        val peers = mapOf("g1" to idA, "g2" to null)
        assertEquals(listOf("g1", "g2"), allowedTo(FfiContactsForm.ALL, peers, emptySet()))
    }

    @Test
    fun `under chosen only a chosen contact gets it, and no known id gets none`() {
        val peers = mapOf("g1" to idA, "g2" to idB, "g3" to null)
        assertEquals(listOf("g2"), allowedTo(FfiContactsForm.CHOSEN, peers, setOf(idB)))
        assertEquals(emptyList<String>(), allowedTo(FfiContactsForm.CHOSEN, peers, emptySet()))
    }

    @Test
    fun `the offer goes to the allowed contacts and the bundle to every contact`() = runTest {
        val r = Recorder()
        publishIntroductions(
            IntroPublicationState(),
            inputs(offered = listOf("aa"), groups = listOf("g1", "g2"), offerTo = listOf("g2")),
            ops(r),
        )
        assertEquals(listOf("g2"), r.sent.filter { it.second.startsWith("offer") }.map { it.first })
        assertEquals(listOf("g1", "g2"), r.sent.filter { it.second == "bundle" }.map { it.first })
    }

    @Test
    fun `an ask from a contact not allowed gets no offer`() = runTest {
        val r = Recorder()
        val state = publishIntroductions(IntroPublicationState(), inputs(offerTo = listOf("g2")), ops(r))
        r.sent.clear()
        val reopened = reopenedByAsk(state, "g1", oneToOne = true, fromOtherPerson = true, nowMs = 1000)!!
        publishIntroductions(reopened, inputs(offerTo = listOf("g2")), ops(r))
        assertTrue(r.sent.none { it.second.startsWith("offer") })
    }

    @Test
    fun `a new version is built and offered only to the contacts still allowed`() = runTest {
        val r = Recorder()
        val state = publishIntroductions(IntroPublicationState(), inputs(fofVersion = 2), ops(r))
        r.sent.clear()
        publishIntroductions(state, inputs(fofVersion = 3, offerTo = listOf("g2")), ops(r))
        assertEquals(listOf("g2"), r.sent.map { it.first })
    }

    @Test
    fun `a widened contact gets the record held, with no new version`() = runTest {
        val r = Recorder()
        val state = publishIntroductions(IntroPublicationState(), inputs(offerTo = listOf("g2")), ops(r))
        r.sent.clear()
        publishIntroductions(state, inputs(offerTo = listOf("g1", "g2")), ops(r))
        assertEquals(listOf("g1"), r.sent.map { it.first })
    }

    private fun setting(chosen: List<ByteArray>, form: FfiContactsForm = FfiContactsForm.CHOSEN) = FfiVisibilitySetting(
        mode = FfiVisibilityMode.SOME,
        contactsOfContacts = true,
        contactsForm = form,
        chosenContacts = chosen,
        groups = null,
        chosen = emptyList(),
        lastContactsOfContacts = true,
        lastGroups = null,
        updatedAtMs = 1uL,
    )

    @Test
    fun `a removed contact leaves the chosen list`() {
        val after = withoutChosenContact(setting(listOf(idA, idB)), idA.copyOf())!!
        assertEquals(1, after.chosenContacts.size)
        assertTrue(after.chosenContacts.single().contentEquals(idB))
        assertNull("not on the list: nothing to change", withoutChosenContact(setting(listOf(idB)), idA))
    }

    @Test
    fun `the setting reads as the narrowing rule reads it`() {
        val reach = setting(listOf(idA)).contactsReach()
        assertTrue(reach.onlyChosen)
        assertEquals(setOf(reachHex(idA)), reach.chosenHex)
        assertEquals(false, setting(emptyList(), FfiContactsForm.ALL).contactsReach().onlyChosen)
    }

    @Test
    fun `the introduction's terms go out in one batch and only the missing ones`() = runTest {
        val r = Recorder()
        val batches = mutableListOf<List<me.lakko.app.discovery.OwnTerm>>()
        val base = ops(r)
        val batched = IntroPublicationOps(
            nameSearchTerms = base.nameSearchTerms,
            oprfNameTerm = base.oprfNameTerm,
            oprfUsername = base.oprfUsername,
            composeIntroduction = base.composeIntroduction,
            composeOffer = base.composeOffer,
            composeBundle = base.composeBundle,
            maxBundleRecords = base.maxBundleRecords,
            send = base.send,
            save = {},
            oprfOwnTerms = { own ->
                batches += own
                own.map { t ->
                    val tag = if (t is me.lakko.app.discovery.OwnTerm.Username) "u:${t.value}" else "n:${t.value}"
                    OprfResult("out:$tag".toByteArray(), "key:$tag".toByteArray())
                }
            },
        )
        val held = IntroPublicationState(termOutputs = mapOf("n:anna" to reachHex("out:n:anna".toByteArray())))
        publishIntroductions(held, inputs(), batched)
        assertEquals(1, batches.size)
        assertEquals(
            listOf(
                me.lakko.app.discovery.OwnTerm.NameTerm("weber"),
                me.lakko.app.discovery.OwnTerm.Username("anna"),
            ),
            batches[0],
        )
        assertTrue(r.oprf.isEmpty())
        assertEquals(listOf("out:n:anna", "out:n:weber", "out:u:anna"), r.composedOutputs)
    }
}
