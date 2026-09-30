// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.coroutines.CancellationException
import me.lakko.app.l10n.UiText

/**
 * Looking a name up on the network, from the conversation list.
 *
 * `docs/ux/navigation.md` section 4. A group and a channel are kinds of
 * conversation, not kinds of contact, so finding one belongs in Chats —
 * Contacts is about people you already know. And finding a person to talk to
 * starts a conversation too, so it belongs here as well.
 *
 * **There is one lookup, not one per kind.** `@username` and a channel's
 * `@handle` are the same mechanism over two namespaces: blind the name, ask,
 * get an exact answer or nothing. A group handle would be a third namespace and
 * the same mechanism again. The person typing does not know which namespace the
 * name they were given lives in, and should not have to — so this asks both and
 * reports what it found.
 *
 * **It cannot be fuzzy.** The server evaluates the blinded name without
 * learning it (security invariant #5), which answers "does this exact name
 * exist" and cannot answer "which names start with these letters", because a
 * prefix search is the server learning the prefix. The caller must offer this
 * as one row for the whole name, never as a list of partial matches.
 *
 * **The gate in front of it is not here.** Whether a typed string could be a
 * name at all is `looks_like_a_name` in `crates/lakko-ui-rules`, shared with
 * web and iOS; what this file owns is asking the two namespaces and telling the
 * difference between "no such name" and "no answer".
 *
 * Pure of the UI: the two lookups and the two shared rules arrive as
 * parameters, so the whole decision runs in a host JVM unit test without the
 * network and without the native library.
 */

/** What a lookup turned up. */
public sealed interface FoundByName {
    /** The name is a channel handle; [channelIdHex] opens its subscribe page. */
    public data class Channel(val channelIdHex: String, val handle: String) : FoundByName

    /** The name is a person's username. Contacts owns the request flow. */
    public data class Person(val username: String) : FoundByName

    /** Both namespaces answered, and neither holds the name. */
    public data object NothingFound : FoundByName

    /** Neither namespace answered. NOT the same fact as [NothingFound]. */
    public data object Unreachable : FoundByName

    /** The day's allowance of lookups (ADR-0054 anonymous tokens) is used
     *  up; not an answer about the name. */
    public data object AllowanceUsed : FoundByName
}

/**
 * The two namespaces, as the caller can ask them.
 *
 * Each returns `null` when the name is not in that namespace and THROWS when
 * the network is the problem. The difference is the whole point: one is an
 * answer and the other is no answer at all.
 */
public interface NameLookups {
    /** The channel handle namespace; the channel id in lowercase hex. */
    public suspend fun channel(handle: String): String?

    /** The username namespace; the normalised username. */
    public suspend fun person(username: String): String?
}

/**
 * Ask both namespaces for one name.
 *
 * The channel goes first only because its answer is actionable on its own —
 * subscribing needs nothing else — while a person leads into the contact
 * request flow. Nothing about correctness depends on the order: a name cannot
 * be in both namespaces, since they are separate registries.
 *
 * A network failure is [FoundByName.Unreachable] rather than
 * [FoundByName.NothingFound]: telling someone their friend does not exist
 * because the connection dropped is the worst thing this function could do.
 *
 * A cancellation is never swallowed — it is not a lookup failure, it is the
 * caller going away, and turning it into "nothing found" would report a
 * conclusion nobody reached.
 */
public suspend fun findByName(
    typed: String,
    lookups: NameLookups,
    bareName: (String) -> String = { uniffi.lakko_ffi_uniffi.bareName(it) },
    looksLikeAName: (String) -> Boolean = { uniffi.lakko_ffi_uniffi.looksLikeAName(it) },
): FoundByName {
    val name = bareName(typed)
    if (!looksLikeAName(name)) return FoundByName.NothingFound
    var reachedTheServer = false
    var allowanceUsed = false
    try {
        val channelIdHex = lookups.channel(name)
        reachedTheServer = true
        if (channelIdHex != null) return FoundByName.Channel(channelIdHex, name)
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (e: Throwable) {
        // Fall through: the other namespace may still answer.
        if (me.lakko.app.anon.isAnonTokenQuotaExhausted(e)) allowanceUsed = true
    }
    try {
        val username = lookups.person(name)
        reachedTheServer = true
        if (username != null) return FoundByName.Person(username)
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (e: Throwable) {
        // Handled below.
        if (me.lakko.app.anon.isAnonTokenQuotaExhausted(e)) allowanceUsed = true
    }
    return when {
        reachedTheServer -> FoundByName.NothingFound
        allowanceUsed -> FoundByName.AllowanceUsed
        else -> FoundByName.Unreachable
    }
}

/**
 * What to say about a result, or `null` when the result navigates and there is
 * nothing to say. One sentence on what happened, one on what to do, and never a
 * protocol word.
 */
public fun findByNameMessage(
    result: FoundByName,
    typed: String,
    bareName: (String) -> String = { uniffi.lakko_ffi_uniffi.bareName(it) },
): UiText? = when (result) {
    is FoundByName.NothingFound -> UiText("chats.lookup.nothing", texts = mapOf("name" to "@${bareName(typed)}"))
    is FoundByName.Unreachable -> UiText("chats.lookup.unreachable")
    is FoundByName.AllowanceUsed -> UiText("tokens.lookup_allowance_used")
    else -> null
}

/** The label of the one lookup row: the whole name, never a prefix. */
public fun findByNameRowLabel(
    typed: String,
    bareName: (String) -> String = { uniffi.lakko_ffi_uniffi.bareName(it) },
): UiText = UiText("chats.lookup.action", texts = mapOf("name" to "@${bareName(typed)}"))
