// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.discovery.INVITE_LINK_DAMAGED_COPY
import me.lakko.app.discovery.InviteLinkFragment
import me.lakko.app.discovery.inviteLinkSlugOf
import me.lakko.app.discovery.isInviteLinkDamaged

/**
 * The group invite page (ADR-0028 v2, `docs/ux/group-invite-page.md`) — the
 * pure half: the route argument that carries the fragment with the slug, the
 * preview decision, and the card copy derived from a preview.
 *
 * Since the sealed-link amendment of 2026-09-28 the whole capability is
 * encrypted under the 12-byte link secret in the URL **fragment** —
 * `https://lakko.me/g/<8-char id>#<16-char secret>` — and the same secret
 * opens the capability's sealed preview (group name, inviter name, member
 * count at mint time). The fragment is the one part of a URL a browser never
 * sends and an app never uploads, so the discovery ledger and every access log
 * stay ciphertext-only. A link without the fragment, or with one that does not
 * open the blob, is **damaged**: nothing opens without it. The fragment
 * helpers are shared by all kinds and live in `discovery/InviteLink.kt`.
 */

/** What the page makes of the opened capability's preview. */
public sealed interface GroupInvitePreviewOutcome {
    /** Show the page; [preview] is `null` for a capability minted without
     *  one (the capability itself opened under the secret, so it is the one
     *  the inviter made). */
    public data class Shown(val preview: GroupInvitePreviewUi?) : GroupInvitePreviewOutcome

    /** The link carried no usable secret, or the preview does not open under
     *  it: refuse the link, never show it without a preview. */
    public data object Damaged : GroupInvitePreviewOutcome
}

/**
 * Decide the page's preview, fail closed. [open] opens the capability's
 * sealed preview under the secret, returning `null` when the capability has
 * none and throwing when it does not open. A missing or malformed fragment is
 * [GroupInvitePreviewOutcome.Damaged] without calling [open]; so is a preview
 * that throws. A capability that carries no preview is shown without one: the
 * capability already opened under the same secret, so a server could not
 * have substituted it.
 */
public fun resolveGroupInvitePreview(
    fragment: InviteLinkFragment,
    open: (ByteArray) -> GroupInvitePreviewUi?,
): GroupInvitePreviewOutcome = when (fragment) {
    InviteLinkFragment.Absent, InviteLinkFragment.Malformed -> GroupInvitePreviewOutcome.Damaged
    is InviteLinkFragment.Secret -> try {
        GroupInvitePreviewOutcome.Shown(open(fragment.bytes))
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (_: Exception) {
        GroupInvitePreviewOutcome.Damaged
    }
}

/** Thrown by [ChatRepository.inspectGroupInvite] when the link is damaged
 *  (see [GroupInvitePreviewOutcome.Damaged]). */
public class GroupInviteLinkDamaged : Exception("group invite link is damaged")

/**
 * The `/g/` slug of a full invite link, with any `#fragment` stripped — the
 * revoke path parses this, not the whole tail. `null` when [link] has no `/g/`
 * segment or an empty slug.
 */
public fun groupInviteSlugOf(link: String): String? = inviteLinkSlugOf(link, GROUP_INVITE_URL_PREFIX)

// =============================================================
// Route argument
// =============================================================

/**
 * What a tapped `/g/` deep-link brings into the app: the slug, and the
 * fragment when the link still had one (a chat app that strips fragments hands
 * us `null`, and the link is then damaged).
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

/** Copy for a damaged link (fail closed). */
public const val GROUP_INVITE_DAMAGED_COPY: String = INVITE_LINK_DAMAGED_COPY

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

    /** No usable secret in the fragment, or the blob does not open under
     *  it. */
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
    if (isInviteLinkDamaged(error)) return GroupInviteOpenFailure.Damaged
    if (me.lakko.app.discovery.isDiscoveryRateLimited(error)) return GroupInviteOpenFailure.RateLimited
    return GroupInviteOpenFailure.Invalid
}
