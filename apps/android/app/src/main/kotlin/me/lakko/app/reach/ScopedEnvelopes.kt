// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

/*
 * ADR-0058 client contract, the last bullet — the five envelope kinds scoped
 * discovery adds (and the ADR-0057 registrar seed, which this client does not
 * act on yet) are dispatched and NEVER rendered: on the foreground timeline and
 * in the background push-preview filter alike.
 *
 * Each domain separator ends in a NUL byte, so a prefix match is a complete
 * CLASSIFICATION (never a decode): no chat envelope (`{`) and no text a person
 * typed can open with one. A plaintext that carries a domain but does not
 * decode is still that kind, and is dropped rather than handed to the chat
 * parser, whose lossy-UTF-8 fallback would render the domain string as a
 * bubble. Built programmatically: a raw NUL in a source file makes git treat it
 * as binary.
 */

/** The service envelopes ADR-0058 (and ADR-0057) add to an MLS conversation or
 *  the DeviceGroup. */
public enum class ScopedEnvelopeKind(domain: String) {
    /** An introduction record a contact offers (1:1 conversation). */
    INTRODUCTION_OFFER("lakko/introduction-offer/v1"),

    /** A padded bundle of a contact's contacts' records (1:1 conversation). */
    INTRODUCTIONS("lakko/introductions/v1"),

    /** The "who can find me" setting (DeviceGroup). */
    VISIBILITY("lakko/visibility/v1"),

    /** The reach-key version counters (DeviceGroup). */
    REACH_STATE("lakko/reach-state/v1"),

    /** A contact's delegations for the committers of an Add (group). */
    COMMITTER_DELEGATION("lakko/committer-delegation/v1"),

    /** ADR-0057 — a journal root's registrar seed. Suppressed only. */
    REGISTRAR_SEED("lakko/ds-registrar-seed/v1"),
    ;

    /** The frozen domain separator: the ASCII name followed by one NUL. */
    public val prefix: ByteArray = domain.toByteArray(Charsets.US_ASCII) + byteArrayOf(0)
}

/** The scoped-discovery kind [bytes] opens with, or `null` for anything else.
 *  Pure — host-testable. */
public fun scopedEnvelopeKindOf(bytes: ByteArray): ScopedEnvelopeKind? {
    if (bytes.isEmpty() || (bytes[0].toInt() and 0xFF) != SERVICE_ENVELOPE_FIRST_BYTE) return null
    for (kind in ScopedEnvelopeKind.entries) {
        val prefix = kind.prefix
        if (bytes.size < prefix.size) continue
        var match = true
        for (i in prefix.indices) {
            if (bytes[i] != prefix[i]) {
                match = false
                break
            }
        }
        if (match) return kind
    }
    return null
}

/** `'l'`, the first byte of every `lakko/...` domain. */
private const val SERVICE_ENVELOPE_FIRST_BYTE: Int = 0x6c
