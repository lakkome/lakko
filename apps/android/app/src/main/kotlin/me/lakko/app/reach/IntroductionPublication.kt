// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.delay
import me.lakko.app.discovery.isDiscoveryRateLimited
import uniffi.lakko_ffi_uniffi.FfiContactsForm
import uniffi.lakko_ffi_uniffi.FfiIntroductionPayload
import uniffi.lakko_ffi_uniffi.FfiReachGrant
import uniffi.lakko_ffi_uniffi.FfiVisibilitySetting
import uniffi.lakko_ffi_uniffi.OprfResult

/*
 * ADR-0060 section 4 — contacts of contacts, the sending half. The Android
 * twin of `apps/web/src/reach/publication-model.ts` and steps 2-4 of
 * `ScopedDiscoveryRuntime.tsx`:
 *
 * - the **introduction record**, rebuilt when 2.1 is on and the `fof` key
 *   version, the name or the username changed, sealed under the OPRF OUTPUTS
 *   of the name search terms and the username;
 * - offered to every allowed one-to-one contact not sent this version yet
 *   (only while 2.1 is on);
 * - the **bundle** this account relays: the records its contacts offered,
 *   capped at `maxBundleRecords()`, re-sent to a contact when that set
 *   changed.
 *
 * Every OPRF evaluation costs an anonymous token, so the outputs are cached
 * ([IntroPublicationState.termOutputs]) and nothing is re-sent unless an
 * input moved. Kept free of Android and of native calls (every FFI function
 * is passed in), so the decisions run on the host JVM.
 */

/** The inputs an introduction record is built from, as one key. */
internal fun introductionOfferKey(
    fofVersion: Int,
    name: String?,
    username: String?,
    /** ADR-0078 — the mailbox binding the record carries, hex; a record built
     *  before it (or with another binding) is rebuilt, as on iOS. */
    mailboxBindingHex: String = "",
): String =
    listOf(fofVersion.toString(), name ?: "", username ?: "", mailboxBindingHex).joinToString("\u0001")

/** Whether the held record must be rebuilt. `false` when 2.1 is off. */
internal fun offerNeedsRebuild(heldKey: String?, contactsOfContacts: Boolean, key: String): Boolean =
    contactsOfContacts && (heldKey == null || heldKey != key)

/** The conversations the current record has not been sent to yet. */
internal fun offerTargets(
    offerKey: String?,
    sentTo: Map<String, String>,
    contactsOfContacts: Boolean,
    oneToOneGroups: List<String>,
): List<String> {
    if (!contactsOfContacts || offerKey == null) return emptyList()
    return oneToOneGroups.filter { sentTo[it] != offerKey }
}

/**
 * The one-to-ones the record may go to, [oneToOneGroups] kept in order. Under
 * "only the ones I choose" a contact with no known account id cannot be chosen,
 * so it gets none; [allowsContact] answers for the rest. The relay is not this:
 * the bundle goes to every contact.
 */
internal fun offerGroups(
    form: FfiContactsForm,
    oneToOneGroups: List<String>,
    peerAccountId: (groupHex: String) -> ByteArray?,
    allowsContact: (ByteArray) -> Boolean,
): List<String> = oneToOneGroups.filter { groupHex ->
    val accountId = peerAccountId(groupHex)
    if (accountId == null) form == FfiContactsForm.ALL else allowsContact(accountId)
}

/** What the narrowing rule reads of a setting. */
internal fun FfiVisibilitySetting.contactsReach(): ContactsReach = ContactsReach(
    onlyChosen = contactsForm == FfiContactsForm.CHOSEN,
    chosenHex = chosenContacts.mapTo(HashSet(), ::reachHex),
)

/** [setting] with [accountId] off the chosen list, or `null` when it is not on
 *  it. Removing an id needs no length check. */
internal fun withoutChosenContact(setting: FfiVisibilitySetting, accountId: ByteArray): FfiVisibilitySetting? {
    val kept = setting.chosenContacts.filterNot { it.contentEquals(accountId) }
    return if (kept.size == setting.chosenContacts.size) null else setting.copy(chosenContacts = kept)
}

/** A stable digest of a set of record wires (order-independent). */
internal fun recordsDigest(recordsHex: Collection<String>): String = recordsHex.sorted().joinToString(",")

/** The digest of a relayed set with no record in it. */
internal val EMPTY_RECORDS_DIGEST: String = recordsDigest(emptyList())

/**
 * The conversations the relayed bundle (of [digest]) must go to. A set that
 * became empty (the last contact who offered a record was removed) goes, as a
 * bundle of padding only, to the conversations that were sent a bundle
 * before: it replaces theirs, so the removed person's record stops being
 * relayed. A conversation never sent a bundle gets no empty one.
 */
