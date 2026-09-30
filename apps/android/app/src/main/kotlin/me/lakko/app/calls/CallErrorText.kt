// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.l10n.UiText

/*
 * ADR-0058 section 6 — what a call screen says about a failed start or join.
 *
 * The Kotlin twin of `apps/web/src/calls/call-errors.ts`. A failure renders
 * from its kind, never from core, SFU, server or transport prose: that prose
 * goes to the log. The kinds, in the order they are asked:
 *
 *  1. a call-admission refusal from the auth service: `429` (the call is not
 *     accepting new participants, ADR-0033), `401` (the session expired),
 *     `503` (calls are not available);
 *  2. no network (`common.error.offline`);
 *  3. a refused microphone (`call.error.microphone_not_allowed`).
 *
 * Anything else has no sentence of its own, and the caller shows its flow's
 * fallback.
 */

/** The sentence for the kind of [error], or `null` when it names no kind. */
public fun callErrorReason(error: Throwable?): UiText? {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        when (cause) {
            is CallJwtHttpError -> return when (cause.status) {
                429 -> UiText("call.error.not_accepting")
                401 -> UiText("common.error.session_expired")
                503 -> UiText("call.error.unavailable")
                else -> null
            }
            is CallJwtNetworkError, is java.io.IOException -> return UiText("common.error.offline")
            is SecurityException -> return UiText("call.error.microphone_not_allowed")
        }
        cause = cause.cause
    }
    return null
}

/** [callErrorReason] for [error], else the flow's own [fallbackKey]. */
public fun callErrorText(error: Throwable?, fallbackKey: String): UiText =
    callErrorReason(error) ?: UiText(fallbackKey)
