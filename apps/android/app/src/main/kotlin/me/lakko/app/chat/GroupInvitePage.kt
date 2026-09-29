// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.InviteLinkLengths

/**
 * The group invite page (ADR-0028 v2, `docs/ux/group-invite-page.md`) — the
 * pure half: the link secret in the URL fragment, the route argument that
 * carries the fragment with the slug, and the card copy derived from a preview.
 *
 * A v2 capability carries a sealed preview (group name, inviter name, member
 * count at mint time) encrypted under a key derived from a random 12-byte
 * **link secret** bound to the capability's group id and issuer (ADR-0028
 * amendment of 2026-09-27). The secret rides in the URL **fragment** —
 * `https://lakko.me/g/<8-char id>#<16-char secret>` — because the fragment is
 * the one part of a URL a browser never sends and an app never uploads, so the
 * discovery ledger and every access log stay ciphertext-only.
 *
 * A link without a fragment still works; it only loses the preview. A link
 * WITH a fragment whose preview does not open is refused as damaged: since the
 * ledger id is no longer a hash of the capability, a server could serve a
 * different validly-signed capability under a live id, and the secret not
 * opening its preview is how the page notices. The widths come from the core
 * ([InviteLinkLengths.core]) and are a parameter so a host JVM test can pass
 * them in.
 */

/**
 * Append the link secret to a finished `/g/<slug>` URL as its fragment. Both
 * the pointer and the inline form take the same suffix; `Copy link` copies it
 * whole.
 *
 * @throws IllegalArgumentException when [linkSecret] is not the core's secret
 *   width (12 bytes) — a mint handed the wrong bytes must not produce a link
 *   every opener then refuses as damaged.
 */
public fun appendGroupInviteLinkKey(
    url: String,
    linkSecret: ByteArray,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String {
    require(linkSecret.size == lengths.linkSecretLen) {
        "link secret must be ${lengths.linkSecretLen} bytes, got ${linkSecret.size}"
    }
    return "$url#${base64UrlEncodeNoPad(linkSecret)}"
}

/** What the fragment of a tapped `/g/` link carries. */
public sealed interface GroupInviteFragment {
    /** No fragment (or a blank one): the link works, without a preview. */
    public data object Absent : GroupInviteFragment

    /** A fragment that is not exactly the 16-character / 12-byte secret — the
     *  pre-amendment 43-character key included. The link is refused. */
    public data object Malformed : GroupInviteFragment

    /** The 12-byte link secret. */
    public class Secret(public val bytes: ByteArray) : GroupInviteFragment {
        override fun equals(other: Any?): Boolean = other is Secret && bytes.contentEquals(other.bytes)
        override fun hashCode(): Int = bytes.contentHashCode()
    }
}

/**
 * Read the link secret back out of a URL fragment (the text after `#`,
 * without the `#`). Absent for a missing or blank fragment; Malformed for one
 * that is not exactly [InviteLinkLengths.linkSecretUrlLen] base64url
 * characters decoding to [InviteLinkLengths.linkSecretLen] bytes.
 */
public fun parseGroupInviteFragment(
    fragment: String?,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): GroupInviteFragment {
    val text = fragment?.trim().orEmpty()
    if (text.isEmpty()) return GroupInviteFragment.Absent
    if (text.length != lengths.linkSecretUrlLen) return GroupInviteFragment.Malformed
    val bytes = try {
        base64UrlDecodeNoPad(text)
    } catch (_: IllegalArgumentException) {
        return GroupInviteFragment.Malformed
    }
    return if (bytes.size == lengths.linkSecretLen) {
        GroupInviteFragment.Secret(bytes)
    } else {
        GroupInviteFragment.Malformed
    }
}

/** What the page makes of the fragment and the capability's preview. */
public sealed interface GroupInvitePreviewOutcome {
    /** Show the page; [preview] is `null` for a link without a fragment. */
    public data class Shown(val preview: GroupInvitePreviewUi?) : GroupInvitePreviewOutcome

    /** The link carried a fragment the preview does not open under (or a
     *  malformed one): refuse the link, never show it without a preview. */
    public data object Damaged : GroupInvitePreviewOutcome
}

/**
 * Decide the page's preview, fail closed. [open] opens the capability's
 * sealed preview under the secret, returning `null` when the capability has
 * none and throwing when it does not open; both are [GroupInvitePreviewOutcome.Damaged]
 * when the link carried a secret.
 */
public fun resolveGroupInvitePreview(
    fragment: GroupInviteFragment,
    open: (ByteArray) -> GroupInvitePreviewUi?,
): GroupInvitePreviewOutcome = when (fragment) {
    GroupInviteFragment.Absent -> GroupInvitePreviewOutcome.Shown(null)
    GroupInviteFragment.Malformed -> GroupInvitePreviewOutcome.Damaged
    is GroupInviteFragment.Secret -> {
        val opened = try {
            open(fragment.bytes)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Exception) {
            null
        }
        if (opened != null) GroupInvitePreviewOutcome.Shown(opened) else GroupInvitePreviewOutcome.Damaged
    }
}

/** Thrown by [ChatRepository.inspectGroupInvite] when the link is damaged
 *  (see [GroupInvitePreviewOutcome.Damaged]). */
public class GroupInviteLinkDamaged : Exception("group invite link fragment does not open the preview")

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
 * us `null`, and the page then shows no preview; a fragment that is present
 * but wrong refuses the link).
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

/** Copy for a link whose fragment does not open its preview (fail closed). */
public const val GROUP_INVITE_DAMAGED_COPY: String =
    "This invite link is damaged. Ask for a new link."

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

/** Why a `/g/` link did not open, as the page shows it. */
public enum class GroupInviteOpenFailure {
    /** Does not decode, verify, resolve, or has expired. */
    Invalid,

    /** Carries a fragment its preview does not open under. */
    Damaged,

    /** The discovery service answered `429`: try again later, the link may
     *  well be fine. */
    RateLimited,
}

/** Classify a failure to open a `/g/` link (the page's three refusal states). */
public fun groupInviteOpenFailure(error: Throwable): GroupInviteOpenFailure {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is GroupInviteLinkDamaged) return GroupInviteOpenFailure.Damaged
        cause = cause.cause
    }
    if (me.lakko.app.discovery.isDiscoveryRateLimited(error)) return GroupInviteOpenFailure.RateLimited
    return GroupInviteOpenFailure.Invalid
}
