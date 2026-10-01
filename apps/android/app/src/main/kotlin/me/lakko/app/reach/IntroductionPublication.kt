// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import uniffi.lakko_ffi_uniffi.FfiIntroductionPayload
import uniffi.lakko_ffi_uniffi.FfiReachGrant
import uniffi.lakko_ffi_uniffi.OprfResult

/*
 * ADR-0060 section 4 — contacts of contacts, the sending half. The Android
 * twin of `apps/web/src/reach/publication-model.ts` and steps 2-4 of
 * `ScopedDiscoveryRuntime.tsx`:
 *
 * - the **introduction record**, rebuilt when 2.1 is on and the `fof` key
 *   version, the name or the username changed, sealed under the OPRF OUTPUTS
 *   of the name search terms and the username;
 * - offered to every one-to-one contact not sent this version yet (only
 *   while 2.1 is on);
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
internal fun introductionOfferKey(fofVersion: Int, name: String?, username: String?): String =
    listOf(fofVersion.toString(), name ?: "", username ?: "").joinToString("\u0001")

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

/** A stable digest of a set of record wires (order-independent). */
internal fun recordsDigest(recordsHex: Collection<String>): String = recordsHex.sorted().joinToString(",")

/** The conversations the relayed bundle (of [digest]) must go to. */
internal fun bundleTargets(sentTo: Map<String, String>, digest: String, oneToOneGroups: List<String>): List<String> =
    oneToOneGroups.filter { sentTo[it] != digest }

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
     *  message, as group hex. */
    val oneToOneGroups: List<String>,
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
    /** Send one envelope into a one-to-one conversation. Throws on failure. */
    val send: suspend (groupHex: String, envelope: ByteArray) -> Unit,
    /** Persist the state after every step that changed it. */
    val save: suspend (IntroPublicationState) -> Unit,
)

private class Term(val key: String, val evaluate: suspend () -> OprfResult)

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
    val key = introductionOfferKey(inputs.fofVersion, name, username)
    if (offerNeedsRebuild(state.ownOffer?.key, inputs.contactsOfContacts, key)) {
        val terms = mutableListOf<Term>()
        name?.let { n ->
            for (t in ops.nameSearchTerms(n)) terms += Term("n:$t") { ops.oprfNameTerm(t) }
        }
        username?.let { u -> terms += Term("u:$u") { ops.oprfUsername(u) } }
        val grant = inputs.fofGrant
        val identityPk = inputs.identityPk
        val accountId = inputs.accountId
        if (terms.isNotEmpty() && grant != null && identityPk != null && accountId != null) {
            val missing = termsToEvaluate(state.termOutputs.keys, terms.map { it.key })
            if (missing.isNotEmpty()) {
                val added = LinkedHashMap<String, String>()
                for (t in terms) {
                    if (t.key !in missing || t.key in added) continue
                    added[t.key] = reachHex(t.evaluate().output)
                }
                save(state.copy(termOutputs = state.termOutputs + added))
            }
            val outputs = terms.map { reachUnhex(state.termOutputs[it.key] ?: "") ?: ByteArray(0) }
            val payload = FfiIntroductionPayload(
                identityPk = identityPk,
                accountId = accountId,
                displayName = name,
                username = username,
                usernameCertificate = if (username == null) null else inputs.certificate,
                fof = grant,
            )
            val record = ops.composeIntroduction(payload, outputs, inputs.nowMs.coerceAtLeast(0L).toULong())
            save(state.copy(ownOffer = StoredOwnOffer(key, reachHex(record)), offerSentTo = emptyMap()))
        }
    }

    // 3. Offer the record to every contact not sent this version yet.
    val offer = state.ownOffer
    if (offer != null) {
        val targets = offerTargets(offer.key, state.offerSentTo, inputs.contactsOfContacts, inputs.oneToOneGroups)
        if (targets.isNotEmpty()) {
            val record = reachUnhex(offer.recordHex)
            if (record != null) {
                val envelope = ops.composeOffer(record)
                for (g in targets) {
                    ops.send(g, envelope)
                    save(state.copy(offerSentTo = state.offerSentTo + (g to offer.key)))
                }
            }
        }
    }

    // 4. Relay the records our contacts offered, as one padded bundle, to
    //    every contact (a contact may receive its own record back; it is
    //    harmless and hides nothing the bundle size does not).
    val records = inputs.offeredRecordsHex
    if (records.isNotEmpty()) {
        val digest = recordsDigest(records)
        val targets = bundleTargets(state.bundleSentTo, digest, inputs.oneToOneGroups)
        if (targets.isNotEmpty()) {
            val wires = records.mapNotNull(::reachUnhex).take(ops.maxBundleRecords.coerceAtLeast(0))
            val bundle = ops.composeBundle(wires, inputs.nowMs.coerceAtLeast(0L).toULong())
            for (g in targets) {
                ops.send(g, bundle)
                save(state.copy(bundleSentTo = state.bundleSentTo + (g to digest)))
            }
        }
    }
    return state
}
