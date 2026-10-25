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
import me.lakko.app.l10n.UiText

/*
 * One search field (`docs/ux/who-can-find-me.md` section 6.1): when the field
 * asks the network, what it remembers, and what a result row offers. Nothing
 * to press: from a text the network can be asked with (3 characters, the core's
 * minimum), after 500 ms without typing, the field asks once; a text already
 * answered in this session is not asked again; what was shown stays until the
 * new answer arrives; a leading `@` is ignored.
 *
 * Pure Kotlin over coroutines, so the host JVM tests drive it on a test
 * dispatcher with a virtual clock.
 */

/** How long the field waits after the last keystroke before it asks. Chosen,
 *  not measured (UX section 6.1). */
public const val NETWORK_SEARCH_DEBOUNCE_MS: Long = 500

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
 * What the field remembers for the whole session, shared by the Chats and the
 * Contacts field: the texts already answered and how each request sent from a
 * row stands (by account key hex). A failed search and a used-up allowance are
 * not remembered: typing the text again asks again.
 */
public class PeopleSearchMemory {
    private val answered = ConcurrentHashMap<String, PeopleSearchOutcome>()
    private val sends = MutableStateFlow<Map<String, RowSendState>>(emptyMap())

    /** How each request sent from a row stands, by account key hex. */
    public val sendStates: StateFlow<Map<String, RowSendState>> = sends.asStateFlow()

    internal fun answered(key: String): PeopleSearchOutcome? = answered[key]

    internal fun remember(key: String, outcome: PeopleSearchOutcome) {
        if (outcome.state == PeopleSearchState.FOUND || outcome.state == PeopleSearchState.NOT_FOUND) {
            answered[key] = outcome
        }
    }

    /** Claim a row for sending; `false` when it is already sending or sent. */
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
    /** Whether a bare text may be asked at all (the core's minimum and the
     *  name-term / username rules). */
    private val asks: (bare: String) -> Boolean,
    /** The network search over a bare text. Never throws but for cancellation. */
    private val search: suspend (bare: String) -> PeopleSearchOutcome,
    /** Send the contact request to a found person, with no introduction. */
    private val send: suspend (PersonResult) -> SendOutcome,
    private val debounceMs: Long = NETWORK_SEARCH_DEBOUNCE_MS,
) {
    private val mutable = MutableStateFlow(PeopleNetworkView.HIDDEN)

    /** What the People part shows. */
    public val view: StateFlow<PeopleNetworkView> = mutable.asStateFlow()

    /** How each request sent from a row stands. */
    public val sendStates: StateFlow<Map<String, RowSendState>> get() = memory.sendStates

    private var currentKey: String? = null
    private var waiting: Job? = null
    private val inFlight = HashSet<String>()

    /** The text in the field changed. */
    public fun onQuery(query: String) {
        waiting?.cancel()
        waiting = null
        val bare = bareSearchText(query)
        val may = try {
            asks(bare)
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
                    is SendOutcome.Failed -> RowSendState.FAILED
                },
            )
        }
    }
}
