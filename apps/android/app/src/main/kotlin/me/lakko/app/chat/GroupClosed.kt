// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0065 "Phase 2 revised", R.6 — the wire peek of the group-closed
 * envelope (`lakko-contacts::group_closed`, `lakko/group-closed/v1`).
 *
 * Since the visible flow was removed the envelope has one producer: a device
 * founding the successor of a conversation that cannot be repaired in place
 * posts it into the OLD group as its CLAIM. The receive rule is
 * `SuccessorClaim.kt` ([claimVerdict]); the state it leaves is
 * `SuccessorClaimStore.kt`. Nothing is drawn for it anywhere (R.0).
 */

/**
 * The frozen group-closed envelope domain separator, `lakko/group-closed/v1`
 * followed by one NUL byte (22 bytes). Built programmatically: a raw NUL in a
 * source file makes git treat it as binary.
 */
internal val GROUP_CLOSED_ENVELOPE_DOMAIN: ByteArray =
    "lakko/group-closed/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * Does this application plaintext open with the group-closed domain
 * separator? A complete classification, like the group-name peek: the
 * separator ends in a NUL byte, so no chat envelope and no typed legacy
 * message can open with it. `internal` so the background push filter reuses
 * this exact test. Pure — host-testable.
 */
internal fun looksLikeGroupClosedEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size < GROUP_CLOSED_ENVELOPE_DOMAIN.size) return false
    for (i in GROUP_CLOSED_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != GROUP_CLOSED_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}
