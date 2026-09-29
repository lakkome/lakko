// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.InviteLinkLengths
import me.lakko.app.discovery.appendInviteLinkSecret

/**
 * Android-side helpers for the group join-ticket URL shape (ADR-0028, G-3).
 *
 * Sibling of [me.lakko.app.calls.parseCallInviteSlug] for the
 * `GroupInviteCapability` ecosystem. The `/g/<slug>` slug comes in two forms,
 * discriminated purely by the decoded byte length:
 *
 *   - **pointer** — the 6-byte server-assigned ledger id (8 base64url
 *     characters, ADR-0028 amendment of 2026-09-27) from
 *     [me.lakko.app.discovery.DiscoveryClient.uploadGroupInvite]. The
 *     production default: a short URL, the joiner fetches the capability from
 *     `/discovery/group-invite/<blob_id>`. This is the OQ1 indirection that
 *     adds the server-side anti-abuse ledger (consume dedup + soft max_uses).
 *   - **inline** — the sealed link blob (>= 138 bytes), used only when the
 *     upload failed on transport: no `/discovery/group-invite` round-trip.
 *
 * Both forms carry the link secret after `#` (sealed-link amendment of
 * 2026-09-28); the slug parsed here never includes it. The smallest legal
 * sealed blob is well above the 6-byte pointer, so a byte-length test cleanly
 * discriminates. The pre-amendment 32-byte pointer falls between the two and
 * is refused.
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

/** Floor on the inline form. The smallest sealed capability is 138 bytes (a
 *  108-byte capability with a 1-byte group_id plus the 30-byte seal); the
 *  floor at 100 sits comfortably above the 6-byte pointer and below any real
 *  sealed blob, so the byte-length test never misclassifies. */
public const val GROUP_INVITE_INLINE_MIN_LEN: Int = 100

// =============================================================
// URL builders
// =============================================================

/**
 * Builder for the **pointer** URL form (sealed-link amendment of
 * 2026-09-28): `<origin>/g/<blob_id_b64url>#<secret_b64url>`, backed by
 * `/discovery/group-invite/<blob_id>` lookup.
 *
 * @throws GroupInviteUrlException.WrongBlobIdLength when `blobId` is not the
 *         core's ledger-id width (6 bytes).
 * @throws IllegalArgumentException when `linkSecret` is not 12 bytes.
 */
public fun composePointerGroupInviteUrl(
    blobId: ByteArray,
    linkSecret: ByteArray,
    origin: String = GROUP_INVITE_DEFAULT_ORIGIN,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    if (blobId.size != lengths.blobIdLen) {
        throw GroupInviteUrlException.WrongBlobIdLength(
            expected = lengths.blobIdLen,
            got = blobId.size,
        )
    }
    return appendInviteLinkSecret(
        "$origin$GROUP_INVITE_URL_PREFIX${base64UrlEncodeNoPad(blobId)}",
        linkSecret,
        lengths,
    )
}

/**
 * Builder for the **inline** URL form: `<origin>/g/<sealed_b64url>#<secret>`,
 * the sealed blob in the URL itself, no server round-trip on open. Used only
 * when the upload fails on transport.
 *
 * @throws GroupInviteUrlException.InlineTooSmall when `sealed.size < 100`.
 * @throws IllegalArgumentException when `linkSecret` is not 12 bytes.
 */
public fun composeInlineGroupInviteUrl(
    sealed: ByteArray,
    linkSecret: ByteArray,
    origin: String = GROUP_INVITE_DEFAULT_ORIGIN,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    if (sealed.size < GROUP_INVITE_INLINE_MIN_LEN) {
        throw GroupInviteUrlException.InlineTooSmall(
            minimum = GROUP_INVITE_INLINE_MIN_LEN,
            got = sealed.size,
        )
    }
    return appendInviteLinkSecret(
        "$origin$GROUP_INVITE_URL_PREFIX${base64UrlEncodeNoPad(sealed)}",
        linkSecret,
        lengths,
    )
}

// =============================================================
// URL parsing
// =============================================================

/** Discriminated parse result for a `/g/<slug>` segment. */
public sealed class ParsedGroupInvite {
    /** Short form — [blobId] is the 6-byte ledger id. Joiner
     *  fetches via `DiscoveryClient.fetchGroupInvite(blobId)` ->
     *  `GroupInviteCapability.openLink(sealed, secret)`. */
    public data class Pointer(val blobId: ByteArray) : ParsedGroupInvite() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Pointer) return false
            return blobId.contentEquals(other.blobId)
        }

        override fun hashCode(): Int = blobId.contentHashCode()
    }

    /** Long form — the bytes are the sealed link blob. Joiner runs
     *  `GroupInviteCapability.openLink(sealed, secret)` directly. */
    public data class Inline(val sealed: ByteArray) : ParsedGroupInvite() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Inline) return false
            return sealed.contentEquals(other.sealed)
        }

        override fun hashCode(): Int = sealed.contentHashCode()
    }
}

/**
 * Discriminated parse of a URL slug. The dispatch rule is a byte-length test:
 * the ledger-id width (6) => pointer, >= 100 => inline. Anything else is
 * invalid input, the pre-amendment 32-byte pointer included.
 *
 * @throws GroupInviteUrlException.InvalidBase64Url when the slug doesn't decode.
 * @throws GroupInviteUrlException.SlugWrongLength when the decoded byte count
 *         falls outside both branches.
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
    return when {
        bytes.size == lengths.blobIdLen -> ParsedGroupInvite.Pointer(bytes)
        bytes.size >= GROUP_INVITE_INLINE_MIN_LEN -> ParsedGroupInvite.Inline(bytes)
        else -> throw GroupInviteUrlException.SlugWrongLength(
            blobIdLen = lengths.blobIdLen,
            inlineMinLen = GROUP_INVITE_INLINE_MIN_LEN,
            got = bytes.size,
        )
    }
}

// =============================================================
// Errors
// =============================================================

public sealed class GroupInviteUrlException(message: String) : Exception(message) {
    public data class WrongBlobIdLength(
        val expected: Int,
        val got: Int,
    ) : GroupInviteUrlException("blobId must be $expected bytes, got $got")

    public data class InlineTooSmall(
        val minimum: Int,
        val got: Int,
    ) : GroupInviteUrlException("sealed must be >= $minimum bytes, got $got")

    public object InvalidBase64Url :
        GroupInviteUrlException("slug is not valid base64url-no-pad")

    public data class SlugWrongLength(
        val blobIdLen: Int,
        val inlineMinLen: Int,
        val got: Int,
    ) : GroupInviteUrlException(
        "group-invite slug decoded to $got bytes — neither a $blobIdLen-byte pointer nor a >= $inlineMinLen-byte inline capability",
    )
}
