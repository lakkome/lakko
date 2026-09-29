// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import me.lakko.app.auth.base64UrlDecodeNoPad
import uniffi.lakko_ffi_uniffi.groupInviteLinkSecretLen
import uniffi.lakko_ffi_uniffi.groupInviteLinkSecretUrlLen
import uniffi.lakko_ffi_uniffi.inviteBlobIdLen
import uniffi.lakko_ffi_uniffi.inviteBlobIdUrlLen

/**
 * The widths of a short invite link (ADR-0028 amendment of 2026-09-27), read
 * from the core rather than written down here.
 *
 * - [blobIdLen] / [blobIdUrlLen]: the server-assigned ledger id of an `/i/`,
 *   `/c/` or `/g/` pointer link, 6 bytes / 8 base64url characters.
 * - [linkSecretLen] / [linkSecretUrlLen]: the `/g/` link secret in the URL
 *   fragment, 12 bytes / 16 base64url characters.
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
                linkSecretLen = groupInviteLinkSecretLen().toInt(),
                linkSecretUrlLen = groupInviteLinkSecretUrlLen().toInt(),
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

/** The copy for an invite link whose fetch or consume was rate limited. */
public const val INVITE_RATE_LIMITED_COPY: String =
    "Too many requests from this network right now. Try again in a minute."

private const val HTTP_TOO_MANY_REQUESTS: Int = 429

/**
 * Split a link token — the text after `/i/`, `/c/` or `/g/`, which since the
 * 2026-09-28 security pass carries the link secret as its fragment,
 * `<slug>#<secret>` — into the slug and the fragment (`null` when there is
 * none). The router passes the two as one string so every landing gets both.
 */
public fun splitLinkToken(token: String): Pair<String, String?> {
    val at = token.indexOf('#')
    if (at < 0) return token.trim().removeSuffix("/") to null
    val slug = token.substring(0, at).trim().removeSuffix("/")
    val fragment = token.substring(at + 1).trim().takeIf { it.isNotEmpty() }
    return slug to fragment
}

/** Join a slug and a fragment back into one link token. */
public fun joinLinkToken(slug: String, fragment: String?): String =
    if (fragment.isNullOrBlank()) slug else "$slug#$fragment"
