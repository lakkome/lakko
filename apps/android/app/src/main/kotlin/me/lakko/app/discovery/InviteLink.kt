// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * The sealed invite link, shared by all three kinds (ADR-0028 amendment of
 * 2026-09-28, ADR-0009 amendment of the same day).
 *
 * Every `/i/`, `/c/` and `/g/` link reads
 * `https://<host>/<k>/<8-char id>#<16-char secret>` (pointer) or
 * `https://<host>/<k>/<base64url sealed blob>#<16-char secret>` (the inline
 * fallback, used only when the upload fails on transport). The capability is
 * encrypted under the 12-byte secret in the fragment, the one part of a URL a
 * browser never sends and an app never uploads; the ledger holds the sealed
 * blob only.
 *
 * A link whose fragment is missing, is not exactly the 16-character /
 * 12-byte secret, or does not open the blob is **damaged**, on every kind.
 * There is no fallback without the secret: nothing opens without it.
 *
 * The widths come from the core ([InviteLinkLengths.core]) and are a
 * parameter so a host JVM test (no native library) can pass them in.
 */

/** What the fragment of a tapped invite link carries. */
public sealed interface InviteLinkFragment {
    /** No fragment, or a blank one: the link is damaged. */
    public data object Absent : InviteLinkFragment

    /** A fragment that is not exactly the 16-character / 12-byte secret: the
     *  link is damaged. */
    public data object Malformed : InviteLinkFragment

    /** The 12-byte link secret. */
    public class Secret(public val bytes: ByteArray) : InviteLinkFragment {
        override fun equals(other: Any?): Boolean = other is Secret && bytes.contentEquals(other.bytes)
        override fun hashCode(): Int = bytes.contentHashCode()
        override fun toString(): String = "Secret(<redacted>)"
    }
}

/**
 * Read the link secret out of a URL fragment (the text after `#`, without
 * the `#`). [InviteLinkFragment.Absent] for a missing or blank fragment,
 * [InviteLinkFragment.Malformed] for one that is not exactly
 * [InviteLinkLengths.linkSecretUrlLen] base64url characters decoding to
 * [InviteLinkLengths.linkSecretLen] bytes.
 */
public fun parseInviteLinkFragment(
    fragment: String?,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): InviteLinkFragment {
    val text = fragment?.trim().orEmpty()
    if (text.isEmpty()) return InviteLinkFragment.Absent
    if (text.length != lengths.linkSecretUrlLen) return InviteLinkFragment.Malformed
    val bytes = try {
        base64UrlDecodeNoPad(text)
    } catch (_: IllegalArgumentException) {
        return InviteLinkFragment.Malformed
    }
    return if (bytes.size == lengths.linkSecretLen) {
        InviteLinkFragment.Secret(bytes)
    } else {
        InviteLinkFragment.Malformed
    }
}

/** The secret of a fragment, or `null` when the link is damaged. */
public fun inviteLinkSecretOrNull(
    fragment: String?,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): ByteArray? = (parseInviteLinkFragment(fragment, lengths) as? InviteLinkFragment.Secret)?.bytes

/**
 * Append the link secret to a finished `/<k>/<slug>` URL as its fragment.
 *
 * @throws IllegalArgumentException when [linkSecret] is not the core's secret
 *   width (12 bytes): a mint handed the wrong bytes must not produce a link
 *   every opener then refuses as damaged.
 */
public fun appendInviteLinkSecret(
    url: String,
    linkSecret: ByteArray,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    require(linkSecret.size == lengths.linkSecretLen) {
        "link secret must be ${lengths.linkSecretLen} bytes, got ${linkSecret.size}"
    }
    return "$url#${base64UrlEncodeNoPad(linkSecret)}"
}

/** The fragment of a full link (text after the first `#`), or `null`. */
public fun inviteLinkFragmentOf(link: String): String? {
    val idx = link.indexOf('#')
    if (idx < 0) return null
    return link.substring(idx + 1).ifEmpty { null }
}

/**
 * The slug of a full link after [prefix] (`/i/`, `/c/`, `/g/`), with any
 * `#fragment` stripped. `null` when [link] has no such segment or an empty
 * slug.
 */
public fun inviteLinkSlugOf(link: String, prefix: String): String? {
    val beforeFragment = link.substringBefore('#')
    val idx = beforeFragment.lastIndexOf(prefix)
    if (idx < 0) return null
    val slug = beforeFragment.substring(idx + prefix.length).trim().removeSuffix("/")
    return slug.ifEmpty { null }
}

