// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.InviteLinkLengths

/**
 * Android-side helpers for the group join-ticket URL shape (ADR-0028, G-3).
 *
 * Sibling of [me.lakko.app.calls.parseCallInviteSlug] for the
 * `GroupInviteCapability` ecosystem. The `/g/<slug>` slug is one form:
 *
 *   - **pointer** — the 6-byte server-assigned ledger id (8 base64url
 *     characters, ADR-0028 amendment of 2026-09-27) from
 *     [me.lakko.app.discovery.DiscoveryClient.uploadGroupInvite]. The
 *     production default: a short URL, the joiner fetches the capability from
 *     `/discovery/group-invite/<blob_id>`. This is the OQ1 indirection that
 *     adds the server-side anti-abuse ledger (consume dedup + soft max_uses).
 *
 * Anything else is refused: the pre-amendment 32-byte pointer, and the inline
 * form that carried the sealed blob in the path (removed 2026-10-19: a failed
 * upload mints no link, and an inline URL opens as a dead link).
 *
 * The pointer width comes from the core ([InviteLinkLengths.core]); every
 * function here takes it as a parameter so a host JVM test can pass it in.
 */

/** Path prefix every group-invite URL starts with. Mirror of the web
 *  `GROUP_INVITE_URL_PREFIX = "/g/"`. */
public const val GROUP_INVITE_URL_PREFIX: String = "/g/"

/** Default origin Lakko shares its invite links under: the web origin of
 *  the stack this build talks to ([ApiEndpoints.webOrigin]). */
public val GROUP_INVITE_DEFAULT_ORIGIN: String
    get() = me.lakko.app.config.ApiEndpoints.webOrigin

// =============================================================
// URL builders
// =============================================================

/**
 * Builder for the **pointer** URL form — the short
 * `<origin>/g/<blob_id_b64url>` shape backed by
 * `/discovery/group-invite/<blob_id>` lookup.
 *
 * @throws GroupInviteUrlException.WrongBlobIdLength when `blobId` is not the
 *         core's ledger-id width (6 bytes).
 */
public fun composePointerGroupInviteUrl(
    blobId: ByteArray,
    origin: String = GROUP_INVITE_DEFAULT_ORIGIN,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    if (blobId.size != lengths.blobIdLen) {
        throw GroupInviteUrlException.WrongBlobIdLength(
            expected = lengths.blobIdLen,
            got = blobId.size,
        )
    }
    return "$origin$GROUP_INVITE_URL_PREFIX${base64UrlEncodeNoPad(blobId)}"
}

// =============================================================
// URL parsing
// =============================================================

/** Parse result for a `/g/<slug>` segment: always the ledger id. */
public sealed class ParsedGroupInvite {
    /** Short form — [blobId] is the 6-byte ledger id. Joiner
     *  fetches via `DiscoveryClient.fetchGroupInvite(blobId)` ->
     *  `GroupInviteCapability.decodeAndVerify(...)`. */
    public data class Pointer(val blobId: ByteArray) : ParsedGroupInvite() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Pointer) return false
            return blobId.contentEquals(other.blobId)
        }

        override fun hashCode(): Int = blobId.contentHashCode()
    }
}

/**
 * Parse a URL slug into its ledger id (the core's width, 6 bytes). Anything
 * else is invalid input: the pre-amendment 32-byte pointer and an inline
 * sealed blob included.
 *
 * @throws GroupInviteUrlException.InvalidBase64Url when the slug doesn't decode.
 * @throws GroupInviteUrlException.SlugWrongLength when the decoded byte count
 *         is not the ledger-id width.
 */
public fun parseGroupInviteSlug(
    slug: String,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): ParsedGroupInvite {
    val bytes = try {
        base64UrlDecodeNoPad(slug)
    } catch (e: IllegalArgumentException) {
        throw GroupInviteUrlException.InvalidBase64Url
    }
    if (bytes.size != lengths.blobIdLen) {
        throw GroupInviteUrlException.SlugWrongLength(blobIdLen = lengths.blobIdLen, got = bytes.size)
    }
    return ParsedGroupInvite.Pointer(bytes)
}

// =============================================================
// Errors
// =============================================================

public sealed class GroupInviteUrlException(message: String) : Exception(message) {
    public data class WrongBlobIdLength(
        val expected: Int,
        val got: Int,
    ) : GroupInviteUrlException("blobId must be $expected bytes, got $got")

    public object InvalidBase64Url :
        GroupInviteUrlException("slug is not valid base64url-no-pad")

    public data class SlugWrongLength(
        val blobIdLen: Int,
        val got: Int,
    ) : GroupInviteUrlException("group-invite slug decoded to $got bytes, not a $blobIdLen-byte ledger id")
}
