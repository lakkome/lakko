// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discovery

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.l10n.UiText

/*
 * Sealed invite links (ADR-0028 / ADR-0009, the sealed-link amendment of
 * 2026-09-28) — the pure half shared by the three link kinds (`/i/`, `/c/`,
 * `/g/`).
 *
 * Composing a capability returns an `InviteLinkMint { sealed, linkSecret,
 * issuerPubkey }`: the capability encrypted under a key derived from a random
 * 12-byte link secret. The discovery ledger stores only `sealed`; the secret
 * rides in the URL fragment, which a browser never sends and an app never
 * uploads. A link is therefore
 *
 *     https://<host>/<k>/<8-char id>#<16-char secret>
 *
 * or, only when the upload failed in transport,
 *
 *     https://<host>/<k>/<base64url sealed>#<16-char secret>
 *
 * A link without its fragment, with a malformed one, or whose sealed bytes do
 * not open under it is DAMAGED on every kind, with one sentence
 * ([INVITE_LINK_DAMAGED_COPY]). A missing or malformed fragment is refused
 * before any request is made.
 *
 * Nothing here touches the native core: the open itself is passed in, so a
 * host JVM test drives every rule.
 */

/** The one sentence a damaged link shows, on every link kind. */
public val INVITE_LINK_DAMAGED_COPY: UiText = UiText("invite.link.damaged")

/** A link that cannot be opened: fragment missing or malformed, or the sealed
 *  bytes do not open under it. */
public class InviteLinkDamaged(message: String = "invite link is damaged") : Exception(message)

/** `true` when [error] or anything in its cause chain is [InviteLinkDamaged]. */
public fun isInviteLinkDamaged(error: Throwable): Boolean {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is InviteLinkDamaged) return true
        cause = cause.cause
    }
    return false
}

/**
 * The link secret a URL fragment carries (the text after `#`, without it), or
 * `null` when the fragment is missing, blank, or not exactly
 * [InviteLinkLengths.linkSecretUrlLen] base64url characters decoding to
 * [InviteLinkLengths.linkSecretLen] bytes. `null` is the damaged state.
 */
public fun inviteLinkSecretOf(
    fragment: String?,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): ByteArray? {
    val text = fragment?.trim().orEmpty()
    if (text.length != lengths.linkSecretUrlLen) return null
    val bytes = try {
        base64UrlDecodeNoPad(text)
    } catch (_: IllegalArgumentException) {
        return null
    }
    return bytes.takeIf { it.size == lengths.linkSecretLen }
}

/** [inviteLinkSecretOf], throwing [InviteLinkDamaged] instead of `null`. */
public fun requireInviteLinkSecret(
    fragment: String?,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): ByteArray = inviteLinkSecretOf(fragment, lengths)
    ?: throw InviteLinkDamaged("invite link fragment is missing or malformed")

/**
 * Open [sealed] under [secret] with [open] (the core's
 * `XCapability.openLink(sealed, linkSecret)`). Any failure of the open is the
 * damaged state: a wrong or truncated secret, a tampered link, and a blob a
 * server swapped in under a live id look identical by design.
 */
public fun <T> openSealedInviteLink(
    sealed: ByteArray,
    secret: ByteArray,
    open: (sealed: ByteArray, secret: ByteArray) -> T,
): T = try {
    open(sealed, secret)
} catch (cancellation: kotlinx.coroutines.CancellationException) {
    throw cancellation
} catch (cause: Exception) {
    throw InviteLinkDamaged("invite link does not open under its fragment").also { it.initCause(cause) }
}

/**
 * Append the link secret to a finished `<origin>/<k>/<slug>` URL as its
 * fragment. The share action and `Copy link` must carry the whole string.
 *
 * @throws IllegalArgumentException when [linkSecret] is not the core's secret
 *   width — a mint handed the wrong bytes must not produce a link every opener
 *   then refuses as damaged.
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

/**
 * A tapped or pasted link carried through the app as one string: the slug and
 * the fragment, `<slug>` or `<slug>#<fragment>`. base64url has no `#`, so the
 * split is unambiguous.
 */
public data class InviteLinkArgument(val slug: String, val fragment: String?) {
    /** The single-string form. */
    public fun encode(): String {
        val f = fragment?.takeIf { it.isNotEmpty() } ?: return slug
        return "$slug#$f"
    }

    public companion object {
        /** Reverse of [encode]. An empty fragment reads as absent. */
        public fun decode(argument: String): InviteLinkArgument {
            val idx = argument.indexOf('#')
            if (idx < 0) return InviteLinkArgument(slug = argument, fragment = null)
            return InviteLinkArgument(
                slug = argument.substring(0, idx),
                fragment = argument.substring(idx + 1).ifEmpty { null },
            )
        }
    }
}

/**
 * What an upload to one of the three ledgers carries (all but `sealed` in
 * clear, for the fetch filter, the consume ceiling, the pruner and the
 * issuer's revoke): the sealed link, the 32-byte key that signed the
 * capability, and the capability's `expiry_ms` / `max_uses`. [label] and
 * [bootstrapReferral] are sent by the `/i/` ledger only.
 */
public class SealedInviteUpload(
    public val sealed: ByteArray,
    public val issuerPubkey: ByteArray,
    public val expiryMs: Long,
    public val maxUses: Long,
    public val label: String = "",
    public val bootstrapReferral: Boolean = false,
)

/**
 * The shareable URL for a minted link: the pointer form over [blobId] when the
 * upload landed, the inline form over [sealed] when it failed in transport;
 * either way with the fragment.
 */
public fun composeSealedInviteUrl(
    origin: String,
    prefix: String,
    blobId: ByteArray?,
    sealed: ByteArray,
    linkSecret: ByteArray,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    val slug = if (blobId != null) {
        require(blobId.size == lengths.blobIdLen) {
            "blobId must be ${lengths.blobIdLen} bytes, got ${blobId.size}"
        }
        base64UrlEncodeNoPad(blobId)
    } else {
        base64UrlEncodeNoPad(sealed)
    }
    return appendInviteLinkSecret("$origin$prefix$slug", linkSecret, lengths)
}

/**
 * `true` when an upload failure is a TRANSPORT failure (no route, DNS, TLS, a
 * timeout) — the only case a sealed link falls back to the inline form. A
 * server refusal (any HTTP status) is not.
 */
public fun isInviteUploadTransportFailure(error: Throwable): Boolean {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is DiscoveryHttpError) return false
        if (cause is DiscoveryNetworkError || cause is java.io.IOException) return true
        cause = cause.cause
    }
    return false
}

/**
 * docs/ux/client-redesign.md section 5.3 — the full shareable URL of a pointer
 * link from the server's list whose secret this device kept when it created
 * the link: `<origin><prefix><slug>#<secret>`. `null` when this device holds no
 * secret for it, or the held text is not a well-formed link secret (a link
 * built from it would open as damaged everywhere, so it is not offered).
 */
public fun heldInviteLinkUrl(
    origin: String,
    prefix: String,
    slug: String,
    heldSecret: String?,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String? {
    val secret = heldSecret ?: return null
    val bytes = inviteLinkSecretOf(secret, lengths) ?: return null
    return appendInviteLinkSecret("$origin$prefix$slug", bytes, lengths)
}
