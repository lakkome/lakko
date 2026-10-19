// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.InviteLinkLengths

/**
 * Android-side helpers for the link-based call-invite URL shape
 * (Phase 3c3-url-helpers).
 *
 * Sibling of the contact-invite URL helper that lives in
 * `ContactsRepository.kt` for the `CallInviteCapability`
 * ecosystem. The URL prefix is `/c/` (vs. `/i/` for contact
 * invites), so the Android deep-link intent filter in the
 * manifest can branch on the path segment without parsing the
 * slug.
 *
 * URL plumbing only: [composePointerCallInviteUrl] for the link
 * (`/c/<blob_id_b64url>`) and [parseCallInviteSlug] for the
 * joiner-side dispatcher. The inline form that carried the
 * capability in the URL was removed on 2026-10-19: a failed upload
 * mints no link, and an inline slug is malformed (a dead link).
 *
 * The form-side enums that used to live here — media, expiry and
 * max-uses presets, and the label cap — are gone with the form
 * itself (`docs/ux/call-links.md` section 6). The values the wire
 * still carries are now decided once, in `CallInviteLink.kt`.
 */

/** Path prefix every call-invite URL starts with. Mirror of
 *  the web `CALL_INVITE_URL_PREFIX = "/c/"`. */
public const val CALL_INVITE_URL_PREFIX: String = "/c/"

/** Default origin Lakko shares its invite links under: the web origin of
 *  the stack this build talks to ([ApiEndpoints.webOrigin]), so a link made
 *  against the test stack opens on the test stack. */
public val CALL_INVITE_DEFAULT_ORIGIN: String
    get() = me.lakko.app.config.ApiEndpoints.webOrigin

// =============================================================
// URL builders
// =============================================================

/**
 * Builder for the **pointer** URL form — the short
 * `<origin>/c/<blob_id_b64url>` shape backed by
 * `/discovery/call-invite/<blob_id>` lookup.
 *
 * @throws CallInviteUrlException.WrongBlobIdLength when
 *         `blobId` is not the core's ledger-id width (6 bytes).
 */
public fun composePointerCallInviteUrl(
    blobId: ByteArray,
    origin: String = CALL_INVITE_DEFAULT_ORIGIN,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    if (blobId.size != lengths.blobIdLen) {
        throw CallInviteUrlException.WrongBlobIdLength(
            expected = lengths.blobIdLen,
            got = blobId.size,
        )
    }
    return "$origin$CALL_INVITE_URL_PREFIX${base64UrlEncodeNoPad(blobId)}"
}

// =============================================================
// URL parsing
// =============================================================

/** Parse result for a `/c/<slug>` segment: always the ledger id. */
public sealed class ParsedCallInvite {
    /** Short form — [blobId] is the 6-byte server-
     *  assigned ledger id. Joiner fetches via
     *  `DiscoveryClient.fetchCallInvite(blobId)` →
     *  `CallInviteCapability.decodeAndVerify(...)`. */
    public data class Pointer(val blobId: ByteArray) : ParsedCallInvite() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Pointer) return false
            return blobId.contentEquals(other.blobId)
        }

        override fun hashCode(): Int = blobId.contentHashCode()
    }
}

/**
 * Parse a URL slug into its ledger id (the core's width, 6
 * bytes). Anything else is invalid: the pre-amendment 32-byte
 * pointer and an inline sealed blob included.
 *
 * @throws CallInviteUrlException.InvalidBase64Url when the
 *         slug doesn't decode.
 * @throws CallInviteUrlException.SlugWrongLength when the
 *         decoded byte count is not the ledger-id width.
 */
public fun parseCallInviteSlug(
    slug: String,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): ParsedCallInvite {
    // `base64UrlDecodeNoPad` returns a NON-null ByteArray and THROWS
    // `IllegalArgumentException` on a character outside the base64url
    // alphabet — it never returns null, so an elvis here would be dead
    // code and the raw exception would escape to the caller. The slug
    // comes from a link a stranger handed the user, so it must surface
    // as a typed `CallInviteUrlException` the callers already handle.
    // Same shape as `parseGroupInviteSlug` in `chat/GroupInviteUrl.kt`.
    val bytes = try {
        base64UrlDecodeNoPad(slug)
    } catch (e: IllegalArgumentException) {
        throw CallInviteUrlException.InvalidBase64Url
    }
    if (bytes.size != lengths.blobIdLen) {
        throw CallInviteUrlException.SlugWrongLength(blobIdLen = lengths.blobIdLen, got = bytes.size)
    }
    return ParsedCallInvite.Pointer(bytes)
}

// =============================================================
// Errors
// =============================================================

public sealed class CallInviteUrlException(message: String) : Exception(message) {
    public data class WrongBlobIdLength(
        val expected: Int,
        val got: Int,
    ) : CallInviteUrlException("blobId must be $expected bytes, got $got")

    public object InvalidBase64Url :
        CallInviteUrlException("slug is not valid base64url-no-pad")

    public data class SlugWrongLength(
        val blobIdLen: Int,
        val got: Int,
    ) : CallInviteUrlException("call-invite slug decoded to $got bytes, not a $blobIdLen-byte ledger id")
}
