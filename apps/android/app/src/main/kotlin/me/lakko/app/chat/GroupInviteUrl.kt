// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad

/**
 * Android-side helpers for the group join-ticket URL shape (ADR-0028, G-3).
 *
 * Sibling of [me.lakko.app.calls.parseCallInviteSlug] for the
 * `GroupInviteCapability` ecosystem. The `/g/<slug>` slug comes in two forms,
 * discriminated purely by the decoded byte length:
 *
 *   - **pointer** — the 32-byte content-addressed `blob_id` from
 *     [me.lakko.app.discovery.DiscoveryClient.uploadGroupInvite]. The
 *     production default: a short URL, the joiner fetches the capability from
 *     `/discovery/group-invite/<blob_id>`. This is the OQ1 indirection that
 *     adds the server-side anti-abuse ledger (consume dedup + soft max_uses).
 *   - **inline** — the full `GroupInviteCapability` wire bytes (>= 108 bytes).
 *     The censored-region fallback: no `/discovery/group-invite` round-trip.
 *
 * The smallest legal inline capability is 108 bytes (a 1-byte group_id), well
 * above the 32-byte pointer, so a byte-length test cleanly discriminates.
 */

/** Path prefix every group-invite URL starts with. Mirror of the web
 *  `GROUP_INVITE_URL_PREFIX = "/g/"`. */
public const val GROUP_INVITE_URL_PREFIX: String = "/g/"

/** Default origin Lakko shares its invite links under. Stays in sync with the
 *  web + iOS sides; a link generated on Android works on any platform. */
public const val GROUP_INVITE_DEFAULT_ORIGIN: String = "https://lakko.me"

/** Length of a content-addressed `blob_id` from
 *  `services/discovery/src/group_invite.rs` — SHA-256, 32 bytes. */
public const val GROUP_INVITE_BLOB_ID_LEN: Int = 32

/** Floor on the inline-bytes form. The real minimum is 108 bytes; we pin the
 *  discriminator floor at 100 — comfortably above the 32-byte pointer and below
 *  any real capability, so the byte-length test never misclassifies. Anything
 *  > 32 and < 100 is invalid input. */
public const val GROUP_INVITE_INLINE_MIN_LEN: Int = 100

// =============================================================
// URL builders
// =============================================================

/**
 * Builder for the **pointer** URL form — the short
 * `<origin>/g/<blob_id_b64url>` shape backed by
 * `/discovery/group-invite/<blob_id>` lookup.
 *
 * @throws GroupInviteUrlException.WrongBlobIdLength when `blobId.size != 32`.
 */
public fun composePointerGroupInviteUrl(
    blobId: ByteArray,
    origin: String = GROUP_INVITE_DEFAULT_ORIGIN,
): String {
    if (blobId.size != GROUP_INVITE_BLOB_ID_LEN) {
        throw GroupInviteUrlException.WrongBlobIdLength(
            expected = GROUP_INVITE_BLOB_ID_LEN,
            got = blobId.size,
        )
    }
    return "$origin$GROUP_INVITE_URL_PREFIX${base64UrlEncodeNoPad(blobId)}"
}

/**
 * Builder for the **inline-bytes** URL form — the long shape that carries the
 * capability inside the URL itself, no server round-trip on open. Reserved for
 * the censored-region fallback; the pointer form is the production default.
 *
 * @throws GroupInviteUrlException.InlineTooSmall when
 *         `capabilityBytes.size < 100`.
 */
public fun composeInlineGroupInviteUrl(
    capabilityBytes: ByteArray,
    origin: String = GROUP_INVITE_DEFAULT_ORIGIN,
): String {
    if (capabilityBytes.size < GROUP_INVITE_INLINE_MIN_LEN) {
        throw GroupInviteUrlException.InlineTooSmall(
            minimum = GROUP_INVITE_INLINE_MIN_LEN,
            got = capabilityBytes.size,
        )
    }
    return "$origin$GROUP_INVITE_URL_PREFIX${base64UrlEncodeNoPad(capabilityBytes)}"
}

// =============================================================
// URL parsing
// =============================================================

/** Discriminated parse result for a `/g/<slug>` segment. */
public sealed class ParsedGroupInvite {
    /** Short form — [blobId] is the 32-byte content-addressed id. Joiner
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

    /** Long form — the bytes are the wire-format `GroupInviteCapability`.
     *  Joiner runs `GroupInviteCapability.decodeAndVerify(...)` directly. */
    public data class Inline(val capabilityBytes: ByteArray) : ParsedGroupInvite() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Inline) return false
            return capabilityBytes.contentEquals(other.capabilityBytes)
        }

        override fun hashCode(): Int = capabilityBytes.contentHashCode()
    }
}

/**
 * Discriminated parse of a URL slug. The dispatch rule is a byte-length test:
 * 32 => pointer (SHA-256), >= 100 => inline. Anything else is invalid input.
 *
 * @throws GroupInviteUrlException.InvalidBase64Url when the slug doesn't decode.
 * @throws GroupInviteUrlException.SlugWrongLength when the decoded byte count
 *         falls outside both branches.
 */
public fun parseGroupInviteSlug(slug: String): ParsedGroupInvite {
    val bytes = try {
        base64UrlDecodeNoPad(slug)
    } catch (e: IllegalArgumentException) {
        throw GroupInviteUrlException.InvalidBase64Url
    }
    return when {
        bytes.size == GROUP_INVITE_BLOB_ID_LEN -> ParsedGroupInvite.Pointer(bytes)
        bytes.size >= GROUP_INVITE_INLINE_MIN_LEN -> ParsedGroupInvite.Inline(bytes)
        else -> throw GroupInviteUrlException.SlugWrongLength(
            blobIdLen = GROUP_INVITE_BLOB_ID_LEN,
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
    ) : GroupInviteUrlException("capabilityBytes must be >= $minimum bytes, got $got")

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
