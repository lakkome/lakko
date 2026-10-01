// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import me.lakko.app.auth.base64UrlDecodeNoPad
import uniffi.lakko_ffi_uniffi.inviteBlobIdLen
import uniffi.lakko_ffi_uniffi.inviteBlobIdUrlLen
import uniffi.lakko_ffi_uniffi.inviteLinkSecretLen
import uniffi.lakko_ffi_uniffi.inviteLinkSecretUrlLen

/**
 * The widths of a short invite link (ADR-0028 amendment of 2026-09-27), read
 * from the core rather than written down here.
 *
 * - [blobIdLen] / [blobIdUrlLen]: the server-assigned ledger id of an `/i/`,
 *   `/c/` or `/g/` pointer link, 6 bytes / 8 base64url characters.
 * - [linkSecretLen] / [linkSecretUrlLen]: the link secret in the URL fragment
 *   of every kind (`/i/`, `/c/`, `/g/`), 12 bytes / 16 base64url characters;
 *   since the sealed-link amendment of 2026-09-28 it opens the whole link.
 *
 * Production reads [core], which asks the FFI once. Every pure function that
 * needs a width takes an [InviteLinkLengths] parameter defaulting to [core],
 * so a host JVM test (which has no native library) passes the values in and
 * never touches the FFI.
 */
public data class InviteLinkLengths(
    val blobIdLen: Int,
    val blobIdUrlLen: Int,
    val linkSecretLen: Int,
    val linkSecretUrlLen: Int,
) {
    public companion object {
        /** The widths the linked core defines. */
        public val core: InviteLinkLengths by lazy {
            InviteLinkLengths(
                blobIdLen = inviteBlobIdLen().toInt(),
                blobIdUrlLen = inviteBlobIdUrlLen().toInt(),
                linkSecretLen = inviteLinkSecretLen().toInt(),
                linkSecretUrlLen = inviteLinkSecretUrlLen().toInt(),
            )
        }
    }
}

/**
 * Decode a server-issued ledger id (`{"blob_id": "<8 chars>"}`) or a pointer
 * path segment to its raw bytes, or `null` when it is not exactly
 * [InviteLinkLengths.blobIdUrlLen] base64url characters decoding to
 * [InviteLinkLengths.blobIdLen] bytes. The pre-amendment 43-character /
 * 32-byte id is refused here.
 */
public fun decodeInviteBlobId(text: String, lengths: InviteLinkLengths = InviteLinkLengths.core): ByteArray? {
    if (text.length != lengths.blobIdUrlLen) return null
    val bytes = try {
        base64UrlDecodeNoPad(text)
    } catch (_: IllegalArgumentException) {
        return null
    }
    return if (bytes.size == lengths.blobIdLen) bytes else null
}

/**
 * `true` when [error] (or anything in its cause chain) is the discovery
 * service answering `429`: the per-address budget for invite fetch and
 * consume is spent. The link is not invalid; the person should try again
 * later, and the UI says exactly that.
 */
public fun isDiscoveryRateLimited(error: Throwable): Boolean {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is DiscoveryHttpError && cause.status == HTTP_TOO_MANY_REQUESTS) return true
        cause = cause.cause
    }
    return false
}

/** What an invite link whose fetch or consume was rate limited says (ADR-0061). */
public val INVITE_RATE_LIMITED_TEXT: me.lakko.app.l10n.UiText =
    me.lakko.app.l10n.UiText("invite.link.rate_limited")

private const val HTTP_TOO_MANY_REQUESTS: Int = 429
