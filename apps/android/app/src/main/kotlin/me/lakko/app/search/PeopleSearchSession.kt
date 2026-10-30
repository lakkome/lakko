// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.search

import java.util.concurrent.ConcurrentHashMap
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit
import me.lakko.app.contacts.SentContactRequest
import me.lakko.app.l10n.UiText

/*
 * One search field (`docs/ux/who-can-find-me.md` section 6.1): when the field
 * asks the network, what it remembers, and what a result row offers. Nothing
 * to press: from a text the network can be asked with
 * ([NETWORK_SEARCH_MIN_CHARS]), after 500 ms without typing, the field asks
 * once; an answer is reused for [SEARCH_ANSWER_KEPT_MS]; what was shown stays
 * until the new answer arrives; a leading `@` is ignored.
 *
 * A row says what is true now. A kept-grant check holds for one text, a
 * remembered answer ages, a refused or failed line lasts until the text
 * changes, and **Request sent** comes from the outgoing list (Contacts > Sent).
 *
 * Pure Kotlin over coroutines, so the host JVM tests drive it on a test
 * dispatcher with a virtual clock.
 */

/** How long the field waits after the last keystroke before it asks. Chosen,
 *  not measured (UX section 6.1). */
public const val NETWORK_SEARCH_DEBOUNCE_MS: Long = 500

/** Fewest characters (after a leading `@`) the network is asked with. Chosen,
 *  not measured (UX section 6.1). Not the core's `nameSearchMinChars`, which is
 *  2: a name word of two letters is searchable, but below this the People part
 *  stays hidden and no token is spent. */
public const val NETWORK_SEARCH_MIN_CHARS: Int = 3

/** How long a network answer is reused for its text, a not-found included.
 *  Past it the text is asked again: a person may have widened who can find
 *  them. Chosen, not measured. */
public const val SEARCH_ANSWER_KEPT_MS: Long = 60_000

/** How many people's kept grants are checked at one time. The first letter
 *  typed can match every member of every group. Chosen, not measured. */
public const val KEPT_GRANT_CHECKS_AT_ONCE: Int = 4

/** The text the network is asked with: trimmed, a leading `@` dropped. */
public fun bareSearchText(query: String): String = query.trim().removePrefix("@").trim()

private val CACHE_SPACES = Regex("\\s+")

/** The key a text is remembered under: letter case and repeated spaces never
 *  make two searches of one text. */
internal fun searchCacheKey(bare: String): String = bare.lowercase().replace(CACHE_SPACES, " ")

/** How a contact request sent from a result row stands. */
public enum class RowSendState { SENDING, SENT, REFUSED, FAILED }

/** The one control a result row shows. */
public enum class RowAction {
    /** An existing contact: **Open** their conversation. */
    OPEN,

    /** **Add contact**. */
    ADD,

    /** **Add contact** while the request is going out (not pressable). */
    ADDING,

    /** Nothing to press. */
    NONE,
}

/** What a result row shows: its control and the line under it. */
public data class PersonRowView(val action: RowAction, val note: UiText?)

/** Who the outgoing list (Contacts > Sent) holds. */
public data class OutgoingRequests(val keysHex: Set<String>, val accountIdsHex: Set<String>) {
    /** Whether a request to this person is waiting. By account id too: their
     *  key may have rotated since the request went out. */
    public fun holds(person: PersonResult): Boolean =
        person.keyHex in keysHex || person.accountIdHex?.let { it in accountIdsHex } == true

    public companion object {
        /** Nobody. */
        public val NONE: OutgoingRequests = OutgoingRequests(emptySet(), emptySet())

        /** The people in this Sent list. A value that is not base64 names no one. */
        public fun of(sent: List<SentContactRequest>): OutgoingRequests = OutgoingRequests(
            keysHex = sent.mapNotNullTo(HashSet()) { hexOfBase64(it.recipientKeyB64) },
            accountIdsHex = sent.mapNotNullTo(HashSet()) { hexOfBase64(it.recipientAccountIdB64) },
        )

        private fun hexOfBase64(b64: String?): String? = try {
            b64?.let { searchHex(java.util.Base64.getDecoder().decode(it)) }
        } catch (_: IllegalArgumentException) {
            null
        }
    }
}

