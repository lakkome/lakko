// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.FfiRefusalReason

/*
 * ADR-0061 section 6 — what the chat screen says about a failed action.
 *
 * Errors render from their kind, never from core, server or transport prose:
 * that prose goes to the log, and the screen reads the catalog. The kinds,
 * in the order they are asked (the first that names itself wins):
 *
 *  1. a typed membership refusal ([membershipRefusalMessage], `chat.refusal.*`),
 *     or a [ChatError] that carries its own sentence ([ChatError.userText]);
 *  2. the anonymous-token pool (ADR-0054): a used-up allowance
 *     (`tokens.*_allowance_used`) or any other token failure, which is the
 *     connection (`common.error.offline`);
 *  3. a Delivery Service proof refusal explained by the device clock
 *     (ADR-0053, `ds.clock_skew`);
 *  4. the action's own `chat.error.*` key.
 */

/** Which daily allowance an action draws on (UX `docs/ux/anti-abuse.md` section 3). */
internal enum class AllowanceKind(val key: String) {
    /** Search and adding by username. */
    LOOKUP("tokens.lookup_allowance_used"),

    /** Invite links, group links, channel names. */
    ACTION("tokens.action_allowance_used"),

    /** Following a channel (ADR-0059 section 4, docs/ux/feed.md section 7). */
    SUBSCRIBE("tokens.subscribe_allowance_used"),

    /** A Discover search (docs/ux/feed.md section 7). */
    SEARCH("tokens.search_allowance_used"),
}

/** The token-pool sentence for [error], or `null` when the pool had nothing to do with it. */
internal fun anonTokenFailureText(error: Throwable, allowance: AllowanceKind): UiText? {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is me.lakko.app.anon.AnonTokenQuotaExhausted) return UiText(allowance.key)
        if (cause is me.lakko.app.anon.AnonTokenException) return UiText("common.error.offline")
        cause = cause.cause
    }
    return null
}

/** The sentence a [ChatError] in [error]'s cause chain carries, or `null`. */
internal fun ownUserText(error: Throwable): UiText? {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is ChatError) cause.userText?.let { return it }
        cause = cause.cause
    }
    return null
}

/** `ds.clock_skew` when a proof refusal in [error]'s cause chain is explained by the device clock. */
internal fun dsClockSkewText(error: Throwable): UiText? {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is me.lakko.app.ds.DsProofRefusedError && cause.clockSkewed) return UiText("ds.clock_skew")
        cause = cause.cause
    }
    return null
}

/**
 * The sentence for a failed chat action: the first kind in the list at the top
 * of this file that recognises [error], else [fallbackKey].
 */
internal fun chatErrorText(
    error: Throwable,
    fallbackKey: String,
    retryMaySucceed: (FfiRefusalReason) -> Boolean,
    allowance: AllowanceKind = AllowanceKind.ACTION,
): UiText =
    membershipRefusalMessage(error, retryMaySucceed)
        ?: ownUserText(error)
        ?: anonTokenFailureText(error, allowance)
        ?: dsClockSkewText(error)
        ?: UiText(fallbackKey)
