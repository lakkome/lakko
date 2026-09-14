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
 * Two surfaces:
 *
 *   - URL plumbing — [composePointerCallInviteUrl] for the
 *     production short form (`/c/<blob_id_b64url>`),
 *     [composeInlineCallInviteUrl] for the censored-region
 *     fallback (the bytes are encoded into the URL itself, no
 *     `/discovery/call-invite` round-trip needed), and
 *     [parseCallInviteSlug] for the joiner-side dispatcher.
 *   - Form-side enums — [CallInviteExpiryPreset],
 *     [CallInviteMaxUsesPreset], [CallMediaPreset] —
 *     translate UI dropdown choices into the wire-level
 *     `composeCallInviteCapability` arguments.
 */

/** Path prefix every call-invite URL starts with. Mirror of
 *  the web `CALL_INVITE_URL_PREFIX = "/c/"`. */
public const val CALL_INVITE_URL_PREFIX: String = "/c/"

/** Default origin Lakko shares its invite links under. Stays
 *  in sync with the web + iOS sides; a link generated on
 *  Android works when consumed on any platform. */
public const val CALL_INVITE_DEFAULT_ORIGIN: String = "https://lakko.me"

/** Hard cap on the human-visible label, in bytes (UTF-8).
 *  Mirrors `lakko_contacts::invite::MAX_LABEL_LEN` (64). */
public const val CALL_INVITE_LABEL_MAX_LEN: Int = 64

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

// =============================================================
// Form-side enums
// =============================================================

/**
 * Discrete UI choices the "Create call link" form exposes for
 * the `expiry_ms` field. Same recipes as the contact-invite
 * form — keeps the UX consistent across the two invite kinds.
 */
public enum class CallInviteExpiryPreset {
    /** A call a user is about to hold now — the "Create call link" form's
     *  shortest choice. */
    OneHour,
    OneDay,
    SevenDays,
    ThirtyDays,
    Never;

    /**
     * Translate into the UNIX-millis argument
     * `composeCallInviteCapability` expects. [Never] resolves
     * to `0` — the `CALL_NEVER_EXPIRES` sentinel — and the UI
     * MUST surface a warning before allowing it.
     */
    public fun toMs(nowMs: Long): Long = when (this) {
        OneHour -> nowMs + 3_600_000L
        OneDay -> nowMs + 1L * 86_400_000
        SevenDays -> nowMs + 7L * 86_400_000
        ThirtyDays -> nowMs + 30L * 86_400_000
        Never -> 0L
    }
}

/** Discrete UI choices for `max_uses`. */
public enum class CallInviteMaxUsesPreset {
    Single,
    Ten,
    Unlimited;

    /**
     * Translate into the `max_uses` u32 the wire format
     * carries. [Single] resolves to `0` — the
     * `CALL_SINGLE_USE` sentinel. [Unlimited] saturates at
     * `u32::MAX` (which Kotlin / JVM represents as `UInt.MAX_VALUE`).
     */
    public fun toValue(): UInt = when (this) {
        Single -> 0u
        Ten -> 10u
        Unlimited -> UInt.MAX_VALUE
    }
}

/**
 * Discrete UI choices the "Create call link" form exposes for
 * the media kind — surfaces the same `audio-only` vs
 * `audio-video` toggle the `CallInviteMedia` wire flags carry.
 */
public enum class CallMediaPreset {
    Audio,
    AudioVideo,
}

// =============================================================
// Human-visible renderers
// =============================================================

/** Convert a wire `max_uses` u32 back into a human-visible
 *  rendering for the landing page. */
public fun describeCallInviteMaxUses(maxUses: UInt): String = when (maxUses) {
    0u -> "single-use"
    UInt.MAX_VALUE -> "unlimited"
    else -> "up to $maxUses uses"
}

/**
 * Render an `expiry_ms` (UNIX millis) for the landing page —
 * `"never expires"` for the `CALL_NEVER_EXPIRES` sentinel,
 * otherwise a relative "in 6 days" / "expired 3 hours ago"
 * string.
 */
public fun describeCallInviteExpiry(expiryMs: Long, nowMs: Long): String {
    if (expiryMs == 0L) return "never expires"
    val deltaMs = expiryMs - nowMs
    val futureLabel = if (deltaMs >= 0) "in" else "expired"
    val suffix = if (deltaMs >= 0) "" else " ago"
    val absMs = kotlin.math.abs(deltaMs)
    val day = 86_400_000L
    val hour = 3_600_000L
    val minute = 60_000L
    return when {
        absMs >= day -> {
            val days = (absMs + day / 2) / day
            "$futureLabel $days day${if (days == 1L) "" else "s"}$suffix"
        }
        absMs >= hour -> {
            val hours = (absMs + hour / 2) / hour
            "$futureLabel $hours hour${if (hours == 1L) "" else "s"}$suffix"
        }
        else -> {
            val mins = maxOf(1L, (absMs + minute / 2) / minute)
            "$futureLabel $mins minute${if (mins == 1L) "" else "s"}$suffix"
        }
    }
}
