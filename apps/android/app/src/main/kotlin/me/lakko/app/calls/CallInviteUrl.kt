// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad

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
 * production short form (`/c/<blob_id_b64url>`),
 * [composeInlineCallInviteUrl] for the censored-region fallback
 * (the bytes are encoded into the URL itself, no
 * `/discovery/call-invite` round-trip needed), and
 * [parseCallInviteSlug] for the joiner-side dispatcher.
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

/** Length of a content-addressed `blob_id` from
 *  `services/discovery/src/call_invite.rs` — SHA-256, 32 bytes. */
public const val CALL_INVITE_BLOB_ID_LEN: Int = 32

/** Lower bound on the inline-bytes form. The 3c3 wire format
 *  with a 256-byte one-time KP tops out around 500 bytes; we
 *  pin the floor at 400 to leave headroom for KP formats that
 *  might shrink slightly. */
public const val CALL_INVITE_INLINE_MIN_LEN: Int = 400

// =============================================================
// URL builders
// =============================================================

/**
 * Auto-pick the URL form by argument shape — pointer (32
 * bytes) when the caller has uploaded the capability and
 * wants the short form, inline-bytes (≥ 400 bytes) when the
 * caller is staying server-free.
 *
 * Both produce a `<origin>/c/<base64url>` shape; the joiner
 * side discriminates via [parseCallInviteSlug] on the byte
 * length.
 */
public fun composeCallInviteUrl(
    bytes: ByteArray,
    origin: String = CALL_INVITE_DEFAULT_ORIGIN,
): String {
    return "$origin$CALL_INVITE_URL_PREFIX${base64UrlEncodeNoPad(bytes)}"
}

/**
 * Builder for the **pointer** URL form — the short
 * `<origin>/c/<blob_id_b64url>` shape backed by
 * `/discovery/call-invite/<blob_id>` lookup.
 *
 * @throws CallInviteUrlException.WrongBlobIdLength when
 *         `blobId.size != 32`.
 */
public fun composePointerCallInviteUrl(
    blobId: ByteArray,
    origin: String = CALL_INVITE_DEFAULT_ORIGIN,
): String {
    if (blobId.size != CALL_INVITE_BLOB_ID_LEN) {
        throw CallInviteUrlException.WrongBlobIdLength(
            expected = CALL_INVITE_BLOB_ID_LEN,
            got = blobId.size,
        )
    }
    return composeCallInviteUrl(blobId, origin)
}

/**
 * Builder for the **inline-bytes** URL form — the long shape
 * that carries the capability inside the URL itself, no
 * server round-trip on open. Reserved for the censored-region
 * fallback; the pointer form is the production default.
 *
 * @throws CallInviteUrlException.InlineTooSmall when
 *         `capabilityBytes.size < 400`.
 */
public fun composeInlineCallInviteUrl(
    capabilityBytes: ByteArray,
    origin: String = CALL_INVITE_DEFAULT_ORIGIN,
): String {
    if (capabilityBytes.size < CALL_INVITE_INLINE_MIN_LEN) {
        throw CallInviteUrlException.InlineTooSmall(
            minimum = CALL_INVITE_INLINE_MIN_LEN,
            got = capabilityBytes.size,
        )
    }
    return composeCallInviteUrl(capabilityBytes, origin)
}

// =============================================================
// URL parsing
// =============================================================

/** Discriminated parse result for a `/c/<slug>` segment. */
public sealed class ParsedCallInvite {
    /** Short form — [blobId] is the 32-byte content-
     *  addressed id. Joiner fetches via
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

    /** Long form — the bytes are the wire-format
     *  `CallInviteCapability`. Joiner runs
     *  `CallInviteCapability.decodeAndVerify(...)` directly. */
    public data class Inline(val capabilityBytes: ByteArray) : ParsedCallInvite() {
        override fun equals(other: Any?): Boolean {
            if (this === other) return true
            if (other !is Inline) return false
            return capabilityBytes.contentEquals(other.capabilityBytes)
        }

        override fun hashCode(): Int = capabilityBytes.contentHashCode()
    }
}

/**
 * Discriminated parse of a URL slug. The dispatch rule is a
 * clean byte-length test: 32 ⇒ pointer (SHA-256), ≥ 400 ⇒
 * inline (3c3 wire-format minimum). Anything else is invalid.
 *
 * @throws CallInviteUrlException.InvalidBase64Url when the
 *         slug doesn't decode.
 * @throws CallInviteUrlException.SlugWrongLength when the
 *         decoded byte count falls outside both branches.
 */
public fun parseCallInviteSlug(slug: String): ParsedCallInvite {
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
        bytes.size == CALL_INVITE_BLOB_ID_LEN -> ParsedCallInvite.Pointer(bytes)
        bytes.size >= CALL_INVITE_INLINE_MIN_LEN -> ParsedCallInvite.Inline(bytes)
        else -> throw CallInviteUrlException.SlugWrongLength(
            blobIdLen = CALL_INVITE_BLOB_ID_LEN,
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
    ) : CallInviteUrlException("capabilityBytes must be >= $minimum bytes, got $got")

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
