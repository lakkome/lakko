// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import me.lakko.app.l10n.UiText

/*
 * People search, the pure half (`docs/ux/who-can-find-me.md` section 6.1,
 * ADR-0060 "Client contract (S4 / S5)", Search). The Android twin of
 * `apps/web/src/search/people.ts` and `search-state.ts`.
 *
 * What a result is, how the LOCAL half matches ("any part of any word",
 * letter case and accents never matter), how local and network results merge
 * into ONE list keyed on the person's account key, the one rule that decides
 * whether a `@username` may be shown beside a name, and the states of a
 * network search with their copy.
 *
 * No Android, no native calls: the core's normaliser arrives as an argument,
 * so the host JVM tests run it with a stand-in.
 */

/** Normalise a name or a query for search; `null` when it is not searchable. */
public typealias Normalize = (String) -> String?

/** Longest input the core normaliser accepts, in bytes. */
private const val MAX_NAME_INPUT_BYTES = 1024

private val MARKS = Regex("\\p{M}+")
private val SPACES = Regex("\\s+")

/**
 * The same normalisation the core applies (NFKD, marks dropped, lowercase,
 * whitespace collapsed, trimmed), for what the core refuses because it is
 * SHORTER than a network query may be: a local search runs from the first
 * letter, while the core's `normalizeNameForSearch` answers `null` under 2
 * characters.
 */
public fun fallbackNormalize(input: String): String? {
    if (input.toByteArray(Charsets.UTF_8).size > MAX_NAME_INPUT_BYTES) return null
    val out = java.text.Normalizer.normalize(input, java.text.Normalizer.Form.NFKD)
        .replace(MARKS, "")
        .lowercase()
        .replace(SPACES, " ")
        .trim()
    return out.ifEmpty { null }
}

/** The core normaliser, with the short-input fallback beside it. */
public fun searchNormalizer(core: (String) -> String?): Normalize = { input ->
    core(input) ?: fallbackNormalize(input)
}

/**
 * The local match: every word of the query is a part of some word of the
 * name. `ann`, `nna` and `weber an` all find `Anna Weber`.
 */
public fun matchesLoosely(name: String, query: String, normalize: Normalize): Boolean {
    val q = normalize(query) ?: return false
    val n = normalize(name) ?: return false
    val words = n.split(" ")
    return q.split(" ").all { part -> words.any { it.contains(part) } }
}

/**
 * How this account may reach a person, strongest first:
 * - [CONTACT]: already a contact, nothing to add;
 * - [OPEN]: the person is in Everyone, their mailbox is open;
 * - [GRANT]: they handed this account a reach key (a group or channel they
 *   let find them, or an introduction's contacts-of-contacts key);
 * - [NONE]: they do not let this account find them.
 */
public enum class PersonReach(internal val rank: Int) {
    CONTACT(3),
    OPEN(2),
    GRANT(1),
    NONE(0),
}

/** One person in the result list. */
public data class PersonResult(
    /** The account key (the person key, ADR-0050), standard base64. */
    val keyB64: String,
    /** The same key as lower-case hex. */
    val keyHex: String,
    /** The 16-byte auth account id as hex, when known. A request needs it. */
    val accountIdHex: String?,
    /** The name to show. */
    val name: String,
    /** A VERIFIED username, already passed through [shownUsername]. */
    val username: String?,
    val reach: PersonReach,
    /** For [PersonReach.GRANT]: standard base64 of the grant wire
     *  (`encodeReachGrant`) the reach check and the request are signed with.
     *  Secret. */
    val grantWireB64: String?,
    /** ADR-0078 - for a person found in Everyone: standard base64 of the
     *  97-byte mailbox binding their public record carried. The request is
     *  sealed to the key it names once it verifies against the person's
     *  account key; without it (and without a grant) nothing is written. */
    val mailboxBindingB64: String? = null,
    /** [grantWireB64] is a grant the DS reach check just passed, not one this
     *  device merely kept. */
    val grantChecked: Boolean = false,
)

/**
 * Whether a verified username may be shown (UX section 6.2): a username is a
 * handle for finding someone, so it goes with the permission to use it. A
 * contact's always shows; a hidden person's never does.
 */
