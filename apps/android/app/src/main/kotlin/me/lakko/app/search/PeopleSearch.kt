// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import java.util.Collections
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import uniffi.lakko_ffi_uniffi.OprfResult

/*
 * **Search people** - the network half (`docs/ux/who-can-find-me.md` section
 * 6.1, ADR-0060 "Client contract (S4 / S5)", Search). The Android twin of
 * `apps/web/src/search/people-search.ts`, rule for rule.
 *
 * One search asks, in parallel:
 * - the **channel** namespace for the typed handle (the person does not know
 *   which namespace a name they were given lives in);
 * - the **username** (`/discovery/lookup`, which answers for people in
 *   Everyone only);
 * - **one name term** (`nameQueryTerm` -> `/discovery/name/lookup`), people in
 *   Everyone whose published name carries it;
 * then, with the OPRF outputs of the username and the term, the kept
 * **introductions** (contacts of contacts).
 *
 * Every OPRF round yields two values: the `lookupKey` is what discovery is
 * asked with and what a username certificate binds; the `output` opens
 * records and never leaves the device.
 *
 * Everything is filtered by the WHOLE query (the network matched one term), a
 * username is shown only when verified, an introduction counts only when the
 * identity bundle for its account id serves its key, and a result found
 * through a grant is shown only after the DS reach check says the grant still
 * opens the person's mailbox. A check that says no drops the result silently:
 * the searcher must not be able to tell "hidden" from "does not exist".
 *
 * **What a person in Everyone is called.** Both lookups return, beside each
 * account id, the person's PUBLIC RECORD (ADR-0060 section 5). It is opened
 * with the OUTPUT of the round that found the hit and counts only when it
 * names both the hit's account id and the key the identity bundle serves for
 * that account ([acceptPublicRecord]). A name this device already holds wins
 * over the record's. A name hit nothing names is dropped; a username match
 * without a usable record is shown under its username.
 *
 * **A contact with no held name.** A contact this device knows only by key
 * (no rename, no profile name) has a [KnownPerson.fallbackName] — its
 * fingerprint, or its username — and nothing more. A name the person
 * published (the public record, an introduction) is used INSTEAD of that
 * fallback, so a name search finds the contact under that name; with neither,
 * the contact is still shown by the fallback. The row never says why it was
 * found.
 */

/** What this device knows about a person, by account key (base64). */
public data class KnownPerson(
    /** A name this device holds (rename or profile); `null` for none. */
    val name: String?,
    val verifiedUsername: String?,
    val isContact: Boolean,
    /** What a CONTACT is shown as when nothing better is known (the
     *  fingerprint, or a verified username); `null` for anyone else. Loses
     *  to a held [name] AND to a name found by the search. */
    val fallbackName: String? = null,
)

/** The label a result is shown under: a held name, else the name the search
 *  found (the person's own published one), else a contact's fallback; `null`
 *  (dropped) when none exists. */
public fun resultName(known: KnownPerson, foundName: String?): String? =
    known.name ?: foundName ?: known.fallbackName

/** One lookup hit: an account in Everyone and its public record (`null`
 *  when discovery sent none). */
public class LookupHit(public val accountId: ByteArray, public val publicRecord: ByteArray?)

/** The fields of an opened public record the search reads. */
public class OpenedPublicRecord(
    public val identityPk: ByteArray,
    public val accountId: ByteArray,
    public val displayName: String?,
    /** ADR-0078 - the 97-byte mailbox binding the record carries. */
    public val mailboxBinding: ByteArray? = null,
)

/** A channel the typed handle names. */
public data class FoundChannel(val idHex: String, val handle: String)

/** Everything a search asks the outside world. */
public interface PeopleSearchDeps {
    /** The canonical username the text could be, or `null` (`looksLikeAName`
     *  + `bareName` + `validateUsername`). */
    public fun canonicalUsername(typed: String): String?

    /** `nameQueryTerm`: the one term looked up, `null` when not searchable. */
    public fun queryTerm(query: String): String?

    /** `nameMatchesQuery`: the post-filter over the whole query. */
    public fun nameMatches(displayName: String, query: String): Boolean

    /** The channel namespace; `null` when not a channel. */
    public suspend fun lookupChannel(handle: String): String?

    /** One OPRF round over a username (one anonymous token). */
    public suspend fun usernameOprf(username: String): OprfResult

    /** `/discovery/lookup` by the username's LOOKUP KEY. */
    public suspend fun accountOfUsernameLookupKey(lookupKey: ByteArray): LookupHit?

    /** One OPRF round over a name term (one anonymous token). */
    public suspend fun nameTermOprf(term: String): OprfResult