internal fun bundleTargets(sentTo: Map<String, String>, digest: String, oneToOneGroups: List<String>): List<String> =
    if (digest == EMPTY_RECORDS_DIGEST) {
        oneToOneGroups.filter { sentTo[it] != null && sentTo[it] != digest }
    } else {
        oneToOneGroups.filter { sentTo[it] != digest }
    }

/** The terms still to evaluate (not in the cache). */
internal fun termsToEvaluate(cached: Set<String>, terms: List<String>): List<String> =
    terms.distinct().filterNot { it in cached }

/** What this device last published for contacts of contacts. */
internal data class IntroPublicationState(
    /** Cache key (`"n:<term>"` / `"u:<username>"`) -> OPRF OUTPUT hex. */
    val termOutputs: Map<String, String> = emptyMap(),
    val ownOffer: StoredOwnOffer? = null,
    val offerSentTo: Map<String, String> = emptyMap(),
    val bundleSentTo: Map<String, String> = emptyMap(),
    /** One-to-one group hex -> when an ask last reopened its sent records. */
    val reopenedAtMs: Map<String, Long> = emptyMap(),
)

/** What the run reads. */
internal class IntroPublicationInputs(
    /** `allowsContactsOfContacts(setting)`. */
    val contactsOfContacts: Boolean,
    val fofVersion: Int,
    /** The owner's `fof` grant (SECRET); `null` when none can be derived. */
    val fofGrant: FfiReachGrant?,
    val displayName: String?,
    val username: String?,
    /** The username certificate; carried only with the username. */
    val certificate: ByteArray?,
    val identityPk: ByteArray?,
    val accountId: ByteArray?,
    /** The record each contact offered, hex, in a stable order. */
    val offeredRecordsHex: List<String>,
    /** The one-to-one conversations with contacts that can still carry a
     *  message, as group hex. The bundle goes to all of them. */
    val oneToOneGroups: List<String>,
    /** Those the own record may go to ([offerGroups]). */
    val offerGroups: List<String>,
    val nowMs: Long,
)

/** The native and network steps, passed in. */
internal class IntroPublicationOps(
    val nameSearchTerms: (String) -> List<String>,
    val oprfNameTerm: suspend (String) -> OprfResult,
    val oprfUsername: suspend (String) -> OprfResult,
    val composeIntroduction: (FfiIntroductionPayload, List<ByteArray>, ULong) -> ByteArray,
    val composeOffer: (ByteArray) -> ByteArray,
    val composeBundle: (List<ByteArray>, ULong) -> ByteArray,
    val maxBundleRecords: Int,
    /** Send one envelope into a one-to-one conversation. Throws on failure;
     *  `false` when the send was held (ADR-0091 walk 3, W3-R2: the device
     *  may be behind there), so it is not recorded and goes at the next run. */
    val send: suspend (groupHex: String, envelope: ByteArray) -> Boolean,
    /** Persist the state after every step that changed it. */
    val save: suspend (IntroPublicationState) -> Unit,
    /** The own name's terms at once (`POST /discovery/oprf/batch`, one
     *  register token), in order; `null` evaluates them one by one. */
    val oprfOwnTerms: (suspend (List<me.lakko.app.discovery.OwnTerm>) -> List<OprfResult>)? = null,
)

/**
 * How long a term waits once after a rate limit: a long name has more terms
 * than the server's burst, and the bucket refills about one per second.
 * Chosen, not measured.
 */
internal const val TERM_RATE_LIMIT_WAIT_MS: Long = 8_000L

/** [round], asked once more after the wait when the server rate limited it. */
internal suspend fun <T> waitingOutRateLimit(round: suspend () -> T): T =
    try {
        round()
    } catch (e: CancellationException) {
        throw e
    } catch (e: Exception) {
        if (!isDiscoveryRateLimited(e)) throw e
        delay(TERM_RATE_LIMIT_WAIT_MS)
        round()
    }

private class Term(
    val key: String,
    val own: me.lakko.app.discovery.OwnTerm,
    val evaluate: suspend () -> OprfResult,
)

/**
 * Run the three steps. Throws what [IntroPublicationOps] throws (the caller
 * logs it); every step already done was saved, so a retry resumes.
 */
