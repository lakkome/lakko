// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import me.lakko.app.chat.AllowanceKind
import me.lakko.app.chat.anonTokenFailureText
import me.lakko.app.chat.dsClockSkewText
import me.lakko.app.contacts.ContactsError
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.discovery.DiscoveryNetworkError
import me.lakko.app.ds.DsNetworkError
import me.lakko.app.l10n.UiText

/*
 * ADR-0061 section 6 — what the contacts screens say about a failed action.
 *
 * Errors render from their kind, never from core, server or transport prose
 * (that goes to the log). The kinds, in the order they are asked:
 *
 *  1. a [ContactsError] naming an unknown username
 *     (`chat.members.add.error.unknown_username`) or a person who has not
 *     finished setting up (`chat.members.add.error.no_bundle`);
 *  2. the anonymous-token pool (ADR-0054): a used-up allowance or any other
 *     token failure (`tokens.*`, `common.error.offline`);
 *  3. a Delivery Service proof refusal explained by the device clock
 *     (`ds.clock_skew`);
 *  4. the discovery service answering 429 (`common.error.too_many_attempts`)
 *     or 401 (`common.error.session_ended`), or no network
 *     (`common.error.offline`);
 *  5. the action's own key.
 *
 * Mirror of the web `contactsErrorText` (`contacts/errors.ts`).
 */
internal fun contactsErrorText(
    error: Throwable,
    fallbackKey: String,
    allowance: AllowanceKind = AllowanceKind.ACTION,
): UiText {
    contactsVariantText(error)?.let { return it }
    // A wrapped failure carries its cause in the variant, not in the cause chain.
    val inner = ((error as? ContactsError)?.variant as? ContactsError.Variant.Wrapped)?.cause ?: error
    anonTokenFailureText(inner, allowance)?.let { return it }
    dsClockSkewText(inner)?.let { return it }
    transportText(inner)?.let { return it }
    return UiText(fallbackKey)
}

private fun contactsVariantText(error: Throwable): UiText? {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is ContactsError) {
            when (val v = cause.variant) {
                // ADR-0060 / docs/ux/who-can-find-me.md section 6.1 "Not found":
                // the same whether the account does not exist or does not let
                // the searcher find it, so it never names the username.
                // TODO(ADR-0061): main's literal, carried as the key until it
                // is moved into the catalog (an unknown key renders verbatim).
                is ContactsError.Variant.UsernameUnknown ->
                    return UiText(me.lakko.app.search.SEARCH_NOT_FOUND)
                // ADR-0060 "Request refused"; the same literal-as-key carry.
                is ContactsError.Variant.ReachRefused ->
                    return UiText(ContactsError.REACH_REFUSED_COPY)
                is ContactsError.Variant.NoIdentityBundle ->
                    return UiText("chat.members.add.error.no_bundle")
                is ContactsError.Variant.Wrapped -> Unit
            }
        }
        cause = cause.cause
    }
    return null
}

private fun transportText(error: Throwable): UiText? {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        when (cause) {
            is DiscoveryHttpError -> when (cause.status) {
                HTTP_UNAUTHORIZED -> return UiText("common.error.session_ended")
                HTTP_TOO_MANY_REQUESTS -> return UiText("common.error.too_many_attempts")
            }
            is DiscoveryNetworkError, is DsNetworkError -> return UiText("common.error.offline")
        }
        cause = cause.cause
    }
    return null
}

private const val HTTP_UNAUTHORIZED: Int = 401
private const val HTTP_TOO_MANY_REQUESTS: Int = 429