    /** `/discovery/name/lookup` by the term's LOOKUP KEY. */
    public suspend fun accountsOfNameTermLookupKey(lookupKey: ByteArray): List<LookupHit>

    /** `openPublicRecord(record, oprfOutput)`, or `null`. Never throws. */
    public fun openPublicRecord(record: ByteArray, oprfOutput: ByteArray): OpenedPublicRecord?

    /** The account key the identity bundle publishes, `null` for none. */
    public suspend fun identityKeyOf(accountId: ByteArray): ByteArray?

    /**
     * ADR-0074 section 3.5a - whether [foundKey] is a PROVEN predecessor of
     * [publishedKey] (a recorded proven step, or a supersession proof by
     * [foundKey] over [publishedKey] that verifies): the same person after a
     * rotation of their account keys. An unproven change is never one.
     */
    public fun isProvenPredecessor(foundKey: ByteArray, publishedKey: ByteArray): Boolean = false

    /** What this device knows about a person. */
    public fun known(keyB64: String): KnownPerson

    /** Open the kept introduction records with these probes. */
    public suspend fun openIntroductions(probes: List<IntroductionProbe>): List<OpenedIntroduction>

    /** `verifyUsernameBinding` against the pinned / fetched key: `true` only
     *  when the certificate binds the username's `lookupKey` (never its OPRF
     *  output) to [accountId]. Never throws. */
    public suspend fun verifyUsername(certificate: ByteArray, usernameLookupKey: ByteArray, accountId: ByteArray): Boolean

    /** The DS reach check of the person's account mailbox with a grant wire. */
    public suspend fun reachCheck(identityPk: ByteArray, grantWireB64: String): Boolean

    /** How a failed question ended (the used-up allowance is its own state). */
    public fun legOutcomeOf(error: Throwable): LegOutcome

    public val ownKeyHex: String?

    /** One network search finished. Counts only: this is the one line that
     *  tells a dropped result from no result at all. */
    public fun noteSearch(counts: SearchCounts) {}
}

/** What one network search did, as numbers. Never a name, key, query or grant. */
public data class SearchCounts(
    /** Introduction records opened with this search's probes. */
    val recordsOpened: Int,
    /** Of those, the ones the text matches. */
    val keptAfterMatch: Int,
    /** Results the reach check refused. */
    val refusedByReachCheck: Int,
    /** People in the final result. */
    val people: Int,
)

/** What one search came back with. */
public data class PeopleSearchOutcome(
    val people: List<PersonResult>,
    val channel: FoundChannel?,
    val state: PeopleSearchState,
)

/** At most this many usernames of introduction results are verified per
 *  search beyond the typed one: each costs an OPRF evaluation, i.e. one
 *  anonymous token. The rest show their name without a username. */
public const val MAX_EXTRA_USERNAME_CHECKS: Int = 3

/**
 * The display name a public record gives a lookup hit, or `null`. The record
 * is opened with the OPRF OUTPUT of the round that found the hit, and counts
 * only when it names the hit's account id AND the key the identity bundle
 * serves for that account: any person in Everyone can seal a record naming
 * anyone. Never throws.
 */
public fun acceptPublicRecord(
    open: (ByteArray, ByteArray) -> OpenedPublicRecord?,
    hit: LookupHit,
    oprfOutput: ByteArray,
    bundleKey: ByteArray,
    /** ADR-0074 3.5a - a record sealed under a proven predecessor of the
     *  bundle's key is the same person's. */
    provenPredecessor: (foundKey: ByteArray, publishedKey: ByteArray) -> Boolean = { _, _ -> false },
): String? =
    acceptedPublicRecord(open, hit, oprfOutput, bundleKey, provenPredecessor)
        ?.displayName?.trim()?.takeIf { it.isNotEmpty() }

/**
 * The opened public record of a lookup hit when it counts (the same checks
 * as [acceptPublicRecord]), or `null`. Its [OpenedPublicRecord.mailboxBinding]
 * is what a request to the person is sealed to (ADR-0078), after the binding
 * verifies against their account key at send time. Never throws.
 */
public fun acceptedPublicRecord(
    open: (ByteArray, ByteArray) -> OpenedPublicRecord?,
    hit: LookupHit,
    oprfOutput: ByteArray,
    bundleKey: ByteArray,
    provenPredecessor: (foundKey: ByteArray, publishedKey: ByteArray) -> Boolean = { _, _ -> false },
): OpenedPublicRecord? {
    val record = hit.publicRecord ?: return null
    val opened = try {
        open(record, oprfOutput)
    } catch (_: Throwable) {
        null
    } ?: return null
    if (!opened.accountId.contentEquals(hit.accountId)) return null
    if (!foundKeyIsPublished(opened.identityPk, bundleKey, provenPredecessor)) return null
    return opened
}

