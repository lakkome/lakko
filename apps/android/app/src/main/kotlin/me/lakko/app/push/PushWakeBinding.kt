// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

/**
 * ADR-0042, "A wake is shown only for the signed-in account" (amended
 * 2026-09-30) — the pure half of the wake gate.
 *
 * A device has at most one active account, and a client never shows another
 * account's push. Every wake carries `d`, the device pseudonym that
 * subscribed; it is minted at each sign-in and cleared at Log out, so it names
 * the sign-in session. When the pseudonym is registered for the signed-in
 * account the client records the pair ([PushWakeBinding]) where the FCM
 * service can read it ([PushWakeBindingStore]); Log out and Forget device
 * clear it first ([PushSignOut]).
 *
 * The rules here are framework-free so the host JVM suite pins them:
 *
 *   * [wakeAccountFor] — a wake is handled only when a binding exists and the
 *     wake's `d` equals the bound pseudonym; the account it is handled AS is
 *     the bound one, never one named by a reverse-map row or found by a scan.
 *   * [shouldRegisterOnNewToken] — an FCM token rotation re-registers only
 *     the bound pseudonym, so a signed-out device registers nothing.
 */
public data class PushWakeBinding(
    /** The device pseudonym as lowercase hex (64 chars for 32 bytes). */
    val devicePseudonymHex: String,
    /** The account the pseudonym was registered for (standard base64, the
     *  key every per-account store uses). */
    val accountIdB64: String,
)

/**
 * Normalise a device-pseudonym hex string for comparison: trimmed, lowercase,
 * non-empty, even length, hex digits only. `null` for anything else, which
 * never matches.
 */
public fun normalizePseudonymHex(hex: String?): String? {
    val s = hex?.trim()?.lowercase() ?: return null
    if (s.isEmpty() || s.length % 2 != 0) return null
    for (c in s) {
        if (c !in '0'..'9' && c !in 'a'..'f') return null
    }
    return s
}

/**
 * The wake gate. Returns the account a wake may be handled as, or `null` when
 * it must not be handled at all (no notification).
 *
 * `null` when there is no binding (no account signed in), when the wake has
 * no `d` or a malformed one, or when `d` is not the bound pseudonym (a wake
 * addressed to an earlier sign-in, or a row push-gw has not dropped yet).
 */
public fun wakeAccountFor(wakeDeviceHex: String?, binding: PushWakeBinding?): String? {
    val bound = binding ?: return null
    if (bound.accountIdB64.isEmpty()) return null
    val want = normalizePseudonymHex(bound.devicePseudonymHex) ?: return null
    val got = normalizePseudonymHex(wakeDeviceHex) ?: return null
    return if (got == want) bound.accountIdB64 else null
}

/**
 * Whether an FCM token rotation should re-register [storedPseudonymHex].
 *
 * Only while an account is signed in, which on this path means: a binding
 * exists and it names the pseudonym this device currently holds. A signed-out
 * device (no binding) registers nothing, and never mints a pseudonym to do so.
 */
public fun shouldRegisterOnNewToken(
    binding: PushWakeBinding?,
    storedPseudonymHex: String?,
): Boolean {
    val bound = binding ?: return false
    if (bound.accountIdB64.isEmpty()) return false
    val want = normalizePseudonymHex(bound.devicePseudonymHex) ?: return false
    return normalizePseudonymHex(storedPseudonymHex) == want
}
