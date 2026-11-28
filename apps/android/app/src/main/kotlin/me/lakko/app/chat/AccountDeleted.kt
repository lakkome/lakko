// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0098 section 1 — the wire peek of the account-deleted envelope
 * (`lakko-contacts::account_deleted`, `lakko/account-deleted/v1`).
 *
 * A deleting device posts it in every conversation it holds, after its
 * account is gone at auth. The receive rule ([acceptAccountDeletedStatement])
 * verifies it against the MLS sender resolved at receipt (C-68) and this
 * conversation's group id; nothing is ever drawn for it, and the background
 * readers stay silent about it.
 */

/**
 * The frozen account-deleted envelope domain separator,
 * `lakko/account-deleted/v1` followed by one NUL byte (25 bytes). Built
 * programmatically: a raw NUL in a source file makes git treat it as binary.
 */
internal val ACCOUNT_DELETED_ENVELOPE_DOMAIN: ByteArray =
    "lakko/account-deleted/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * Does this application plaintext open with the account-deleted domain
 * separator? A complete classification (the separator ends in NUL, so no chat
 * envelope can open with it). `internal` so the background push filter reuses
 * this exact test. Pure — host-testable.
 */
internal fun looksLikeAccountDeletedEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size < ACCOUNT_DELETED_ENVELOPE_DOMAIN.size) return false
    for (i in ACCOUNT_DELETED_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != ACCOUNT_DELETED_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}

/**
 * The receive rule: the statement is accepted only when [verify] (the core's
 * `verifyAccountDeletedEnvelope`) says it is signed by the sender's account
 * key resolved at receipt, over this conversation's group id. An unresolved
 * sender or a malformed key is dropped. Returns the key to mark deleted, or
 * `null` for "change nothing".
 */
internal fun acceptAccountDeletedStatement(
    envelope: ByteArray,
    groupId: ByteArray,
    senderKeyB64: MemberIdentity?,
    verify: (envelope: ByteArray, groupId: ByteArray, senderKey: ByteArray) -> Boolean,
): MemberIdentity? {
    val sender = senderKeyB64?.takeIf { it.isNotEmpty() && it != OWN_REACTION_SENTINEL } ?: return null
    val key = try {
        java.util.Base64.getDecoder().decode(sender)
    } catch (_: IllegalArgumentException) {
        return null
    }
    if (key.size != 32) return null
    val ok = try {
        verify(envelope, groupId, key)
    } catch (_: Throwable) {
        false
    }
    return if (ok) sender else null
}

/** The core's verifier, over UniFFI. */
internal fun verifyAccountDeletedWithCore(envelope: ByteArray, groupId: ByteArray, senderKey: ByteArray): Boolean =
    uniffi.lakko_ffi_uniffi.verifyAccountDeletedEnvelope(envelope, groupId, senderKey)

/**
 * Whether the chat header hides its call and video buttons: a call already
 * occupies the slot, this device's membership ended, the conversation has no
 * call controls (a group of one), or — ADR-0098 — the other person of this
 * one-to-one deleted their account, or — UX 16.11 — started over with a new
 * account (their old devices are gone).
 */
internal fun callButtonsHidden(
    callOccupied: Boolean,
    membershipEnded: Boolean,
    callControls: Boolean,
    peerDeleted: Boolean,
    peerStartedOver: Boolean = false,
): Boolean = callOccupied || membershipEnded || !callControls || peerDeleted || peerStartedOver
