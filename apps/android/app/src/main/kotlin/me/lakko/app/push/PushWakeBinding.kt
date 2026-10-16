// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

/**
 * ADR-0042, "A wake is shown only for the signed-in account" (amended
 * 2026-09-30), with ADR-0078 section 3 — the pure half of the wake gate.
 *
 * A device has at most one active account, and a client never shows another
 * account's push. There is no device pseudonym any more: every wake carries
 * `s`, the push-gw subscription id it was delivered for, and this device's
 * subscription ids live in the bound account's route rows
 * ([PushRouteStore.subscriptionId]). The client records which account push is
 * set up for ([PushWakeBinding]) where the FCM service can read it
 * ([PushWakeBindingStore]); Log out and Forget device clear it first
 * ([PushSignOut]).
 *
 * The rule here is framework-free so the host JVM suite pins it:
 * [wakeAccountFor] — a wake is handled only when a binding exists and the
 * wake carries a well-formed `s`; the account it is handled AS is the bound
 * one. The caller must still require the bound account's route row for the
 * wake's `g` to hold that same id ([PushRouteStore.lookup]).
 */
public data class PushWakeBinding(
    /** The account push is set up for (standard base64, the key every
     *  per-account store uses). */
    val accountIdB64: String,
)

/** Subscription id length in hex characters (16 bytes). */
public const val SUBSCRIPTION_ID_HEX_LEN: Int = 32

/**
 * Normalise a subscription-id hex string for comparison: trimmed, lowercase,
 * exactly [SUBSCRIPTION_ID_HEX_LEN] hex digits. `null` for anything else,
 * which never matches.
 */
public fun normalizeSubscriptionIdHex(hex: String?): String? {
    val s = hex?.trim()?.lowercase() ?: return null
    if (s.length != SUBSCRIPTION_ID_HEX_LEN) return null
    for (c in s) {
        if (c !in '0'..'9' && c !in 'a'..'f') return null
    }
    return s
}

/**
 * The wake gate. Returns the account a wake may be handled as, or `null` when
 * it must not be handled at all (no notification).
 *
 * `null` when there is no binding (no account signed in), or when the wake has
 * no `s` or a malformed one.
 */
public fun wakeAccountFor(wakeSubscriptionIdHex: String?, binding: PushWakeBinding?): String? {
    val bound = binding ?: return null
    if (bound.accountIdB64.isEmpty()) return null
    normalizeSubscriptionIdHex(wakeSubscriptionIdHex) ?: return null
    return bound.accountIdB64
}
