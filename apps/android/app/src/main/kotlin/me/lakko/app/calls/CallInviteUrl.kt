// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.InviteLinkLengths
import me.lakko.app.discovery.appendInviteLinkSecret

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
 * URL plumbing only: [composePointerCallInviteUrl] for the
 * production short form (`/c/<blob_id_b64url>#<secret>`),
 * [composeInlineCallInviteUrl] for the fallback when the upload fails on
 * transport (the sealed blob is encoded into the URL itself, no
 * `/discovery/call-invite` round-trip needed), and
 * [parseCallInviteSlug] for the joiner-side dispatcher. Every form carries
 * the link secret after `#` (sealed-link amendment of 2026-09-28).
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

/** Lower bound on the inline-bytes form: a sealed call capability (it
 *  embeds a one-time KeyPackage, several hundred bytes plus the 30-byte
 *  seal). The floor at 400 is well above the 6-byte pointer and below any
 *  real sealed blob, so the byte-length test never misclassifies. */
public const val CALL_INVITE_INLINE_MIN_LEN: Int = 400

// =============================================================
// URL builders
// =============================================================

/**
 * Builder for the **pointer** URL form (sealed-link amendment of
 * 2026-09-28): `<origin>/c/<blob_id_b64url>#<secret_b64url>`, backed by
 * `/discovery/call-invite/<blob_id>` lookup. The fragment carries the link
 * secret the sealed blob opens under; without it the link is damaged.
 *
 * @throws CallInviteUrlException.WrongBlobIdLength when `blobId` is not the
 *         core's ledger-id width (6 bytes).
 * @throws IllegalArgumentException when `linkSecret` is not 12 bytes.
 */
public fun composePointerCallInviteUrl(
    blobId: ByteArray,
    linkSecret: ByteArray,
    origin: String = CALL_INVITE_DEFAULT_ORIGIN,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    if (blobId.size != lengths.blobIdLen) {
        throw CallInviteUrlException.WrongBlobIdLength(
            expected = lengths.blobIdLen,
            got = blobId.size,
        )
    }
    return appendInviteLinkSecret(
        "$origin$CALL_INVITE_URL_PREFIX${base64UrlEncodeNoPad(blobId)}",
        linkSecret,
        lengths,
    )
}

/**
 * Builder for the **inline** URL form: `<origin>/c/<sealed_b64url>#<secret>`,
 * the sealed blob carried in the URL itself, no server round-trip on open.
 * Used only when the upload fails on transport.
 *
 * @throws CallInviteUrlException.InlineTooSmall when `sealed.size < 400`.
 * @throws IllegalArgumentException when `linkSecret` is not 12 bytes.
 */
public fun composeInlineCallInviteUrl(
    sealed: ByteArray,
    linkSecret: ByteArray,
    origin: String = CALL_INVITE_DEFAULT_ORIGIN,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    if (sealed.size < CALL_INVITE_INLINE_MIN_LEN) {
        throw CallInviteUrlException.InlineTooSmall(
            minimum = CALL_INVITE_INLINE_MIN_LEN,
            got = sealed.size,
        )
    }
    return appendInviteLinkSecret(
        "$origin$CALL_INVITE_URL_PREFIX${base64UrlEncodeNoPad(sealed)}",
        linkSecret,
        lengths,
    )
}

// =============================================================
// URL parsing
// =============================================================

/** Discriminated parse result for a `/c/<slug>` segment. */
public sealed class ParsedCallInvite {
    /** Short form — [blobId] is the 6-byte server-
     *  assigned ledger id. Joiner fetches via
     *  `DiscoveryClient.fetchCallInvite(blobId)` →
     *  `CallInviteCapability.openLink(sealed, secret)`. */
    public data class Pointer(val blobId: ByteArray) : ParsedCallInvite() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Pointer) return false
            return blobId.contentEquals(other.blobId)
        }

        override fun hashCode(): Int = blobId.contentHashCode()
    }

    /** Long form — the bytes are the sealed link blob. Joiner runs
     *  `CallInviteCapability.openLink(sealed, secret)` directly. */
    public data class Inline(val sealed: ByteArray) : ParsedCallInvite() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Inline) return false
            return sealed.contentEquals(other.sealed)
        }

        override fun hashCode(): Int = sealed.contentHashCode()
    }
}

/**
 * Discriminated parse of a URL slug. The dispatch rule is a
 * clean byte-length test: the ledger-id width (6) ⇒ pointer,
 * ≥ 400 ⇒ inline (a sealed blob). Anything else is invalid, the
 * pre-amendment 32-byte pointer included. The slug must not carry the
 * `#fragment` (strip it with [me.lakko.app.discovery.inviteLinkSlugOf] or
 * read it separately from the intent's `Uri.fragment`).
 *
 * @throws CallInviteUrlException.InvalidBase64Url when the
 *         slug doesn't decode.
 * @throws CallInviteUrlException.SlugWrongLength when the
 *         decoded byte count falls outside both branches.
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
    return when {
        bytes.size == lengths.blobIdLen -> ParsedCallInvite.Pointer(bytes)
        bytes.size >= CALL_INVITE_INLINE_MIN_LEN -> ParsedCallInvite.Inline(bytes)
        else -> throw CallInviteUrlException.SlugWrongLength(
            blobIdLen = lengths.blobIdLen,
            inlineMinLen = CALL_INVITE_INLINE_MIN_LEN,
            got = bytes.size,
        )
    }
}

// =============================================================
// Errors
// =============================================================

public sealed class CallInviteUrlException(message: String) : Exception(message) {
    public data class WrongBlobIdLength(
        val expected: Int,
        val got: Int,
    ) : CallInviteUrlException("blobId must be $expected bytes, got $got")

    public data class InlineTooSmall(
        val minimum: Int,
        val got: Int,
    ) : CallInviteUrlException("sealed must be >= $minimum bytes, got $got")

    public object InvalidBase64Url :
        CallInviteUrlException("slug is not valid base64url-no-pad")

    public data class SlugWrongLength(
        val blobIdLen: Int,
        val inlineMinLen: Int,
        val got: Int,
    ) : CallInviteUrlException(
        "call-invite slug decoded to $got bytes — neither a $blobIdLen-byte pointer nor a >= $inlineMinLen-byte inline capability",
    )
}