/**
 * How a row's request stands: [RowSendState.SENT] exactly while the person is
 * in the outgoing list. Declined and dismissed, or accepted, the row goes back
 * to what the check says.
 */
public fun rowSendState(
    person: PersonResult,
    held: RowSendState?,
    outgoing: OutgoingRequests,
): RowSendState? = when {
    outgoing.holds(person) -> RowSendState.SENT
    held == RowSendState.SENT -> null
    else -> held
}

/** What a finished check leaves. A check that could not be made learned
 *  nothing, so it does not replace a verdict. */
internal fun checkAfter(held: KeptGrantCheck?, answer: KeptGrantCheck): KeptGrantCheck = when {
    answer !is KeptGrantCheck.Unreached -> answer
    held == null || held is KeptGrantCheck.Unreached -> answer
    else -> held
}

/** The line after a request went out. */
public val REQUEST_SENT: UiText = UiText("search.people.request_sent")

/** The line after a request failed for any reason but a refusal. */
public val REQUEST_FAILED: UiText = UiText("search.people.request_failed")

/** What a result row shows, from the person and how their request stands. */
public fun personRowView(person: PersonResult, send: RowSendState?): PersonRowView = when {
    person.reach == PersonReach.CONTACT -> PersonRowView(RowAction.OPEN, null)
    send == RowSendState.SENT -> PersonRowView(RowAction.NONE, REQUEST_SENT)
    send == RowSendState.REFUSED -> PersonRowView(RowAction.NONE, REQUEST_REFUSED)
    !canAddContact(person) -> PersonRowView(RowAction.NONE, null)
    send == RowSendState.SENDING -> PersonRowView(RowAction.ADDING, null)
    send == RowSendState.FAILED -> PersonRowView(RowAction.ADD, REQUEST_FAILED)
    else -> PersonRowView(RowAction.ADD, null)
}

/**
 * Ask the Delivery Service about a person's kept grants, in order, stopping at
 * the first that passes. All refused is [KeptGrantCheck.Dead]; a check that
 * could not be made is [KeptGrantCheck.Unreached], which is no verdict. No
 * anonymous token is spent.
 */
public suspend fun checkGrantsInOrder(
    wiresB64: List<String>,
    check: suspend (wireB64: String) -> Boolean,
): KeptGrantCheck {
    for (wire in wiresB64) {
        val passed = try {
            check(wire)
        } catch (cancel: CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            return KeptGrantCheck.Unreached(wire)
        }
        if (passed) return KeptGrantCheck.Live(wire)
    }
    return KeptGrantCheck.Dead
}

/**
 * What the field remembers for the whole session, shared by the Chats and the
 * Contacts field: the answers of the last [SEARCH_ANSWER_KEPT_MS] and how each
 * request sent from a row stands (by account key hex). A failed search and a
 * used-up allowance are not remembered: typing the text again asks again.
 */