public fun shownUsername(verified: String?, reach: PersonReach): String? =
    if (reach == PersonReach.NONE) null else verified

/** Whether the row offers **Add contact**. */
public fun canAddContact(person: PersonResult): Boolean =
    (person.reach == PersonReach.OPEN || person.reach == PersonReach.GRANT) && person.accountIdHex != null

/**
 * What the Delivery Service said about the grants kept for a person (UX
 * section 6.1, "A kept grant is checked before it is offered"). A person this
 * device knows from a group may have left it, and then the grant it kept no
 * longer reaches them. No answer yet: the row is the name alone.
 */
public sealed interface KeptGrantCheck {
    /** A verdict: this grant passed. It is the one the row offers; the send
     *  tries every other grant held for the person if it no longer does. */
    public data class Live(val wireB64: String) : KeptGrantCheck

    /** A verdict: every kept grant was refused. */
    public data object Dead : KeptGrantCheck

    /** Not a verdict: the check of this grant could not be made (a network
     *  failure). The row offers it as it would have without a check, the send
     *  checks again, and the next text in the field asks again. */
    public data class Unreached(val wireB64: String) : KeptGrantCheck
}

/** A person this device already knows: a contact, or a member of one of its
 *  groups or channels whose profile it holds. */
public data class LocalCandidate(
    val keyB64: String,
    val keyHex: String,
    /** The resolved name (a local rename, their profile name); `null` when
     *  this device holds no name for them. */
    val name: String?,
    /** Their username, when verified. */
    val verifiedUsername: String?,
    val isContact: Boolean,
    val accountIdHex: String?,
    /** Every grant kept for them, standard base64 of each wire, in the order
     *  they are tried: contact, each group, each channel, the found one.
     *  Never a bare `member` grant. Secret. */
    val keptGrantWiresB64: List<String>,
    /** What the Delivery Service said about those grants, if it has. */
    val keptGrantCheck: KeptGrantCheck? = null,
)

/** Each candidate with what the Delivery Service said about its grants, the
 *  step before the local match. */
public fun withKeptGrantChecks(
    candidates: List<LocalCandidate>,
    checks: Map<String, KeptGrantCheck>,
): List<LocalCandidate> = candidates.map { it.copy(keptGrantCheck = checks[it.keyHex]) }

/**
 * A local candidate as a result, with its reach worked out. A kept grant
 * counts only once the Delivery Service answered for it: with no answer yet,
 * or with every grant refused, the person is a name and nothing more.
 */
public fun localResult(c: LocalCandidate, name: String): PersonResult {
    val check = c.keptGrantCheck
    val wire = when (check) {
        is KeptGrantCheck.Live -> check.wireB64
        is KeptGrantCheck.Unreached -> check.wireB64
        KeptGrantCheck.Dead, null -> null
    }
    val reach = when {
        c.isContact -> PersonReach.CONTACT
        wire != null -> PersonReach.GRANT
        else -> PersonReach.NONE
    }
    return PersonResult(
        keyB64 = c.keyB64,
        keyHex = c.keyHex,
        accountIdHex = c.accountIdHex,
        name = name,
        username = shownUsername(c.verifiedUsername, reach),
        reach = reach,
        grantWireB64 = if (reach == PersonReach.GRANT) wire else null,
        grantChecked = reach == PersonReach.GRANT && check is KeptGrantCheck.Live,
    )
}

/**
 * The local half: the candidates whose name - or whose verified username,
 * for someone this account may find - matches the query loosely. A candidate
 * with no name is never listed (there is nothing to show).
 */
public fun localPeople(
    candidates: List<LocalCandidate>,
    query: String,
    normalize: Normalize,
    ownKeyHex: String?,
): List<PersonResult> {
    if (normalize(query) == null) return emptyList()
    val bare = query.trim().trimStart('@')
    val seen = HashSet<String>()
    val out = mutableListOf<PersonResult>()
    for (c in candidates) {
        if (c.keyHex == ownKeyHex || c.keyHex in seen || c.name == null) continue
        val result = localResult(c, c.name)
        val byName = matchesLoosely(c.name, query, normalize)
        val username = result.username
        val byUsername = username != null && bare.isNotEmpty() && matchesLoosely(username, bare, normalize)
        if (!byName && !byUsername) continue
        seen += c.keyHex
        out += result
    }
    return out
}