internal suspend fun publishIntroductions(
    initial: IntroPublicationState,
    inputs: IntroPublicationInputs,
    ops: IntroPublicationOps,
): IntroPublicationState {
    var state = initial
    suspend fun save(next: IntroPublicationState) {
        state = next
        ops.save(next)
    }

    val name = inputs.displayName?.takeIf { it.isNotBlank() }
    val username = inputs.username?.takeIf { it.isNotEmpty() }

    // 2. The introduction record, rebuilt when an input moved.
    val key = introductionOfferKey(
        inputs.fofVersion,
        name,
        username,
        inputs.fofGrant?.binding?.let(::reachHex).orEmpty(),
    )
    // A failure building the own record (an OPRF token refused, a network
    // error) must not cost the relay of the contacts' records (step 4), as on
    // iOS: it is kept and rethrown at the end.
    var buildFailure: Exception? = null
    if (offerNeedsRebuild(state.ownOffer?.key, inputs.contactsOfContacts, key)) try {
        val terms = mutableListOf<Term>()
        name?.let { n ->
            for (t in ops.nameSearchTerms(n)) {
                terms += Term("n:$t", me.lakko.app.discovery.OwnTerm.NameTerm(t)) { ops.oprfNameTerm(t) }
            }
        }
        username?.let { u -> terms += Term("u:$u", me.lakko.app.discovery.OwnTerm.Username(u)) { ops.oprfUsername(u) } }
        val grant = inputs.fofGrant
        val identityPk = inputs.identityPk
        val accountId = inputs.accountId
        if (terms.isNotEmpty() && grant != null && identityPk != null && accountId != null) {
            // Each evaluation costs an anonymous token, so an output is kept as
            // soon as it is known: a failed build resumes where it stopped.
            // Keeping them only at the end spent a token per term on every
            // retry and ran through the account's daily quota.
            val missing = termsToEvaluate(state.termOutputs.keys, terms.map { it.key }).toMutableSet()
            val batch = ops.oprfOwnTerms
            if (batch != null) {
                // The own name's terms in one batch, one token; kept together.
                val todo = terms.filter { it.key in missing }.distinctBy { it.key }
                if (todo.isNotEmpty()) {
                    val results = waitingOutRateLimit { batch(todo.map { it.own }) }
                    val kept = todo.indices.associate { i -> todo[i].key to reachHex(results[i].output) }
                    save(state.copy(termOutputs = state.termOutputs + kept))
                }
            } else {
                for (t in terms) {
                    if (!missing.remove(t.key)) continue
                    val output = waitingOutRateLimit { t.evaluate() }.output
                    save(state.copy(termOutputs = state.termOutputs + (t.key to reachHex(output))))
                }
            }
            val outputs = terms.map { reachUnhex(state.termOutputs[it.key] ?: "") ?: ByteArray(0) }
            val payload = FfiIntroductionPayload(
                identityPk = identityPk,
                accountId = accountId,
                displayName = name,
                username = username,
                usernameCertificate = if (username == null) null else inputs.certificate,
                fof = grant,
                // ADR-0078: the owner's mailbox binding, the same one the
                // `fof` grant carries.
                mailboxBinding = grant.binding,
            )
            val record = ops.composeIntroduction(payload, outputs, inputs.nowMs.coerceAtLeast(0L).toULong())
            save(state.copy(ownOffer = StoredOwnOffer(key, reachHex(record)), offerSentTo = emptyMap()))
        }
    } catch (e: CancellationException) {
        throw e
    } catch (e: Exception) {
        buildFailure = e
    }

    // 3. Offer the record to every allowed contact not sent this version yet.
    //    An ask reopens a conversation, and goes through this same list. A
    //    record that failed to rebuild is not offered stale.
    val offer = state.ownOffer.takeIf { buildFailure == null }
    if (offer != null) {
        val targets = offerTargets(offer.key, state.offerSentTo, inputs.contactsOfContacts, inputs.offerGroups)
        if (targets.isNotEmpty()) {
            val record = reachUnhex(offer.recordHex)
            if (record != null) {
                val envelope = ops.composeOffer(record)
                for (g in targets) {
                    if (!ops.send(g, envelope)) continue
                    save(state.copy(offerSentTo = state.offerSentTo + (g to offer.key)))
                }
            }
        }
    }

    // 4. Relay the records our contacts offered, as one padded bundle, to
    //    every contact (a contact may receive its own record back; it is
    //    harmless and hides nothing the bundle size does not).
    // An empty set still replaces a bundle sent before ([bundleTargets]): a
    // removed contact's record stops being relayed.
    val records = inputs.offeredRecordsHex
    run {
        val digest = recordsDigest(records)
        val targets = bundleTargets(state.bundleSentTo, digest, inputs.oneToOneGroups)
        if (targets.isNotEmpty()) {
            val wires = records.mapNotNull(::reachUnhex).take(ops.maxBundleRecords.coerceAtLeast(0))
            val bundle = ops.composeBundle(wires, inputs.nowMs.coerceAtLeast(0L).toULong())
            for (g in targets) {
                if (!ops.send(g, bundle)) continue
                save(state.copy(bundleSentTo = state.bundleSentTo + (g to digest)))
            }
        }
    }
    buildFailure?.let { throw it }
    return state
}