/**
 * What a tapped invite deep-link brings into the app: the slug, and the
 * fragment when the link still had one (a chat app that strips fragments
 * hands us `null`, and the link is then damaged).
 */
public data class InviteLinkTap(
    val slug: String,
    val fragment: String?,
) {
    override fun toString(): String = "InviteLinkTap(slug=$slug, fragment=${if (fragment == null) "none" else "<redacted>"})"
}

/**
 * One navigation / state argument carrying both parts: `<slug>` or
 * `<slug>#<fragment>`. base64url has no `#`, so the split is unambiguous; a
 * router URL-encodes the whole value, so the `#` never reaches a navigation
 * graph as a real fragment.
 */
public fun composeInviteLinkArgument(tap: InviteLinkTap): String {
    val fragment = tap.fragment?.takeIf { it.isNotEmpty() } ?: return tap.slug
    return "${tap.slug}#$fragment"
}

/** Reverse of [composeInviteLinkArgument]. An empty fragment reads as absent. */
public fun parseInviteLinkArgument(argument: String): InviteLinkTap {
    val idx = argument.indexOf('#')
    if (idx < 0) return InviteLinkTap(slug = argument, fragment = null)
    return InviteLinkTap(
        slug = argument.substring(0, idx),
        fragment = argument.substring(idx + 1).ifEmpty { null },
    )
}

/**
 * Thrown by an open path when the link cannot be opened with the secret it
 * carries (missing, malformed, or not opening the sealed blob).
 */
public class InviteLinkDamagedException(cause: Throwable? = null) :
    Exception("invite link is damaged", cause)

/**
 * `true` when [error] (or anything in its cause chain) is a damaged link:
 * [InviteLinkDamagedException] or the core's `FfiException.InviteLinkDamaged`.
 */
public fun isInviteLinkDamaged(error: Throwable): Boolean {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is InviteLinkDamagedException) return true
        if (cause is FfiException.InviteLinkDamaged) return true
        cause = cause.cause
    }
    return false
}

/** The copy for a damaged invite link, every kind (`docs/ux/group-invite-page.md`). */
public const val INVITE_LINK_DAMAGED_COPY: String =
    "This invite link is damaged. Ask for a new link."

/**
 * The size of a sealed blob from the size of the capability it seals:
 * `kind(1) ‖ version(1) ‖ nonce(12) ‖ ciphertext ‖ tag(16)`
 * (`lakko_contacts::link_seal::LINK_SEAL_OVERHEAD`).
 */
public const val INVITE_LINK_SEAL_OVERHEAD: Int = 30

/** A `/i/` slug: the 6-byte ledger id, or the inline sealed blob. */
public sealed interface ParsedContactInvite {
    /** The ledger id; fetch the sealed blob by it. */
    public class Pointer(public val blobId: ByteArray) : ParsedContactInvite {
        override fun equals(other: Any?): Boolean = other is Pointer && blobId.contentEquals(other.blobId)
        override fun hashCode(): Int = blobId.contentHashCode()
    }

    /** The sealed blob itself (the upload failed on transport at mint). */
    public class Inline(public val sealed: ByteArray) : ParsedContactInvite {
        override fun equals(other: Any?): Boolean = other is Inline && sealed.contentEquals(other.sealed)
        override fun hashCode(): Int = sealed.contentHashCode()
    }
}

/** Floor on an inline `/i/` slug: a sealed contact capability is well above
 *  it (two 32-byte keys, a 64-byte signature and the 30-byte seal), the
 *  6-byte pointer well below. */
public const val CONTACT_INVITE_INLINE_MIN_LEN: Int = 100

/**
 * Discriminate a `/i/` slug (fragment already stripped): exactly
 * [InviteLinkLengths.blobIdUrlLen] characters decoding to the ledger-id width
 * is a pointer; a base64url blob of at least [CONTACT_INVITE_INLINE_MIN_LEN]
 * bytes is inline; anything else, the pre-amendment 43-character id included,
 * is `null`.
 */
public fun parseContactInviteSlug(
    slug: String,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): ParsedContactInvite? {
    val text = slug.trim()
    decodeInviteBlobId(text, lengths)?.let { return ParsedContactInvite.Pointer(it) }
    val bytes = try {
        base64UrlDecodeNoPad(text)
    } catch (_: IllegalArgumentException) {
        return null
    }
    return if (bytes.size >= CONTACT_INVITE_INLINE_MIN_LEN) ParsedContactInvite.Inline(bytes) else null
}
