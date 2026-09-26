// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad

/**
 * The group invite page (ADR-0028 v2, `docs/ux/group-invite-page.md`) — the
 * pure half: the link key in the URL fragment, the route argument that carries
 * the fragment with the slug, and the card copy derived from a preview.
 *
 * A v2 capability carries a sealed preview (group name, inviter name, member
 * count at mint time) encrypted under a random 32-byte **link key**. The key
 * rides in the URL **fragment** — `https://lakko.me/g/<slug>#<key>` — because
 * the fragment is the one part of a URL a browser never sends and an app never
 * uploads, so the discovery ledger and every access log stay ciphertext-only.
 * A link without a fragment still works; it only loses the preview.
 */

/** The link key is 32 bytes, as `lakko-contacts::group_invite::generate_link_key` mints it. */
public const val GROUP_INVITE_LINK_KEY_LEN: Int = 32

/**
 * Append the link key to a finished `/g/<slug>` URL as its fragment. Both the
 * pointer and the inline form take the same suffix; `Copy link` copies it whole.
 *
 * @throws IllegalArgumentException when [linkKey] is not 32 bytes — a mint
 *   handed the wrong bytes must not produce a link that silently has no preview.
 */
public fun appendGroupInviteLinkKey(url: String, linkKey: ByteArray): String {
    require(linkKey.size == GROUP_INVITE_LINK_KEY_LEN) {
        "link key must be $GROUP_INVITE_LINK_KEY_LEN bytes, got ${linkKey.size}"
    }
    return "$url#${base64UrlEncodeNoPad(linkKey)}"
}

/**
 * Read the link key back out of a URL fragment (the text after `#`, without
 * the `#`). `null` for an absent or blank fragment, one that is not base64url,
 * or one that decodes to anything but 32 bytes — every one of those means "no
 * preview", never an error the person sees (spec section 3).
 */
public fun parseGroupInviteLinkKey(fragment: String?): ByteArray? {
    val text = fragment?.trim().orEmpty()
    if (text.isEmpty()) return null
    val bytes = try {
        base64UrlDecodeNoPad(text)
    } catch (_: IllegalArgumentException) {
        return null
    }
    return if (bytes.size == GROUP_INVITE_LINK_KEY_LEN) bytes else null
}

/**
 * The `/g/` slug of a full invite link, with any `#fragment` stripped — the
 * revoke path parses this, not the whole tail. `null` when [link] has no `/g/`
 * segment or an empty slug.
 */
public fun groupInviteSlugOf(link: String): String? {
    val idx = link.lastIndexOf(GROUP_INVITE_URL_PREFIX)
    if (idx < 0) return null
    val slug = link.substring(idx + GROUP_INVITE_URL_PREFIX.length)
        .substringBefore('#')
        .trim()
        .removeSuffix("/")
    return slug.ifEmpty { null }
}

/** The fragment of a full invite link (text after the first `#`), or `null`. */
public fun groupInviteFragmentOf(link: String): String? {
    val idx = link.indexOf('#')
    if (idx < 0) return null
    return link.substring(idx + 1).ifEmpty { null }
}

// =============================================================
// Route argument
// =============================================================

/**
 * What a tapped `/g/` deep-link brings into the app: the slug, and the
 * fragment when the link still had one (a chat app that strips fragments hands
 * us `null`, and the page then shows no preview).
 */
public data class GroupJoinLink(
    val slug: String,
    val fragment: String?,
)

/**
 * One navigation argument for `Routes.GROUP_JOIN` carrying both parts:
 * `<slug>` or `<slug>#<fragment>`. base64url has no `#`, so the split is
 * unambiguous; the router URL-encodes the whole value, so the `#` never reaches
 * the navigation graph as a real fragment.
 */
public fun composeGroupJoinArgument(link: GroupJoinLink): String {
    val fragment = link.fragment?.takeIf { it.isNotEmpty() } ?: return link.slug
    return "${link.slug}#$fragment"
}

/** Reverse of [composeGroupJoinArgument]. An empty fragment reads as absent. */
public fun parseGroupJoinArgument(argument: String): GroupJoinLink {
    val idx = argument.indexOf('#')
    if (idx < 0) return GroupJoinLink(slug = argument, fragment = null)
    return GroupJoinLink(
        slug = argument.substring(0, idx),
        fragment = argument.substring(idx + 1).ifEmpty { null },
    )
}

// =============================================================
// Card copy
// =============================================================

/**
 * A decrypted preview, as the page consumes it: what the inviter's device
 * knew at mint time. Blank names are absent (the core already refuses them,
 * this is defence for the page).
 */
public data class GroupInvitePreviewUi(
    val groupName: String?,
    val inviterName: String?,
    val memberCount: Int,
)

/**
 * The card's text, spec section 3. [headline] is always present; [groupLine]
 * and [membersLine] only when the preview supplies them. The fingerprint foot
 * is [invitedByLine].
 */
public data class GroupInvitePageLines(
    val headline: String,
    val groupLine: String?,
    val membersLine: String?,
)

/** Text of the muted foot under the button: `Invited by <fingerprint>`. */
public fun invitedByLine(fingerprint: String): String = "Invited by $fingerprint"

/**
 * Derive the card copy from a preview (or its absence).
 *
 * - `<Inviter> invites you to` when the inviter's name is known, else
 *   `You are invited to`.
 * - The group name on its own line; a missing name folds `a group` into the
 *   headline and omits the line.
 * - `1 member` / `N members`, the roster at mint time; omitted with no
 *   preview.
 *
 * No preview reads `You are invited to a group` and nothing else: nothing
 * invents a name, and the page does not say why the preview is missing.
 */
public fun groupInvitePageLines(preview: GroupInvitePreviewUi?): GroupInvitePageLines {
    val inviter = preview?.inviterName?.trim()?.takeIf { it.isNotEmpty() }
    val groupName = preview?.groupName?.trim()?.takeIf { it.isNotEmpty() }
    val subject = if (inviter != null) "$inviter invites you to" else "You are invited to"
    val headline = if (groupName != null) subject else "$subject a group"
    val membersLine = preview?.let { memberCountLine(it.memberCount) }
    return GroupInvitePageLines(
        headline = headline,
        groupLine = groupName,
        membersLine = membersLine,
    )
}

/** `1 member` / `N members`. */
public fun memberCountLine(count: Int): String =
    if (count == 1) "1 member" else "$count members"

/** Copy for a link that does not decode, verify, or has expired. */
public const val GROUP_INVITE_INVALID_COPY: String = "This invite link is invalid or has expired."

/** The one control of the card. */
public const val GROUP_INVITE_REQUEST_TO_JOIN: String = "Request to join"

/**
 * What [ChatRepository.inspectGroupInvite] hands the page: the issuer's key
 * fingerprint (the one thing on the page the inviter cannot fake), whether the
 * link is still within its expiry, and the opened preview or `null`.
 */
public data class GroupInviteCard(
    val fingerprint: String,
    val valid: Boolean,
    val preview: GroupInvitePreviewUi?,
)
