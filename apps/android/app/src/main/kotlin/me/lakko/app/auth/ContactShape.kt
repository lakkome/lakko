// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/**
 * Light client-side contact-shape validation (ADR-0018 factor (d)).
 *
 * A PERMISSIVE mirror of the server's `normalize_contact` gate
 * (`services/auth/src/recovery_contact.rs`) and of the web
 * `apps/web/src/auth/contact-shape.ts`: just enough of a check to
 * catch an obviously-malformed contact (an email with no `@`, a
 * phone with no leading `+`) BEFORE spending a round-trip and one
 * of the server's per-name attempts. The server is authoritative — it does the full
 * E.164 / RFC-ish normalization and rejects with a 400 — so this
 * deliberately stays loose and never blocks a borderline value the
 * server might accept.
 */

/**
 * Return `true` when [contact] is plausibly shaped for [kind]
 * ([ContactKind.EMAIL] or [ContactKind.PHONE]):
 *
 *   - **email** — contains exactly one `@` with a non-empty local
 *     part and domain, no whitespace, and is at most 254 chars.
 *   - **phone** — after stripping the common human separators
 *     (spaces, dashes, parentheses, non-breaking space), starts with
 *     `+` followed by 1..=15 digits (ASCII digits only thereafter).
 *
 * Whitespace is trimmed first. Anything else returns `false` so the
 * form can surface a precise message instead of burning a round trip.
 * An unknown `kind` returns `false` (fail-closed).
 */
public fun isPlausibleContact(kind: Int, contact: String): Boolean {
    val trimmed = contact.trim()
    if (trimmed.isEmpty()) {
        return false
    }
    return when (kind) {
        ContactKind.EMAIL -> isPlausibleEmail(trimmed)
        ContactKind.PHONE -> isPlausiblePhone(trimmed)
        else -> false
    }
}

/** Email: exactly one `@`, non-empty either side, no whitespace,
 *  <= 254 chars (mirror of the server's structural check). */
private fun isPlausibleEmail(trimmed: String): Boolean {
    if (trimmed.any { it.isWhitespace() }) {
        return false
    }
    val at = trimmed.indexOf('@')
    return at > 0 &&
        at == trimmed.lastIndexOf('@') &&
        at < trimmed.length - 1 &&
        trimmed.length <= 254
}

/** Phone: strip the separators the server also strips, then require
 *  a leading `+` and 1..=15 ASCII digits. */
private fun isPlausiblePhone(trimmed: String): Boolean {
    // Mirror the server's separator set: space, dash, parens, NBSP.
    val stripped = trimmed.filterNot {
        it == ' ' || it == '-' || it == '(' || it == ')' || it == ' '
    }
    if (!stripped.startsWith('+')) {
        return false
    }
    val digits = stripped.substring(1)
    return digits.length in 1..15 && digits.all { it in '0'..'9' }
}