public class PeopleSearchMemory(
    /** A clock that never runs backwards, in milliseconds. */
    private val nowMs: () -> Long = { System.nanoTime() / NANOS_PER_MS },
) {
    private class Kept(val outcome: PeopleSearchOutcome, val atMs: Long)

    private val answered = ConcurrentHashMap<String, Kept>()
    private val sends = MutableStateFlow<Map<String, RowSendState>>(emptyMap())

    /** How each request sent from a row stands, by account key hex. */
    public val sendStates: StateFlow<Map<String, RowSendState>> = sends.asStateFlow()

    /** The answer for [key], while it is young enough to reuse. */
    internal fun answered(key: String): PeopleSearchOutcome? {
        val kept = answered[key] ?: return null
        if (nowMs() - kept.atMs < SEARCH_ANSWER_KEPT_MS) return kept.outcome
        answered.remove(key, kept)
        return null
    }

    internal fun remember(key: String, outcome: PeopleSearchOutcome) {
        if (outcome.state == PeopleSearchState.FOUND || outcome.state == PeopleSearchState.NOT_FOUND) {
            answered[key] = Kept(outcome, nowMs())
        }
    }

    /** Claim a row for sending; `false` while its state says not to. */
    internal fun claimSend(keyHex: String): Boolean {
        var claimed = false
        sends.update { held ->
            val now = held[keyHex]
            if (now == RowSendState.SENDING || now == RowSendState.SENT || now == RowSendState.REFUSED) {
                held
            } else {
                claimed = true
                held + (keyHex to RowSendState.SENDING)
            }
        }
        return claimed
    }

    internal fun setSend(keyHex: String, state: RowSendState) {
        sends.update { it + (keyHex to state) }
    }

    /** The text changed: a refused or failed line goes, so the row reflects
     *  the new check. A request on its way stays claimed. */
    internal fun clearRowLines() {
        sends.update { held ->
            held.filterValues { it != RowSendState.REFUSED && it != RowSendState.FAILED }
        }
    }

    /** Drop [RowSendState.SENT] for whoever the outgoing list no longer holds.
     *  A row reads Request sent from the list itself ([rowSendState]); this
     *  lets the person be asked again. */
    internal fun dropSentExcept(outgoing: OutgoingRequests) {
        sends.update { held ->
            held.filterNot { (keyHex, state) -> state == RowSendState.SENT && keyHex !in outgoing.keysHex }
        }
    }
}

/** What the People part of the field shows. */
public data class PeopleNetworkView(
    /** The text can be asked with; `false` hides the heading (fewer than 3
     *  characters, or nothing the network could match). */
    val asks: Boolean,
    /** An answer for the current text is awaited (the spinner by the heading). */
    val searching: Boolean,
    /** The answer shown: the current text's, or the earlier one while the
     *  current text is being asked. */
    val shown: PeopleSearchOutcome?,
) {
    /** The state the line under the rows describes: the shown answer's once
     *  it belongs to the current text, else searching. */
    val state: PeopleSearchState
        get() = when {
            !asks -> PeopleSearchState.IDLE
            searching -> PeopleSearchState.SEARCHING
            else -> shown?.state ?: PeopleSearchState.IDLE
        }

    public companion object {
        /** Nothing under People. */
        public val HIDDEN: PeopleNetworkView = PeopleNetworkView(asks = false, searching = false, shown = null)
    }
}

/**
 * The network half of one search field. [onQuery] on every keystroke; [view]
 * is what the People part shows. [add] sends a contact request from a row at
 * once, with no introduction.
 *
 * Calls are expected on one thread (the UI thread), with [scope] confined to
 * it.
 */