/**
 * Who the Delivery Service is asked about for this text: each candidate that
 * holds a grant and matches it, whatever was said before. An answer holds for
 * the text it was asked for: a person may have left a group, or come back,
 * since (`docs/ux/who-can-find-me.md` section 6.1). A contact is never asked
 * about. The match is [localPeople]'s, run as if the grant had passed, because
 * a person this account may find also matches by username.
 */
public fun keptGrantsToCheck(
    candidates: List<LocalCandidate>,
    query: String,
    normalize: Normalize,
    ownKeyHex: String?,
): List<LocalCandidate> {
    val holding = candidates.filter { c -> !c.isContact && c.keptGrantWiresB64.isNotEmpty() }
    val asIfLive = holding.map { it.copy(keptGrantCheck = KeptGrantCheck.Live(it.keptGrantWiresB64.first())) }
    val matching = localPeople(asIfLive, query, normalize, ownKeyHex).mapTo(HashSet()) { it.keyHex }
    return holding.filter { it.keyHex in matching }
}

/**
 * Merge result lists into one, keyed on the account key: the strongest reach
 * wins, the first list's name is kept (the local one, which respects a
 * rename), and a username is kept from whichever list verified one - then
 * gated again by the merged reach. Between two grants, the one that passed a
 * check wins. The own account never appears.
 */
public fun mergePeople(lists: List<List<PersonResult>>, ownKeyHex: String?): List<PersonResult> {
    val byKey = LinkedHashMap<String, PersonResult>()
    for (list in lists) {
        for (p in list) {
            if (p.keyHex == ownKeyHex) continue
            val held = byKey[p.keyHex]
            if (held == null) {
                byKey[p.keyHex] = p
                continue
            }
            val stronger = if (p.reach.rank > held.reach.rank) p else held
            val reach = stronger.reach
            val checked = listOf(held, p).firstOrNull { it.grantChecked && it.grantWireB64 != null }
            byKey[p.keyHex] = PersonResult(
                keyB64 = held.keyB64,
                keyHex = held.keyHex,
                accountIdHex = held.accountIdHex ?: p.accountIdHex,
                name = held.name,
                username = shownUsername(held.username ?: p.username, reach),
                reach = reach,
                grantWireB64 = if (reach == PersonReach.GRANT) {
                    checked?.grantWireB64 ?: stronger.grantWireB64 ?: held.grantWireB64 ?: p.grantWireB64
                } else {
                    null
                },
                mailboxBindingB64 = held.mailboxBindingB64 ?: p.mailboxBindingB64,
                grantChecked = reach == PersonReach.GRANT && checked != null,
            )
        }
    }
    return byKey.values.toList()
}

// -----------------------------------------------------------------------------
// The states of the network half of the search field (web `search-state.ts`)
// -----------------------------------------------------------------------------

/** Not found - identical whether the person does not exist or does not let
 *  the searcher find them. */
public val SEARCH_NOT_FOUND: UiText = UiText("search.people.not_found")

/** Network failure. Never "not found". */
public val SEARCH_FAILED: UiText = UiText("search.people.failed")

/** The anonymous lookup tokens ran out. Never "not found", never the network
 *  failure. */
public val SEARCH_OUT_OF_SEARCHES: UiText = UiText("search.people.out_of_searches")

/** The person changed their setting after they were found. */
public val REQUEST_REFUSED: UiText = UiText("search.people.request_refused")

/** How one question of a search ended. */
public enum class LegOutcome { ANSWERED, FAILED, ALLOWANCE_USED }

/** The states of the network search. */
public enum class PeopleSearchState { IDLE, SEARCHING, FOUND, NOT_FOUND, FAILED, ALLOWANCE_USED }

/**
 * Fold the questions a search asked into its state. Anything found is shown,
 * whatever else failed; nothing found is "not found" only when every
 * question was answered.
 */