private fun recordName(record: OpenedPublicRecord?): String? = record?.displayName?.trim()?.takeIf { it.isNotEmpty() }

private fun recordBindingB64(record: OpenedPublicRecord?): String? =
    record?.mailboxBinding?.let { java.util.Base64.getEncoder().encodeToString(it) }

/** A result built from what is known locally, over a network finding. */
private fun resultFor(
    deps: PeopleSearchDeps,
    keyB64: String,
    keyHex: String,
    accountIdHex: String?,
    foundReach: PersonReach,
    foundName: String?,
    foundUsername: String?,
    grantWireB64: String?,
    bindingB64: String? = null,
): PersonResult? {
    val k = deps.known(keyB64)
    val reach = if (k.isContact) PersonReach.CONTACT else foundReach
    val name = resultName(k, foundName) ?: return null
    return PersonResult(
        keyB64 = keyB64,
        keyHex = keyHex,
        accountIdHex = accountIdHex,
        name = name,
        username = shownUsername(foundUsername ?: k.verifiedUsername, reach),
        reach = reach,
        grantWireB64 = if (reach == PersonReach.GRANT) grantWireB64 else null,
        mailboxBindingB64 = bindingB64,
    )
}

/**
 * ADR-0074 section 3.5a - whether the key a person was FOUND as is the
 * person the identity bundle publishes: the published key itself, or a
 * proven predecessor of it ([provenPredecessor]; a throw is "no"). Two keys
 * nothing proved related are two people, whatever account id they claim.
 */
public fun foundKeyIsPublished(
    foundKey: ByteArray,
    publishedKey: ByteArray,
    provenPredecessor: (foundKey: ByteArray, publishedKey: ByteArray) -> Boolean,
): Boolean {
    if (foundKey.contentEquals(publishedKey)) return true
    return try {
        provenPredecessor(foundKey, publishedKey)
    } catch (_: Throwable) {
        false
    }
}

private class BundleKey(val b64: String, val hex: String, val bytes: ByteArray)

private suspend fun identityKey(deps: PeopleSearchDeps, accountId: ByteArray): BundleKey? {
    val key = deps.identityKeyOf(accountId) ?: return null
    if (key.size != IDENTITY_KEY_LEN) return null
    return BundleKey(searchKeyB64(key), searchHex(key), key)
}

private const val IDENTITY_KEY_LEN = 32