public class PeopleSearchSession(
    private val scope: CoroutineScope,
    private val memory: PeopleSearchMemory,
    /** Whether a bare text of [NETWORK_SEARCH_MIN_CHARS] or more may be asked
     *  at all (the name-term and username rules). */
    private val asks: (bare: String) -> Boolean,
    /** The network search over a bare text. Never throws but for cancellation. */
    private val search: suspend (bare: String) -> PeopleSearchOutcome,
    /** Send the contact request to a found person, with no introduction. */
    private val send: suspend (PersonResult) -> SendOutcome,
    /** The DS reach check of a person (account key, base64) with one grant. */
    private val reachCheck: suspend (keyB64: String, grantWireB64: String) -> Boolean = { _, _ -> false },
    private val debounceMs: Long = NETWORK_SEARCH_DEBOUNCE_MS,
) {
    private val mutable = MutableStateFlow(PeopleNetworkView.HIDDEN)

    /** What the People part shows. */
    public val view: StateFlow<PeopleNetworkView> = mutable.asStateFlow()

    /** How each request sent from a row stands. */
    public val sendStates: StateFlow<Map<String, RowSendState>> get() = memory.sendStates

    private val grantChecks = MutableStateFlow<Map<String, KeptGrantCheck>>(emptyMap())

    /** What the Delivery Service last said about kept grants, by account key
     *  hex, for as long as the field holds text. It stands until the next
     *  answer arrives, so a row does not flicker on every keystroke. */
    public val keptGrantChecks: StateFlow<Map<String, KeptGrantCheck>> = grantChecks.asStateFlow()

    private val checking = HashSet<String>()
    private val checkSlots = Semaphore(KEPT_GRANT_CHECKS_AT_ONCE)

    private var lastText: String? = null
    private var currentKey: String? = null
    private var waiting: Job? = null
    private val inFlight = HashSet<String>()

    /** The text in the field changed. */
    public fun onQuery(query: String) {
        waiting?.cancel()
        waiting = null
        val before = lastText
        lastText = query
        if (before != null && before != query) memory.clearRowLines()
        val bare = bareSearchText(query)
        if (bare.isEmpty()) grantChecks.value = emptyMap()
        val may = try {
            bare.codePointCount(0, bare.length) >= NETWORK_SEARCH_MIN_CHARS && asks(bare)
        } catch (_: Throwable) {
            false
        }
        if (!may) {
            currentKey = null
            mutable.value = PeopleNetworkView.HIDDEN
            return
        }
        val key = searchCacheKey(bare)
        currentKey = key
        memory.answered(key)?.let { held ->
            mutable.value = PeopleNetworkView(asks = true, searching = false, shown = held)
            return
        }
        mutable.update { it.copy(asks = true, searching = true) }
        if (key in inFlight) return
        waiting = scope.launch {
            delay(debounceMs)
            ask(key, bare)
        }
    }

    // The question itself is its own job: typing on after it went out must not
    // cancel it (the tokens are spent; the answer is remembered).
    private fun ask(key: String, bare: String) {
        if (!inFlight.add(key)) return
        scope.launch {
            val outcome = try {
                search(bare)
            } finally {
                inFlight.remove(key)
            }
            memory.remember(key, outcome)
            if (currentKey == key) {
                mutable.value = PeopleNetworkView(asks = true, searching = false, shown = outcome)
            }
        }
    }

    /**
     * Check the kept grants of these people ([keptGrantsToCheck]) now: no
     * minimum length and no debounce, since this spends no token. One check
     * per person at a time; typing on does not cancel it. The last answer
     * stands until this one arrives.
     */
    public fun checkKeptGrants(candidates: List<LocalCandidate>) {
        for (c in candidates) {
            if (!checking.add(c.keyHex)) continue
            scope.launch {
                try {
                    val answer = checkSlots.withPermit {
                        checkGrantsInOrder(c.keptGrantWiresB64) { wire -> reachCheck(c.keyB64, wire) }
                    }
                    grantChecks.update { it + (c.keyHex to checkAfter(it[c.keyHex], answer)) }
                } finally {
                    checking.remove(c.keyHex)
                }
            }
        }
    }

    /** The outgoing list changed. */
    public fun onOutgoing(outgoing: OutgoingRequests) {
        memory.dropSentExcept(outgoing)
    }

    /** **Add contact** on a row: the request goes out at once. */
    public fun add(person: PersonResult) {
        if (!canAddContact(person)) return
        if (!memory.claimSend(person.keyHex)) return
        scope.launch {
            val outcome = try {
                send(person)
            } catch (cancel: CancellationException) {
                memory.setSend(person.keyHex, RowSendState.FAILED)
                throw cancel
            } catch (_: Throwable) {
                SendOutcome.Failed(REQUEST_FAILED)
            }
            memory.setSend(
                person.keyHex,
                when (outcome) {
                    SendOutcome.Sent -> RowSendState.SENT
                    SendOutcome.Refused -> RowSendState.REFUSED
                    SendOutcome.ReachRefused -> {
                        grantChecks.update { it + (person.keyHex to KeptGrantCheck.Dead) }
                        RowSendState.REFUSED
                    }
                    is SendOutcome.Failed -> RowSendState.FAILED
                },
            )
        }
    }
}

private const val NANOS_PER_MS = 1_000_000L