public fun foldSearch(foundCount: Int, legs: Collection<LegOutcome>): PeopleSearchState = when {
    foundCount > 0 -> PeopleSearchState.FOUND
    LegOutcome.ALLOWANCE_USED in legs -> PeopleSearchState.ALLOWANCE_USED
    LegOutcome.FAILED in legs -> PeopleSearchState.FAILED
    else -> PeopleSearchState.NOT_FOUND
}

/** The line under the People part for a state; `null` when it says nothing in
 *  words (idle, searching - the spinner says it - and found). */
public fun searchStateMessage(state: PeopleSearchState): UiText? = when (state) {
    PeopleSearchState.NOT_FOUND -> SEARCH_NOT_FOUND
    PeopleSearchState.FAILED -> SEARCH_FAILED
    PeopleSearchState.ALLOWANCE_USED -> SEARCH_OUT_OF_SEARCHES
    else -> null
}

/**
 * The rows under People. [listedKeysB64] are shown above already (the Contacts
 * list). [chatPeersB64] are the people whose one-to-one is among the matching
 * conversations above: a contact among them stands there, so they are not
 * listed twice. A person who is no contact keeps their row, and with it the way
 * to add them.
 */
public fun peopleRows(
    merged: List<PersonResult>,
    listedKeysB64: Set<String>,
    chatPeersB64: Set<String>,
): List<PersonResult> = merged.filter { p ->
    p.keyB64 !in listedKeysB64 && !(p.reach == PersonReach.CONTACT && p.keyB64 in chatPeersB64)
}

/**
 * [merged] with whoever is a contact NOW shown as one. A remembered answer is up
 * to [SEARCH_ANSWER_KEPT_MS] old, and a person who just accepted may not match
 * the local half yet, because their name arrives after the Welcome: their row
 * must not offer a second request.
 */
public fun withCurrentContacts(merged: List<PersonResult>, contactKeysB64: Set<String>): List<PersonResult> =
    merged.map { p ->
        if (p.reach == PersonReach.CONTACT || p.keyB64 !in contactKeysB64) {
            p
        } else {
            p.copy(reach = PersonReach.CONTACT, grantWireB64 = null, grantChecked = false)
        }
    }

/**
 * Whether an answer names as a contact someone who is no longer one. Such a row
 * is wrong twice: the person has no way to be added from it, and a contact row
 * may be hidden as listed above, where they no longer stand.
 */
public fun namesFormerContact(people: List<PersonResult>, contactKeysB64: Set<String>): Boolean =
    people.any { it.reach == PersonReach.CONTACT && it.keyB64 !in contactKeysB64 }

/**
 * `docs/ux/who-can-find-me.md` section 6.1, "One list" — the line at the end
 * of the one result list: a network failure or an empty token pool. "Not
 * found" is never said here (the empty-list line says it), and nothing while
 * a question is out.
 */
public fun searchEndLine(state: PeopleSearchState): UiText? = when (state) {
    PeopleSearchState.FAILED -> SEARCH_FAILED
    PeopleSearchState.ALLOWANCE_USED -> SEARCH_OUT_OF_SEARCHES
    else -> null
}

/**
 * Section 6.1, "One list" — whether the field says "Nothing found": nothing
 * stands in the list (no conversation or contact row, no person, no group or
 * channel), nothing is still being asked, and no end line explains why.
 */
public fun searchShowsNothingFound(
    listRows: Int,
    people: Int,
    channelFound: Boolean,
    conversationsFound: Int,
    searching: Boolean,
    endLine: Boolean,
): Boolean = listRows == 0 && people == 0 && !channelFound && conversationsFound == 0 && !searching && !endLine

/** What a channel found by its handle reads as: its name when one is known,
 *  else the handle. */
public fun channelResultLabel(name: String?, handle: String): String =
    name?.trim()?.takeIf { it.isNotEmpty() } ?: "@$handle"

/** Lower-case hex of a byte string. */
public fun searchHex(bytes: ByteArray): String {
    val sb = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val v = b.toInt() and 0xff
        sb.append("0123456789abcdef"[v ushr 4]).append("0123456789abcdef"[v and 0x0f])
    }
    return sb.toString()
}

/** Standard base64 of a key, the form the profile store keys on. */
public fun searchKeyB64(key: ByteArray): String = java.util.Base64.getEncoder().encodeToString(key)