/** Run one search. Never throws (a cancellation is let through). */
public suspend fun searchPeople(query: String, deps: PeopleSearchDeps): PeopleSearchOutcome = coroutineScope {
    val legs: MutableList<LegOutcome> = Collections.synchronizedList(mutableListOf())
    val probes: MutableList<IntroductionProbe> = Collections.synchronizedList(mutableListOf())
    val username = deps.canonicalUsername(query)
    val term = deps.queryTerm(query)
    var usernameRound: OprfResult? = null

    // ---- the channel namespace ----------------------------------------
    val channelLeg = async {
        if (username == null) return@async null
        try {
            val idHex = deps.lookupChannel(username)
            legs += LegOutcome.ANSWERED
            idHex?.let { FoundChannel(it, username) }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (err: Throwable) {
            legs += deps.legOutcomeOf(err)
            null
        }
    }

    // ---- the username -------------------------------------------------
    val usernameLeg = async {
        if (username == null) return@async emptyList<PersonResult>()
        try {
            val round = deps.usernameOprf(username)
            usernameRound = round
            probes += IntroductionProbe(round.output, ProbeVia.USERNAME)
            val hit = deps.accountOfUsernameLookupKey(round.lookupKey)
            if (hit == null) {
                legs += LegOutcome.ANSWERED
                return@async emptyList()
            }
            // The key comes from the identity bundle discovery serves for the
            // account, never from a profile's CLAIMED account id.
            val key = identityKey(deps, hit.accountId)
            legs += LegOutcome.ANSWERED
            if (key == null) return@async emptyList()
            val record =
                acceptedPublicRecord(deps::openPublicRecord, hit, round.output, key.bytes, deps::isProvenPredecessor)
            val published = recordName(record)
            // Discovery itself resolved this username to this account: that
            // is the binding a certificate would prove, so the username shows.
            listOfNotNull(
                resultFor(
                    deps, key.b64, key.hex, searchHex(hit.accountId), PersonReach.OPEN,
                    published ?: "@$username", username, null, recordBindingB64(record),
                ),
            )
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (err: Throwable) {
            legs += deps.legOutcomeOf(err)
            emptyList()
        }
    }

    // ---- one name term ------------------------------------------------
    val nameLeg = async {
        if (term == null) return@async emptyList<PersonResult>()
        try {
            val round = deps.nameTermOprf(term)
            probes += IntroductionProbe(round.output, ProbeVia.TERM)
            val out = mutableListOf<PersonResult>()
            for (hit in deps.accountsOfNameTermLookupKey(round.lookupKey)) {
                val key = identityKey(deps, hit.accountId) ?: continue
                // A name this device holds, else the one the person published
                // (also for a contact held only by fingerprint); an unnamed
                // hit is dropped.
                val record =
                    acceptedPublicRecord(deps::openPublicRecord, hit, round.output, key.bytes, deps::isProvenPredecessor)
                val published = recordName(record)
                val name = deps.known(key.b64).name ?: published ?: continue
                if (!deps.nameMatches(name, query)) continue
                resultFor(
                    deps, key.b64, key.hex, searchHex(hit.accountId), PersonReach.OPEN,
                    published, null, null, recordBindingB64(record),
                )?.let { out += it }
            }
            legs += LegOutcome.ANSWERED
            out
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (err: Throwable) {
            legs += deps.legOutcomeOf(err)
            emptyList()
        }
    }

    val channel = channelLeg.await()
    val byUsername = usernameLeg.await()
    val byName = nameLeg.await()

    // ---- contacts of contacts ----------------------------------------
    val byIntroduction = mutableListOf<PersonResult>()
    var introFailed = false
    var recordsOpened = 0
    var keptAfterMatch = 0
    val probeList = synchronized(probes) { probes.toList() }
    if (probeList.isNotEmpty()) {
        val opened = try {
            deps.openIntroductions(probeList)
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            emptyList()
        }
        recordsOpened = opened.size
        var extraChecks = 0
        for (intro in opened) {
            val matches = when (intro.via) {
                ProbeVia.USERNAME -> username != null && intro.username == username
                ProbeVia.TERM -> intro.displayName != null && deps.nameMatches(intro.displayName, query)
            }
            if (!matches) continue
            keptAfterMatch += 1
            // The record is the introduced person's own claim: its account id
            // must be the account the identity bundle publishes this key for,
            // or a record could pair its author's key with someone else's
            // account id - and that account's username certificate.
            val bound = try {
                val published = deps.identityKeyOf(intro.accountId)
                published != null && foundKeyIsPublished(intro.identityPk, published, deps::isProvenPredecessor)
            } catch (cancel: CancellationException) {
                throw cancel
            } catch (_: Throwable) {
                introFailed = true
                continue
            }
            if (!bound) continue
            var verified: String? = null
            val claimed = intro.username
            val certificate = intro.usernameCertificate
            if (claimed != null && certificate != null) {
                val round = usernameRound
                val lookupKey: ByteArray? = if (claimed == username && round != null) {
                    round.lookupKey
                } else if (extraChecks < MAX_EXTRA_USERNAME_CHECKS) {
                    extraChecks += 1
                    try {
                        deps.usernameOprf(claimed).lookupKey
                    } catch (cancel: CancellationException) {
                        throw cancel
                    } catch (_: Throwable) {
                        null
                    }
                } else {
                    null
                }
                if (lookupKey != null && deps.verifyUsername(certificate, lookupKey, intro.accountId)) {
                    verified = claimed
                }
            }
            val keyB64 = searchKeyB64(intro.identityPk)
            val name = intro.displayName
                ?: verified?.let { "@$it" }
                ?: me.lakko.app.profile.fingerprintLabel(intro.identityPk)
            resultFor(
                deps, keyB64, searchHex(intro.identityPk), searchHex(intro.accountId), PersonReach.GRANT,
                name, verified, java.util.Base64.getEncoder().encodeToString(intro.fofWire),
            )?.let { byIntroduction += it }
        }
    }

    // ---- one list, then the reach check -------------------------------
    val merged = mergePeople(listOf(byUsername, byName, byIntroduction), deps.ownKeyHex)
    val people = mutableListOf<PersonResult>()
    var checkFailed = false
    var refusedByReachCheck = 0
    for (p in merged) {
        if (p.reach != PersonReach.GRANT) {
            people += p
            continue
        }
        val grant = p.grantWireB64 ?: continue
        try {
            val pk = java.util.Base64.getDecoder().decode(p.keyB64)
            if (deps.reachCheck(pk, grant)) {
                people += p.copy(grantChecked = true)
            } else {
                refusedByReachCheck += 1
            }
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            checkFailed = true
        }
    }
    val allLegs = synchronized(legs) { legs.toMutableList() }
    if (checkFailed || introFailed) allLegs += LegOutcome.FAILED
    val found = people.size + (if (channel == null) 0 else 1)
    deps.noteSearch(SearchCounts(recordsOpened, keptAfterMatch, refusedByReachCheck, people.size))
    PeopleSearchOutcome(people, channel, foldSearch(found, allLegs))
}