// ---------------------------------------------------------------------
// A device that holds nothing asks (ADR-0060 section 4)
// ---------------------------------------------------------------------

/** An ask reopens a conversation's sent records at most this often. Chosen,
 *  not measured. */
internal const val INTRODUCTIONS_RESEND_MIN_MS: Long = 60L * 60 * 1000

/** With a contact grant held, a device asks this rarely. Chosen, not measured. */
internal const val INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS: Long = 7L * 24 * 60 * 60 * 1000

/** What this device holds from the other person of a one-to-one. */
internal data class IntroductionsHeld(val offer: Boolean, val bundle: Boolean, val contactGrant: Boolean)

/**
 * Whether a conversation lacks introductions: no offer from the other person.
 * A bundle from them does not stand in for it (it usually carries this
 * device's own record back, so a device that missed the offer but held a
 * bundle never asked again). A contact grant does not count, or a device that once got a
 * profile would never ask; it only makes the ask rarer
 * ([introductionsAskStamp]). [held] is `null` for a conversation that is not a
 * one-to-one, or whose other person cannot be named: a torn roster read decides
 * nothing.
 */
internal fun lacksIntroductions(held: IntroductionsHeld?): Boolean =
    held != null && !held.offer

/**
 * The stamp to record for an ask now, or `null` not to ask. [lastMs] is when
 * this device last asked the conversation, by either route. With no contact
 * grant held the profile request's own rule decides ([noGrantRule], at most
 * daily); with one, the first ask goes out at once and later ones after
 * [INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS] (a backwards clock reads as recently
 * asked).
 */
internal fun introductionsAskStamp(
    held: IntroductionsHeld,
    lastMs: ULong?,
    nowMs: ULong,
    noGrantRule: (lastMs: ULong?, nowMs: ULong) -> Boolean,
): ULong? {
    val ask = if (held.contactGrant) {
        lastMs == null || (nowMs >= lastMs && nowMs - lastMs >= INTRODUCTIONS_ASK_WITH_GRANT_MIN_MS.toULong())
    } else {
        noGrantRule(lastMs, nowMs)
    }
    return if (ask) nowMs else null
}

/**
 * Ask in every conversation of [heldByGroup] that lacks introductions and
 * that [decide] lets ask now: the field-less profile request is the ask.
 * [decide] answers with the stamp to [record] once the request went out; a
 * send that throws or reports `false` records nothing, so the next run asks
 * again, and does not stop the others.
 */
internal suspend fun askForIntroductions(
    heldByGroup: Map<String, IntroductionsHeld?>,
    decide: (groupHex: String, held: IntroductionsHeld) -> ULong?,
    send: suspend (groupHex: String) -> Boolean,
    record: suspend (groupHex: String, stamp: ULong) -> Unit,
    noteFailure: (Throwable) -> Unit = {},
) {
    for ((groupHex, viewed) in heldByGroup) {
        val held = viewed?.takeIf(::lacksIntroductions) ?: continue
        val stamp = decide(groupHex, held) ?: continue
        val sent = try {
            send(groupHex)
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (cause: Throwable) {
            noteFailure(cause)
            false
        }
        if (sent) record(groupHex, stamp)
    }
}

/**
 * The publication state after a profile request was received in [groupHex], or
 * `null` to leave it alone. Only the OTHER person of a ONE-TO-ONE reopens it,
 * at most once per [INTRODUCTIONS_RESEND_MIN_MS] per conversation (a backwards
 * clock reads as recently done). Reopening forgets that one conversation in
 * the sent records, so the next run sends the offer and the bundle there again.
 */
internal fun reopenedByAsk(
    state: IntroPublicationState,
    groupHex: String,
    oneToOne: Boolean,
    fromOtherPerson: Boolean,
    nowMs: Long,
): IntroPublicationState? {
    if (!oneToOne || !fromOtherPerson) return null
    val last = state.reopenedAtMs[groupHex]
    if (last != null && nowMs - last < INTRODUCTIONS_RESEND_MIN_MS) return null
    return state.copy(
        offerSentTo = state.offerSentTo - groupHex,
        bundleSentTo = state.bundleSentTo - groupHex,
        reopenedAtMs = state.reopenedAtMs + (groupHex to nowMs),
    )
}
